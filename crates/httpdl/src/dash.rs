//! DASH（ISO/IEC 23009-1，MPD 清单）下载支持（C-DASH）。
//!
//! v1 范围（与 HLS v1 同哲学：static VOD 最小可用面 + 明确拒绝清单）：
//! - **识别**：`HttpEngine::add` 对 URL 路径以 `.mpd` 结尾（剥 query/fragment，
//!   大小写无关）的任务分流至本模块；其余任务照常走探测/分段链。
//! - **选流**：优先 contentType/mimeType 为 video 的 AdaptationSet，取其中
//!   BANDWIDTH 最高的 Representation（质量优先，同 HLS master 变体策略）；
//!   全清单无视频轨（纯音频内容）→ 回退取最高码率音频轨。v1 仅取单轨
//!   （音视频分轨清单不混流，产出无音轨视频文件——明示边界）。
//! - **分段寻址**：`SegmentTemplate`（`duration` 属性 → $Number$ 定址，段数 =
//!   ceil(Period 时长 × timescale / duration)；`SegmentTimeline` → $Time$
//!   定址，`<S t d r>` 展开）与 `SegmentList`（显式 `<SegmentURL>` 列表）；
//!   无分段且 Representation 自带 BaseURL → 单文件整下。`SegmentBase`
//!   （单文件索引 byte-range）/ `xlink` / 多 Period / `type="dynamic"`
//!   （live）/ DRM（ContentProtection）/ `$SubNumber$` / `mediaRange` →
//!   v1 明确拒绝（错误信息给出替代路径）。
//! - **URL 构建**：模板替换 `$RepresentationID$`/`$Bandwidth$`/`$Number$`/
//!   `$Time$`（含 `%0Nd` 零宽格式，`$$` 转义）→ 按 BaseURL 链（MPD →
//!   Period → AdaptationSet → Representation，逐级相对解析）落绝对 URL；
//!   SegmentTemplate 段对 Representation base 解析，SegmentList 对其所属
//!   元素 base 解析（spec 5.3.9.4.2 口径）。
//! - **下载**：init 段（如有）+ 媒体段**顺序**下载逐段 append 到 `.part`
//!   （fMP4 init+media 裸拼接 = 标准可播放流文件，由播放器封装解码），
//!   段账本（`.part.dash-ledger` JSON）记录已完成段数与字节；恢复 = 重拉
//!   MPD → 对账（清单指纹/段数）→ 从断点段续传。
//! - **进度**：段长事先未知（v1 不预 HEAD）→ total = 0（未知，HLS 同口径），
//!   done 按字节累计。
use crate::hls::resolve_url;
use crate::rate::RateLimiter;
use smart_dl_core::types::EngineError;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// abort 约定错误串：pause()/remove() 置位 abort flag → 段间检查点返回此
/// 错误，spawn_stream_loop 识别后静默返回（状态由 pause()/remove() 管理）。
pub const DASH_ABORTED_MSG: &str = "dash-aborted";

/// 段数安全上限（duration 定址段数推导 / SegmentTimeline 展开共用——
/// 防御异常清单撑爆内存；真实 VOD 段数 ≤ 数千）。
const MAX_SEGMENTS: usize = 1_000_000;

/// 解析产出的下载计划：init 段（可选）+ 有序媒体段（绝对 URL）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashPlan {
    pub init: Option<String>,
    pub segments: Vec<String>,
}

/// `.mpd` 后缀识别（剥 query/fragment，大小写无关；B1 metalink 同口径）。
pub fn is_dash_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_lowercase();
    lower.ends_with(".mpd")
}

/// 派生落盘名：`<mpd 文件名>.mp4`（x/path/ep.mpd → ep.mp4）。
pub fn derive_mp4_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let stem = last
        .strip_suffix(".mpd")
        .or_else(|| last.strip_suffix(".MPD"))?;
    if stem.is_empty() {
        return None;
    }
    smart_dl_core::session::output::sanitize_rel(&format!("{stem}.mp4"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// XML 树（quick-xml 事件 → 轻量元素树；MPD 需结构性访问 AS→Rep→Template→
// Timeline 嵌套，事件流状态机不可读且易漏，树规模 = 清单 KB 级可忽略）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Elt {
    /// 本地名（namespace 剥离——MPD 命名空间版本各异，按本地名匹配）。
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Elt>,
    /// 聚合文本子节点（BaseURL 等文本元素用）。
    text: String,
}

impl Elt {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Elt> {
        self.children.iter().find(|e| e.name == name)
    }
}

fn build_tree(xml: &str) -> Result<Vec<Elt>, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    let mut reader = Reader::from_str(xml);
    // quick-xml 0.37：trim 开关收拢进 Config（metalink 同口径）。
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Elt> = Vec::new();
    let mut roots: Vec<Elt> = Vec::new();

    macro_rules! attach {
        ($elt:expr) => {
            if let Some(top) = stack.last_mut() {
                top.children.push($elt);
            } else {
                roots.push($elt);
            }
        };
    }

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let elt = elt_of(&e, &reader)?;
                stack.push(elt);
            }
            Ok(Event::Empty(e)) => {
                let elt = elt_of(&e, &reader)?;
                attach!(elt);
            }
            Ok(Event::End(_)) => {
                let elt = stack.pop().ok_or("DASH 清单结构非法：多余的结束标签")?;
                attach!(elt);
            }
            Ok(Event::Text(t)) => {
                let s = t
                    .unescape()
                    .map(|c| c.into_owned())
                    .map_err(|e| format!("DASH 清单文本转义非法: {e}"))?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("DASH XML 解析失败: {e}")),
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("DASH 清单结构非法：标签未闭合".into());
    }
    Ok(roots)
}

