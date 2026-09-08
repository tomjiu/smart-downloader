//! BaiduClient：百度网盘分享免登录 API 封装（2026-09-05 真实链接实测协议）。
//!
//! 实测协议（A 级证据，`docs/research/baidu/share_protocol.md`）：
//! 1. `POST /share/verify?surl=<code>&t=<ms>&channel=chunlei&web=1&app_id=250528&clienttype=0`
//!    body `pwd=<pwd>`，Referer=`/share/init?surl=<code>` →
//!    `{"errno":0,"randsk":"<url-encoded>"}`；randsk 即 BDCLND cookie 值。
//!    **GET 形态同参数实测被风控 -12，必须 POST**。
//! 2. `GET /s/1<code>`（带 BDCLND）→ 分享页 HTML 内嵌 `shareid:"<num>"` /
//!    `uk:"<num>"`（JS 赋值形状；兼容 JSON 形状 `shareid":"<num>"`）。
//!    无 BDCLND 时返回密码页（无 shareid）。
//! 3. `GET /share/list?shareid=&uk=&root=1`（根目录）或 `&dir=<path>`（子目录）
//!    → `{"errno":0,"list":[...]}`；字段值全为字符串数字（实测）。
//!
//! dlink 直链需要登录态（免登录 `/api/download` 实测 errno -6、
//! `/share/download` 实测 errno 2；sign3/timestamp 仅登录后下发），
//! 属 B3-b（登录态 + 转存/直链链真机校准），本模块不含。
//!
//! mock 测试：axum 本地服务按上述协议形状回放（与 quark share 测试同构）。

use super::types::{BaiduError, APP_ID, BASE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// 分享文件条目（`/share/list` 的 `list[]`；数值字段实测全为字符串）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaiduShareFile {
    /// 文件 fsid（dlink 转换用，B3-b）。
    #[serde(default, deserialize_with = "de_string")]
    pub fs_id: String,
    /// 云盘内全路径（`/SDP新客户端/xxx.7z`）。
    #[serde(default, deserialize_with = "de_string")]
    pub path: String,
    /// 文件名。
    #[serde(default, rename = "server_filename", deserialize_with = "de_string")]
    pub name: String,
    /// 字节数（字符串数字）。
    #[serde(default, deserialize_with = "de_string")]
    pub size: String,
    /// 是否目录（"1" = 目录）。
    #[serde(default, deserialize_with = "de_string")]
    pub isdir: String,
    /// 内容 md5（目录为空）。
    #[serde(default, deserialize_with = "de_string")]
    pub md5: String,
    /// 百度类目（4 = 文档 / 6 = 压缩包等，实测值）。
    #[serde(default, deserialize_with = "de_string")]
    pub category: String,
}

impl BaiduShareFile {
    /// 字节数（解析失败为 0）。
    pub fn size_bytes(&self) -> u64 {
        self.size.parse().unwrap_or(0)
    }

    /// 是否目录。
    pub fn is_dir(&self) -> bool {
        self.isdir == "1"
    }
}

/// 分享元信息（分享页 HTML 提取）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaiduShareMeta {
    /// 分享 id（list 接口 shareid 参数）。
    pub share_id: String,
    /// 分享者 uk（list 接口 uk 参数）。
    pub uk: String,
}

/// 百度分享免登录客户端（cookie store 内部持有 BDCLND）。
#[derive(Clone)]
pub struct BaiduClient {
    http: reqwest::Client,
    jar: Arc<reqwest::cookie::Jar>,
    base: String,
}

impl Default for BaiduClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BaiduClient {
    pub fn new() -> Self {
        Self::with_base(BASE.to_string())
    }

    /// mock 测试注入自定义基址。
    pub fn with_base(base: String) -> Self {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .cookie_provider(jar.clone())
            .timeout(Duration::from_secs(30))
            .build()
            .expect("baidu http client build");
        Self { http, jar, base }
    }

