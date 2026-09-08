//! 百度网盘分享链接解析（B3-a）。
//!
//! 支持形状（2026-09-05 真实链接实测）：
//! - `https://pan.baidu.com/s/1<code>`（网页短码，固定带 `1` 前缀）
//! - `https://pan.baidu.com/s/1<code>?pwd=nsdp`
//! - `https://pan.baidu.com/share/init?surl=<code>`（密码输入页，surl **无** `1` 前缀）
//! - `https://pan.baidu.com/share/init?surl=<code>&pwd=nsdp`
//!
//! 统一规约：[`BaiduShareLink::code`] 存**去 `1` 前缀**短码——与 verify
//! 接口的 `surl=` 参数、`/share/init?surl=` 形状一致；网页路径短码 =
//! `1` + code（[`BaiduShareLink::page_code`]）。
//!
//! 提取码经 `url::Url` 解析，保留原始大小写（百度提取码大小写敏感）。

/// 解析后的百度分享链接。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaiduShareLink {
    /// 分享短码（无 `1` 前缀；verify `surl=` 参数形状）。
    pub code: String,
    /// 提取码（`?pwd=` / `&pwd=`；公开分享为空串）。
    pub passcode: String,
}

impl BaiduShareLink {
    /// 网页分享页路径短码（`1` + code）。
    pub fn page_code(&self) -> String {
        format!("1{}", self.code)
    }
}

/// 识别并解析百度网盘分享链接；非百度分享返回 None。
pub fn parse_share_link(url: &str) -> Option<BaiduShareLink> {
    let u = url::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if host != "pan.baidu.com" {
        return None;
    }
    let path = u.path();
    if let Some(rest) = path.strip_prefix("/s/") {
        // 网页短码形态：/s/1<code>；短码段不含 `/`，去 `1` 前缀
        if rest.is_empty() || rest.contains('/') {
            return None;
        }
        let code = rest.strip_prefix('1')?;
        if code.is_empty() {
            return None;
        }
        let passcode = query_param(&u, "pwd").unwrap_or_default();
        return Some(BaiduShareLink {
            code: code.to_string(),
            passcode,
        });
    }
    if path == "/share/init" {
        // 密码页形态：surl 已去 `1` 前缀
        let code = query_param(&u, "surl")?;
        if code.is_empty() || code.contains('/') {
            return None;
        }
        let passcode = query_param(&u, "pwd").unwrap_or_default();
        return Some(BaiduShareLink { code, passcode });
    }
    None
}

/// query 参数提取（url crate 自动 percent-decode，保留原值大小写）。
fn query_param(u: &url::Url, key: &str) -> Option<String> {
    u.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_s_form_with_pwd() {
        let l =
            parse_share_link("https://pan.baidu.com/s/13fTBd5tvk-6a7TdxsTaS_w?pwd=nsdp").unwrap();
        assert_eq!(l.code, "3fTBd5tvk-6a7TdxsTaS_w");
        assert_eq!(l.passcode, "nsdp");
        assert_eq!(l.page_code(), "13fTBd5tvk-6a7TdxsTaS_w");
    }

    #[test]
    fn parse_s_form_without_pwd() {
        let l = parse_share_link("https://pan.baidu.com/s/1abcdef").unwrap();
        assert_eq!(l.code, "abcdef");
        assert_eq!(l.passcode, "");
    }

    #[test]
    fn parse_s_form_case_insensitive_host_keeps_pwd_case() {
        // host 大小写不敏感；提取码大小写保留（百度提取码大小写敏感）
        let l = parse_share_link("HTTPS://PAN.BAIDU.COM/s/1AbCd?pwd=NsDp").unwrap();
        assert_eq!(l.code, "AbCd");
        assert_eq!(l.passcode, "NsDp");
    }

    #[test]
    fn parse_share_init_form() {
        let l = parse_share_link(
            "https://pan.baidu.com/share/init?surl=3fTBd5tvk-6a7TdxsTaS_w&pwd=nsdp",
        )
        .unwrap();
        assert_eq!(l.code, "3fTBd5tvk-6a7TdxsTaS_w");
        assert_eq!(l.passcode, "nsdp");
        // 与 /s/1 形态解析结果一致（同一分享）
        let l2 =
            parse_share_link("https://pan.baidu.com/s/13fTBd5tvk-6a7TdxsTaS_w?pwd=nsdp").unwrap();
        assert_eq!(l, l2);
    }

    #[test]
    fn parse_http_scheme() {
        let l = parse_share_link("http://pan.baidu.com/s/1abc?pwd=1234").unwrap();
        assert_eq!(l.code, "abc");
        assert_eq!(l.passcode, "1234");
    }

    #[test]
    fn reject_non_share() {
        // 非百度 / 非 /s/ 路径 / 空短码 / 无 1 前缀 / 非 URL
        assert!(parse_share_link("https://pan.quark.cn/s/abc").is_none());
        assert!(parse_share_link("https://pan.baidu.com/home").is_none());
        assert!(parse_share_link("https://pan.baidu.com/s/").is_none());
        assert!(parse_share_link("https://pan.baidu.com/s/abc").is_none());
        assert!(parse_share_link("not a url").is_none());
        assert!(parse_share_link("magnet:?xt=urn:btih:AA").is_none());
    }
}