fn elt_of(
    e: &quick_xml::events::BytesStart<'_>,
    reader: &quick_xml::Reader<&[u8]>,
) -> Result<Elt, String> {
    let mut elt = Elt {
        name: String::from_utf8_lossy(e.name().local_name().as_ref()).into_owned(),
        attrs: Vec::new(),
        children: Vec::new(),
        text: String::new(),
    };
    for a in e.attributes() {
        let a = a.map_err(|er| format!("DASH 清单 <{}> 属性解析失败: {er}", elt.name))?;
        let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        let v = a
            .decode_and_unescape_value(reader.decoder())
            .map_err(|er| format!("DASH 清单 <{}> 属性值非法: {er}", elt.name))?;
        elt.attrs.push((k, v.into_owned()));
    }
    Ok(elt)
}

// ---------------------------------------------------------------------------
// ISO 8601 duration（P[nY][nM][nW][nD][T[nH][nM][nS]]；Y/M/W 按固定换算——
// 媒体时长场景年月月缺省语义影响可忽略）
// ---------------------------------------------------------------------------

/// 解析 ISO 8601 时长 → 秒。`PT0S` 视为非法（时长非正）。
pub fn parse_iso8601_duration(s: &str) -> Result<f64, String> {
    let t = s.trim();
    let Some(rest) = t.strip_prefix('P') else {
        return Err(format!("ISO8601 时长缺 P 前缀: {s:?}"));
    };
    if rest.is_empty() {
        return Err(format!("ISO8601 时长为空: {s:?}"));
    }
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, tp)) => (d, Some(tp)),
        None => (rest, None),
    };
    let mut total = scan_units(
        date_part,
        &[
            ('Y', 31_557_600.0),
            ('M', 2_629_746.0),
            ('W', 604_800.0),
            ('D', 86_400.0),
        ],
    )?;
    if let Some(tp) = time_part {
        total += scan_units(tp, &[('H', 3_600.0), ('M', 60.0), ('S', 1.0)])?;
    }
    if total <= 0.0 {
        return Err(format!("ISO8601 时长非正: {s:?}"));
    }
    Ok(total)
}

fn scan_units(part: &str, units: &[(char, f64)]) -> Result<f64, String> {
    if part.is_empty() {
        return Ok(0.0);
    }
    let mut total = 0.0;
    let mut num = String::new();
    for c in part.chars() {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            if c == '.' && num.contains('.') {
                return Err(format!("ISO8601 时长数值多小数点: {part:?}"));
            }
            num.push(if c == ',' { '.' } else { c });
            continue;
        }
        let mult = units
            .iter()
            .find(|(u, _)| *u == c)
            .map(|(_, m)| *m)
            .ok_or_else(|| format!("ISO8601 时长单位非法或位置错误: {c:?}（{part:?}）"))?;
        let v: f64 = num
            .parse()
            .map_err(|_| format!("ISO8601 时长数值非法: {num:?}"))?;
        total += v * mult;
        num.clear();
    }
    if !num.is_empty() {
        return Err(format!("ISO8601 时长数值缺单位: {part:?}"));
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// MPD 结构捕获（单遍建树 + 结构解析；S/Tpl/SegList 全量捕获后按选流结果
// 取用，避免两遍扫描）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SEntry {
    /// 段起始时间（timescale 单位；None = 继承前段终点，首段缺省 0）。
    t: Option<u64>,
    d: u64,
    /// 重复数（r ≥ 0；负值 v1 拒绝）。
    r: u64,
}

#[derive(Debug, Clone)]
struct Tpl {
    init: Option<String>,
    media: Option<String>,
    timescale: u64,
    start_number: u64,
    /// duration 定址：单段时长（timescale 单位）。
    duration: Option<u64>,
    timeline: Option<Vec<SEntry>>,
}

#[derive(Debug, Clone)]
struct SegList {
    init: Option<String>,
    urls: Vec<String>,
    /// 解析基准（所属元素的 effective base——spec 5.3.9.4.2）。
    base: String,
}

#[derive(Debug, Clone)]
struct RepNode {
    id: String,
    bandwidth: u64,
    /// effective base（MPD→Period→AS→Rep BaseURL 链逐级解析）。
    base: String,
    /// Representation 自身带 BaseURL（单文件寻址可用性判据）。
    has_own_base: bool,
    /// SegmentBase（单文件索引 byte-range）→ v1 拒绝。
    has_seg_base: bool,
    has_cp: bool,
    mime: Option<String>,
    tpl: Option<(Tpl, TplMask)>,
    list: Option<SegList>,
}

#[derive(Debug, Clone)]
struct SetNode {
    is_video: bool,
    has_cp: bool,
    tpl: Option<(Tpl, TplMask)>,
    list: Option<SegList>,
    reps: Vec<RepNode>,
}

/// Rep 级 SegmentTemplate 显式属性掩码（batch6-P2，ISO 23009-1
/// §5.3.9.4.2 属性覆盖继承语义）：Rep 级模板对 AS 级同名属性逐一覆盖，
/// 缺失属性回退 AS 级同名字段（旧实现整元素替换 → AS 级带
/// duration/timescale、Rep 级仅覆写 media/init 的合法清单被误拒）。
#[derive(Debug, Clone, Copy, Default)]
struct TplMask {
    timescale: bool,
    start_number: bool,
    duration: bool,
    timeline: bool,
    init: bool,
    media: bool,
}

