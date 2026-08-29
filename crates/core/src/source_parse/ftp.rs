//! FTP URL 辅助解析：提取 user/pass（匿名回退 `anonymous`）。
//!
//! 格式：`ftp://[user:pass@]host[:port]/path`。
//! 无 user 时 → anonymous / 空密码（FTP 匿名惯例）。

/// 从 `ftp://...` URL 提取 `(user, pass)`；无 `user:pass@` → `("anonymous", "")`。
pub fn parse_ftp_auth(url: &str) -> (String, String) {
    let rest = match url.strip_prefix("ftp://") {
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
        Some((u, p)) => (u.to_string(), p.to_string()),
        None => (auth.to_string(), String::new()),
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
}
