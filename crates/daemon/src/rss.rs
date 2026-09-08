//! RSS 订阅自动下载（qBittorrent RSS 对标，v1）。
//!
//! 定位：订阅 feed（RSS 2.0 / Atom）→ 周期拉取 → 关键词规则匹配 → 命中条目
//! 自动建 HTTP 任务（复用既有 add_link_task_opts 全链：探测/分段/限速/校验/
//! 事件/Webhook）。独立状态（feeds+items+rules）持久化 rss.json（与
//! tasks.json 同目录），任务创建复用 DaemonState 既有链路——不新增引擎。
//!
//! 解析口径（v1，对齐 metalink.rs 哲学）：
//! - 按 **local name** 匹配标签（命名空间无关；RSS 2.0 `<rss>` 与 Atom
//!   `<feed>` 同一循环处理，按条目元素 item/entry 自适应分流）。
//! - 条目 URL：RSS 2.0 取 `<link>` 文本；Atom 取 `<link href>`（rel=alternate
//!   优先，无 rel 次之）；link 空 + guid 为 http(s) 文本时用 guid 兜底。
//!   均缺 → **跳过该条目**（逐条容错，feed 现实质量参差，不因单条脏数据
//!   整体 400）。
//! - 条目 guid 缺省 = url（去重键 = guid；同一 feed 内 guid 重复保留首见）。
//! - channel/feed 级 `<title>` → 订阅标题（首次拉取后锁定，后续刷新不改写
//!   ——站点改标题不应悄悄变更用户可见的订阅名）。
//! - CDATA **独立事件接取**（quick-xml 将 `<![CDATA[...]]>` 投递为 `Event::CData`
//!   而非 Text——WordPress 等主流 CMS 的 feed 用 CDATA 包裹 title/link，漏接 =
//!   真实世界 feed 条目全丢；CDATA 内容为原始文本，不做实体反转义）；注释/PI 忽略。
//!   根元素不校验。
//! - 无任何有效条目 → Err（上层转 400；空 feed 对订阅场景无意义且多半是
//!   解析失败或页面误投）。
//!
//! 规则口径（v1，qbit 基础语义，无 regex 依赖）：
//! - `must_contain`：全部关键词（大小写不敏感子串）命中 title 才算匹配；
//! - `must_not_contain`：任一关键词命中 title 即排除；
//! - `feed_id`：Some = 规则只作用于该订阅；None = 全部订阅；
//! - 命中 → `add_link_task_opts(url, rule.dest, name=title)` + `set_task_tags`
//!   （tags 非空时）；item.task_id 落位 = 去重标记（refresh 重复执行不重建）。

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

use crate::state::{DaemonError, DaemonState};

/// 订阅条目。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RssItem {
    /// 去重键（条目 guid；缺省 = url）。
    pub guid: String,
    pub title: String,
    /// 下载目标（http(s)/ftp 等 add 链可接受的链接）。
    pub url: String,
    /// 原文 pubDate（RSS 2.0）/ updated（Atom），仅透传展示。
    pub pub_date: Option<String>,
    /// 命中规则后建的任务 id（None = 未处理/未匹配）。
    #[serde(default)]
    pub task_id: Option<String>,
}

/// 订阅源。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RssFeed {
    pub id: u64,
    pub url: String,
    /// 首次拉取成功后锁定的 feed 标题。
    pub title: String,
    pub added_at_unix: u64,
    #[serde(default)]
    pub last_refresh_unix: Option<u64>,
    /// 每 feed 独立刷新间隔（batch5 对标 qB RSS Downloader；秒）。0（默认）
    /// = 跟随全局 `[rss] refresh_interval_secs`；>0 = 该 feed 最小刷新间隔
    ///（自动刷新 ticker 中未到期即跳过；手动 POST /rss/refresh 仍全量）。
    #[serde(default)]
    pub interval_override_secs: u64,
    #[serde(default)]
    pub items: Vec<RssItem>,
}

/// 自动下载规则。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RssRule {
    pub id: u64,
    pub name: String,
    #[serde(default = "crate::rss::default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub must_contain: Vec<String>,
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    /// Some = 只作用于该订阅。
    #[serde(default)]
    pub feed_id: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 命中任务的落盘目录（None = default dest_root）。
    #[serde(default)]
    pub dest: Option<String>,
    /// 关键词按正则解释（batch5 对标 qB「使用正则表达式」）：true = 必须含
    /// 列表逐条按大小写不敏感正则匹配标题；false（默认）= 大小写不敏感子串。
    /// 正则编译失败按不命中处理（warn 一次由调用方路径记日志；此处每条
    /// 编译成本低，规则量级 ≤ 百）。
    #[serde(default)]
    pub use_regex: bool,
    /// 集数过滤（batch5 对标 qB「集数过滤」）：如 `1x02;1x04-1x06;S02E01`。
    /// 分号分隔多模式；`SxxEyy`/`NxxEyy`/`Nyy` 单集 + `A-B` 区间；标题中
    /// 出现的所有 S/E 模式逐个提取，任一命中过滤集即通过；空/None = 不过滤。
    #[serde(default)]
    pub episode_filter: Option<String>,
}

fn default_true() -> bool {
    true
}