fn parse_tpl(elt: &Elt) -> Result<(Tpl, TplMask), String> {
    let mut mask = TplMask::default();
    let pos_u64 = |v: &str| -> Result<u64, String> {
        v.trim()
            .parse::<u64>()
            .map_err(|_| format!("DASH SegmentTemplate 数值属性非法: {v:?}"))
    };
    let timescale = match elt.attr("timescale") {
        Some(v) => {
            mask.timescale = true;
            let ts = pos_u64(v)?;
            if ts == 0 {
                return Err("DASH SegmentTemplate timescale 须为正".into());
            }
            ts
        }
        None => 1,
    };
    let start_number = match elt.attr("startNumber") {
        Some(v) => {
            mask.start_number = true;
            pos_u64(v)?
        }
        None => 1,
    };
    let duration = match elt.attr("duration") {
        Some(v) => {
            mask.duration = true;
            let d = pos_u64(v)?;
            if d == 0 {
                return Err("DASH SegmentTemplate duration 须为正".into());
            }
            Some(d)
        }
        None => None,
    };
    let timeline = match elt.child("SegmentTimeline") {
        None => None,
        Some(stl) => {
            mask.timeline = true;
            let mut entries = Vec::new();
            for s in stl.children.iter().filter(|e| e.name == "S") {
                let d = match s.attr("d") {
                    Some(v) => {
                        let d = pos_u64(v)?;
                        if d == 0 {
                            return Err("DASH SegmentTimeline <S> d 须为正".into());
                        }
                        d
                    }
                    None => return Err("DASH SegmentTimeline <S> 缺 d".into()),
                };
                if let Some(rv) = s.attr("r") {
                    let raw = rv.trim();
                    let neg = raw.starts_with('-');
                    let r = pos_u64(raw.trim_start_matches('+'))?;
                    if neg && r > 0 {
                        return Err("DASH SegmentTimeline 负 r（段数由前段推导）v1 不支持".into());
                    }
                    entries.push(SEntry {
                        t: s.attr("t").map(pos_u64).transpose()?,
                        d,
                        r,
                    });
                } else {
                    entries.push(SEntry {
                        t: s.attr("t").map(pos_u64).transpose()?,
                        d,
                        r: 0,
                    });
                }
            }
            Some(entries)
        }
    };
    mask.init = elt
        .attr("initialization")
        .filter(|s| !s.is_empty())
        .is_some();
    mask.media = elt.attr("media").filter(|s| !s.is_empty()).is_some();
    Ok((
        Tpl {
            init: elt
                .attr("initialization")
                .filter(|s| !s.is_empty())
                .map(String::from),
            media: elt
                .attr("media")
                .filter(|s| !s.is_empty())
                .map(String::from),
            timescale,
            start_number,
            duration,
            timeline,
        },
        mask,
    ))
}

fn parse_list(elt: &Elt, base: String) -> Result<SegList, String> {
    let init = match elt.child("Initialization") {
        None => None,
        Some(init_elt) => {
            if init_elt.attr("range").is_some() {
                return Err("DASH SegmentList Initialization range 定址 v1 不支持".into());
            }
            init_elt
                .attr("sourceURL")
                .filter(|s| !s.is_empty())
                .map(String::from)
        }
    };
    let mut urls = Vec::new();
    for u in elt.children.iter().filter(|e| e.name == "SegmentURL") {
        if u.attr("index").is_some() {
            return Err("DASH SegmentURL index 定址 v1 不支持".into());
        }
        if u.attr("mediaRange").is_some() {
            return Err("DASH SegmentURL mediaRange 子范围 v1 不支持".into());
        }
        match u.attr("media") {
            Some(m) if !m.is_empty() => urls.push(m.to_string()),
            _ => return Err("DASH SegmentURL 缺 media 属性".into()),
        }
    }
    Ok(SegList { init, urls, base })
}

/// 元素 BaseURL 链推进：子级 effective base = resolve(父 base, `<BaseURL>` 文本)。
/// 多 BaseURL（CDN 冗余）→ v1 拒绝；空文本 → 继承父 base。
fn base_of(parent_base: &str, elt: &Elt) -> Result<String, String> {
    let bases: Vec<&Elt> = elt
        .children
        .iter()
        .filter(|e| e.name == "BaseURL")
        .collect();
    match bases.len() {
        0 => Ok(parent_base.to_string()),
        1 => {
            let text = bases[0].text.trim();
            if text.is_empty() {
                Ok(parent_base.to_string())
            } else {
                Ok(resolve_url(parent_base, text))
            }
        }
        _ => Err("DASH 多 BaseURL（CDN 冗余候选）v1 不支持".into()),
    }
}

fn is_video_set(elt: &Elt, reps: &[RepNode]) -> bool {
    let starts_video = |v: &str| v.trim().to_lowercase().starts_with("video");
    if elt.attr("contentType").map(starts_video).unwrap_or(false)
        || elt.attr("mimeType").map(starts_video).unwrap_or(false)
    {
        return true;
    }
    reps.iter()
        .any(|r| r.mime.as_deref().map(starts_video).unwrap_or(false))
}

/// 模板替换：`$Name$` / `$Name%0Nd$` / `$$` 转义。Number/Time 两种定址模式
/// 统一供给（模板未引用即忽略）；`$SubNumber$` / 未知标识符 → 拒绝。
fn subst_template(t: &str, number: u64, time: u64, rep: &RepNode) -> Result<String, String> {
    let mut out = String::with_capacity(t.len() + 16);
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match it.peek().copied() {
            Some('$') => {
                it.next();
                out.push('$');
            }
            Some(_) => {
                let mut id = String::new();
                let mut closed = false;
                for ic in it.by_ref() {
                    if ic == '$' {
                        closed = true;
                        break;
                    }
                    id.push(ic);
                    if id.len() > 64 {
                        return Err("DASH 模板标识符未闭合".into());
                    }
                }
                if !closed {
                    return Err("DASH 模板 `$` 未闭合".into());
                }
                let (name, fmt) = match id.split_once('%') {
                    Some((n, f)) => (n, Some(f)),
                    None => (id.as_str(), None),
                };
                let val = match name {
                    "RepresentationID" => rep.id.clone(),
                    "Bandwidth" => rep.bandwidth.to_string(),
                    "Number" => number.to_string(),
                    "Time" => time.to_string(),
                    "SubNumber" => return Err("DASH v1 不支持 $SubNumber$ 寻址".into()),
                    other => return Err(format!("DASH 模板未知标识符: ${other}$")),
                };
                match fmt {
                    None => out.push_str(&val),
                    Some(f) => {
                        let w = f
                            .strip_prefix('0')
                            .and_then(|r| r.strip_suffix('d'))
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|w| (1..=19).contains(w))
                            .ok_or_else(|| format!("DASH 模板宽度格式非法: %{f}（仅支持 %0Nd）"))?;
                        out.push_str(&format!("{val:0>w$}"));
                    }
                }
            }
            None => return Err("DASH 模板 `$` 未闭合".into()),
        }
    }
    Ok(out)
}