    /// 提交提取码（POST，实测协议）→ randsk 并种 BDCLND cookie。
    ///
    /// 公开分享（无提取码）跳过 verify 直接返回 Ok(())——分享页可直接取 meta。
    pub async fn verify_passcode(
        &self,
        link: &super::share::BaiduShareLink,
    ) -> Result<(), BaiduError> {
        if link.passcode.is_empty() {
            return Ok(());
        }
        let t = ms_now();
        let url = format!(
            "{}/share/verify?surl={}&t={}&channel=chunlei&web=1&app_id={}&clienttype=0",
            self.base, link.code, t, APP_ID
        );
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("pwd", &link.passcode)
            .finish();
        let resp = self
            .http
            .post(&url)
            .header(
                reqwest::header::REFERER,
                format!("{}/share/init?surl={}", self.base, link.code),
            )
            .header(reqwest::header::ORIGIN, self.base.as_str())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?;
        let v: serde_json::Value = resp.json().await?;
        let errno = v["errno"].as_i64().unwrap_or(-1);
        if errno != 0 {
            return Err(match errno {
                -12 => BaiduError::WrongPasscode,
                other => BaiduError::Protocol(other),
            });
        }
        let randsk = v["randsk"]
            .as_str()
            .ok_or(BaiduError::Protocol(0))?
            .to_string();
        // BDCLND host-only 种入（无 Domain 属性 → 绑定当前 host；mock 基址下同样生效，
        // 且不得带 Secure——测试 mock 为 http）
        let cookie = format!("BDCLND={randsk}; Path=/");
        let jar_url = url::Url::parse(&self.base).map_err(|e| BaiduError::Http(e.to_string()))?;
        self.jar.add_cookie_str(&cookie, &jar_url);
        Ok(())
    }

    /// 拉分享页 HTML 并提取 shareid / uk（需先 verify 种 BDCLND，带码分享）。
    pub async fn fetch_share_meta(
        &self,
        link: &super::share::BaiduShareLink,
    ) -> Result<BaiduShareMeta, BaiduError> {
        let url = format!("{}/s/{}", self.base, link.page_code());
        let html = self
            .http
            .get(&url)
            .header(
                reqwest::header::REFERER,
                format!("{}/share/init?surl={}", self.base, link.code),
            )
            .send()
            .await?
            .text()
            .await?;
        extract_meta(&html).ok_or(BaiduError::MetaParse)
    }

