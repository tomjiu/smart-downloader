//! FTP URL 辅助解析：提取 user/pass（匿名回退 `anonymous`）。
//!
//! 格式：`ftp(s)://[user:pass@]host[:port]/path`（B2：ftps:// = 显式
//! AUTH TLS，默认端口仍 21，与 FileZilla/wget 惯例一致）。
//! 无 user 时 → anonymous / 空密码（FTP 匿名惯例）。

/// `ftp://` / `ftps://` 前缀剥离（其余部分同构）。
fn strip_scheme(url: &str) -> Option<&str> {
    url.strip_prefix("ftp://")
        .or_else(|| url.strip_prefix("ftps://"))
}

/// 命令注入防线（CWE-147）：FTP 控制通道命令以 CRLF 结尾，user/pass 内嵌
/// CR/LF/NUL 可在受害会话内注入任意 FTP 命令（对齐 curl 的拒绝语义）。
/// 检测到控制字符 → 返回 ("", "") 哨兵：后续 `USER `（空用户名）必然被
/// 服务器 5xx 拒绝，注入请求变成认证失败。路径侧由 httpdl parse_ftp_url 拒绝。
fn has_ctl_injection(s: &str) -> bool {
    s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// 从 `ftp(s)://...` URL 提取 `(user, pass)`；无 `user:pass@` → `("anonymous", "")`。
pub fn parse_ftp_auth(url: &str) -> (String, String) {
    let rest = match strip_scheme(url) {
        Some(r) => r,
        None => return ("anonymous".to_string(), String::new()),
    };
    // 截取 `@` 之前的 auth 段（可能不存在）
    let auth_host = match rest.split_once('/') {
        Some((ah, _)) => ah,
        None => rest,
    };
    let auth = match auth_host.rsplit_once('@') {
        Some((a, _)) => a,
        None => return ("anonymous".to_string(), String::new()),
    };
    match auth.split_once(':') {
        Some((u, p)) => {
            if has_ctl_injection(u) || has_ctl_injection(p) {
                (String::new(), String::new())
            } else {
                (u.to_string(), p.to_string())
            }
        }
        None => {
            if has_ctl_injection(auth) {
                (String::new(), String::new())
            } else {
                (auth.to_string(), String::new())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_user_pass() {
        assert_eq!(
            parse_ftp_auth("ftp://alice:secret@host/file"),
            ("alice".to_string(), "secret".to_string())
        );
    }

    #[test]
    fn user_only() {
        assert_eq!(
            parse_ftp_auth("ftp://alice@host/file"),
            ("alice".to_string(), "".to_string())
        );
    }

    #[test]
    fn anonymous() {
        assert_eq!(
            parse_ftp_auth("ftp://host/file"),
            ("anonymous".to_string(), "".to_string())
        );
    }

    #[test]
    fn pass_with_special_chars() {
        assert_eq!(
            parse_ftp_auth("ftp://u:p@ss:word@host/file"),
            ("u".to_string(), "p@ss:word".to_string())
        );
    }

    #[test]
    fn ftps_scheme_same_auth() {
        assert_eq!(
            parse_ftp_auth("ftps://alice:secret@host:2121/file"),
            ("alice".to_string(), "secret".to_string())
        );
        assert_eq!(
            parse_ftp_auth("ftps://host/file"),
            ("anonymous".to_string(), "".to_string())
        );
    }

    #[test]
    fn crlf_injection_rejected() {
        // 审查修复 P0：user/pass 内嵌 CR/LF → 哨兵空凭据（认证失败而非命令注入）。
        // 注意：path 中的 CR/LF 在 auth 段之外（split_once('/') 先截断），由
        // httpdl parse_ftp_url 拒绝（见 parse_ftp_url_rejects_injection）。
        assert_eq!(
            parse_ftp_auth("ftp://u\r\nEXPLOIT:pass@host/file"),
            (String::new(), String::new())
        );
        assert_eq!(
            parse_ftp_auth("ftp://u:p\nass@host/file"),
            (String::new(), String::new())
        );
        assert_eq!(
            parse_ftp_auth("ftp://u:p\x00@host/file"),
            (String::new(), String::new())
        );
    }
}