/// 解析 MPD → DashPlan（选流 + 段 URL 展开）。
pub fn parse_mpd(base_url: &str, xml: &str) -> Result<DashPlan, String> {
    let roots = build_tree(xml)?;
    let mpd = roots
        .iter()
        .find(|e| e.name == "MPD")
        .ok_or("DASH 清单无 <MPD> 根元素")?;
    match mpd.attr("type") {
        Some("dynamic") => {
            return Err("DASH 清单为 dynamic（live 流），v1 仅支持 static 点播".into())
        }
        Some(other) if other != "static" => return Err(format!("DASH MPD@type 未知: {other:?}")),
        _ => {}
    }
    let mpd_duration = mpd
        .attr("mediaPresentationDuration")
        .map(parse_iso8601_duration)
        .transpose()?;
    // MPD 级 BaseURL 参与链解析（xlink:href 经 quick-xml local_name 剥前缀
    // 后即 href——MPD/Period 上下文无其他 href 属性，按 href 匹配即可）
    if mpd.attr("href").is_some() {
        return Err("DASH v1 不支持 xlink 外链清单".into());
    }
    let mpd_base = base_of(base_url, mpd)?;

    let periods: Vec<&Elt> = mpd.children.iter().filter(|e| e.name == "Period").collect();
    if periods.is_empty() {
        return Err("DASH 清单无 <Period>".into());
    }
    if periods.len() > 1 {
        return Err("DASH v1 仅支持单 Period 清单".into());
    }
    let period = periods[0];
    if period.attr("href").is_some() {
        return Err("DASH v1 不支持 xlink 外链 Period".into());
    }
    let period_secs = period
        .attr("duration")
        .map(parse_iso8601_duration)
        .transpose()?
        .or(mpd_duration);
    let period_base = base_of(&mpd_base, period)?;

    // 捕获全部 AdaptationSet（含 Representation 子树）
    let mut sets: Vec<SetNode> = Vec::new();
    for as_elt in period.children.iter().filter(|e| e.name == "AdaptationSet") {
        let as_base = base_of(&period_base, as_elt)?;
        let has_cp = as_elt
            .children
            .iter()
            .any(|e| e.name == "ContentProtection");
        let tpl = match as_elt.child("SegmentTemplate") {
            Some(t) => Some(parse_tpl(t)?),
            None => None,
        };
        let list = match as_elt.child("SegmentList") {
            Some(l) => Some(parse_list(l, as_base.clone())?),
            None => None,
        };
        let mut reps: Vec<RepNode> = Vec::new();
        for r in as_elt
            .children
            .iter()
            .filter(|e| e.name == "Representation")
        {
            let rep_base = base_of(&as_base, r)?;
            reps.push(RepNode {
                id: r.attr("id").unwrap_or("").to_string(),
                bandwidth: r
                    .attr("bandwidth")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(0),
                has_own_base: r.child("BaseURL").is_some(),
                has_seg_base: r.child("SegmentBase").is_some(),
                has_cp: r.children.iter().any(|e| e.name == "ContentProtection"),
                mime: r.attr("mimeType").map(String::from),
                tpl: match r.child("SegmentTemplate") {
                    Some(t) => Some(parse_tpl(t)?),
                    None => None,
                },
                list: match r.child("SegmentList") {
                    Some(l) => Some(parse_list(l, rep_base.clone())?),
                    None => None,
                },
                base: rep_base,
            });
        }
        sets.push(SetNode {
            is_video: is_video_set(as_elt, &reps),
            has_cp,
            tpl,
            list,
            reps,
        });
    }

    // 选流：视频优先 → 集合内最高码率 Representation
    let mut pool: Vec<&SetNode> = sets.iter().filter(|s| !s.reps.is_empty()).collect();
    if pool.is_empty() {
        return Err("DASH 清单无可用 AdaptationSet/Representation".into());
    }
    let video: Vec<&SetNode> = pool.iter().copied().filter(|s| s.is_video).collect();
    if !video.is_empty() {
        pool = video;
    }
    let chosen_set = pool
        .into_iter()
        .max_by_key(|s| s.reps.iter().map(|r| r.bandwidth).max().unwrap_or(0))
        .expect("pool 非空已保证");
    let chosen_rep = chosen_set
        .reps
        .iter()
        .max_by_key(|r| r.bandwidth)
        .expect("reps 非空已保证");
    if chosen_set.has_cp || chosen_rep.has_cp {
        return Err("DASH v1 不支持 DRM/ContentProtection 加密内容".into());
    }
    if chosen_rep.has_seg_base {
        return Err("DASH SegmentBase（单文件索引 byte-range）v1 不支持——请用直链任务下载".into());
    }

    // 分段寻址展开（batch6-P2：Rep 级对 AS 级按 ISO 23009-1 §5.3.9.4.2
    // 逐属性覆盖继承——Rep 缺失的属性回退 AS 级同名字段；旧实现整元素
    // 替换 → AS 级带 duration/timescale、Rep 级仅覆写 media/init 的合法
    // 清单被误拒「缺 duration 且无 SegmentTimeline」。混用拒绝）
    let tpl = match (chosen_set.tpl.as_ref(), chosen_rep.tpl.as_ref()) {
        (Some((as_t, _)), Some((rep_t, rep_m))) => {
            let mut t = rep_t.clone();
            if !rep_m.timescale {
                t.timescale = as_t.timescale;
            }
            if !rep_m.start_number {
                t.start_number = as_t.start_number;
            }
            if !rep_m.duration {
                t.duration = as_t.duration;
            }
            if !rep_m.timeline {
                t.timeline = as_t.timeline.clone();
            }
            if !rep_m.init {
                t.init = as_t.init.clone();
            }
            if !rep_m.media {
                t.media = as_t.media.clone();
            }
            Some(t)
        }
        (_, Some((t, _))) => Some(t.clone()),
        (Some((t, _)), None) => Some(t.clone()),
        (None, None) => None,
    };
    let list = chosen_rep.list.clone().or_else(|| chosen_set.list.clone());
    match (tpl, list) {
        (Some(_), Some(_)) => Err("DASH 混用 SegmentTemplate 与 SegmentList 寻址 v1 不支持".into()),
        (Some(t), None) => build_from_template(&t, chosen_rep, period_secs),
        (None, Some(l)) => {
            if l.urls.is_empty() {
                return Err("DASH SegmentList 无 SegmentURL".into());
            }
            let init = l
                .init
                .as_deref()
                .map(|u| normalize_dots(&resolve_url(&l.base, u)));
            Ok(DashPlan {
                init,
                segments: l
                    .urls
                    .iter()
                    .map(|u| normalize_dots(&resolve_url(&l.base, u)))
                    .collect(),
            })
        }
        (None, None) => {
            if chosen_rep.has_own_base {
                // 单文件表示：表示自身即媒体（无 init 段）
                Ok(DashPlan {
                    init: None,
                    segments: vec![chosen_rep.base.clone()],
                })
            } else {
                Err("DASH 表示无 BaseURL 且无分段信息，无法定位媒体（请用直链任务下载）".into())
            }
        }
    }
}

