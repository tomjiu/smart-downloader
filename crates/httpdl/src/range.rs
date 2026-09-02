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
    /// 文件总长（Content-Range 或 Content-Length）。
    pub total: Option<u64>,
    /// 服务端声明文件名（Content-Disposition；已剥目录成分/控制符，仍需
    /// sanitize_rel 终审）。无声明 → None（引擎侧回退 URL 末段）。
    pub filename: Option<String>,
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
    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_disposition_filename);

    match status {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            let total = content_range_total(resp.headers()).or(resp.content_length());
            Ok(Probe {
                range_supported: true,
                etag,
                total,
                filename,
            })
        }
        reqwest::StatusCode::OK => Ok(Probe {
            range_supported: false,
            etag,
            total: resp.content_length(),
            filename,
        }),
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => Ok(Probe {
            range_supported: false,
            etag,
            // 416 通常带 Content-Range: bytes */TOTAL → 仍可取总长
            total: content_range_total(resp.headers()),
            filename,
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
