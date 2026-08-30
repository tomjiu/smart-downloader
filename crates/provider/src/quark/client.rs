//! QuarkClient：夸克 drive 网关的 HTTP 端点封装。
//!
//! 端点形状（**待真机验证**）：`pr=ucpro&fr=pc` 公共参数 + 统一响应壳
//! `{"status":200,"code":0,"message":"","data":...}`，对齐夸克 PC Web API
//! 的公开互操作形状（alist / quark-auto-save 等）。本任务的分析文档
//! （05_quark installer 逆向）不含分享 API 抓包，故按通用网盘 REST 形状
//! 实现，mock 测试与本实现形状一致；真机对接只需改端点路径/字段名。
//!
//! 流程（分享链接 → 直链）：
//! 1. `POST /share/sharepage/token`   → stoken（分享凭据）
//! 2. `GET  /share/sharepage/detail`  → 文件列表（fid/文件名/大小/是否目录）
//! 3. `POST /share/sharepage/save`    → 转存到自己的网盘（返回 task_id）
//! 4. `GET  /task`                    → 轮询转存任务（status==2 成功）
//! 5. `POST /file/download`           → 直链列表（download_url）

use super::types::{classify_envelope, QuarkAuth, QuarkError, BASE, PR_FR, REFERER, USER_AGENT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// 夸克 HTTP 客户端（轻量：无状态，登录态由调用方显式传入）。
#[derive(Clone)]
pub struct QuarkClient {
    http: reqwest::Client,
    base: String,
}

/// 分享文件条目（`/share/sharepage/detail` 的 `data.list[]`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShareFile {
    /// 文件/目录 fid。
    #[serde(default)]
    pub fid: String,
    /// 文件名（字段名兼容 `file_name` / `share_name`）。
    #[serde(default)]
    pub file_name: String,
    /// 字节数（目录为 0）。
    #[serde(default)]
    pub size: u64,
    /// 是否目录（字段名兼容 `dir` / `file_type`）。
    #[serde(default)]
    pub dir: bool,
}

/// 转存任务状态（`/task` 的 `data.status`：2 = 成功，其余按运行中/失败归类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveTaskState {
    Pending,
    Running,
    Success,
    Failed,
}

/// 直链条目（`/file/download` 的 `data[]`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadLink {
    #[serde(default)]
    pub fid: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, rename = "download_url")]
    pub url: String,
}

/// 统一响应壳（data 形状由调用点决定）。Option 字段缺省自动为 None
/// （serde 对 Option 隐式缺省，避免对泛型 T 误加 T: Default 约束）。
#[derive(Deserialize)]
struct Envelope<T> {
    status: Option<i64>,
    code: Option<i64>,
    message: Option<String>,
    data: Option<T>,
}

impl QuarkClient {
    pub fn new() -> Self {
        Self::with_base(BASE.to_string())
    }

    /// 自定义基址（mock 测试注入本地 axum 服务地址）。
    pub fn with_base(base: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client build");
        QuarkClient { http, base: base.trim_end_matches('/').to_string() }
    }

