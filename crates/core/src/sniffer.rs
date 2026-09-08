//! 协议嗅探规则引擎（Task 5-d/T3）。
//!
//! 来源：FileCentipede（文件蜈蚣）三层/四级嗅探规则引擎逆向分析
//! （`docs/research/clients/multi_downloader/analysis/02_filecentipede/
//! filecentipede_architecture.md` §4.3-§4.8，Rust 原型
//! `07_rust_proto/multi_downloader/src/sniffer/`）。
//!
//! 移植范围（本模块 = 「输入 URL/文本 → 输出候选下载源」的纯函数层）：
//! 1. **scheme 直判**：`thunder:// qqdl:// flashget:// fs2you:// magnet:? ed2k://
//!    ftp:// http(s)://`（对齐 BitComet `url_helper_bclink` 的 7 协议面）。
//!    thunder/qqdl/flashget 为 base64 封装链，解码还原内层真实 URL。
//! 2. **文本嗅探**：从剪贴板/页面文本中按边界切词提取全部链接
//!    （对应 FC `content_extract.js` 的字符串分析与 `main_window.cpp`
//!    剪贴板监听；正则规则在无 `regex` 依赖下用切词 + 前缀表等价实现）。
//! 3. **协议推断**：按 URL 特征推断语义 —— `.torrent` 后缀 → 种子文件、
//!    `pan.xunlei.com/s/` → 迅雷网盘分享、`pan.quark.cn/s/` → 夸克网盘分享
//!    （对齐 FC「磁链/ed2k 无脑嗅、HTTP 必须有可识别特征才嗅」的取舍，
//!    见 FC 分析 §4.8）。
//!
//! 规则表可配置（优先级数组，`Sniffer::with_rules`），默认规则内置
//! （`default_rules`）；对应 FC 的「站点规则 > 扩展名 > MIME > 正则」
//! 优先级思想（本模块为 URL 级输入，无 Content-Type，故 MIME 层不适用）。
//!
//! 设计约束：`smart-dl-core` 无 `regex` 依赖，嗅探全部用前缀表 + 切词实现；
//! 输出 [`SniffedSource`] 可经 [`SniffedSource::to_download_source`] 直接
//! 映射为 `DownloadSource`，供 `router`/`source_parse`（Task 5-a 独占）消费。

use crate::types::DownloadSource;

/// 嗅探结果类别（规则表的动作标签）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SniffKind {
    /// `thunder://` 迅雷链（base64 包裹 `AA<url>ZZ`，§7.1）。
    Thunder,
    /// `qqdl://`（QQ 旋风，base64 直包 URL）。
    QqDl,
    /// `flashget://`（快车链，`[FLASHGET]` 包裹 base64）。
    FlashGet,
    /// `fs2you://`（RayFile，base64 内嵌 `cachefile://` 或 http 直链）。
    Fs2You,
    /// `magnet:?xt=urn:btih:...`。
    Magnet,
    /// `ed2k://`。
    Ed2k,
    /// `ftp://`。
    Ftp,
    /// 普通 `http(s)://` 直链。
    Http,
    /// `.torrent` 后缀（种子文件 URL，需先取回字节再入 BT 引擎）。
    TorrentFile,
    /// 迅雷网盘分享（`https://pan.xunlei.com/s/<id>`）。
    XunleiShare,
    /// 夸克网盘分享（`https://pan.quark.cn/s/<pwd_id>`）。
    QuarkShare,
}

impl SniffKind {
    /// 规则名（诊断/日志用，对应 FC 规则表的 `rule_name`）。
    pub fn rule_name(self) -> &'static str {
        match self {
            SniffKind::Thunder => "thunder_link",
            SniffKind::QqDl => "qqdl_link",
            SniffKind::FlashGet => "flashget_link",
            SniffKind::Fs2You => "fs2you_link",
            SniffKind::Magnet => "magnet_link",
            SniffKind::Ed2k => "ed2k_link",
            SniffKind::Ftp => "ftp_link",
            SniffKind::Http => "http_link",
            SniffKind::TorrentFile => "torrent_suffix",
            SniffKind::XunleiShare => "xunlei_share",
            SniffKind::QuarkShare => "quark_share",
        }
    }
}