/// RSS 持久化状态（rss.json）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RssState {
    #[serde(default)]
    pub next_feed_id: u64,
    #[serde(default)]
    pub next_rule_id: u64,
    #[serde(default)]
    pub feeds: Vec<RssFeed>,
    #[serde(default)]
    pub rules: Vec<RssRule>,
}

impl RssState {
    pub fn load(path: &std::path::Path) -> Option<RssState> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // 不存在 = 首次启动，静默空状态
            Err(_) => return None,
        };
        match serde_json::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                // batch6-P1：坏文件留存改名（原内容是用户订阅+规则的唯一
                // 证据），再按空启动——fail-open 不再静默覆盖销毁。
                let corrupt = path.with_extension("json.corrupt");
                if std::fs::rename(path, &corrupt).is_ok() {
                    tracing::error!(
                        "rss.json 解析失败（已留存为 {}，按空状态启动）: {e}",
                        corrupt.display()
                    );
                } else {
                    tracing::error!("rss.json 解析失败（留存改名也失败，按空状态启动）: {e}");
                }
                None
            }
        }
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // batch6-P1：与 write_tasks_atomic / write_bans_atomic 同配方——
        // 唯一 tmp 名 + 0600 + rename（feed URL 常内嵌 apikey/token，权限
        // 加固对齐 tasks.json V12 口径）；固定 tmp 名并发写会交错损坏。
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            tracing::warn!("rss.json 序列化失败（保留旧文件）");
            return;
        };
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = path.with_file_name(format!(
            "{}-{unique}.tmp",
            path.file_name().map_or_else(
                || "rss.json".to_string(),
                |f| f.to_string_lossy().into_owned(),
            )
        ));
        let res = (|| {
            std::fs::write(&tmp, &json)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, path)
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!("rss.json 写盘失败（保留旧文件）");
        }
    }
}

/// 单条 feed 解析产物（标题 + 有效条目）。
pub struct ParsedFeed {
    pub title: String,
    pub items: Vec<RssItem>,
}

/// 当前正在收集文本的叶子元素（条目内）。
#[derive(Debug)]
enum TextTarget {
    /// channel/feed 级标题。
    FeedTitle,
    /// 条目标题。
    ItemTitle,
    /// 条目 link 文本（RSS 2.0）。
    ItemLink,
    /// 条目 guid 文本。
    ItemGuid,
    /// 条目时间（pubDate / updated）。
    ItemDate,
}

