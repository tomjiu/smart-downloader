//! 云盘搜索客户端（api-gateway-pan 网关）——【B级待验】。
//!
//! 端点来源：桌面迅雷 Chromium disk cache 取证
//! (`%APPDATA%\thunder\Cache\Cache_Data\data_1`)，对
//! `https://api-gateway-pan.xunlei.com` 的引用中还原出两个 `/xlppc.searcher.api` 端点：
//! - `xlppc.searcher.api/drive_common_search` —— 云盘通用搜索（关键字/磁力/链接，最常用）
//! - `xlppc.searcher.api/drive_file_search`   —— 云盘文件搜索
//!
//! 鉴权推断（【B级】）：disk cache 只落响应体与请求 URL，**不缓存任何请求头**
//! （全文件 grep `Authorization`/`Bearer `/`x-captcha-token` 计数均为 0），
//! 因此无法从 cache 直接取证鉴权头。鉴于同属迅雷云盘体系、且桌面 App 以带 `user_id`
//! 的 `drive_common_search` 调起，暂推断与 `api-pan` 同构的三要素头
//! （`Authorization: Bearer` / `x-captcha-token` / `x-device-id`）。
//! 实际请求形态（是否需三要素、是否走 OAuth）待真实抓包/实测验证，故标注【B级待验】。
//!
//! 本模块只实现【URL 组装纯函数 + 结构体 + 方法】骨架，不改 `client.rs`；
//! 网络方法未实测，仅在 offline 单测中验证纯函数 URL 形状。

use crate::xunlei::auth::AuthState;
use crate::xunlei::client::{url_encode, Client, ClientError};