/// 单条嗅探规则（可配置规则表项；priority 越小越优先）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SniffRule {
    /// 规则名（诊断用）。
    pub name: &'static str,
    /// 命中后的类别。
    pub kind: SniffKind,
    /// scheme 前缀（小写，含 `://`；magnet 用 `magnet:?`）。
    pub prefix: &'static str,
    /// 优先级（小 = 先匹配；同前缀多规则时先到先得）。
    pub priority: u32,
    /// 是否启用（对应 FC 规则表 enable_<scheme> 开关）。
    pub enabled: bool,
}

/// 嗅探结果：一条候选下载源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SniffedSource {
    /// 类别。
    pub kind: SniffKind,
    /// 原始输入（未加工）。
    pub raw: String,
    /// 规范化载荷：http(s) 为 URL 本身；封装链为解码后的内层 URL；
    /// magnet/ed2k 为链接本身。
    pub payload: String,
    /// 命中的规则名。
    pub rule: &'static str,
    /// 附加说明（如解码失败降级、需二次获取种子等）。
    pub note: Option<String>,
}

/// 嗅探器：规则表 + 匹配入口。
#[derive(Clone, Debug)]
pub struct Sniffer {
    rules: Vec<SniffRule>,
}

impl Default for Sniffer {
    fn default() -> Self {
        Sniffer {
            rules: default_rules(),
        }
    }
}

/// 默认规则表（优先级数组；顺序即匹配顺序）。
///
/// 优先级设计（对齐 FC「明确协议 > 特征推断」思想）：
/// 1. 私有/封装链最先（thunder/qqdl/flashget/fs2you）——前缀最具体；
/// 2. magnet/ed2k/ftp —— 无歧义 scheme；
/// 3. http(s) 最后兜底，命中后再做网盘分享 / `.torrent` 后缀二次推断。
pub fn default_rules() -> Vec<SniffRule> {
    let mut rules = vec![
        SniffRule {
            name: "thunder_link",
            kind: SniffKind::Thunder,
            prefix: "thunder://",
            priority: 10,
            enabled: true,
        },
        SniffRule {
            name: "qqdl_link",
            kind: SniffKind::QqDl,
            prefix: "qqdl://",
            priority: 11,
            enabled: true,
        },
        SniffRule {
            name: "flashget_link",
            kind: SniffKind::FlashGet,
            prefix: "flashget://",
            priority: 12,
            enabled: true,
        },
        SniffRule {
            name: "fs2you_link",
            kind: SniffKind::Fs2You,
            prefix: "fs2you://",
            priority: 13,
            enabled: true,
        },
        SniffRule {
            name: "magnet_link",
            kind: SniffKind::Magnet,
            prefix: "magnet:?",
            priority: 20,
            enabled: true,
        },
        SniffRule {
            name: "ed2k_link",
            kind: SniffKind::Ed2k,
            prefix: "ed2k://",
            priority: 21,
            enabled: true,
        },
        SniffRule {
            name: "ftp_link",
            kind: SniffKind::Ftp,
            prefix: "ftp://",
            priority: 22,
            enabled: true,
        },
        SniffRule {
            name: "http_link",
            kind: SniffKind::Http,
            prefix: "https://",
            priority: 30,
            enabled: true,
        },
        SniffRule {
            name: "http_link",
            kind: SniffKind::Http,
            prefix: "http://",
            priority: 31,
            enabled: true,
        },
    ];
    rules.sort_by_key(|r| r.priority);
    rules
}

impl Sniffer {
    /// 默认规则集。
    pub fn new() -> Self {
        Self::default()
    }