/// 解析 RSS 2.0 / Atom feed XML → 标题 + 条目列表。
///
/// 逐条容错：缺 URL 条目静默跳过；guid 重复保留首见；整体无有效条目 → Err。
pub fn parse_feed(xml: &str) -> Result<ParsedFeed, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut feed_title: Option<String> = None;
    let mut items: Vec<RssItem> = Vec::new();
    // 条目内状态（None = 不在条目内）。
    let mut in_item = false;
    let mut cur_title: Option<String> = None;
    // Atom `<link href>` 候选（rel=alternate 优先；单项记录两级：首选与兜底）。
    let mut link_href_preferred: Option<String> = None;
    let mut link_href_fallback: Option<String> = None;
    let mut cur_link_text: Option<String> = None;
    let mut cur_guid: Option<String> = None;
    let mut cur_date: Option<String> = None;
    let mut target: Option<TextTarget> = None;

    /// 条目收口：URL 三级兜底（link 文本 → link href → http 形态 guid）+
    /// guid 缺省 = url + guid 去重保留首见。独立函数避免闭包可变捕获与
    /// 解析循环赋值冲突。
    #[allow(clippy::too_many_arguments)]
    fn finish_item(
        title: &mut Option<String>,
        link_text: &mut Option<String>,
        guid: &mut Option<String>,
        date: &mut Option<String>,
        link_href_preferred: &mut Option<String>,
        link_href_fallback: &mut Option<String>,
        items: &mut Vec<RssItem>,
    ) {
        let href = link_href_preferred
            .clone()
            .or_else(|| link_href_fallback.clone());
        let url = link_text
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or(href)
            .or_else(|| {
                guid.clone().filter(|g| {
                    let g = g.trim();
                    g.starts_with("http://") || g.starts_with("https://")
                })
            });
        if let (Some(title), Some(url)) = (
            title.take().filter(|s| !s.trim().is_empty()),
            url.map(|u| u.trim().to_string()).filter(|s| !s.is_empty()),
        ) {
            let guid_key = guid
                .take()
                .map(|g| g.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| url.clone());
            if !items.iter().any(|i| i.guid == guid_key) {
                items.push(RssItem {
                    guid: guid_key,
                    title: title.trim().to_string(),
                    url,
                    pub_date: date.take(),
                    task_id: None,
                });
            }
        }
        // 清场（未被消费的残留状态）
        *title = None;
        *link_text = None;
        *guid = None;
        *date = None;
        *link_href_preferred = None;
        *link_href_fallback = None;
    }

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().local_name().as_ref() {
                b"item" | b"entry" => {
                    in_item = true;
                    cur_title = None;
                    cur_link_text = None;
                    cur_guid = None;
                    cur_date = None;
                    link_href_preferred = None;
                    link_href_fallback = None;
                }
                b"title" => {
                    target = Some(if in_item {
                        TextTarget::ItemTitle
                    } else {
                        TextTarget::FeedTitle
                    })
                }
                b"link" if in_item => {
                    let mut href: Option<String> = None;
                    let mut rel: Option<String> = None;
                    for a in e.attributes() {
                        let a = a.map_err(|er| format!("rss <link> 属性解析失败: {er}"))?;
                        match a.key.local_name().as_ref() {
                            b"href" => href = Some(decode(a, &reader)?),
                            b"rel" => rel = Some(decode(a, &reader)?),
                            _ => {}
                        }
                    }
                    if let Some(href) = href {
                        // rel=alternate（或无 rel）为正主；其余（self/enclosure…）兜底
                        match rel.as_deref() {
                            None | Some("alternate") => {
                                link_href_preferred = Some(href);
                            }
                            _ => {
                                if link_href_fallback.is_none() {
                                    link_href_fallback = Some(href);
                                }
                            }
                        }
                    }
                    target = Some(TextTarget::ItemLink);
                }
                b"guid" if in_item => target = Some(TextTarget::ItemGuid),
                // Atom 条目标识 = <id>（RSS 2.0 为 <guid>；feed 级 <id> 被
                // in_item 守卫排除）
                b"id" if in_item => target = Some(TextTarget::ItemGuid),
                b"pubDate" if in_item => target = Some(TextTarget::ItemDate),
                b"updated" if in_item => target = Some(TextTarget::ItemDate),
                _ => {}
            },
            Ok(Event::Empty(e)) if e.name().local_name().as_ref() == b"link" && in_item => {
                // Atom 自闭合 `<link href="..." rel="..."/>`（Empty 事件不进
                // Start 分支，href 属性在此抓取）
                let mut href: Option<String> = None;
                let mut rel: Option<String> = None;
                for a in e.attributes() {
                    let a = a.map_err(|er| format!("rss <link> 属性解析失败: {er}"))?;
                    match a.key.local_name().as_ref() {
                        b"href" => href = Some(decode(a, &reader)?),
                        b"rel" => rel = Some(decode(a, &reader)?),
                        _ => {}
                    }
                }
                if let Some(href) = href {
                    match rel.as_deref() {
                        None | Some("alternate") => link_href_preferred = Some(href),
                        _ => {
                            if link_href_fallback.is_none() {
                                link_href_fallback = Some(href);
                            }
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let Some(tgt) = &target else { continue };
                let text = t
                    .xml10_content()
                    .map(|c| c.into_owned())
                    .map_err(|e| format!("rss 文本转义非法: {e}"))?;
                store_text(
                    tgt,
                    text,
                    &mut feed_title,
                    &mut cur_title,
                    &mut cur_link_text,
                    &mut cur_guid,
                    &mut cur_date,
                );
            }
            Ok(Event::GeneralRef(r)) => {
                // `&amp;` 等实体引用自 0.38 起拆为独立事件，解析回值走同一累加链
                let Some(tgt) = &target else { continue };
                let text = resolve_ref(&r)?;
                store_text(
                    tgt,
                    text,
                    &mut feed_title,
                    &mut cur_title,
                    &mut cur_link_text,
                    &mut cur_guid,
                    &mut cur_date,
                );
            }
            Ok(Event::CData(t)) => {
                // CDATA 内容为原始文本（XML 实体不转义），直接取用。
                // 输入已是 UTF-8 String，lossy 即恒等。
                let Some(tgt) = &target else { continue };
                let text = String::from_utf8_lossy(&t).into_owned();
                store_text(
                    tgt,
                    text,
                    &mut feed_title,
                    &mut cur_title,
                    &mut cur_link_text,
                    &mut cur_guid,
                    &mut cur_date,
                );
            }
            Ok(Event::End(e)) => match e.name().local_name().as_ref() {
                b"item" | b"entry" => {
                    in_item = false;
                    finish_item(
                        &mut cur_title,
                        &mut cur_link_text,
                        &mut cur_guid,
                        &mut cur_date,
                        &mut link_href_preferred,
                        &mut link_href_fallback,
                        &mut items,
                    );
                }
                b"title" | b"link" | b"guid" | b"pubDate" | b"updated" => target = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("rss XML 解析失败: {e}")),
            _ => {}
        }
    }

    if items.is_empty() {
        return Err("rss feed 无有效条目（非 XML / 无 item/entry / 条目均缺链接）".into());
    }
    Ok(ParsedFeed {
        title: feed_title.unwrap_or_default(),
        items,
    })
}

/// 文本事件按目标槽位落位（Text 与 CData 两分支共用）。
/// 条目槽位（title/link/guid）为累加语义：混合内容（如
/// `plain<![CDATA[..]]>tail` 产生 Text+CData+Text 三事件）拼接完整值；
/// feed 标题/日期保持首见非空优先。
fn store_text(
    tgt: &TextTarget,
    text: String,
    feed_title: &mut Option<String>,
    cur_title: &mut Option<String>,
    cur_link_text: &mut Option<String>,
    cur_guid: &mut Option<String>,
    cur_date: &mut Option<String>,
) {
    match tgt {
        TextTarget::FeedTitle => {
            if feed_title.is_none() && !text.trim().is_empty() {
                *feed_title = Some(text.trim().to_string());
            }
        }
        TextTarget::ItemTitle => append_slot(cur_title, text),
        TextTarget::ItemLink => append_slot(cur_link_text, text),
        TextTarget::ItemGuid => append_slot(cur_guid, text),
        TextTarget::ItemDate => {
            if cur_date.is_none() && !text.trim().is_empty() {
                *cur_date = Some(text.trim().to_string());
            }
        }
    }
}