/// 压平 URL 路径中的 `.`/`..` 段（模板相对回溯场景；仅作用于 path 部分，
/// query/fragment 原样保留；scheme://host 前缀不参与归一）。
fn normalize_dots(url: &str) -> String {
    let (prefix_path, suffix) = match url.find(['?', '#']) {
        Some(i) => (&url[..i], &url[i..]),
        None => (url, ""),
    };
    let (scheme_host, path) = match prefix_path.find("://") {
        Some(i) => {
            let after = &prefix_path[i + 3..];
            match after.find('/') {
                Some(j) => (&prefix_path[..i + 3 + j], &after[j..]),
                None => (prefix_path, ""),
            }
        }
        None => ("", prefix_path),
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let mut out = String::with_capacity(url.len());
    out.push_str(scheme_host);
    out.push_str(&segs.join("/"));
    if path.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(suffix);
    out
}

fn build_from_template(
    t: &Tpl,
    rep: &RepNode,
    period_secs: Option<f64>,
) -> Result<DashPlan, String> {
    let media = t
        .media
        .as_deref()
        .ok_or("DASH SegmentTemplate 缺 media 模板")?;
    let filled = |number: u64, time: u64| -> Result<String, String> {
        let s = subst_template(media, number, time, rep)?;
        Ok(normalize_dots(&resolve_url(&rep.base, &s)))
    };
    let init = t
        .init
        .as_deref()
        .map(|s| {
            let f = subst_template(s, t.start_number, 0, rep)?;
            Ok::<String, String>(normalize_dots(&resolve_url(&rep.base, &f)))
        })
        .transpose()?;
    let segments = match &t.timeline {
        Some(tl) => {
            if tl.is_empty() {
                return Err("DASH SegmentTimeline 为空".into());
            }
            let mut segs: Vec<String> = Vec::new();
            let mut number = t.start_number;
            let mut cur: Option<u64> = None;
            for s in tl {
                let start = s.t.unwrap_or_else(|| cur.unwrap_or(0));
                for k in 0..=s.r {
                    let time = start + k * s.d;
                    if segs.len() >= MAX_SEGMENTS {
                        return Err(format!("DASH 段数异常（> {MAX_SEGMENTS}）"));
                    }
                    segs.push(filled(number, time)?);
                    number += 1;
                }
                cur = Some(start + (s.r + 1) * s.d);
            }
            segs
        }
        None => {
            let d = t
                .duration
                .ok_or("DASH SegmentTemplate 缺 duration 且无 SegmentTimeline，无法确定段数")?;
            let secs = period_secs.ok_or(
                "DASH 无法确定 Period 时长（Period@duration / MPD@mediaPresentationDuration 均缺）",
            )?;
            let units = (secs * t.timescale as f64).round() as u64;
            let count = units.div_ceil(d) as usize;
            if count == 0 {
                return Err("DASH 段数为 0（Period 时长为 0？）".into());
            }
            if count > MAX_SEGMENTS {
                return Err(format!("DASH 段数异常: {count}（> {MAX_SEGMENTS}）"));
            }
            (0..count)
                .map(|i| filled(t.start_number + i as u64, i as u64 * d))
                .collect::<Result<Vec<_>, String>>()?
        }
    };
    if segments.is_empty() {
        return Err("DASH 清单无媒体段".into());
    }
    Ok(DashPlan { init, segments })
}

// ---------------------------------------------------------------------------
// 段账本（恢复凭据）+ 下载核心（HLS 同构：清单指纹对账 → 顺序段下载 append）
// ---------------------------------------------------------------------------

/// 段账本（恢复凭据）：清单指纹 + 已完成段数 + 累计字节。
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
pub struct DashLedger {
    pub version: u32,
    /// 清单指纹（sha256 hex，MPD 文本 + base_url）——恢复时对账，失配作废重下。
    pub manifest_fingerprint: String,
    /// 已完成段数（顺序语义 → 前缀完整；init 段计为第 0 项）。
    pub segments_done: usize,
    /// 已落盘字节（= .part 当前长度，冗余记录供校验）。
    pub bytes_done: u64,
}

pub const DASH_LEDGER_VERSION: u32 = 1;

pub fn dash_ledger_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(".dash-ledger");
    PathBuf::from(s)
}