    /// 公共头：Cookie 登录态 + Referer（pan.quark.cn）。
    fn headers(&self, auth: &QuarkAuth) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER as REF};
        let mut h = HeaderMap::new();
        if !auth.cookie.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&auth.cookie) {
                h.insert(COOKIE, v);
            }
        }
        if let Ok(v) = HeaderValue::from_str(REFERER) {
            h.insert(REF, v);
        }
        h
    }

    /// 发请求 + 统一壳解析 + 错误分类。
    async fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        json_body: Option<serde_json::Value>,
        auth: &QuarkAuth,
    ) -> Result<T, QuarkError> {
        let url = format!("{}/{}?{}", self.base, path.trim_start_matches('/'), PR_FR);
        let mut req = self.http.request(method, &url).headers(self.headers(auth));
        for (k, v) in query {
            req = req.query(&[(k, v)]);
        }
        if let Some(body) = json_body {
            req = req.json(&body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| QuarkError::Network(e.to_string()))?;
        let http_status = resp.status().as_u16();
        // 非 2xx：按状态/响应体 message 归类（401/403 → NotLogin）
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()));
            return Err(classify_envelope(http_status, None, message)
                .unwrap_or(QuarkError::BadResponse(format!("HTTP {http_status}: {body}"))));
        }
        let env: Envelope<T> = resp
            .json()
            .await
            .map_err(|e| QuarkError::BadResponse(format!("json decode: {e}")))?;
        // 业务失败：壳内 status != 2xx → 按 code/message 分类
        if let Some(s) = env.status {
            if !(200..=299).contains(&s) {
                return Err(classify_envelope(http_status, env.code, env.message.clone())
                    .unwrap_or(QuarkError::BadResponse(
                        env.message.unwrap_or_else(|| format!("status {s}")),
                    )));
            }
        }
        env.data.ok_or(QuarkError::BadResponse("响应缺少 data".into()))
    }

    /// 1) 分享凭据 stoken：`POST /share/sharepage/token`。
    pub async fn share_stoken(
        &self,
        auth: &QuarkAuth,
        pwd_id: &str,
        passcode: &str,
    ) -> Result<String, QuarkError> {
        #[derive(Deserialize)]
        struct Data { #[serde(default)] stoken: String }
        let body = serde_json::json!({ "pwd_id": pwd_id, "passcode": passcode });
        let d: Data = self
            .call(reqwest::Method::POST, "/share/sharepage/token", &[], Some(body), auth)
            .await?;
        if d.stoken.is_empty() {
            return Err(QuarkError::BadResponse("stoken 为空".into()));
        }
        Ok(d.stoken)
    }

    /// 2) 分享文件列表：`GET /share/sharepage/detail`（单层；翻页后续）。
    pub async fn share_detail(
        &self,
        auth: &QuarkAuth,
        pwd_id: &str,
        stoken: &str,
        pdir_fid: &str,
    ) -> Result<Vec<ShareFile>, QuarkError> {
        #[derive(Deserialize)]
        struct Data { #[serde(default)] list: Vec<ShareFile> }
        let d: Data = self
            .call(
                reqwest::Method::GET,
                "/share/sharepage/detail",
                &[("pwd_id", pwd_id), ("stoken", stoken), ("pdir_fid", pdir_fid), ("_page", "1"), ("_size", "100")],
                None,
                auth,
            )
            .await?;
        Ok(d.list)
    }

    /// 3) 转存：`POST /share/sharepage/save` → task_id。
    pub async fn share_save(
        &self,
        auth: &QuarkAuth,
        pwd_id: &str,
        stoken: &str,
        to_pdir_fid: &str,
        fids: &[String],
    ) -> Result<String, QuarkError> {
        #[derive(Deserialize)]
        struct Data { #[serde(default)] task_id: String }
        let body = serde_json::json!({
            "to_pdir_fid": to_pdir_fid,
            "fid_list": fids,
            "pwd_id": pwd_id,
            "stoken": stoken,
        });
        let d: Data = self
            .call(reqwest::Method::POST, "/share/sharepage/save", &[], Some(body), auth)
            .await?;
        if d.task_id.is_empty() {
            return Err(QuarkError::BadResponse("转存未返回 task_id".into()));
        }
        Ok(d.task_id)
    }

    /// 4) 转存任务轮询：`GET /task`（status==2 成功）。
    pub async fn task_state(
        &self,
        auth: &QuarkAuth,
        task_id: &str,
    ) -> Result<SaveTaskState, QuarkError> {
        #[derive(Deserialize)]
        struct Data { #[serde(default)] status: i64 }
        let d: Data = self
            .call(
                reqwest::Method::GET,
                "/task",
                &[("task_id", task_id), ("retry_index", "0")],
                None,
                auth,
            )
            .await?;
        Ok(match d.status {
            2 => SaveTaskState::Success,
            3.. => SaveTaskState::Failed,
            1 => SaveTaskState::Running,
            _ => SaveTaskState::Pending,
        })
    }

    /// 5) 直链：`POST /file/download`（fids → download_url 列表）。
    pub async fn file_download(
        &self,
        auth: &QuarkAuth,
        fids: &[String],
    ) -> Result<Vec<DownloadLink>, QuarkError> {
        let body = serde_json::json!({ "fids": fids });
        let d: Vec<DownloadLink> = self
            .call(reqwest::Method::POST, "/file/download", &[], Some(body), auth)
            .await?;
        Ok(d)
    }
}

impl Default for QuarkClient {
    fn default() -> Self {
        Self::new()
    }
}