/// 混合内容累加：已有值则拼接，否则首见。
fn append_slot(slot: &mut Option<String>, text: String) {
    match slot {
        Some(existing) => existing.push_str(&text),
        None => *slot = Some(text),
    }
}

/// 属性值解码（统一错误文案）。
fn decode(
    a: quick_xml::events::attributes::Attribute,
    reader: &Reader<&[u8]>,
) -> Result<String, String> {
    a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
        .map(|c| c.into_owned())
        .map_err(|e| format!("rss 属性解码失败: {e}"))
}

/// 实体引用事件（0.38+ `Event::GeneralRef`）解析回原值：
/// 数字字符引用与预定义 XML 实体；未知命名实体与旧版 unescape 一致地报错。
fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> Result<String, String> {
    if r.is_char_ref() {
        return r
            .resolve_char_ref()
            .map_err(|e| format!("rss 数字字符引用非法: {e}"))?
            .map(|c| c.to_string())
            .ok_or_else(|| "rss 数字字符引用非法".to_string());
    }
    let name = r
        .xml10_content()
        .map_err(|e| format!("rss 实体引用解码失败: {e}"))?;
    quick_xml::escape::resolve_xml_entity(&name)
        .map(str::to_string)
        .ok_or_else(|| format!("rss 未知实体引用: &{name};"))
}

/// 规则匹配（batch5）：关键词（子串或正则）must 全命中 且 must_not 全不命中，
/// 且集数过滤（若配置）命中。
pub fn item_matches(rule: &RssRule, title: &str) -> bool {
    let kw_ok = if rule.use_regex {
        let re_hit = |k: &String| {
            // 用户关键词即正则本体（不转义）；(?i) 大小写不敏感
            regex::Regex::new(&format!("(?i){}", k.trim()))
                .map(|re| re.is_match(title))
                .unwrap_or(false)
        };
        rule.must_contain.iter().all(re_hit) && !rule.must_not_contain.iter().any(re_hit)
    } else {
        let lower = title.to_lowercase();
        let hit = |k: &String| lower.contains(&k.trim().to_lowercase());
        rule.must_contain.iter().all(hit) && !rule.must_not_contain.iter().any(hit)
    };
    if !kw_ok {
        return false;
    }
    match &rule.episode_filter {
        Some(f) if !f.trim().is_empty() => episode_filter_matches(f, title),
        _ => true,
    }
}

/// 集数过滤命中（batch5）：解析过滤串为 (季,集) 单集/区间集合，与标题中
/// 提取的全部 (季,集) 求交。任一标题集数命中集合即 true；过滤串非法
/// → 一律不命中（保守，防误下整季）。
pub fn episode_filter_matches(filter: &str, title: &str) -> bool {
    let mut allow: Vec<EpisodeSel> = Vec::new();
    for tok in filter.split(';') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        match parse_episode_token(tok) {
            Some(sel) => allow.push(sel),
            None => return false,
        }
    }
    if allow.is_empty() {
        return false;
    }
    extract_episodes(title).into_iter().any(|ep| {
        allow
            .iter()
            .any(|a| ep.0 == a.0 && ep.1 >= a.1 && ep.1 <= a.2)
    })
}

/// 过滤集数选择：季 + 集闭区间 [start, end]。
type EpisodeSel = (u32, u32, u32);

/// 单 token 解析：`S02E03` / `2x03` / `2x03-2x06` / `S02E03-S02E06`。
fn parse_episode_token(tok: &str) -> Option<EpisodeSel> {
    let t = tok.trim().to_ascii_lowercase();
    if let Some((x, y)) = t.split_once('-') {
        let (s1, e1) = parse_s_e(x)?;
        let (s2, e2) = parse_s_e(y)?;
        if s1 != s2 || e2 < e1 {
            return None; // 跨季/逆序区间不支持（保守拒绝）
        }
        Some((s1, e1, e2))
    } else {
        let (s, e) = parse_s_e(&t)?;
        Some((s, e, e))
    }
}

/// `s02e03` / `2x03` → (季, 集)。
fn parse_s_e(tok: &str) -> Option<(u32, u32)> {
    let t = tok.trim().to_ascii_lowercase();
    if let Some(rest) = t.strip_prefix('s') {
        let (s, e) = rest.split_once('e')?;
        Some((s.trim().parse().ok()?, e.trim().parse().ok()?))
    } else {
        let (s, e) = t.split_once('x')?;
        Some((s.trim().parse().ok()?, e.trim().parse().ok()?))
    }
}

/// 标题集数提取（regex 实现）：扫描全部 `SxxEyy` 与 `NxxEyy`（大小写不敏感）。
/// S 形态要求词边界防误吸（如 "S1E2" ok；"CASSEROLE" 不含合法 S..E.. 结构
/// 自然不匹配）；x 形态要求 x 两侧均为数字。
fn extract_episodes(title: &str) -> Vec<(u32, u32)> {
    use std::sync::OnceLock;
    static RE_S: OnceLock<regex::Regex> = OnceLock::new();
    static RE_X: OnceLock<regex::Regex> = OnceLock::new();
    let re_s = RE_S.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:^|[^a-z0-9])s(\d{1,3})e(\d{1,3})(?:[^0-9]|$)").unwrap()
    });
    let re_x = RE_X.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:^|[^a-z0-9])(\d{1,3})x(\d{1,3})(?:[^0-9]|$)").unwrap()
    });
    let mut out: Vec<(u32, u32)> = Vec::new();
    // captures_iter（batch6-P2）：标题含多集时全部提取（"S01E01 S01E02"）；
    // captures 只取最左匹配会漏后续集，过滤串含第二集时被误拒。
    for c in re_s.captures_iter(title) {
        if let (Some(s), Some(e)) = (c[1].parse().ok(), c[2].parse().ok()) {
            out.push((s, e));
        }
    }
    for c in re_x.captures_iter(title) {
        if let (Some(s), Some(e)) = (c[1].parse().ok(), c[2].parse().ok()) {
            out.push((s, e));
        }
    }
    out.dedup();
    out
}

