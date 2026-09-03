//! Range 探测（§14）：GET Range: bytes=0-0 判定服务器是否支持 Range，并取 ETag/总长。
//! 206 → 支持（多连接/续传能力）；200 → 忽略 Range（单连接流式）；416 → 范围非法（重下）。
//! 同时捕获 Content-Disposition 文件名（E4：落盘名服务端声明优先于 URL 末段）。

use smart_dl_core::types::EngineError;
use std::time::Duration;

/// 探测结果（M4a：M4b 多连接/续传的输入）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// 服务器尊重 Range（探测响应 206）。
    pub range_supported: bool,
    pub etag: Option<String>,
    /// 内容指纹备援（E26）：Last-Modified 原始串。服务器无 ETag 时的续传
    /// 指纹；与 ETag 各自独立参与账本核对（见 ledger::decide）。
    pub last_modified: Option<String>,
    /// 文件总长（Content-Range 或 Content-Length）。
    pub total: Option<u64>,
    /// 服务端声明文件名（Content-Disposition；已剥目录成分/控制符，仍需
    /// sanitize_rel 终审）。无声明 → None（引擎侧回退 URL 末段）。
    pub filename: Option<String>,
    /// Content-Type 原始串（E31 probe 预览透出；引擎内部不消费，None = 无）。
    pub content_type: Option<String>,
}

/// 探测单个 URL。`headers` 为任务级自定义头（如 Referer/Cookie）。
pub async fn probe_range(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<Probe, EngineError> {
    let mut req = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .timeout(Duration::from_secs(30));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| EngineError::Other(e.to_string()))?;
    let status = resp.status();
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_disposition_filename);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match status {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            let total = content_range_total(resp.headers()).or(resp.content_length());
            Ok(Probe {
                range_supported: true,
                etag,
                last_modified,
                total,
                filename,
                content_type,
            })
        }
        reqwest::StatusCode::OK => Ok(Probe {
            range_supported: false,
            etag,
            last_modified,
            total: resp.content_length(),
            filename,
            content_type,
        }),
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => Ok(Probe {
            range_supported: false,
            etag,
            last_modified,
            // 416 通常带 Content-Range: bytes */TOTAL → 仍可取总长
            total: content_range_total(resp.headers()),
            filename,
            content_type,
        }),
        other => Err(EngineError::Other(format!("probe status {other}"))),
    }
}

/// 解析 `Content-Range: bytes 0-0/TOTAL`（或 `bytes */TOTAL`）的总长。
fn content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cr| cr.rsplit('/').next())
        .and_then(|t| t.parse().ok())
}

/// 强 ETag 判定（E24）：`W/` 前缀 = 弱 ETag（同一资源不同表示都可同值，
/// 不可作内容一致性证据）；空/缺失同样不合格。
pub(crate) fn is_strong_etag(e: &Option<String>) -> bool {
    e.as_deref()
        .is_some_and(|v| !v.is_empty() && !v.starts_with("W/"))
}

/// 多源并行身份门控（E24）：双源**强 ETag 相等**（服务器生成的内容指纹
/// 一致——跨源混拼段的安全性证据）且均支持 Range 且总长一致。任一不满足
/// → 保持单源 + 兜底语义（跨源内容可能不同，混拼 = 静默损坏）。
pub(crate) fn multi_source_ok(primary: &Probe, backup: &Probe) -> bool {
    is_strong_etag(&primary.etag)
        && is_strong_etag(&backup.etag)
        && primary.etag == backup.etag
        && primary.range_supported
        && backup.range_supported
        && primary.total.is_some()
        && backup.total.is_some()
        && primary.total == backup.total
}

