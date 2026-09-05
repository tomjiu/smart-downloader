//! Metalink4（RFC 5854）XML 解析（B1）。
//!
//! 定位：daemon add 链路的「展开器」——把 `.meta4`/`.metalink` 描述文件解析为
//! 文件列表（每文件：主/备 URL + 内建哈希），逐文件复用现有 HTTP 任务链
//! （mirror failover E2/E3 + 哈希校验 E3 直通），不新增引擎类型。
//!
//! 解析口径（v1 范围）：
//! - 按 **local name** 匹配标签（默认命名空间 `xmlns="urn:ietf:params:xml:ns:metalink"`
//!   与带前缀 `<ml:file>` 均兼容；前缀绑定是否真为 metalink 命名空间不做校验——
//!   RFC 5854 文档实践中前缀罕见，误绑定视为恶意输入由哈希校验兜底）。
//! - `<file name="...">` name 为必需属性（缺 → Err）；带子目录的 name 仅取
//!   末段文件名（HTTP 单文件引擎无子目录落盘语义，见 [`MetalinkFile::display_name`]）。
//! - `<size>` 非法数字 → Err（RFC 必为非负整数，静默吞错会破坏空间预检语义）。
//! - `<hash type="...">` 仅识别 sha256/sha1/md5（现有校验链支持的集合），
//!   连字符变体（`sha-256`/`sha-1`，现实文件常见）归一化后识别；其余类型
//!   （pgp/ed2k-sha1 等）忽略文本。
//! - `<url priority= location=>`：priority 非法整数 → Err；语义 1 最高（1..=999），
//!   排序升序在前、None 排后、同值保持出现序（稳定排序）。
//! - CDATA/注释/PI 等非 Text 叶子事件忽略（URL/哈希实践中不会用 CDATA 书写；
//!   引入只会扩大容错面）。
//! - 根元素不校验（`<files>` 包一层或根为 `<file>` 的松散输入也能展开）；
//!   无任何 `<file>` → Err（上层转 400）。

use quick_xml::events::Event;
use quick_xml::Reader;

/// `<url>` 元素（RFC 5854 §3.2）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetalinkUrl {
    pub url: String,
    /// RFC 5854 priority：1..=999，**数值越小优先级越高**；None = 未声明。
    pub priority: Option<i32>,
    /// 地理/组织暗示（如 "US"、"cn"），仅透传展示用，v1 不参与选路。
    pub location: Option<String>,
}

/// `<file>` 元素（RFC 5854 §3.1）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetalinkFile {
    /// name 属性（必需；可能含 `/` 子目录前缀）。
    pub name: String,
    /// `<size>`（字节）。
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    /// 出现序的 `<url>` 列表（排序用 [`MetalinkFile::sorted_urls`]）。
    pub urls: Vec<MetalinkUrl>,
}

impl MetalinkFile {
    /// 按 priority 升序（小者优先），None 排后，同值保持出现序（`sort_by_key` 稳定）。
    pub fn sorted_urls(&self) -> Vec<&MetalinkUrl> {
        let mut v: Vec<&MetalinkUrl> = self.urls.iter().collect();
        v.sort_by_key(|u| u.priority.unwrap_or(i32::MAX));
        v
    }

    /// 仅 http(s) URL 的 priority 排序视图（B1 v1 范围：展开走 HTTP 引擎链）。
    /// RFC 5854 允许 ftp:// 等混合协议，非 http(s) 部分过滤不参与展开
    /// （v1 无 FTP 任务生成面；后续可按协议分流）。过滤后为空 → 上层拒绝该 file。
    pub fn http_sorted_urls(&self) -> Vec<&MetalinkUrl> {
        let mut v: Vec<&MetalinkUrl> = self
            .urls
            .iter()
            .filter(|u| u.url.starts_with("http://") || u.url.starts_with("https://"))
            .collect();
        v.sort_by_key(|u| u.priority.unwrap_or(i32::MAX));
        v
    }

    /// 主源校验目标择强（sha256 > sha1 > md5）：`AddHttpOpts` 主源哈希互斥
    /// 单槽位，metalink 多 hash 并存时按强度取一，其余丢弃。
    pub fn best_hash(&self) -> Option<(&'static str, &str)> {
        if let Some(h) = &self.sha256 {
            return Some(("sha256", h.as_str()));
        }
        if let Some(h) = &self.sha1 {
            return Some(("sha1", h.as_str()));
        }
        self.md5.as_ref().map(|h| ("md5", h.as_str()))
    }