// ===== DaemonState 集成 =====

impl DaemonState {
    /// RSS 状态锁入口（字段在 DaemonState.rss，parking_lot Mutex）。
    pub(crate) fn rss_state(&self) -> &parking_lot::Mutex<RssState> {
        &self.rss
    }

    /// RSS 持久化路径（persist_path 同目录 rss.json；None = 不落盘）。
    pub(crate) fn rss_persist_path(&self) -> Option<std::path::PathBuf> {
        self.rss_persist_path.clone()
    }

    fn rss_save(&self, st: &RssState) {
        if let Some(p) = self.rss_persist_path() {
            st.save(&p);
        }
    }

    /// 添加订阅（立即拉取一次，失败即 Err——坏 URL 不应静默入列）。
    pub async fn rss_add_feed(&self, url: String) -> Result<(u64, String, usize), DaemonError> {
        let client = self.rss_client();
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("rss feed 拉取失败: {e}")))?;
        if !resp.status().is_success() {
            return Err(DaemonError::InvalidSource(format!(
                "rss feed 拉取 HTTP {}",
                resp.status()
            )));
        }
        // 审查修复（P2）：限长读取（与 rss_refresh_all 的 16MB 上限同口径），
        // 防恶意/超大 feed 撑爆内存（resp.text() 无上限）。
        const FEED_MAX: usize = 16 * 1024 * 1024;
        let mut xml_buf: Vec<u8> = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("rss feed 读取失败: {e}")))?
        {
            if xml_buf.len() + chunk.len() > FEED_MAX {
                return Err(DaemonError::InvalidSource(
                    "rss feed 响应超过 16MB 上限".into(),
                ));
            }
            xml_buf.extend_from_slice(&chunk);
        }
        let xml = String::from_utf8_lossy(&xml_buf).into_owned();
        let parsed = parse_feed(&xml).map_err(DaemonError::InvalidSource)?;

        let mut st = self.rss_state().lock();
        // 同 URL 订阅去重（409 语义由 handler 转）
        if st.feeds.iter().any(|f| f.url == url) {
            return Err(DaemonError::Duplicate(url));
        }
        st.next_feed_id += 1;
        let id = st.next_feed_id;
        let title = if parsed.title.is_empty() {
            url.clone()
        } else {
            parsed.title
        };
        let count = parsed.items.len();
        st.feeds.push(RssFeed {
            id,
            url,
            title,
            added_at_unix: now_unix(),
            last_refresh_unix: Some(now_unix()),
            interval_override_secs: 0,
            items: parsed.items,
        });
        self.rss_save(&st);
        Ok((id, st.feeds.last().unwrap().title.clone(), count))
    }

    /// 更新订阅设置（batch5.1）：interval_override_secs（0 = 跟随全局）。
    /// 未知 id → Err NotFound。
    pub fn rss_update_feed(&self, id: u64, interval_override_secs: u64) -> Result<(), DaemonError> {
        let mut st = self.rss_state().lock();
        let feed = st
            .feeds
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or_else(|| DaemonError::NotFound(format!("feed {id} 不存在")))?;
        feed.interval_override_secs = interval_override_secs;
        self.rss_save(&st);
        Ok(())
    }

    /// 移除订阅（未知 id → false）。
    pub fn rss_remove_feed(&self, id: u64) -> bool {
        let mut st = self.rss_state().lock();
        let before = st.feeds.len();
        st.feeds.retain(|f| f.id != id);
        let removed = st.feeds.len() != before;
        if removed {
            self.rss_save(&st);
        }
        removed
    }

    /// 添加规则。
    #[allow(clippy::too_many_arguments)]
    pub fn rss_add_rule(
        &self,
        name: String,
        enabled: bool,
        must_contain: Vec<String>,
        must_not_contain: Vec<String>,
        feed_id: Option<u64>,
        tags: Vec<String>,
        dest: Option<String>,
        use_regex: bool,
        episode_filter: Option<String>,
    ) -> Result<u64, DaemonError> {
        // batch5：use_regex 时逐条编译校验（非法正则 add 即拒，避免静默不命中）
        if use_regex {
            for k in &must_contain {
                if regex::Regex::new(&format!("(?i){}", k.trim())).is_err() {
                    return Err(DaemonError::InvalidSource(format!(
                        "正则非法（must_contain）: {k:?}"
                    )));
                }
            }
            for k in &must_not_contain {
                if regex::Regex::new(&format!("(?i){}", k.trim())).is_err() {
                    return Err(DaemonError::InvalidSource(format!(
                        "正则非法（must_not_contain）: {k:?}"
                    )));
                }
            }
        }
        if name.trim().is_empty() {
            return Err(DaemonError::InvalidSource("规则名不可为空".into()));
        }
        if must_contain.is_empty() && must_not_contain.is_empty() {
            return Err(DaemonError::InvalidSource(
                "规则至少需要一个关键词（must_contain / must_not_contain 之一）".into(),
            ));
        }
        // batch6-P2：空白关键词拒绝——trim 后空串的 contains("") 恒 true，
        // must_contain 空白 = 匹配一切，must_not_contain 空白 = 排除一切（语义翻转）。
        for k in must_contain.iter().chain(must_not_contain.iter()) {
            if k.trim().is_empty() {
                return Err(DaemonError::InvalidSource(
                    "关键词不可为空白（空串会恒命中/恒排除）".into(),
                ));
            }
        }
        let mut st = self.rss_state().lock();
        if let Some(fid) = feed_id {
            if !st.feeds.iter().any(|f| f.id == fid) {
                return Err(DaemonError::InvalidSource(format!("feed_id {fid} 不存在")));
            }
        }
        st.next_rule_id += 1;
        let id = st.next_rule_id;
        st.rules.push(RssRule {
            id,
            name: name.trim().to_string(),
            enabled,
            must_contain,
            must_not_contain,
            feed_id,
            tags,
            dest,
            use_regex,
            episode_filter,
        });
        self.rss_save(&st);
        Ok(id)
    }

    /// 移除规则（未知 id → false）。
    pub fn rss_remove_rule(&self, id: u64) -> bool {
        let mut st = self.rss_state().lock();
        let before = st.rules.len();
        st.rules.retain(|r| r.id != id);
        let removed = st.rules.len() != before;
        if removed {
            self.rss_save(&st);
        }
        removed
    }

    /// 刷新全部订阅（手动入口，force=true 不做到期过滤）：拉取 → 条目合并
    ///（guid 去重）→ 规则匹配 → 自动建任务。
    ///
    /// 单 feed 拉取失败不整体失败（错误收集进返回 Vec）；返回
    /// `(新增条目总数, 命中并建任务数, task_ids, 错误列表)`。
    pub async fn rss_refresh_all(&self) -> (usize, usize, Vec<String>, Vec<String>) {
        self.rss_refresh_impl(true).await
    }

    /// 刷新到期订阅（batch5 每 feed 独立间隔；ticker 入口）：
    /// feed 自带 `interval_override_secs > 0` 且距上次刷新未到期 → 跳过。
    pub async fn rss_refresh_due(&self) -> (usize, usize, Vec<String>, Vec<String>) {
        self.rss_refresh_impl(false).await
    }

    /// 刷新实现（force=false 时按 feed 级间隔过滤）。
    /// 持有 `rss_refresh_gate` 串行化整轮刷新（batch6-P1）：手动刷新与
    /// ticker 并发（或 UI 双击）时，后到者排队而非基于同一未处理快照
    /// 各自建任务（同一条目重复下载）。
    async fn rss_refresh_impl(&self, force: bool) -> (usize, usize, Vec<String>, Vec<String>) {
        let _gate = self.rss_refresh_gate.lock().await;
        let client = self.rss_client();
        let mut new_items_total = 0usize;
        let mut matched = 0usize;
        let mut task_ids: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        let now = now_unix();
        let feed_urls: Vec<(u64, String)> = {
            let st = self.rss_state().lock();
            st.feeds
                .iter()
                .filter(|f| {
                    force
                        || f.interval_override_secs == 0
                        || f.last_refresh_unix
                            .map(|t| now.saturating_sub(t) >= f.interval_override_secs)
                            .unwrap_or(true)
                })
                .map(|f| (f.id, f.url.clone()))
                .collect()
        };

        for (feed_id, url) in feed_urls {
            let fetch = async {
                let resp = client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| format!("拉取失败: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status()));
                }
                // batch3-P2：限长读取（16MB 封顶，防恶意 feed 打爆内存）
                const FEED_MAX: usize = 16 * 1024 * 1024;
                let mut xml: Vec<u8> = Vec::new();
                let mut resp = resp;
                while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取失败: {e}"))?
                {
                    if xml.len() + chunk.len() > FEED_MAX {
                        return Err("feed 响应超过 16MB 上限".to_string());
                    }
                    xml.extend_from_slice(&chunk);
                }
                let xml = String::from_utf8_lossy(&xml).into_owned();
                parse_feed(&xml)
            };
            let parsed = match fetch.await {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!("feed #{feed_id} ({url}): {e}"));
                    continue;
                }
            };

            // 合并条目 + 标题锁定（首次空标题回填）+ 已处理条目上限截断
            let existing: Vec<RssItem> = {
                let mut st = self.rss_state().lock();
                let Some(feed) = st.feeds.iter_mut().find(|f| f.id == feed_id) else {
                    continue;
                };
                let known: std::collections::HashSet<String> =
                    feed.items.iter().map(|i| i.guid.clone()).collect();
                let fresh: Vec<RssItem> = parsed
                    .items
                    .into_iter()
                    .filter(|i| !known.contains(&i.guid))
                    .collect();
                new_items_total += fresh.len();
                feed.items.extend(fresh.clone());
                feed.last_refresh_unix = Some(now_unix());
                if feed.title.is_empty() && !parsed.title.is_empty() {
                    feed.title = parsed.title;
                }
                // 防膨胀：已处理条目保留最近 N 条（N=[rss]
                // max_processed_items_per_feed）；未处理条目全保留——截掉会丢
                // 去重标记，源刷新时重复建任务。
                let cap = self.rss_max_processed_items();
                let mut processed: Vec<RssItem> = feed
                    .items
                    .iter()
                    .filter(|i| i.task_id.is_some())
                    .cloned()
                    .collect();
                if processed.len() > cap {
                    let keep = processed.split_off(processed.len() - cap);
                    feed.items.retain(|i| i.task_id.is_none());
                    feed.items.extend(keep);
                }
                feed.items.clone()
            };

            // 规则匹配（对全部未处理条目；含旧条目——新建规则能回溯未处理历史）。
            // 命中后同步标记本地快照（batch6-P1）：否则同一 feed 内多条规则
            // 命中同一条目时会重复建任务（违背首见规则生效语义）。
            let rules: Vec<RssRule> = {
                let st = self.rss_state().lock();
                st.rules.iter().filter(|r| r.enabled).cloned().collect()
            };
            let mut existing = existing;
            for rule in rules {
                if rule.feed_id.map(|fid| fid != feed_id).unwrap_or(false) {
                    continue;
                }
                for item in existing.iter_mut() {
                    if item.task_id.is_some() {
                        continue;
                    }
                    if !item_matches(&rule, &item.title) {
                        continue;
                    }
                    let opts = crate::state::AddHttpOpts {
                        name: Some(item.title.clone()),
                        ..Default::default()
                    };
                    match self
                        .add_link_task_opts(item.url.clone(), rule.dest.clone(), opts)
                        .await
                    {
                        Ok(task_id) => {
                            if !rule.tags.is_empty() {
                                let _ = self.set_task_tags(&task_id, Some(rule.tags.clone()));
                            }
                            let mut st = self.rss_state().lock();
                            if let Some(f) = st.feeds.iter_mut().find(|f| f.id == feed_id) {
                                if let Some(it) = f.items.iter_mut().find(|i| i.guid == item.guid) {
                                    it.task_id = Some(task_id.clone());
                                }
                            }
                            self.rss_save(&st);
                            drop(st);
                            // 本地快照同步标记：后续规则跳过该条目
                            item.task_id = Some(task_id.clone());
                            matched += 1;
                            task_ids.push(task_id);
                        }
                        Err(e) => {
                            errors.push(format!(
                                "feed #{feed_id} 条目 {:?} 建任务失败: {e}",
                                item.title
                            ));
                        }
                    }
                }
            }
        }
        (new_items_total, matched, task_ids, errors)
    }

    /// RSS 拉取 client（bootstrap_client 同口径克隆；None 时裸建）。
    fn rss_client(&self) -> reqwest::Client {
        self.bootstrap_client_opt().unwrap_or_default()
    }

    /// 已处理条目保留上限（live_config 注入时取 [rss] 配置；否则默认 200）。
    /// 0 视为无效回落默认（batch6-P2）：cap=0 会在每次刷新清空全部已处理
    /// 去重标记 → 源 feed 仍列出这些条目 → 规则命中无限重下。
    fn rss_max_processed_items(&self) -> usize {
        self.rss_max_processed_items_opt()
            .filter(|&v| v > 0)
            .unwrap_or(200)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS2: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title>Ubuntu Releases</title>
<item><title>Ubuntu 24.04.2 Desktop amd64 iso</title><link>https://releases.example.com/ubuntu-24.04.iso</link><guid isPermaLink="false">u24042</guid><pubDate>Tue, 01 Sep 2026 10:00:00 GMT</pubDate></item>
<item><title>Ubuntu 23.10 old release</title><link>https://releases.example.com/ubuntu-23.10.iso</link></item>
<item><title>no link item</title><guid>internal-123</guid></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>Debian News</title>
<entry><title>Debian 13 released</title><link rel="self" href="https://example.com/self"/><link href="https://example.com/debian-13.iso"/><id>tag:debian,2026:13</id><updated>2026-09-01T00:00:00Z</updated></entry>
</feed>"#;

    #[test]
    fn parse_rss2_basic() {
        let f = parse_feed(RSS2).unwrap();
        assert_eq!(f.title, "Ubuntu Releases");
        // 缺 link 且 guid 非 URL 的第 3 条跳过
        assert_eq!(f.items.len(), 2);
        assert_eq!(f.items[0].guid, "u24042");
        assert_eq!(
            f.items[0].url,
            "https://releases.example.com/ubuntu-24.04.iso"
        );
        assert_eq!(
            f.items[0].pub_date.as_deref(),
            Some("Tue, 01 Sep 2026 10:00:00 GMT")
        );
        // 缺 guid → url 兜底
        assert_eq!(
            f.items[1].guid,
            "https://releases.example.com/ubuntu-23.10.iso"
        );
    }

    #[test]
    fn parse_atom_basic() {
        let f = parse_feed(ATOM).unwrap();
        assert_eq!(f.title, "Debian News");
        assert_eq!(f.items.len(), 1);
        // rel=self 排除，取无 rel 的 alternate href
        assert_eq!(f.items[0].url, "https://example.com/debian-13.iso");
        assert_eq!(f.items[0].guid, "tag:debian,2026:13");
        assert_eq!(f.items[0].pub_date.as_deref(), Some("2026-09-01T00:00:00Z"));
    }

    const RSS2_CDATA: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title><![CDATA[Arch Linux Releases]]></title>
<item><title><![CDATA[Arch 2026.09 iso (x86_64)]]></title><link><![CDATA[https://geo.example.com/arch-2026.09.iso]]></link><guid isPermaLink="false"><![CDATA[arch-202609]]></guid><pubDate>Wed, 02 Sep 2026 10:00:00 GMT</pubDate></item>
</channel></rss>"#;

    #[test]
    fn parse_rss2_cdata_title_link_guid() {
        // WordPress 等主流 CMS 默认用 CDATA 包裹字段；quick-xml 将其投递为
        // 独立 CData 事件——漏接 = 条目全丢（batch6 审计 P0 回归锚）。
        let f = parse_feed(RSS2_CDATA).unwrap();
        assert_eq!(f.title, "Arch Linux Releases");
        assert_eq!(f.items.len(), 1);
        assert_eq!(f.items[0].title, "Arch 2026.09 iso (x86_64)");
        assert_eq!(f.items[0].url, "https://geo.example.com/arch-2026.09.iso");
        assert_eq!(f.items[0].guid, "arch-202609");
        // 同元素混合形态：Text + CData 相邻时首见优先（Text 先到先得）。
        let mixed = r#"<rss><channel><title>T</title>
<item><title>plain<![CDATA[ + cdata]]>tail</title><link>https://e.example/x.iso</link></item>
</channel></rss>"#;
        let f = parse_feed(mixed).unwrap();
        assert_eq!(f.items[0].title, "plain + cdatatail");
    }

    #[test]
    fn parse_guid_permalink_fallback_and_dedup() {
        let xml = r#"<rss><channel><title>t</title>
<item><title>a</title><guid>https://a.example/x.iso</guid></item>
<item><title>b</title><guid>https://a.example/x.iso</guid></item>
</channel></rss>"#;
        let f = parse_feed(xml).unwrap();
        // guid(http 文本) 兜底 url；重复 guid 去重保留首见
        assert_eq!(f.items.len(), 1);
        assert_eq!(f.items[0].url, "https://a.example/x.iso");
    }

    #[test]
    fn parse_empty_and_bad_xml() {
        assert!(parse_feed("<html><body>not a feed</body></html>").is_err());
        assert!(parse_feed("this is not xml <").is_err());
        assert!(parse_feed("").is_err());
    }

    #[test]
    fn atom_selfclosing_link_variants() {
        let xml = r#"<feed>
<entry><title>e1</title><link href="https://x/1.iso" rel="alternate"/><id>e1</id></entry>
<entry><title>e2</title><link href="https://x/2.iso"/><id>e2</id></entry>
<entry><title>e3</title><link rel="enclosure" href="https://x/3.iso" type="application/x-iso"/><link rel="self" href="https://x/3-self"/><id>e3</id></entry>
</feed>"#;
        let f = parse_feed(xml).unwrap();
        assert_eq!(f.items.len(), 3);
        assert_eq!(f.items[0].url, "https://x/1.iso");
        assert_eq!(f.items[1].url, "https://x/2.iso");
        // enclosure 兜底命中
        assert_eq!(f.items[2].url, "https://x/3.iso");
    }

    #[test]
    fn rule_matching_case_insensitive() {
        let rule = RssRule {
            id: 1,
            name: "r".into(),
            enabled: true,
            must_contain: vec!["ubuntu".into(), "24.04".into()],
            must_not_contain: vec!["beta".into()],
            feed_id: None,
            tags: vec![],
            dest: None,
            use_regex: false,
            episode_filter: None,
        };
        assert!(item_matches(&rule, "Ubuntu 24.04.2 Desktop"));
        assert!(!item_matches(&rule, "Ubuntu 24.04 beta"));
        assert!(!item_matches(&rule, "Debian 13"));
        let exclude_only = RssRule {
            must_contain: vec![],
            must_not_contain: vec!["alpha".into()],
            ..rule.clone()
        };
        assert!(item_matches(&exclude_only, "Anything stable"));
        assert!(!item_matches(&exclude_only, "x ALPHA y"));
    }

    #[test]
    fn rss_update_feed_interval_and_notfound() {
        use crate::state::state_tests::FakeEngine;
        use crate::state::DaemonState;
        use std::sync::Arc;

        let state = DaemonState::new(
            Arc::new(FakeEngine::new(smart_dl_core::types::EngineKind::Http)),
            vec![],
        );
        {
            let mut st = state.rss_state().lock();
            st.next_feed_id += 1;
            let fid = st.next_feed_id;
            st.feeds.push(RssFeed {
                id: fid,
                url: "https://example/rss.xml".into(),
                title: "t".into(),
                added_at_unix: 0,
                last_refresh_unix: None,
                interval_override_secs: 0,
                items: vec![],
            });
        }
        let id = 1;
        state.rss_update_feed(id, 300).unwrap();
        assert_eq!(
            state.rss_state().lock().feeds[0].interval_override_secs,
            300
        );
        assert!(state.rss_update_feed(id, 0).is_ok());
        assert_eq!(state.rss_state().lock().feeds[0].interval_override_secs, 0);
        assert!(
            state.rss_update_feed(99, 100).is_err(),
            "未知 id → NotFound"
        );
    }
}