    /// 列目录（None = 根目录；实测协议 root=1 / dir=<path> 双形态）。
    pub async fn list_dir(
        &self,
        meta: &BaiduShareMeta,
        link: &super::share::BaiduShareLink,
        dir: Option<&str>,
    ) -> Result<Vec<BaiduShareFile>, BaiduError> {
        let mut url = url::Url::parse(&format!("{}/share/list", self.base))
            .map_err(|e| BaiduError::Http(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("shareid", &meta.share_id)
                .append_pair("uk", &meta.uk)
                .append_pair("clienttype", "0")
                .append_pair("web", "1")
                .append_pair("app_id", APP_ID);
            match dir {
                Some(d) => q.append_pair("dir", d),
                None => q.append_pair("root", "1"),
            };
        }
        let resp = self
            .http
            .get(url)
            .header(
                reqwest::header::REFERER,
                format!("{}/s/{}", self.base, link.page_code()),
            )
            .send()
            .await?;
        let v: serde_json::Value = resp.json().await?;
        let errno = v["errno"].as_i64().unwrap_or(-1);
        if errno != 0 {
            return Err(match errno {
                9019 => BaiduError::NeedVerify(9019),
                other => BaiduError::Protocol(other),
            });
        }
        let list = v
            .get("list")
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default();
        let files: Vec<BaiduShareFile> = list
            .into_iter()
            .filter_map(|f| serde_json::from_value(f).ok())
            .collect();
        Ok(files)
    }

    /// 免登录完整链：verify（带码）→ 分享页 meta → 根目录清单。
    pub async fn resolve_share(
        &self,
        link: &super::share::BaiduShareLink,
    ) -> Result<(BaiduShareMeta, Vec<BaiduShareFile>), BaiduError> {
        self.verify_passcode(link).await?;
        let meta = self.fetch_share_meta(link).await?;
        let files = self.list_dir(&meta, link, None).await?;
        Ok((meta, files))
    }
}

/// 从分享页 HTML 提取 shareid / uk。
///
/// 实测两种形状并存：JS 赋值 `shareid:"16364495271"` 与 JSON 字符串
/// `shareid":"16364495271"`；uk 存在噪声 `"uk":0`（数字值形状）——
/// 用 `uk:"`（带引号值）匹配天然规避。
fn extract_meta(html: &str) -> Option<BaiduShareMeta> {
    let share_id = extract_after_label(html, &["shareid:\"", "shareid\":\""])?;
    let uk = extract_after_label(html, &["uk:\"", "uk\":\""])?;
    Some(BaiduShareMeta { share_id, uk })
}

/// 依序尝试标签，命中后读连续 ASCII 数字（空结果继续尝试下一标签）。
fn extract_after_label(html: &str, labels: &[&str]) -> Option<String> {
    for label in labels {
        if let Some(pos) = html.find(label) {
            let rest = &html[pos + label.len()..];
            let bytes = rest.as_bytes();
            let mut end = 0;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > 0 {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn ms_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 数值字段兼容反序列化：字符串/数字统一为 String（实测 share/list 全为
/// 字符串，但接口形态跨端可能波动）。
fn de_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Ok(String::new()),
    }
}

// ---------------------------------------------------------------------------
// 测试：HTML 提取单测 + axum 本地 mock 全流程（形状 = 实测协议）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baidu::share::{parse_share_link, BaiduShareLink};
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::Json;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    const PASSCODE: &str = "nsdp";
    const RANDSK: &str = "YSIAmmkz1F%2FgW0OI2xmqqzSZQS%2BOdvYE9MPwrKCZg%2FE%3D";
    const SHARE_ID: &str = "16364495271";
    const UK: &str = "1227964813";

    fn link() -> BaiduShareLink {
        BaiduShareLink {
            code: "3fTBd5tvk-6a7TdxsTaS_w".into(),
            passcode: PASSCODE.into(),
        }
    }

    // ---- extract_meta 单测（实测 HTML 形状）----

    #[test]
    fn extract_meta_js_shape() {
        let html = r#"...(function(){var d={shareid:"16364495271",uk:"1227964813",sign1:"x"};...)"#;
        let m = extract_meta(html).unwrap();
        assert_eq!(m.share_id, "16364495271");
        assert_eq!(m.uk, "1227964813");
    }

    #[test]
    fn extract_meta_json_shape_and_uk_noise() {
        // JSON 字符串形状（shareid":" 与 uk":"）均可提取
        let html = r#"{"shareid":"16364495271","uk":"1227964813"}"#;
        let m = extract_meta(html).unwrap();
        assert_eq!(m.share_id, "16364495271");
        assert_eq!(m.uk, "1227964813");
        // uk 数字噪声（"uk":0 非引号值形状）不得命中
        assert!(extract_meta(r#"{"shareid":"16364495271","uk":0}"#).is_none());
    }

    #[test]
    fn extract_meta_missing() {
        assert!(extract_meta("<html>password page</html>").is_none());
    }

    // ---- axum mock（形状与实测一致）----

    #[derive(Clone, Default)]
    struct MockState {
        calls: Arc<AtomicU32>,
    }

    fn has_bdclnd(headers: &HeaderMap) -> bool {
        headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .map(|c| c.contains("BDCLND="))
            .unwrap_or(false)
    }

    async fn verify(
        State(st): State<MockState>,
        Query(q): Query<HashMap<String, String>>,
        body: String,
    ) -> Json<Value> {
        st.calls.fetch_add(1, Ordering::Relaxed);
        // POST 形状校验：surl 在 query，pwd 在 body
        let surl = q.get("surl").cloned().unwrap_or_default();
        let pwd = body
            .split('&')
            .find_map(|p| p.strip_prefix("pwd="))
            .unwrap_or("")
            .to_string();
        if surl == link().code && pwd == PASSCODE {
            Json(json!({"errno": 0i64, "err_msg": "", "randsk": RANDSK}))
        } else {
            Json(json!({"errno": -12i64, "err_msg": ""}))
        }
    }

    async fn share_page(headers: HeaderMap) -> Result<axum::response::Html<String>, StatusCode> {
        // 无 BDCLND → 密码页（无 shareid）
        if !has_bdclnd(&headers) {
            return Ok(axum::response::Html(
                "<html>bdverify password page</html>".into(),
            ));
        }
        Ok(axum::response::Html(format!(
            r#"<html><script>var d={{shareid:"{SHARE_ID}",uk:"{UK}"}};</script></html>"#
        )))
    }

    async fn share_list(
        State(st): State<MockState>,
        Query(q): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, StatusCode> {
        st.calls.fetch_add(1, Ordering::Relaxed);
        if !has_bdclnd(&headers) {
            return Ok(Json(json!({"errno": 9019i64, "errmsg": "need verify"})));
        }
        if q.get("shareid").map(|s| s.as_str()) != Some(SHARE_ID)
            || q.get("uk").map(|s| s.as_str()) != Some(UK)
        {
            return Ok(Json(json!({"errno": -66i64, "errmsg": "bad shareid/uk"})));
        }
        if q.contains_key("root") {
            Ok(Json(json!({
                "errno": 0i64,
                "list": [
                    {"fs_id": "639972768103564", "isdir": "1", "path": "/SDP",
                     "server_filename": "SDP", "size": "0", "md5": "", "category": "6"}
                ]
            })))
        } else if let Some(dir) = q.get("dir") {
            Ok(Json(json!({
                "errno": 0i64,
                "list": [
                    {"fs_id": "892549727248113", "isdir": "0",
                     "path": format!("{dir}/a.pdf"), "server_filename": "a.pdf",
                     "size": "1082476", "md5": "4f013bf9c", "category": "4"},
                    {"fs_id": 552502013661897i64, "isdir": 0,
                     "path": format!("{dir}/b.zip"), "server_filename": "b.zip",
                     "size": 82560954u64, "md5": "6b5793e37", "category": "6"}
                ]
            })))
        } else {
            Ok(Json(json!({"errno": -65i64})))
        }
    }

    fn mock_router(st: MockState) -> axum::Router {
        axum::Router::new()
            .route("/share/verify", post(verify))
            .route("/s/:code", get(share_page))
            .route("/share/list", get(share_list))
            .with_state(st)
    }

    async fn spawn_mock(st: MockState) -> (String, tokio::task::JoinHandle<()>) {
        let app = mock_router(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn resolve_share_happy_path() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let c = BaiduClient::with_base(base);
        let (meta, files) = c.resolve_share(&link()).await.unwrap();
        assert_eq!(meta.share_id, SHARE_ID);
        assert_eq!(meta.uk, UK);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_dir());
        assert_eq!(files[0].name, "SDP");
        server.abort();
    }

    #[tokio::test]
    async fn wrong_passcode_classified() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let c = BaiduClient::with_base(base);
        let bad = BaiduShareLink {
            code: "3fTBd5tvk-6a7TdxsTaS_w".into(),
            passcode: "0000".into(),
        };
        let err = c.resolve_share(&bad).await.unwrap_err();
        assert_eq!(err, BaiduError::WrongPasscode);
        server.abort();
    }

    #[tokio::test]
    async fn list_without_verify_needs_verify() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let c = BaiduClient::with_base(base);
        let meta = BaiduShareMeta {
            share_id: SHARE_ID.into(),
            uk: UK.into(),
        };
        // 未 verify（无 BDCLND）→ 9019
        let err = c.list_dir(&meta, &link(), None).await.unwrap_err();
        assert_eq!(err, BaiduError::NeedVerify(9019));
        server.abort();
    }

    #[tokio::test]
    async fn list_subdir_and_numeric_fields() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let c = BaiduClient::with_base(base);
        c.verify_passcode(&link()).await.unwrap();
        let meta = c.fetch_share_meta(&link()).await.unwrap();
        let files = c.list_dir(&meta, &link(), Some("/SDP")).await.unwrap();
        assert_eq!(files.len(), 2);
        // 字符串字段
        assert_eq!(files[0].fs_id, "892549727248113");
        assert_eq!(files[0].size_bytes(), 1082476);
        // 数字字段兼容（de_string）
        assert_eq!(files[1].fs_id, "552502013661897");
        assert_eq!(files[1].size_bytes(), 82560954);
        assert!(!files[1].is_dir());
        server.abort();
    }

    #[test]
    fn parse_real_share_url() {
        let l =
            parse_share_link("https://pan.baidu.com/s/13fTBd5tvk-6a7TdxsTaS_w?pwd=nsdp").unwrap();
        assert_eq!(l.code, "3fTBd5tvk-6a7TdxsTaS_w");
        assert_eq!(l.passcode, "nsdp");
    }
}