pub fn dash_fingerprint(base_url: &str, mpd_text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(base_url.as_bytes());
    h.update(b"\n");
    h.update(mpd_text.as_bytes());
    hex_encode(&h.finalize())
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// 拉取文本资源（MPD 共用：任务级 headers 透传，30s 超时对齐探测口径）。
/// batch6-P2：限长读取（16MB 封顶，与 HLS/RSS/metalink 引导同口径）。
async fn fetch_text(
    client: &reqwest::Client,
    headers: &[(String, String)],
    url: &str,
) -> Result<String, EngineError> {
    const FETCH_TEXT_MAX: usize = 16 * 1024 * 1024;
    let mut req = client.get(url).timeout(std::time::Duration::from_secs(30));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| EngineError::Other(format!("DASH 清单拉取失败: {e}")))?;
    let mut out: Vec<u8> = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| EngineError::Other(format!("DASH 清单读取失败: {e}")))?
    {
        if out.len() + chunk.len() > FETCH_TEXT_MAX {
            return Err(EngineError::Other(format!(
                "MPD 超过 {FETCH_TEXT_MAX} 字节上限"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8(out).map_err(|e| EngineError::Other(format!("MPD 非 UTF-8: {e}")))
}

#[allow(clippy::too_many_arguments)] // 协议会话要素，同 hls.rs 惯例
pub async fn download_dash(
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    dest: PathBuf,
    limiter: Arc<RateLimiter>,
    on_progress: Arc<dyn Fn(u64) + Send + Sync>,
    is_aborted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), EngineError> {
    let mpd_text = fetch_text(&client, &headers, &url).await?;
    let plan = parse_mpd(&url, &mpd_text).map_err(EngineError::Other)?;
    // init 段计为第 0 项参与账本前缀语义
    let items: Vec<String> = plan
        .init
        .iter()
        .chain(plan.segments.iter())
        .cloned()
        .collect();

    // .part 续传对账
    let part = part_path_of(&dest);
    let ledger_path = dash_ledger_path(&part);
    let fingerprint = dash_fingerprint(&url, &mpd_text);
    let mut segments_done: usize = 0;
    let mut bytes_done: u64 = 0;
    if let Some(ld) = load_ledger(&ledger_path) {
        if ld.version == DASH_LEDGER_VERSION
            && ld.manifest_fingerprint == fingerprint
            && ld.segments_done <= items.len()
            && part.is_file()
            && std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == ld.bytes_done
        {
            segments_done = ld.segments_done;
            bytes_done = ld.bytes_done;
        } else {
            let _ = std::fs::remove_file(&part);
        }
    } else {
        let _ = std::fs::remove_file(&part);
    }

    // 打开 .part：续传 = append（位置在末尾）；全新/作废 = write+truncate
    //（Windows append 句柄权限约束同 HLS——直接以 truncate 语义打开）
    let mut f = if segments_done == 0 {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part)
            .map_err(|e| EngineError::Other(format!("part open: {e}")))?
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .map_err(|e| EngineError::Other(format!("part open: {e}")))?
    };
    if segments_done == 0 {
        on_progress(0); // 起点锚（total=0 未知口径）
    } else {
        on_progress(bytes_done); // 恢复口径回填
    }

    let total_segs = items.len();
    for (idx, iurl) in items.iter().enumerate().skip(segments_done) {
        // abort 检查点（段间）：pause/remove/被新 epoch 取代 → 立即中断
        // （账本已落，resume 凭 segments_done 续传）。batch6-P0：is_aborted
        // 闭包同时校验 pause / epoch 过期 / 任务已移除——旧实现只看 pause
        // 旗标（resume 会清掉），旧循环与 resume 新循环并发 append 同一
        // .part（双写者静默损坏），保证 epoch 单写者不变量。
        if is_aborted() {
            return Err(EngineError::Other(DASH_ABORTED_MSG.into()));
        }
        // batch6-P1：段拉取不再设 120s 总超时（reqwest .timeout 含响应体）
        // ——单文件表示（无分段 MPD，segments=[整个文件]）耗时必然超限，
        // 任务永远无法完成；且 resp.bytes() 全量驻内存。改为流式写盘：
        // 停滞由 client 级 read_timeout（idle 30s，serve/builder 同口径）
        // 兑底，连接由 connect_timeout 兑底。
        let mut req = client.get(iurl);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| EngineError::Other(format!("段 {idx}/{total_segs} 拉取失败: {e}")))?;
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| EngineError::Other(format!("段 {idx}/{total_segs} 读取失败: {e}")))?
        {
            limiter.wait(chunk.len() as u64).await;
            f.write_all(&chunk)
                .map_err(|e| EngineError::Other(format!("part 写入: {e}")))?;
            bytes_done += chunk.len() as u64;
        }
        // batch3-P1：进度统一绝对累计语义（与回填同口径，见 hls 同批修复）
        on_progress(bytes_done);
        // 段完成即落账本（顺序前缀语义 → 崩溃后从下一段续）
        save_ledger(
            &ledger_path,
            &DashLedger {
                version: DASH_LEDGER_VERSION,
                manifest_fingerprint: fingerprint.clone(),
                segments_done: idx + 1,
                bytes_done,
            },
        );
    }
    drop(f);

    // finalize：.part → dest（batch6-P0：落位前终检——过期循环不得落位，
    // 否则可在新循环写到一半时把 .part 改名交付半成品）。
    if is_aborted() {
        return Err(EngineError::Other(DASH_ABORTED_MSG.into()));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| EngineError::Other(format!("dest mkdir: {e}")))?;
    }
    std::fs::rename(&part, &dest).map_err(|e| {
        EngineError::Other(format!(
            "finalize {} → {}: {e}",
            part.display(),
            dest.display()
        ))
    })?;
    let _ = std::fs::remove_file(&ledger_path);
    Ok(())
}