/// CD 文件名清洗：剥目录成分（`/` `\` 都算——CD 可来自任意服务器）、剥引号
/// 与空白、去控制字符、限长。返回 None = 无可用文件名（引擎侧逐级回退）。
fn cd_clean_filename(raw: &str) -> Option<String> {
    let base = raw.trim().rsplit(['/', '\\']).next().unwrap_or(raw);
    let base = base.trim().trim_matches('"').trim();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    // 255 字节为常见文件系统分量上限；按字符截 200 留余量（UTF-8 名安全）
    let cleaned: String = cleaned.chars().take(200).collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// 最小 percent-decode（RFC 5987 filename\* 用；不引外部依赖）。
/// 非 UTF-8 序列 → None（调用方回退普通 filename 参数）。
fn percent_decode_utf8(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16)?;
            let lo = (b[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 解析 Content-Disposition 文件名（RFC 6266 + RFC 5987）。
/// 优先级：`filename*`（RFC 5987 编码）> `filename`；引号内 `;` 不拆分。
fn parse_content_disposition_filename(cd: &str) -> Option<String> {
    // 引号感知拆分：`filename="a;b.bin"; foo=bar` 不得在引号内断开
    let mut parts: Vec<&str> = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in cd.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => {
                parts.push(&cd[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&cd[start..]);

    let mut filename = None;
    let mut filename_star = None;
    for part in parts.into_iter().skip(1) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename" {
            let v = v.trim_matches('"');
            if !v.is_empty() {
                filename = Some(v.to_string());
            }
        } else if k == "filename*" {
            // RFC 5987: charset'lang'percent-encoded —— charset 恒取后段
            //（UTF-8 之外按字节透传，from_utf8 失败 → 回退普通 filename）
            if let Some((_, rest)) = v.split_once('\'') {
                if let Some((_, encoded)) = rest.rsplit_once('\'') {
                    if let Some(dec) = percent_decode_utf8(encoded) {
                        if !dec.is_empty() {
                            filename_star = Some(dec);
                        }
                    }
                }
            }
        }
    }
    filename_star
        .or(filename)
        .and_then(|f| cd_clean_filename(&f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_etag_judgement() {
        // E24：强/弱/缺失 ETag 判定
        assert!(is_strong_etag(&Some("\"abc123\"".into())));
        assert!(is_strong_etag(&Some("33a64df551425fcc".into())));
        assert!(
            !is_strong_etag(&Some("W/\"abc123\"".into())),
            "弱 ETag 不合格"
        );
        assert!(!is_strong_etag(&Some(String::new())), "空 ETag 不合格");
        assert!(!is_strong_etag(&None), "缺失 ETag 不合格");
    }

    #[test]
    fn multi_source_gate_truth_table() {
        let base = Probe {
            range_supported: true,
            etag: Some("\"v1\"".into()),
            total: Some(1024),
            last_modified: None,
            filename: None,
            content_type: None,
        };
        // 同强 ETag + 同长 + 双 Range → 通过
        assert!(multi_source_ok(&base, &base.clone()));
        // ETag 不一致 → 拒绝
        let other = Probe {
            etag: Some("\"v2\"".into()),
            ..base.clone()
        };
        assert!(!multi_source_ok(&base, &other));
        // 弱 ETag → 拒绝
        let weak = Probe {
            etag: Some("W/\"v1\"".into()),
            ..base.clone()
        };
        assert!(!multi_source_ok(&base, &weak));
        assert!(!multi_source_ok(&weak, &base));
        // 缺 ETag → 拒绝
        let none = Probe {
            etag: None,
            ..base.clone()
        };
        assert!(!multi_source_ok(&base, &none));
        // 总长不一致 → 拒绝
        let short = Probe {
            total: Some(512),
            ..base.clone()
        };
        assert!(!multi_source_ok(&base, &short));
        // 备用源不支持 Range → 拒绝（段请求会 200 全量，混拼写错位）
        let norange = Probe {
            range_supported: false,
            ..base.clone()
        };
        assert!(!multi_source_ok(&base, &norange));
    }

    #[test]
    fn cd_plain_quoted_and_bare() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"setup.exe\""),
            Some("setup.exe".into())
        );
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=setup.exe"),
            Some("setup.exe".into())
        );
    }

    #[test]
    fn cd_quoted_semicolon_not_split() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"a;b.bin\"; size=3"),
            Some("a;b.bin".into())
        );
    }

    #[test]
    fn cd_filename_star_utf8_wins() {
        // RFC 5987：UTF-8''%E4%B8%AD%E6%96%87.bin → 中文.bin；且优先于普通 filename
        assert_eq!(
            parse_content_disposition_filename(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''%E4%B8%AD%E6%96%87.bin"
            ),
            Some("中文.bin".into())
        );
    }

    #[test]
    fn cd_path_components_stripped() {
        // 目录成分（含 Windows 反斜杠）剥离，不产生穿越载体
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"../../etc/passwd\""),
            Some("passwd".into())
        );
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"..\\..\\evil.bin\""),
            Some("evil.bin".into())
        );
    }

    #[test]
    fn cd_empty_and_garbage_fall_to_none() {
        assert_eq!(parse_content_disposition_filename("attachment"), None);
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"\""),
            None
        );
        // 纯目录成分 → 剥完为空 → None
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"dir/\""),
            None
        );
    }
}