    /// 自定义规则表（按 priority 升序排序后生效）。
    pub fn with_rules(mut rules: Vec<SniffRule>) -> Self {
        rules.sort_by_key(|r| r.priority);
        Sniffer { rules }
    }

    /// 当前规则表（优先级序）。
    pub fn rules(&self) -> &[SniffRule] {
        &self.rules
    }

    /// 启用/停用指定规则名（对应 FC 剪贴板监听的 enable_<scheme> 开关）。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        for r in &mut self.rules {
            if r.name == name {
                r.enabled = enabled;
            }
        }
    }

    /// 嗅探单个 URL/链接（scheme 直判 + 特征推断）。
    ///
    /// 输入容忍首尾空白与常见包裹符（引号/尖括号）。
    pub fn sniff_url(&self, input: &str) -> Option<SniffedSource> {
        let raw = trim_wrappers(input);
        if raw.is_empty() {
            return None;
        }
        let lower = raw.to_ascii_lowercase();
        for rule in &self.rules {
            if !rule.enabled || !lower.starts_with(rule.prefix) {
                continue;
            }
            return Some(self.classify(rule, &raw, &lower));
        }
        None
    }

    /// 文本嗅探：从混合文本（剪贴板/页面/论坛帖）提取全部链接。
    ///
    /// 对齐 FC `content_extract.js` 的字符串分析 + `main_window.cpp`
    /// 剪贴板监听语义：按边界切词 → 逐词 `sniff_url` → 去重（按 payload）。
    pub fn sniff_text(&self, text: &str) -> Vec<SniffedSource> {
        let mut out: Vec<SniffedSource> = Vec::new();
        for token in tokenize(text) {
            if let Some(src) = self.sniff_url(&token) {
                if !out.iter().any(|s| s.payload == src.payload) {
                    out.push(src);
                }
            }
        }
        out
    }

    /// 规则命中后的类别细分（网盘分享 / .torrent 推断）。
    fn classify(&self, rule: &SniffRule, raw: &str, lower: &str) -> SniffedSource {
        // ---- scheme 直判的解码分支 ----
        match rule.kind {
            SniffKind::Thunder => {
                return decode_wrapped(raw, rule.kind, rule.name, |s| {
                    let s = s.strip_prefix("AA").unwrap_or(s);
                    let s = s.strip_suffix("ZZ").unwrap_or(s);
                    s.to_string()
                });
            }
            SniffKind::QqDl => {
                return decode_wrapped(raw, rule.kind, rule.name, |s| s.to_string());
            }
            SniffKind::FlashGet => {
                // [FLASHGET]<base64>[FLASHGET]，包裹符可能被文本嗅探剥掉
                return decode_wrapped(raw, rule.kind, rule.name, |s| {
                    let mut t = s.trim().to_string();
                    for pat in ["[FLASHGET]", "FLASHGET"] {
                        if let Some(x) = t.strip_prefix(pat) {
                            t = x.to_string();
                            break;
                        }
                    }
                    for pat in ["[FLASHGET]", "FLASHGET"] {
                        if let Some(x) = t.strip_suffix(pat) {
                            t = x.to_string();
                            break;
                        }
                    }
                    t
                });
            }
            SniffKind::Fs2You => {
                // fs2you://<base64>，内层可能是 cachefile:// 或 http(s)
                return decode_wrapped(raw, rule.kind, rule.name, |s| s.to_string());
            }
            _ => {}
        }

        // ---- http(s) 兜底后的特征推断（协议推断层）----
        if rule.kind == SniffKind::Http {
            if let Some(share) = infer_share(raw, lower) {
                return share;
            }
            if has_torrent_suffix(lower) {
                return SniffedSource {
                    kind: SniffKind::TorrentFile,
                    raw: raw.to_string(),
                    payload: raw.to_string(),
                    rule: "torrent_suffix",
                    note: Some("需下载 .torrent 字节后以 TorrentFile 提交 BT 引擎".into()),
                };
            }
        }

        // ---- 直判（magnet/ed2k/ftp/http 普通链）----
        SniffedSource {
            kind: rule.kind,
            raw: raw.to_string(),
            payload: raw.to_string(),
            rule: rule.name,
            note: None,
        }
    }
}