fn load_ledger(path: &Path) -> Option<DashLedger> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn save_ledger(path: &Path, ld: &DashLedger) {
    if let Ok(json) = serde_json::to_vec(ld) {
        let tmp = {
            let mut s = path.as_os_str().to_os_string();
            s.push(".tmp");
            PathBuf::from(s)
        };
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// .part 路径（DASH 无 gen 语义：`<dest>.part`，与 engine.rs gen0 同形）。
fn part_path_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // duration 定址：Period@duration=PT40S, duration=4, timescale=1 → 10 段
    #[test]
    fn parses_number_template_with_period_duration() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT40S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1" duration="4" initialization="init.mp4" media="chunk-$Number$.m4s"/>
      <Representation id="1080p" bandwidth="4000000"/>
      <Representation id="720p" bandwidth="2000000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/v/manifest.mpd", xml).unwrap();
        assert_eq!(p.init.as_deref(), Some("https://h/v/init.mp4"));
        assert_eq!(p.segments.len(), 10);
        assert_eq!(p.segments[0], "https://h/v/chunk-1.m4s");
        assert_eq!(p.segments[9], "https://h/v/chunk-10.m4s");
    }

    // Period@duration 缺省 → 回落 MPD@mediaPresentationDuration；ceil 取整
    #[test]
    fn period_duration_falls_back_to_mpd_and_ceils() {
        let xml = r#"<MPD mediaPresentationDuration="PT41S">
  <Period>
    <AdaptationSet contentType="video">
      <SegmentTemplate duration="10" media="s$Number$.m4s"/>
      <Representation id="a" bandwidth="1"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/m.mpd", xml).unwrap();
        assert_eq!(p.segments.len(), 5, "41/10 → ceil = 5 段");
    }

    // SegmentTimeline：t 显式/缺省继承、r 重复、$Time%08d$ 零宽格式
    #[test]
    fn parses_segment_timeline_with_r_and_time_format() {
        let xml = r#"<MPD type="static">
  <Period>
    <AdaptationSet contentType="video">
      <SegmentTemplate timescale="90000" initialization="i.mp4" media="c-$Time%08d$.m4s">
        <SegmentTimeline>
          <S t="0" d="90000" r="2"/>
          <S d="45000"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="v" bandwidth="1000000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/m.mpd", xml).unwrap();
        assert_eq!(p.segments.len(), 4, "r=2 → 3 段 + 1 段");
        assert_eq!(p.segments[0], "https://h/c-00000000.m4s");
        assert_eq!(p.segments[2], "https://h/c-00180000.m4s");
        // 第二个 S 缺 t → 继承前段终点 0 + 3*90000 = 270000
        assert_eq!(p.segments[3], "https://h/c-00270000.m4s");
    }

    // $RepresentationID$ / $Bandwidth$ 替换 + BaseURL 链逐级解析
    #[test]
    fn base_url_chain_and_rep_substitution() {
        let xml = r#"<MPD type="static" mediaPresentationDuration="PT4S">
  <BaseURL>cdn/</BaseURL>
  <Period>
    <AdaptationSet contentType="video">
      <BaseURL>1080p/</BaseURL>
      <SegmentTemplate duration="2" media="../seg/$Bandwidth$-$RepresentationID$-$Number%03d$.m4s"/>
      <Representation id="v1" bandwidth="2500000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/v/m.mpd", xml).unwrap();
        // base 链：https://h/v/m.mpd → https://h/v/cdn/ → https://h/v/cdn/1080p/
        // → ../seg 回溯压平 → https://h/v/cdn/seg/；$Number$ 缺省 startNumber=1
        assert_eq!(p.segments[0], "https://h/v/cdn/seg/2500000-v1-001.m4s");
        assert_eq!(p.segments[1], "https://h/v/cdn/seg/2500000-v1-002.m4s");
    }

    // 无视频轨（纯音频）→ 回退取最高码率音频 Representation
    #[test]
    fn audio_fallback_when_no_video_set() {
        let xml = r#"<MPD type="static" mediaPresentationDuration="PT5S">
  <Period>
    <AdaptationSet contentType="audio">
      <SegmentTemplate duration="5" media="a-$Number$.m4s"/>
      <Representation id="lo" bandwidth="64000"/>
      <Representation id="hi" bandwidth="192000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/m.mpd", xml).unwrap();
        assert_eq!(p.segments[0], "https://h/a-1.m4s");
        assert_eq!(p.segments.len(), 1);
    }

    // SegmentList：Initialization sourceURL + SegmentURL media
    #[test]
    fn parses_segment_list() {
        let xml = r#"<MPD type="static">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="v" bandwidth="1">
        <SegmentList timescale="1" duration="4">
          <Initialization sourceURL="init.mp4"/>
          <SegmentURL media="s0.m4s"/>
          <SegmentURL media="s1.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let p = parse_mpd("https://h/sub/m.mpd", xml).unwrap();
        assert_eq!(p.init.as_deref(), Some("https://h/sub/init.mp4"));
        assert_eq!(
            p.segments,
            vec!["https://h/sub/s0.m4s", "https://h/sub/s1.m4s"]
        );
    }

    // SegmentURL mediaRange / index / Initialization range → 拒绝
    #[test]
    fn segment_list_range_forms_rejected() {
        let head = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
<Representation id="v" bandwidth="1"><SegmentList>"#;
        let tail = "</SegmentList></Representation></AdaptationSet></Period></MPD>";
        let with_range = format!(
            r#"{head}<Initialization sourceURL="i.mp4" range="0-999"/><SegmentURL media="s.m4s"/>{tail}"#
        );
        assert!(parse_mpd("https://h/m.mpd", &with_range)
            .unwrap_err()
            .contains("range"));
        let with_mediarange =
            format!(r#"{head}<SegmentURL media="s.m4s" mediaRange="0-100"/>{tail}"#);
        assert!(parse_mpd("https://h/m.mpd", &with_mediarange)
            .unwrap_err()
            .contains("mediaRange"));
        let with_index = format!(r#"{head}<SegmentURL media="s.m4s" index="x.idx"/>{tail}"#);
        assert!(parse_mpd("https://h/m.mpd", &with_index)
            .unwrap_err()
            .contains("index"));
    }

    // 明确拒绝面：dynamic / 多 Period / ContentProtection / SegmentBase / xlink
    #[test]
    fn rejects_dynamic_mpd() {
        let xml = r#"<MPD type="dynamic"><Period><AdaptationSet><SegmentTemplate duration="1" media="s$Number$.m4s"/><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let e = parse_mpd("https://h/m.mpd", xml).unwrap_err();
        assert!(e.contains("dynamic"), "{e}");
    }

    #[test]
    fn rejects_multiple_periods() {
        let xml = r#"<MPD type="static">
  <Period><AdaptationSet contentType="video"><Representation id="a" bandwidth="1"><BaseURL>f.mp4</BaseURL></Representation></AdaptationSet></Period>
  <Period><AdaptationSet contentType="video"><Representation id="b" bandwidth="1"><BaseURL>f.mp4</BaseURL></Representation></AdaptationSet></Period>
</MPD>"#;
        assert!(parse_mpd("https://h/m.mpd", xml)
            .unwrap_err()
            .contains("单 Period"));
    }

    #[test]
    fn rejects_content_protection() {
        let xml = r#"<MPD type="static">
  <Period>
    <AdaptationSet contentType="video">
      <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011"/>
      <SegmentTemplate duration="1" media="s$Number$.m4s"/>
      <Representation id="a" bandwidth="1"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let e = parse_mpd("https://h/m.mpd", xml).unwrap_err();
        assert!(e.contains("DRM"), "{e}");
    }

    #[test]
    fn rejects_segment_base_and_xlink() {
        let sb = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
<Representation id="v" bandwidth="1"><BaseURL>file.mp4</BaseURL>
<SegmentBase indexRange="0-999"><Initialization range="0-99"/></SegmentBase>
</Representation></AdaptationSet></Period></MPD>"#;
        assert!(parse_mpd("https://h/m.mpd", sb)
            .unwrap_err()
            .contains("SegmentBase"));
        let xl = r#"<MPD type="static" xlink:href="http://o/other.mpd"><Period/></MPD>"#;
        assert!(parse_mpd("https://h/m.mpd", xl)
            .unwrap_err()
            .contains("xlink"));
    }

    // 模板边角：未知标识符 / $SubNumber$ / $$ 转义 / 未闭合 / 非法宽度
    #[test]
    fn template_edges() {
        let rep = RepNode {
            id: "v1".into(),
            bandwidth: 1000,
            base: "https://h/".into(),
            has_own_base: false,
            has_seg_base: false,
            has_cp: false,
            mime: None,
            tpl: None,
            list: None,
        };
        assert_eq!(
            subst_template("a$$b-$Number$", 7, 0, &rep).unwrap(),
            "a$b-7"
        );
        assert!(subst_template("x-$Foo$", 1, 0, &rep)
            .unwrap_err()
            .contains("未知标识符"));
        assert!(subst_template("x-$SubNumber$", 1, 0, &rep)
            .unwrap_err()
            .contains("SubNumber"));
        assert!(subst_template("x-$Number", 1, 0, &rep)
            .unwrap_err()
            .contains("未闭合"));
        assert!(subst_template("x-$Number%2q$", 1, 0, &rep)
            .unwrap_err()
            .contains("宽度"));
        // 非数值标识符遇宽度格式 → 左补零至指定位宽
        assert_eq!(
            subst_template("$RepresentationID%03d$", 1, 0, &rep).unwrap(),
            "0v1"
        );
    }

    // 模板缺 media / duration 与 timeline 双缺 / 空清单 → 拒绝
    #[test]
    fn template_missing_fields_rejected() {
        let no_media = r#"<MPD mediaPresentationDuration="PT10S"><Period>
<AdaptationSet contentType="video"><SegmentTemplate duration="5" initialization="i.mp4"/>
<Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(parse_mpd("https://h/m.mpd", no_media)
            .unwrap_err()
            .contains("media"));
        let no_count = r#"<MPD><Period>
<AdaptationSet contentType="video"><SegmentTemplate media="s$Number$.m4s"/>
<Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let e = parse_mpd("https://h/m.mpd", no_count).unwrap_err();
        assert!(e.contains("无法确定"), "{e}");
    }

    // 无分段且 Representation 自带 BaseURL → 单文件整下；无 BaseURL → 拒绝
    #[test]
    fn single_file_representation() {
        let with_base = r#"<MPD type="static"><Period>
<AdaptationSet mimeType="video/mp4"><Representation id="a" bandwidth="1">
<BaseURL>movie.mp4</BaseURL></Representation></AdaptationSet>
</Period></MPD>"#;
        let p = parse_mpd("https://h/dash/m.mpd", with_base).unwrap();
        assert_eq!(p.segments, vec!["https://h/dash/movie.mp4"]);
        assert!(p.init.is_none());
        let no_base = with_base.replace("<BaseURL>movie.mp4</BaseURL>", "");
        let e = parse_mpd("https://h/dash/m.mpd", &no_base).unwrap_err();
        assert!(e.contains("BaseURL"), "{e}");
    }

    // ISO 8601 duration
    #[test]
    fn iso8601_durations() {
        assert_eq!(parse_iso8601_duration("PT1H2M3S").unwrap(), 3723.0);
        assert_eq!(parse_iso8601_duration("PT4.5S").unwrap(), 4.5);
        assert_eq!(parse_iso8601_duration("P10D").unwrap(), 864_000.0);
        assert_eq!(parse_iso8601_duration("P1W").unwrap(), 604_800.0);
        assert_eq!(parse_iso8601_duration("PT0.001S").unwrap(), 0.001);
        assert!(parse_iso8601_duration("PT0S").is_err());
        assert!(parse_iso8601_duration("1:00").is_err());
        assert!(parse_iso8601_duration("PTxS").is_err());
    }

    // 识别与命名
    #[test]
    fn url_detection_and_naming() {
        assert!(is_dash_url("https://h/v/manifest.MPD?tok=1"));
        assert!(is_dash_url("https://h/v/list.mpd"));
        assert!(!is_dash_url("https://h/v/list.mpdf"));
        assert!(!is_dash_url("https://h/file.m3u8"));
        assert_eq!(
            derive_mp4_name("https://h/v/ep01.mpd?tok=x").unwrap(),
            "ep01.mp4"
        );
        assert_eq!(derive_mp4_name("https://h/片头.MPD").unwrap(), "片头.mp4");
        assert!(derive_mp4_name("https://h/v/").is_none());
    }

    // timeline 空清单 / SegmentList 空 → 拒绝
    #[test]
    fn empty_segment_sources_rejected() {
        let empty_tl = r#"<MPD type="static"><Period>
<AdaptationSet contentType="video"><SegmentTemplate media="s$Number$.m4s">
<SegmentTimeline/></SegmentTemplate><Representation id="a" bandwidth="1"/>
</AdaptationSet></Period></MPD>"#;
        assert!(parse_mpd("https://h/m.mpd", empty_tl)
            .unwrap_err()
            .contains("为空"));
    }
}