    /// 显式落盘名（v1 简化）：name 带子目录（`dir/file.iso`，含 Windows 风格
    /// `\`）时仅取末段——HTTP 单文件引擎按「文件名」落盘，无子目录展开语义
    /// （aria2 的多级目录展开属 B 档后续增强）。末段再过 `sanitize_rel` V3 终审
    /// （拒绝绝对路径 / `..` / 空名），`.` 与空串显式拒（sanitize_rel 放行 CurDir）。
    pub fn display_name(&self) -> Result<String, String> {
        let seg = self.name.rsplit('/').next().unwrap_or("");
        let seg = seg.rsplit('\\').next().unwrap_or(seg).trim();
        if seg.is_empty() || seg == "." {
            return Err(format!(
                "metalink <file> name 非法（末段为空或 .）: {:?}",
                self.name
            ));
        }
        smart_dl_core::session::output::sanitize_rel(seg)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("metalink <file> name 非法: {e}"))
    }
}

/// 当前正在收集文本的叶子元素。
#[derive(Debug)]
enum TextTarget {
    /// `<size>`。
    Size,
    /// `<url>` 文本回填 `urls` 最后一个元素。
    Url,
    /// `<hash type="...">`（type 已小写归一；未识别类型不产生此目标）。
    Hash(String),
}

/// 解析 Metalink4 XML → 文件列表。出现顺序与文档一致（不重排）。
pub fn parse_metalink4(xml: &str) -> Result<Vec<MetalinkFile>, String> {
    let mut reader = Reader::from_str(xml);
    // quick-xml 0.37：trim 开关收拢进 Config（吃掉标签间空白文本节点，避免
    // 空白 <url> 文本污染）。
    reader.config_mut().trim_text(true);
    let mut files: Vec<MetalinkFile> = Vec::new();
    let mut cur: Option<MetalinkFile> = None;
    let mut target: Option<TextTarget> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().local_name().as_ref() {
                b"file" => {
                    if cur.is_some() {
                        return Err("metalink <file> 不允许嵌套".into());
                    }
                    let mut name: Option<String> = None;
                    for a in e.attributes() {
                        let a = a.map_err(|er| format!("metalink <file> 属性解析失败: {er}"))?;
                        if a.key.local_name().as_ref() == b"name" {
                            name = Some(decode_attr(&a, &reader)?);
                        }
                    }
                    let name = name.ok_or("metalink <file> 缺必需属性 name")?;
                    cur = Some(MetalinkFile {
                        name,
                        ..Default::default()
                    });
                }
                b"size" if cur.is_some() => target = Some(TextTarget::Size),
                b"hash" if cur.is_some() => {
                    let mut ty: Option<String> = None;
                    for a in e.attributes() {
                        let a = a.map_err(|er| format!("metalink <hash> 属性解析失败: {er}"))?;
                        if a.key.local_name().as_ref() == b"type" {
                            ty = Some(decode_attr(&a, &reader)?.to_lowercase());
                        }
                    }
                    if let Some(ty) = ty {
                        // 连字符归一：sha-256 → sha256 / sha-1 → sha1
                        let norm = ty.replace('-', "");
                        if matches!(norm.as_str(), "sha256" | "sha1" | "md5") {
                            target = Some(TextTarget::Hash(norm));
                        }
                    }
                }
                b"url" if cur.is_some() => {
                    let mut u = MetalinkUrl::default();
                    for a in e.attributes() {
                        let a = a.map_err(|er| format!("metalink <url> 属性解析失败: {er}"))?;
                        match a.key.local_name().as_ref() {
                            b"priority" => {
                                let p = decode_attr(&a, &reader)?;
                                u.priority = Some(p.trim().parse::<i32>().map_err(|_| {
                                    format!("metalink <url priority> 非法整数: {p:?}")
                                })?);
                            }
                            b"location" => u.location = Some(decode_attr(&a, &reader)?),
                            _ => {}
                        }
                    }
                    if let Some(f) = cur.as_mut() {
                        f.urls.push(u);
                    }
                    target = Some(TextTarget::Url);
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                // target=None → 非收集目标（文档级元数据 <publisher> 等）直接忽略
                let Some(tgt) = &target else { continue };
                let text = t
                    .unescape()
                    .map(|c| c.into_owned())
                    .map_err(|e| format!("metalink 文本转义非法: {e}"))?;
                let f = cur.as_mut().ok_or("metalink 叶子元素出现在 <file> 之外")?;
                match tgt {
                    TextTarget::Size => {
                        f.size = Some(text.trim().parse::<u64>().map_err(|_| {
                            format!("metalink <size> 非法非负整数: {:?}", text.trim())
                        })?);
                    }
                    TextTarget::Url => {
                        let u = f.urls.last_mut().expect("url 文本前必有 url 起始事件");
                        u.url = text.trim().to_string();
                    }
                    TextTarget::Hash(ty) => match ty.as_str() {
                        "sha256" => f.sha256 = Some(text.trim().to_lowercase()),
                        "sha1" => f.sha1 = Some(text.trim().to_lowercase()),
                        _ => f.md5 = Some(text.trim().to_lowercase()),
                    },
                }
            }
            Ok(Event::End(e)) => match e.name().local_name().as_ref() {
                b"file" => {
                    if let Some(f) = cur.take() {
                        files.push(f);
                    }
                    target = None;
                }
                b"size" | b"hash" | b"url" => target = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("metalink XML 解析失败: {e}")),
            _ => {}
        }
    }
    if files.is_empty() {
        return Err("metalink 无 <file> 条目".into());
    }
    Ok(files)
}