/// 与 `client::url_encode` 同构，但把百分号编码的十六进制（如 `%3a`）转为大写（`%3A`），
/// 以忠实复现 desktop App 的请求形状（cache 取证为大写编码）。仅转 hex，不动原文可见字符。
fn url_encode_upper(s: &str) -> String {
    let enc = url_encode(s);
    let bytes = enc.as_bytes();
    let mut out = String::with_capacity(enc.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push('%');
            out.push((bytes[i + 1] as char).to_ascii_uppercase());
            out.push((bytes[i + 2] as char).to_ascii_uppercase());
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// gateway 第二网关 base（桌面迅雷重度使用，cache 命中次数仅次于 api-pan）。
pub const GATEWAY_BASE: &str = "https://api-gateway-pan.xunlei.com";
/// 云盘通用搜索路径（cache 中 4 次，全为带 keyword/磁力 的查询）。
pub const COMMON_SEARCH_PATH: &str = "xlppc.searcher.api/drive_common_search";
/// 云盘文件搜索路径（cache 中 2 次）。
pub const FILE_SEARCH_PATH: &str = "xlppc.searcher.api/drive_file_search";

/// 纯函数：组装 `drive_common_search` 请求 URL。
///
/// 参数形状来自 cache 取证：
/// `?user_id=<id>&keyword=<q>&limit=<n>&order_by_fields=<enc>`。
/// `keyword`/`order_by_fields` 经 `url_encode`（RFC3986 非 unreserved 百分号编码），
/// 与 cache 中 `keyword=magnet%3A%3Fxt%3D...`、`order_by_fields=created_time%3Adesc` 一致。
pub fn build_common_search_url(keyword: &str, user_id: &str, limit: u32, order_by_fields: &str) -> String {
    format!(
        "{}/{COMMON_SEARCH_PATH}?user_id={}&keyword={}&limit={}&order_by_fields={}",
        GATEWAY_BASE,
        // 取证：cache 中 magnet/order_by_fields 的百分号编码为大写（%3A/%3F/%3D），
        // 故用 url_encode_upper 仅转大写 hex 以忠实复现 desktop App 的请求形状。
        url_encode_upper(user_id),
        url_encode_upper(keyword),
        limit,
        url_encode_upper(order_by_fields),
    )
}

/// 纯函数：组装 `drive_file_search` 请求 URL。
///
/// 参数形状来自 cache 取证：
/// `?user_id=<id>&limit=<n>&keyword=<q>&space=*`。
/// 注：`space` 在 cache 原始请求中为**未编码**的 `*`（disk cache 直接落盘的值），
/// 故此处按原样透传，不做百分号编码（与 `url_encode` 行为不同，已取证）。
pub fn build_file_search_url(keyword: &str, user_id: &str, limit: u32, space: &str) -> String {
    format!(
        "{}/{FILE_SEARCH_PATH}?user_id={}&limit={}&keyword={}&space={}",
        GATEWAY_BASE,
        url_encode_upper(user_id),
        limit,
        url_encode_upper(keyword),
        space,
    )
}

/// 通用搜索响应（B级：字段形状未实测，仅占位以便后续接 JSON 解析）。
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct SearchResp {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub next_page_token: String,
}

/// 云盘搜索客户端（gateway 网关）。【B级待验】：网络方法未实测，鉴权推断见模块头。
#[derive(Clone)]
pub struct CloudSearch {
    http: reqwest::Client,
}

impl Default for CloudSearch {
    fn default() -> Self { Self::new() }
}

impl CloudSearch {
    pub fn new() -> Self {
        CloudSearch { http: reqwest::Client::new() }
    }

    /// 云盘通用搜索（【B级待验】未实测）。鉴权复用 `api-pan` 三要素头推断。
    pub async fn common_search(
        &self,
        state: &AuthState,
        keyword: &str,
        limit: u32,
        order_by_fields: &str,
    ) -> Result<SearchResp, ClientError> {
        let url = build_common_search_url(keyword, &state.user_id, limit, order_by_fields);
        let resp: SearchResp = self
            .http
            .get(&url)
            .headers(Client::auth_headers(state))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// 云盘文件搜索（【B级待验】未实测）。鉴权复用 `api-pan` 三要素头推断。
    pub async fn file_search(
        &self,
        state: &AuthState,
        keyword: &str,
        limit: u32,
        space: &str,
    ) -> Result<SearchResp, ClientError> {
        let url = build_file_search_url(keyword, &state.user_id, limit, space);
        let resp: SearchResp = self
            .http
            .get(&url)
            .headers(Client::auth_headers(state))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_search_url_has_base_and_path() {
        let u = build_common_search_url("test", "860599297", 20, "created_time:desc");
        assert!(u.starts_with(
            "https://api-gateway-pan.xunlei.com/xlppc.searcher.api/drive_common_search?"
        ));
    }

    #[test]
    fn common_search_url_query_params() {
        let u = build_common_search_url("test", "860599297", 20, "created_time:desc");
        assert!(u.contains("user_id=860599297"));
        assert!(u.contains("keyword=test"));
        assert!(u.contains("limit=20"));
        assert!(u.contains("order_by_fields=created_time%3Adesc"));
    }

    #[test]
    fn common_search_url_encodes_magnet_keyword() {
        // cache 实证：keyword=magnet%3A%3Fxt%3Durn%3Abtih%3A...
        let u = build_common_search_url("magnet:?xt=urn:btih:abcd", "1", 50, "created_time:desc");
        assert!(u.contains("keyword=magnet%3A%3Fxt%3Durn%3Abtih%3Aabcd"));
    }

    #[test]
    fn file_search_url_has_base_and_path() {
        let u = build_file_search_url("kw", "860599297", 10, "*");
        assert!(u.starts_with(
            "https://api-gateway-pan.xunlei.com/xlppc.searcher.api/drive_file_search?"
        ));
    }

    #[test]
    fn file_search_url_query_params_and_raw_star_space() {
        let u = build_file_search_url("kw", "860599297", 10, "*");
        assert!(u.contains("user_id=860599297"));
        assert!(u.contains("limit=10"));
        assert!(u.contains("keyword=kw"));
        // space 按 cache 原样透传（未编码）
        assert!(u.contains("space=*"));
        assert!(!u.contains("space=%2A"));
    }
}