impl SniffedSource {
    /// 映射为主工作区 `DownloadSource`（router 可直接消费）。
    ///
    /// 映射说明：
    /// - Thunder/QqDl/FlashGet/Fs2You → 解码后的内层 URL 归类：
    ///   ftp:// → `Ftp`（匿名占位，用户名口令未知），其余 → `Thunder` /
    ///   `Http`。`Thunder` 变体本身即「解码为 Http（§7.1）」的承载。
    /// - QuarkShare → `Http{}` 承载（provider::quark 按 URL 识别分享）。
    /// - TorrentFile（语义）→ `Http{}` 承载（.torrent 是一个待下载的 URL，
    ///   取回字节后才成为 `DownloadSource::TorrentFile`）。
    pub fn to_download_source(&self) -> DownloadSource {
        match self.kind {
            SniffKind::Magnet => DownloadSource::Magnet(self.payload.clone()),
            SniffKind::Ed2k => DownloadSource::Ed2k(self.payload.clone()),
            SniffKind::Ftp => DownloadSource::Ftp {
                url: self.payload.clone(),
                user: "anonymous".into(),
                pass: "anonymous".into(),
            },
            SniffKind::Thunder => DownloadSource::Thunder(self.payload.clone()),
            SniffKind::XunleiShare => DownloadSource::XunleiShare(self.payload.clone()),
            SniffKind::Http
            | SniffKind::TorrentFile
            | SniffKind::QuarkShare
            | SniffKind::QqDl
            | SniffKind::FlashGet
            | SniffKind::Fs2You => DownloadSource::Http {
                url: self.payload.clone(),
                headers: vec![],
                auth: None,
                backup_url: None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 剥离包裹符与首尾空白（`<url>`、`"url"`、`url。` 等）。
fn trim_wrappers(s: &str) -> String {
    let mut out = s
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string();
    out = out
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    // 词尾标点循环剥离：URL 不会以这些字符合法结尾；
    // "…/d.torrent."（句末点）剥掉最后一个点后恰保留合法的 .torrent 后缀。
    while let Some(last) = out.chars().last() {
        if !matches!(last, '.' | ',' | ';' | '。' | '，' | '；' | '！' | '？') {
            break;
        }
        out.truncate(out.len() - last.len_utf8());
    }
    out.trim().to_string()
}

/// 文本切词：链接边界字符集（空白 + 常见引号/括号/书名号/全角标点）。
fn tokenize(text: &str) -> Vec<String> {
    const BOUND: &[char] = &[
        ' ', '\t', '\r', '\n', '"', '\'', '`', '<', '>', '(', ')', '[', ']', '{', '}', '《', '》',
        '“', '”', '‘', '’', '（', '）', '【', '】',
    ];
    text.split(|c: char| BOUND.contains(&c))
        .map(|t| {
            // 词内尾部标点清理（保留 scheme 内合法字符）
            t.trim_end_matches([',', ';', '。', '，', '；']).to_string()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// base64 解码（标准字母表，容忍缺省 padding；失败返回 None）。
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        if c == b'=' || c == b'\r' || c == b'\n' {
            break;
        }
        let idx = TABLE.iter().position(|&t| t == c)?;
        buf = (buf << 6) | idx as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// 封装链通用解码：scheme 后取 base64 段 → 解码 → 按 `unwrap` 规则剥包裹 →
/// 校验内层是 http/ftp/magnet/ed2k 之一；解码失败则降级保留原文（note 说明）。
fn decode_wrapped(
    raw: &str,
    kind: SniffKind,
    rule_name: &'static str,
    unwrap: impl Fn(&str) -> String,
) -> SniffedSource {
    let b64 = raw.split("://").nth(1).unwrap_or("");
    let degraded = |note: &str| SniffedSource {
        kind,
        raw: raw.to_string(),
        payload: raw.to_string(),
        rule: rule_name,
        note: Some(note.to_string()),
    };
    let decoded = match b64_decode(b64) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => return degraded("base64 解码失败，保留原文待上游处理"),
    };
    let inner = unwrap(&decoded);
    let ok = [
        "http://",
        "https://",
        "ftp://",
        "magnet:?",
        "ed2k://",
        "cachefile://",
    ]
    .iter()
    .any(|p| inner.to_ascii_lowercase().starts_with(p));
    if !ok {
        return degraded("内层非已知协议，保留原文待上游处理");
    }
    SniffedSource {
        kind,
        raw: raw.to_string(),
        payload: inner,
        rule: rule_name,
        note: None,
    }
}

/// http(s) 特征推断：网盘分享链接。
fn infer_share(raw: &str, lower: &str) -> Option<SniffedSource> {
    if lower.starts_with("https://pan.xunlei.com/s/")
        || lower.starts_with("http://pan.xunlei.com/s/")
    {
        return Some(SniffedSource {
            kind: SniffKind::XunleiShare,
            raw: raw.to_string(),
            payload: raw.to_string(),
            rule: "xunlei_share",
            note: None,
        });
    }
    if lower.starts_with("https://pan.quark.cn/s/") || lower.starts_with("http://pan.quark.cn/s/") {
        return Some(SniffedSource {
            kind: SniffKind::QuarkShare,
            raw: raw.to_string(),
            payload: raw.to_string(),
            rule: "quark_share",
            note: Some("交由 provider::quark 转存解析为直链".into()),
        });
    }
    None
}

/// `.torrent` 后缀判断（去 query/fragment 后检查路径最后一段）。
fn has_torrent_suffix(lower: &str) -> bool {
    let no_frag = lower.split('#').next().unwrap_or(lower);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    no_query
        .rsplit('/')
        .next()
        .map(|seg| seg.ends_with(".torrent"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thunder_decodes_to_inner_url() {
        // "AAhttps://example.com/file.zipZZ" 的 base64
        let b64 = "QUFodHRwczovL2V4YW1wbGUuY29tL2ZpbGUuemlwWlo=";
        let s = Sniffer::new()
            .sniff_url(&format!("thunder://{b64}"))
            .unwrap();
        assert_eq!(s.kind, SniffKind::Thunder);
        assert_eq!(s.payload, "https://example.com/file.zip");
        assert_eq!(s.note, None);
        assert_eq!(s.rule, "thunder_link");
    }

    #[test]
    fn qqdl_and_flashget_decode() {
        let sn = Sniffer::new();
        // qqdl：base64 直包
        let b64 = "aHR0cDovL2V4YW1wbGUuY29tL2EucmFy";
        let s = sn.sniff_url(&format!("qqdl://{b64}")).unwrap();
        assert_eq!(s.payload, "http://example.com/a.rar");
        // flashget：[FLASHGET] 包裹
        let b64 = "W0ZMQVNIR0VUXWh0dHA6Ly9leGFtcGxlLmNvbS9iLmV4ZVtGTEFTSEdFVF0=";
        let s2 = sn.sniff_url(&format!("flashget://{b64}")).unwrap();
        assert_eq!(s2.payload, "http://example.com/b.exe");
    }

    #[test]
    fn fs2you_decodes_cachefile_inner() {
        // "cachefile://fs2.example.com/data/file.rar" 的 base64
        let b64 = "Y2FjaGVmaWxlOi8vZnMyLmV4YW1wbGUuY29tL2RhdGEvZmlsZS5yYXI=";
        let s = Sniffer::new()
            .sniff_url(&format!("fs2you://{b64}"))
            .unwrap();
        assert_eq!(s.kind, SniffKind::Fs2You);
        assert_eq!(s.payload, "cachefile://fs2.example.com/data/file.rar");
        assert_eq!(s.note, None);
    }

    #[test]
    fn wrapped_scheme_decode_failure_degrades() {
        let s = Sniffer::new()
            .sniff_url("thunder://!!!not-base64!!!")
            .unwrap();
        assert_eq!(s.kind, SniffKind::Thunder);
        assert!(s.note.is_some(), "解码失败应降级并给 note");
        assert_eq!(s.payload, "thunder://!!!not-base64!!!");
    }

    #[test]
    fn direct_schemes_pass_through() {
        let sn = Sniffer::new();
        let m = sn.sniff_url("magnet:?xt=urn:btih:DEADBEEF&dn=iso").unwrap();
        assert_eq!(m.kind, SniffKind::Magnet);
        assert_eq!(m.payload, "magnet:?xt=urn:btih:DEADBEEF&dn=iso");
        assert_eq!(
            sn.sniff_url("ed2k://|file|a.bin|1024|abcd|").unwrap().kind,
            SniffKind::Ed2k
        );
        assert_eq!(
            sn.sniff_url("ftp://host/pub/x.zip").unwrap().kind,
            SniffKind::Ftp
        );
        assert_eq!(
            sn.sniff_url("https://cdn.example.com/x.iso").unwrap().kind,
            SniffKind::Http
        );
    }

    #[test]
    fn share_links_inferred() {
        let sn = Sniffer::new();
        let x = sn
            .sniff_url("https://pan.xunlei.com/s/ABC123?pwd=abcd")
            .unwrap();
        assert_eq!(x.kind, SniffKind::XunleiShare);
        let q = sn
            .sniff_url("https://pan.quark.cn/s/8a7b6c5d#/list/share")
            .unwrap();
        assert_eq!(q.kind, SniffKind::QuarkShare);
        assert!(q.note.as_deref().unwrap().contains("provider::quark"));
        // 普通站点不误判
        assert_eq!(
            sn.sniff_url("https://github.com/s/repo").unwrap().kind,
            SniffKind::Http
        );
    }

    #[test]
    fn torrent_suffix_inferred() {
        let sn = Sniffer::new();
        let t = sn
            .sniff_url("https://tracker.example.org/dl/BigFile.iso.torrent?token=1")
            .unwrap();
        assert_eq!(t.kind, SniffKind::TorrentFile);
        assert_eq!(
            t.payload,
            "https://tracker.example.org/dl/BigFile.iso.torrent?token=1"
        );
        // query 里的 .torrent 不算后缀（最后一段是路径段）
        let n = sn
            .sniff_url("https://a.com/torrent.list?url=x.torrent")
            .unwrap();
        assert_eq!(n.kind, SniffKind::Http);
    }

    #[test]
    fn mixed_text_extracts_multiple_links_deduped() {
        let text = "冲这仨：\n\
                    1) 种子 https://a.com/x.torrent，\n\
                    2) 磁力 magnet:?xt=urn:btih:AAAA1111；\n\
                    3) <https://b.com/y.zip> 以及重复的 magnet:?xt=urn:btih:AAAA1111\n\
                    还有垃圾 token=abc 不是链接。";
        let out = Sniffer::new().sniff_text(text);
        let payloads: Vec<&str> = out.iter().map(|s| s.payload.as_str()).collect();
        assert_eq!(payloads.len(), 3, "去重后应恰 3 条：{payloads:?}");
        assert!(payloads.contains(&"https://a.com/x.torrent"));
        assert!(payloads.contains(&"magnet:?xt=urn:btih:AAAA1111"));
        assert!(payloads.contains(&"https://b.com/y.zip"));
        assert_eq!(out[0].kind, SniffKind::TorrentFile);
    }

    #[test]
    fn mixed_text_with_wrapped_and_share_links() {
        // 混合文本：封装链（thunder）+ 网盘分享（quark）+ 干扰词
        let b64 = "QUFodHRwczovL2V4YW1wbGUuY29tL2lubmVyLnppcFpa";
        let text = format!(
            "链接① thunder://{b64} 链接② https://pan.quark.cn/s/qk42#/list/share?pwd=7777 完"
        );
        let out = Sniffer::new().sniff_text(&text);
        assert_eq!(
            out.len(),
            2,
            "应恰 2 条：{:?}",
            out.iter().map(|s| &s.payload).collect::<Vec<_>>()
        );
        assert_eq!(out[0].kind, SniffKind::Thunder);
        assert_eq!(out[0].payload, "https://example.com/inner.zip");
        assert_eq!(out[1].kind, SniffKind::QuarkShare);
        // 网盘分享保留完整 URL（fragment/query 由 provider::quark::parse_share_link 再解析）
        assert_eq!(
            out[1].payload,
            "https://pan.quark.cn/s/qk42#/list/share?pwd=7777"
        );
    }

    #[test]
    fn invalid_inputs_return_none() {
        let sn = Sniffer::new();
        assert!(sn.sniff_url("").is_none());
        assert!(sn.sniff_url("   ").is_none());
        assert!(sn.sniff_url("随便说点什么 not a link").is_none());
        assert!(
            sn.sniff_url("www.example.com/file.zip").is_none(),
            "无 scheme 不嗅探"
        );
        assert!(sn.sniff_url("file:///C:/x.exe").is_none());
    }

    #[test]
    fn rules_are_configurable() {
        let mut sn = Sniffer::new();
        sn.set_enabled("ed2k_link", false);
        assert!(
            sn.sniff_url("ed2k://|file|a.bin|1|aa|").is_none(),
            "停用规则后不再命中"
        );
        // 自定义规则表：只保留 magnet
        let sn2 = Sniffer::with_rules(vec![SniffRule {
            name: "magnet_link",
            kind: SniffKind::Magnet,
            prefix: "magnet:?",
            priority: 1,
            enabled: true,
        }]);
        assert!(sn2.sniff_url("magnet:?xt=urn:btih:BB22").is_some());
        assert!(sn2.sniff_url("https://a.com/z.bin").is_none());
        // 规则表按 priority 排序
        let rs = sn2.rules();
        assert!(rs.windows(2).all(|w| w[0].priority <= w[1].priority));
    }

    #[test]
    fn to_download_source_mapping() {
        let sn = Sniffer::new();
        let m = sn.sniff_url("magnet:?xt=urn:btih:CC33").unwrap();
        assert_eq!(
            m.to_download_source(),
            DownloadSource::Magnet("magnet:?xt=urn:btih:CC33".into())
        );
        let f = sn.sniff_url("ftp://h/x.iso").unwrap();
        match f.to_download_source() {
            DownloadSource::Ftp { url, user, .. } => {
                assert_eq!(url, "ftp://h/x.iso");
                assert_eq!(user, "anonymous");
            }
            other => panic!("应为 Ftp：{other:?}"),
        }
        let t = sn.sniff_url("https://a.com/x.torrent").unwrap();
        match t.to_download_source() {
            DownloadSource::Http { url, .. } => assert_eq!(url, "https://a.com/x.torrent"),
            other => panic!("TorrentFile 语义以 Http 承载：{other:?}"),
        }
    }

    #[test]
    fn wrappers_and_punctuation_trimmed() {
        let sn = Sniffer::new();
        assert_eq!(
            sn.sniff_url("<https://a.com/a.zip>").unwrap().payload,
            "https://a.com/a.zip"
        );
        assert_eq!(
            sn.sniff_url("\"https://a.com/b.zip\"").unwrap().payload,
            "https://a.com/b.zip"
        );
        assert_eq!(
            sn.sniff_url("https://a.com/c.zip。").unwrap().payload,
            "https://a.com/c.zip"
        );
        // .torrent 结尾的句点不能剥掉合法后缀
        let t = sn.sniff_url("https://a.com/d.torrent.").unwrap();
        assert_eq!(t.kind, SniffKind::TorrentFile);
        assert_eq!(t.payload, "https://a.com/d.torrent");
    }
}