/// 属性值解码（UTF-8 + XML 实体 unescape）。
fn decode_attr(
    a: &quick_xml::events::attributes::Attribute,
    reader: &Reader<&[u8]>,
) -> Result<String, String> {
    a.decode_and_unescape_value(reader.decoder())
        .map(|c| c.into_owned())
        .map_err(|e| format!("metalink 属性值解码失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5854 §4 完整示例（裁剪至识别字段）。
    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <publisher><name>example.com</name></publisher>
  <file name="example2.iso">
    <size>4293893120</size>
    <hash type="sha-256">a5c3907e453a0d7a4f7d5c9b2f8e1d0a</hash>
    <hash type="md5">0123456789abcdef0123456789abcdef</hash>
    <url location="FR" priority="1">ftp://ftp.example.com/example2.iso</url>
    <url priority="2">http://example.com/example2.iso</url>
  </file>
</metalink>"#;

    #[test]
    fn parses_basic_fields() {
        let mut files = parse_metalink4(SAMPLE).unwrap();
        assert_eq!(files.len(), 1);
        let f = files.pop().unwrap();
        assert_eq!(f.name, "example2.iso");
        assert_eq!(f.size, Some(4293893120));
        // type="sha-256" 连字符归一后入 sha256 槽位；md5 原样。
        assert_eq!(f.md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(
            f.sha256.as_deref(),
            Some("a5c3907e453a0d7a4f7d5c9b2f8e1d0a")
        );
        assert_eq!(f.urls.len(), 2);
        assert_eq!(f.urls[0].location.as_deref(), Some("FR"));
        assert_eq!(f.urls[0].priority, Some(1));
    }

    #[test]
    fn sorted_urls_priority_asc_none_last_stable() {
        let xml = r#"<metalink><file name="a.bin">
            <url priority="2">http://two/</url>
            <url priority="1">http://one/</url>
            <url>http://none/</url>
            <url priority="1">http://one-b/</url>
        </file></metalink>"#;
        let f = &parse_metalink4(xml).unwrap()[0];
        let sorted: Vec<&str> = f.sorted_urls().iter().map(|u| u.url.as_str()).collect();
        assert_eq!(
            sorted,
            [
                "http://one/",
                "http://one-b/",
                "http://two/",
                "http://none/"
            ]
        );
    }

    #[test]
    fn best_hash_prefers_strongest() {
        let xml = r#"<metalink><file name="a.bin">
            <hash type="md5">aa</hash>
            <hash type="sha1">bb</hash>
            <hash type="sha256">cc</hash>
        </file></metalink>"#;
        let f = &parse_metalink4(xml).unwrap()[0];
        assert_eq!(f.best_hash(), Some(("sha256", "cc")));
    }

    #[test]
    fn namespace_and_prefix_agnostic() {
        let xml = r#"<?xml version="1.0"?>
<ml:metalink xmlns:ml="urn:ietf:params:xml:ns:metalink">
  <ml:file name="p.tgz"><url>http://h/p.tgz</url></ml:file>
</ml:metalink>"#;
        let files = parse_metalink4(xml).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "p.tgz");
        assert_eq!(files[0].urls[0].url, "http://h/p.tgz");
    }

    #[test]
    fn unescapes_entities_in_url_and_name() {
        let xml = r#"<metalink><file name="a&amp;b.bin">
            <url>http://h/x?a=1&amp;b=2</url>
        </file></metalink>"#;
        let f = &parse_metalink4(xml).unwrap()[0];
        assert_eq!(f.name, "a&b.bin");
        assert_eq!(f.urls[0].url, "http://h/x?a=1&b=2");
    }

    #[test]
    fn multiple_files_in_order() {
        let xml = r#"<metalink>
            <file name="one.bin"><url>http://h/1</url></file>
            <file name="two.bin"><url>http://h/2</url></file>
            <file name="three.bin"><url>http://h/3</url></file>
        </metalink>"#;
        let files = parse_metalink4(xml).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["one.bin", "two.bin", "three.bin"]);
    }

    #[test]
    fn http_sorted_urls_filters_non_http_protocols() {
        let xml = r#"<metalink><file name="a.bin">
            <url priority="1">ftp://ftp.example.com/a.bin</url>
            <url priority="2">https://mirror/a.bin</url>
            <url priority="3">http://origin/a.bin</url>
        </file></metalink>"#;
        let f = &parse_metalink4(xml).unwrap()[0];
        let sorted: Vec<&str> = f
            .http_sorted_urls()
            .iter()
            .map(|u| u.url.as_str())
            .collect();
        assert_eq!(sorted, ["https://mirror/a.bin", "http://origin/a.bin"]);
        // 全部非 http → 空（上层拒该 file）
        let xml = r#"<metalink><file name="a.bin">
            <url>ftp://only/a.bin</url>
        </file></metalink>"#;
        let f = &parse_metalink4(xml).unwrap()[0];
        assert!(f.http_sorted_urls().is_empty());
    }

    #[test]
    fn bad_inputs_rejected() {
        // 无 <file>
        assert!(parse_metalink4("<metalink/>").is_err());
        // 缺 name 属性
        assert!(
            parse_metalink4(r#"<metalink><file><url>http://h/</url></file></metalink>"#).is_err()
        );
        // 嵌套 file
        assert!(parse_metalink4(
            r#"<metalink><file name="a"><file name="b"></file></file></metalink>"#
        )
        .is_err());
        // priority 非整数
        assert!(parse_metalink4(
            r#"<metalink><file name="a"><url priority="x">http://h/</url></file></metalink>"#
        )
        .is_err());
        // size 非整数
        assert!(
            parse_metalink4(r#"<metalink><file name="a"><size>abc</size></file></metalink>"#)
                .is_err()
        );
        // 非法 XML
        assert!(parse_metalink4("<metalink><file>").is_err());
    }

    #[test]
    fn metadata_leaf_text_outside_file_ignored() {
        // <publisher>/<license> 等文档级元数据的叶子文本不参与收集（RFC 5854 §3.3）
        let xml = r#"<metalink>
            <publisher><name>出版社名</name><url>http://pub/</url></publisher>
            <file name="a.bin"><url>http://real/a.bin</url></file>
        </metalink>"#;
        let files = parse_metalink4(xml).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].urls.len(), 1);
        assert_eq!(files[0].urls[0].url, "http://real/a.bin");
    }

    #[test]
    fn display_name_takes_last_segment_and_sanitizes() {
        let mut f = MetalinkFile {
            name: "dir/sub/file.iso".into(),
            ..Default::default()
        };
        assert_eq!(f.display_name().unwrap(), "file.iso");
        f.name = "win\\path\\f.bin".into();
        assert_eq!(f.display_name().unwrap(), "f.bin");
        // 穿越段随「仅取末段」语义被整段丢弃（防路径逃逸特性）：`../escape.bin`
        // 的末段是普通文件名，落盘不再逃逸出 dest_root。
        f.name = "../escape.bin".into();
        assert_eq!(f.display_name().unwrap(), "escape.bin");
        f.name = "/abs/file.iso".into();
        assert_eq!(f.display_name().unwrap(), "file.iso");
        // 末段为 "." 或空 → 拒绝
        f.name = ".".into();
        assert!(f.display_name().is_err());
        f.name = "sub/".into();
        assert!(f.display_name().is_err());
    }
}
