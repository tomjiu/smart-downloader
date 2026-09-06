//! SFTP URL 辅助解析（C-S1）：提取 user/pass。
//!
//! 格式：`sftp://[user[:pass]@]host[:port]/path`。与 FTP 的关键差异：
//! **SSH 无匿名惯例**——无 `user:pass@` 段时返回空串（而非 anonymous 回退），
//! 由路由层给出「sftp 需要 user:pass@」的明确错误，而非让认证必然失败。
//! 密码内可含 `@`/`:`（rsplit 取最后一个 `@` 前段，与 ftp.rs 同口径）。

/// 从 `sftp://...` URL 提取 `(user, pass)`；无 `user:pass@` → `("", "")`
/// （SSH 无匿名惯例，路由层对空 user 报明确错误）。
pub fn parse_sftp_auth(url: &str) -> (String, String) {
    let Some(rest) = url.strip_prefix("sftp://") else {
        return (String::new(), String::new());
    };
    // 截取 `@` 之前的 auth 段（可能不存在）
    let auth_host = match rest.split_once('/') {
        Some((ah, _)) => ah,
        None => rest,
    };
    let Some(auth) = auth_host.rsplit_once('@').map(|(a, _)| a) else {
        return (String::new(), String::new());
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
            parse_sftp_auth("sftp://alice:secret@host/file"),
            ("alice".to_string(), "secret".to_string())
        );
    }

    #[test]
    fn user_only() {
        assert_eq!(
            parse_sftp_auth("sftp://alice@host/file"),
            ("alice".to_string(), "".to_string())
        );
    }

    #[test]
    fn no_auth_is_empty_not_anonymous() {
        // SSH 无匿名惯例：与 ftp.rs 的 anonymous 回退刻意不同
        assert_eq!(
            parse_sftp_auth("sftp://host/file"),
            ("".to_string(), "".to_string())
        );
    }

    #[test]
    fn pass_with_special_chars() {
        // 密码含 @ / ：rsplit '@' 取最后一个
        assert_eq!(
            parse_sftp_auth("sftp://u:p@ss:word@host:2222/file"),
            ("u".to_string(), "p@ss:word".to_string())
        );
    }

    #[test]
    fn non_sftp_scheme_is_empty() {
        assert_eq!(
            parse_sftp_auth("ftp://a:b@host/f"),
            ("".to_string(), "".to_string())
        );
    }
}
