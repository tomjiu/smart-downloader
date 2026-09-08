//! `fs2you://`（RaySource 协议族）解码器 —— 2026-08-30 附录 A #7 / 缺口 #1 落地。
//!
//! 协议形状（公开资料 + 历史客户端行为，属 L0 纯解码）：
//!   `fs2you://<base64>` 解码后 = `cachefile://<host>/<path>|<size>|<md5>`
//!   （部分老链接缺 `cachefile://` 前缀，host 直接以 `cachefileNN.` 开头，两者都接受）
//!   - `host/path` → 直链 `http://<host>/<path>`（RaySource 分发服务器为纯 HTTP）
//!   - `size`     → 文件字节数（用于预分配/完整性预算）
//!   - `md5`      → 32 位十六进制文件 MD5（下载后可校验）
//!
//! 解码失败一律返回错误（调用方归一化为 Unsupported，不 panic），与
//! thunder:// / qqdl:// 家族的容错口径一致（`decode_base64_lenient`）。
//!
//! 注：服务器群（zhowta/paofile 等）长期可用性未验证 —— 本模块只负责
//! L0 解码正确性；直链可达性属运行时问题。

use crate::source_parse::thunder::decode_base64_lenient;

/// 解码产物。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fs2YouLink {
    /// 直链（http://host/path）。
    pub url: String,
    /// 文件字节数。
    pub size: u64,
    /// 32 位小写十六进制 MD5。
    pub md5: String,
}

/// 解析 `fs2you://` 链接。
///
/// 接受大小写混合 scheme、可选 `cachefile://` 前缀、宽松 base64（自动补齐）。
pub fn parse_fs2you(link: &str) -> Result<Fs2YouLink, String> {
    let rest = link
        .get("fs2you://".len()..)
        .ok_or_else(|| "缺少载荷".to_string())?
        .trim();

    let decoded = decode_base64_lenient(rest).map_err(|e| format!("base64 解码失败: {e}"))?;
    let text = String::from_utf8_lossy(&decoded).into_owned();

    // 容忍 cachefile:// 前缀缺失（老客户端两种形态都存在）
    let body = text
        .strip_prefix("cachefile://")
        .unwrap_or(text.strip_prefix("cachefile:").unwrap_or(&text));

    let parts: Vec<&str> = body.split('|').collect();
    if parts.len() != 3 {
        return Err(format!(
            "应为 host/path|size|md5 三段，实际 {} 段",
            parts.len()
        ));
    }
    let (hostpath, size_s, md5_s) = (parts[0], parts[1], parts[2]);

    // host 校验：必须含 '/' 且 host 段含 '.'（拦住乱码/错位解码）
    let slash = hostpath
        .find('/')
        .ok_or_else(|| format!("缺少路径分隔符: {hostpath:?}"))?;
    let host = &hostpath[..slash];
    if host.is_empty() || !host.contains('.') {
        return Err(format!("host 不合法: {host:?}"));
    }

    let size: u64 = size_s
        .parse()
        .map_err(|_| format!("size 不是数字: {size_s:?}"))?;

    let md5 = md5_s.trim().to_ascii_lowercase();
    if md5.len() != 32 || !md5.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("md5 不是 32 位十六进制: {md5_s:?}"));
    }

    Ok(Fs2YouLink {
        url: format!("http://{hostpath}"),
        size,
        md5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    fn enc(s: &str) -> String {
        B64.encode(s.as_bytes())
    }

    const MD5: &str = "d41d8cd98f00b204e9800998ecf8427e";

    #[test]
    fn decodes_with_cachefile_prefix() {
        let link = format!(
            "fs2you://{}",
            enc(&format!(
                "cachefile://cache13.zhowta.com/file/a.rar|{size}|{MD5}",
                size = 1048576
            ))
        );
        let f = parse_fs2you(&link).unwrap();
        assert_eq!(f.url, "http://cache13.zhowta.com/file/a.rar");
        assert_eq!(f.size, 1048576);
        assert_eq!(f.md5, MD5);
    }

    #[test]
    fn decodes_without_prefix() {
        let link = format!(
            "fs2you://{}",
            enc(&format!("cachefile9.paofile.com/dir/b.bin|7|{MD5}"))
        );
        let f = parse_fs2you(&link).unwrap();
        assert_eq!(f.url, "http://cachefile9.paofile.com/dir/b.bin");
        assert_eq!(f.size, 7);
    }

    #[test]
    fn scheme_case_insensitive() {
        let link = format!("FS2YOU://{}", enc(&format!("cache.x.com/f|1|{MD5}")));
        assert!(parse_fs2you(&link).is_ok());
    }

    #[test]
    fn md5_normalized_to_lowercase() {
        let link = format!(
            "fs2you://{}",
            enc(&format!("cache.x.com/f|1|{}", MD5.to_uppercase()))
        );
        assert_eq!(parse_fs2you(&link).unwrap().md5, MD5);
    }

    #[test]
    fn bad_base64_is_err() {
        assert!(parse_fs2you("fs2you://!!!not-b64!!!").is_err());
    }

    #[test]
    fn wrong_segment_count_is_err() {
        let link = format!("fs2you://{}", enc("cachefile://cache.x.com/only-two|1"));
        let err = parse_fs2you(&link).unwrap_err();
        assert!(err.contains("三段"), "err={err}");
    }

    #[test]
    fn bad_host_is_err() {
        // 解码成功但 host 段没有 '.'（乱码/错位解码）
        let link = format!("fs2you://{}", enc(&format!("notafile|1|{MD5}")));
        assert!(parse_fs2you(&link).is_err());
    }

    #[test]
    fn bad_md5_is_err() {
        let link = format!("fs2you://{}", enc("cache.x.com/f|1|zzzz"));
        let err = parse_fs2you(&link).unwrap_err();
        assert!(err.contains("32 位"), "err={err}");
    }

    #[test]
    fn bad_size_is_err() {
        let link = format!("fs2you://{}", enc(&format!("cache.x.com/f|NaN|{MD5}")));
        assert!(parse_fs2you(&link).is_err());
    }

    #[test]
    fn missing_payload_is_err() {
        assert!(parse_fs2you("fs2you://").is_err());
    }
}
