//! 夸克分享链接解析 + QuarkProvider（RemoteProvider 实现）。
//!
//! 流程对齐 RemoteProvider 契约（§13：submit → status 轮询 → resolve 直链）：
//! - `submit`：解析分享 → stoken → 文件列表 → **转存**（夸克的"云端缓存"，
//!   等价于迅雷渠道的离线下载语义）→ 返回本地任务句柄；
//! - `status`：轮询转存任务（Success → Ready）；
//! - `resolve`：`/file/download` 取直链列表 → `ResolvedRemoteFile`
//!   （交 HttpEngine 下载；直链带时效 → expires_at 注入保守余量）；
//! - `refresh_links`：直链每次调用都会变 → 重新调 `/file/download`。
//!
//! 失败冷却对齐 `xunlei::provider`：Auth 5min / Quota 1h / 其他 1min。
//! 未吸收项见 `docs/CAPABILITY_ABSORBED.md`（夸克上报通道零埋点决策）。

use crate::types::{
    ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile,
};
use crate::RemoteProvider;
use parking_lot::Mutex;
use smart_dl_core::types::{Capability, DownloadSource};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

use super::client::{QuarkClient, SaveTaskState};
use super::types::{load_auth, save_auth, QuarkAuth, QuarkError};

/// 解析后的夸克分享链接：`https://pan.quark.cn/s/<pwd_id>`（提取码可选）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarkShareLink {
    /// 分享 id（URL 路径段）。
    pub pwd_id: String,
    /// 提取码（`?pwd=` 或 `#/list/share?pwd=`；无则为空串）。
    pub passcode: String,
}

/// 识别并解析夸克分享链接；非夸克分享返回 None。
pub fn parse_share_link(url: &str) -> Option<QuarkShareLink> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://pan.quark.cn/s/")
        .or_else(|| lower.strip_prefix("http://pan.quark.cn/s/"))?;
    // path 段在 `?`/`#` 之前
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let pwd_id = rest[..path_end].trim_end_matches('/').to_string();
    if pwd_id.is_empty() {
        return None;
    }
    // 提取码：query 的 pwd= 优先，其次 fragment 里的 pwd=
    let (query, fragment) = match rest[path_end..].split_once('#') {
        Some((q, f)) => (q.to_string(), f.to_string()),
        None => (rest[path_end..].to_string(), String::new()),
    };
    let passcode = query_param(&query, "pwd")
        .or_else(|| query_param(&fragment, "pwd"))
        .unwrap_or_default();
    Some(QuarkShareLink { pwd_id, passcode })
}

/// 极简 query/fragment 参数提取（不 percent-decode：提取码为字母数字；
/// 容忍 fragment 内含 `?pwd=` 形状，按 `&` 与 `?` 一并切分）。
fn query_param(s: &str, key: &str) -> Option<String> {
    let query = s.strip_prefix('?').unwrap_or(s);
    for pair in query.split(['&', '?']) {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            return kv.next().map(|v| v.to_string());
        }
    }
    None
}

/// Provider 本地任务句柄（ProviderTaskId → 云端上下文）。
#[derive(Clone, Debug)]
struct QuarkTaskHandle {
    /// 转存任务 id（status 轮询用）。
    save_task_id: String,
    /// 转存后的文件 fid（resolve 直链用）。
    fids: Vec<String>,
}

/// 夸克网盘渠道 Provider（分享链接 → 转存 → 直链）。
pub struct QuarkProvider {
    name: String,
    client: QuarkClient,
    auth: Arc<AsyncMutex<Option<QuarkAuth>>>,
    cookie_path: PathBuf,
    tasks: Arc<AsyncMutex<HashMap<ProviderTaskId, QuarkTaskHandle>>>,
    /// 自动冷却：失败后该时刻前不参与 fallback 选择（对齐 xunlei）。
    backoff_until: Arc<Mutex<Option<std::time::Instant>>>,
}

/// 直链保守有效期（秒）：夸克直链实测分钟级失效，
/// resolve 后立即由 HttpEngine 承接，30 分钟余量足够。
const DIRECT_LINK_TTL_SECS: u64 = 1800;

impl QuarkProvider {
    pub fn new(name: &str, cookie_path: PathBuf) -> Self {
        let auth = load_auth(&cookie_path);
        QuarkProvider {
            name: name.to_string(),
            client: QuarkClient::new(),
            auth: Arc::new(AsyncMutex::new(auth)),
            cookie_path,
            tasks: Arc::new(AsyncMutex::new(HashMap::new())),
            backoff_until: Arc::new(Mutex::new(None)),
        }
    }

    /// mock 测试注入自定义基址。
    #[cfg(test)]
    pub(crate) fn with_base(name: &str, cookie_path: PathBuf, base: String) -> Self {
        let mut p = Self::new(name, cookie_path);
        p.client = QuarkClient::with_base(base);
        p
    }

    /// 注入 Cookie 登录态（daemon/cli 登录流程调用）并持久化。
    pub async fn set_cookie(&self, cookie: String, user_id: String) -> Result<(), ProviderError> {
        let auth = QuarkAuth { cookie, user_id };
        save_auth(&self.cookie_path, &auth).map_err(|e| ProviderError::Other(e.to_string()))?;
        *self.auth.lock().await = Some(auth);
        Ok(())
    }

    fn set_backoff(&self, duration: std::time::Duration) {
        *self.backoff_until.lock() = Some(std::time::Instant::now() + duration);
    }

    fn clear_backoff(&self) {
        *self.backoff_until.lock() = None;
    }

    fn backoff_remaining(&self) -> Option<std::time::Duration> {
        self.backoff_until.lock().and_then(|until| {
            let remaining = until.saturating_duration_since(std::time::Instant::now());
            (!remaining.is_zero()).then_some(remaining)
        })
    }

    /// 按错误类型记录冷却（对齐 xunlei：Auth 5min / Quota 1h / 其他 1min）。
    fn mark_failure(&self, err: &ProviderError) {
        let duration = match err {
            ProviderError::Auth => std::time::Duration::from_secs(300),
            ProviderError::Quota => std::time::Duration::from_secs(3600),
            _ => std::time::Duration::from_secs(60),
        };
        self.set_backoff(duration);
    }

    /// 失败自动冷却包装。
    async fn with_backoff<F, T>(&self, f: F) -> Result<T, ProviderError>
    where
        F: std::future::Future<Output = Result<T, ProviderError>>,
    {
        let result = f.await;
        if let Err(e) = &result {
            self.mark_failure(e);
        }
        result
    }

    /// 登录态快照（未登录返回 NotLogin）。
    async fn require_auth(&self) -> Result<QuarkAuth, ProviderError> {
        self.auth
            .lock()
            .await
            .clone()
            .filter(|a| a.is_valid())
            .ok_or(ProviderError::Auth)
    }

    /// 提交的完整流程：解析 → stoken → 列表 → 转存。
    async fn submit_share(&self, link: &QuarkShareLink) -> Result<ProviderTaskId, ProviderError> {
        let auth = self.require_auth().await?;
        let stoken = self
            .client
            .share_stoken(&auth, &link.pwd_id, &link.passcode)
            .await
            .map_err(ProviderError::from)?;
        let files = self
            .client
            .share_detail(&auth, &link.pwd_id, &stoken, "0")
            .await
            .map_err(ProviderError::from)?;
        // v1 只转存分享根目录下的文件（目录递归留后续，见 CAPABILITY_ABSORBED）
        let fids: Vec<String> = files
            .iter()
            .filter(|f| !f.dir && !f.fid.is_empty())
            .map(|f| f.fid.clone())
            .collect();
        if fids.is_empty() {
            return Err(ProviderError::Other(
                "quark share has no files at root".into(),
            ));
        }
        let save_task_id = self
            .client
            .share_save(&auth, &link.pwd_id, &stoken, "0", &fids)
            .await
            .map_err(ProviderError::from)?;
        let pid = format!("{}-{}", self.name, link.pwd_id);
        self.tasks
            .lock()
            .await
            .insert(pid.clone(), QuarkTaskHandle { save_task_id, fids });
        Ok(pid)
    }
}

#[async_trait::async_trait]
impl RemoteProvider for QuarkProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Vec<Capability> {
        // 转存 = 云端缓存（OfflineCache 语义）；直链每次刷新（UrlRefresh）。
        vec![Capability::OfflineCache, Capability::UrlRefresh]
    }

    fn runtime(&self) -> ProviderRuntime {
        let authenticated = self
            .auth
            .try_lock()
            .map(|g| g.as_ref().map(|a| a.is_valid()).unwrap_or(false))
            .unwrap_or(false);
        let backoff_until = self
            .backoff_remaining()
            .map(|d| super::super::types::now_unix() + d.as_secs());
        ProviderRuntime {
            enabled: true,
            authenticated,
            quota_remaining: u64::MAX,
            concurrency_limit: 2,
            busy: 0,
            backoff_until,
            last_error: None,
        }
    }

    async fn refresh_auth(&self) -> Result<(), ProviderError> {
        // Cookie 登录态无刷新流程；未登录时报 Auth 让调用方走登录。
        let guard = self.auth.lock().await;
        if guard.as_ref().map(|a| a.is_valid()).unwrap_or(false) {
            Ok(())
        } else {
            Err(ProviderError::Auth)
        }
    }

    async fn submit(&self, source: &DownloadSource) -> Result<ProviderTaskId, ProviderError> {
        self.clear_backoff();
        self.with_backoff(async {
            let link = match source {
                DownloadSource::Http { url, .. } => parse_share_link(url),
                _ => None,
            };
            let link = link.ok_or(ProviderError::from(QuarkError::NotShareLink))?;
            self.submit_share(&link).await
        })
        .await
    }

    async fn status(&self, id: &ProviderTaskId) -> Result<ProviderStatus, ProviderError> {
        self.clear_backoff();
        let verdict = self
            .with_backoff(async {
                let auth = self.require_auth().await?;
                let handle = self
                    .tasks
                    .lock()
                    .await
                    .get(id)
                    .cloned()
                    .ok_or(ProviderError::NotFound)?;
                let state = self
                    .client
                    .task_state(&auth, &handle.save_task_id)
                    .await
                    .map_err(ProviderError::from)?;
                Ok(match state {
                    SaveTaskState::Success => ProviderStatus::Ready,
                    SaveTaskState::Failed => ProviderStatus::Failed,
                    SaveTaskState::Running => ProviderStatus::Downloading,
                    SaveTaskState::Pending => ProviderStatus::Queued,
                })
            })
            .await;
        if matches!(verdict, Ok(ProviderStatus::Ready | ProviderStatus::Failed)) {
            self.clear_backoff();
        }
        verdict
    }

    async fn resolve(&self, id: &ProviderTaskId) -> Result<Vec<ResolvedRemoteFile>, ProviderError> {
        self.clear_backoff();
        self.with_backoff(async {
            let auth = self.require_auth().await?;
            let handle = self
                .tasks
                .lock()
                .await
                .get(id)
                .cloned()
                .ok_or(ProviderError::NotFound)?;
            let links = self
                .client
                .file_download(&auth, &handle.fids)
                .await
                .map_err(ProviderError::from)?;
            let now = super::super::types::now_unix();
            Ok(links
                .into_iter()
                .filter(|l| !l.url.is_empty())
                .map(|l| ResolvedRemoteFile {
                    rel_path: if l.file_name.is_empty() {
                        l.fid.clone()
                    } else {
                        l.file_name
                    },
                    url: l.url,
                    size: l.size,
                    etag: None,
                    expires_at: Some(now + DIRECT_LINK_TTL_SECS),
                })
                .collect())
        })
        .await
    }

    async fn remove(&self, id: &ProviderTaskId) -> Result<(), ProviderError> {
        self.tasks
            .lock()
            .await
            .remove(id)
            .ok_or(ProviderError::NotFound)?;
        Ok(())
    }

    async fn refresh_links(
        &self,
        id: &ProviderTaskId,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        // 夸克直链每次请求都会重新签发 → 直接重取即可（不像迅雷需要 resubmit）。
        let files = self.resolve(id).await?;
        Ok(Some(files.into_iter().map(|f| f.url).collect()))
    }

    async fn probe(&self) -> Result<(), ProviderError> {
        match self.auth.try_lock() {
            Ok(g) if g.as_ref().map(|a| a.is_valid()).unwrap_or(false) => Ok(()),
            _ => Err(ProviderError::Auth),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试：链接解析单测 + axum 本地 mock 全流程
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderError;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::Json;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU32, Ordering};

    // ---- 链接解析 ----

    #[test]
    fn parse_share_link_variants() {
        let l = parse_share_link("https://pan.quark.cn/s/abc123#/list/share").unwrap();
        assert_eq!(l.pwd_id, "abc123");
        assert_eq!(l.passcode, "");

        let l2 = parse_share_link("https://pan.quark.cn/s/8a7b?pwd=6666").unwrap();
        assert_eq!(l2.pwd_id, "8a7b");
        assert_eq!(l2.passcode, "6666");

        // fragment 内带提取码的形状
        let l3 =
            parse_share_link("https://pan.quark.cn/s/xy99?entry=1#/list/share?pwd=8888").unwrap();
        assert_eq!(l3.pwd_id, "xy99");
        assert_eq!(l3.passcode, "8888");

        // 非夸克 / 空段
        assert!(parse_share_link("https://pan.xunlei.com/s/abc").is_none());
        assert!(parse_share_link("https://pan.quark.cn/s/").is_none());
        assert!(parse_share_link("magnet:?xt=urn:btih:AA").is_none());
    }

    // ---- axum mock 端点（形状与 client.rs 一致）----

    #[derive(Clone, Default)]
    struct MockState {
        fail_auth: bool,
        share_expired: bool,
        calls: std::sync::Arc<AtomicU32>,
    }

    fn ok_envelope(data: Value) -> Json<Value> {
        Json(json!({"status": 200i64, "code": 0i64, "message": "", "data": data}))
    }

    fn err_envelope(status: i64, message: &str) -> (StatusCode, Json<Value>) {
        (
            StatusCode::OK,
            Json(json!({"status": status, "code": status, "message": message})),
        )
    }

    fn has_cookie(headers: &HeaderMap) -> bool {
        headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .map(|c| c.contains("__pus=valid"))
            .unwrap_or(false)
    }

    async fn share_token(
        State(st): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        st.calls.fetch_add(1, Ordering::Relaxed);
        if !has_cookie(&headers) || st.fail_auth {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"message": "login required"})),
            ));
        }
        if st.share_expired || body["pwd_id"] == "expired" {
            return Err(err_envelope(41008, "分享已失效"));
        }
        Ok(ok_envelope(json!({"stoken": "ST-1"})))
    }

    async fn share_detail(
        State(st): State<MockState>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        if !has_cookie(&headers) {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"message": "login required"})),
            ));
        }
        if st.share_expired {
            return Err(err_envelope(41008, "分享已失效"));
        }
        Ok(ok_envelope(json!({"list": [
            {"fid": "F1", "file_name": "setup.zip", "size": 1024u64, "dir": false},
            {"fid": "D1", "file_name": "docs", "size": 0u64, "dir": true},
        ]})))
    }

    async fn share_save(
        State(_st): State<MockState>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        if !has_cookie(&headers) {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"message": "login required"})),
            ));
        }
        Ok(ok_envelope(json!({"task_id": "T-1"})))
    }

    async fn task_poll(State(_st): State<MockState>) -> Json<Value> {
        // 夸克转存任务：status==2 成功
        ok_envelope(json!({"status": 2i64, "task_id": "T-1"}))
    }

    async fn file_download(
        State(_st): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        if !has_cookie(&headers) {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"message": "login required"})),
            ));
        }
        let fids = body["fids"].as_array().cloned().unwrap_or_default();
        let links: Vec<Value> = fids
            .iter()
            .map(|f| {
                json!({"fid": f, "file_name": "setup.zip", "size": 1024u64,
                            "download_url": "https://dl-mock.quark.cn/file/setup.zip?e=999"})
            })
            .collect();
        Ok(ok_envelope(Value::Array(links)))
    }

    fn mock_router(st: MockState) -> axum::Router {
        // 路径与 client.rs 的相对端点一致（测试基址不含 /1.0/clouddrive 前缀）
        axum::Router::new()
            .route("/share/sharepage/token", post(share_token))
            .route("/share/sharepage/detail", get(share_detail))
            .route("/share/sharepage/save", post(share_save))
            .route("/task", get(task_poll))
            .route("/file/download", post(file_download))
            .with_state(st)
    }

    fn share_source(url: &str) -> DownloadSource {
        DownloadSource::Http {
            url: url.into(),
            headers: vec![],
            auth: None,
            backup_url: None,
            proxy: None,
        }
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

    // ---- happy path：submit → status → resolve ----

    #[tokio::test]
    async fn share_flow_happy_path() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let p = QuarkProvider::with_base("quark", dir.path().join("cookie.json"), base);
        p.set_cookie("__pus=valid; __puus=ok".into(), "u1".into())
            .await
            .unwrap();

        // runtime：已登录、可选
        let rt = p.runtime();
        assert!(rt.authenticated && rt.enabled && rt.backoff_until.is_none());
        assert!(p.probe().await.is_ok());

        let pid = p
            .submit(&share_source("https://pan.quark.cn/s/good1#/list/share"))
            .await
            .unwrap();
        assert!(pid.starts_with("quark-good1"));
        assert_eq!(p.status(&pid).await.unwrap(), ProviderStatus::Ready);

        let files = p.resolve(&pid).await.unwrap();
        assert_eq!(files.len(), 1, "目录 D1 不转存");
        assert_eq!(files[0].rel_path, "setup.zip");
        assert_eq!(files[0].size, 1024);
        assert!(files[0].expires_at.is_some(), "直链应带时效");
        assert!(files[0].url.contains("dl-mock.quark.cn"));

        // refresh_links：直链刷新
        let urls = p.refresh_links(&pid).await.unwrap().unwrap();
        assert_eq!(urls.len(), 1);

        // remove 后句柄消失
        p.remove(&pid).await.unwrap();
        assert!(matches!(
            p.resolve(&pid).await,
            Err(ProviderError::NotFound)
        ));
        server.abort();
    }

    // ---- NotLogin：未登录 → Auth 错误 + 冷却 ----

    #[tokio::test]
    async fn not_login_maps_to_auth_and_backoff() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let p = QuarkProvider::with_base("quark", dir.path().join("cookie.json"), base);
        // 未注入 cookie
        assert!(!p.runtime().authenticated);

        let err = p
            .submit(&share_source("https://pan.quark.cn/s/good1"))
            .await
            .unwrap_err();
        assert_eq!(err, ProviderError::Auth, "NotLogin → ProviderError::Auth");
        let rt = p.runtime();
        assert!(rt.backoff_until.is_some(), "Auth 失败应触发 5min 冷却");
        server.abort();
    }

    // ---- ShareExpired：分享失效 → 分类正确 + 冷却 ----

    #[tokio::test]
    async fn share_expired_classified_and_backoff() {
        let (base, server) = spawn_mock(MockState {
            share_expired: true,
            ..Default::default()
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let p = QuarkProvider::with_base("quark", dir.path().join("cookie.json"), base);
        p.set_cookie("__pus=valid".into(), String::new())
            .await
            .unwrap();

        let err = p
            .submit(&share_source("https://pan.quark.cn/s/expired"))
            .await
            .unwrap_err();
        match &err {
            ProviderError::Other(msg) => {
                assert!(msg.contains("share expired"), "ShareExpired 分类：{msg}");
            }
            other => panic!("应为 Other（ShareExpired）：{other:?}"),
        }
        assert!(p.runtime().backoff_until.is_some());
        // 非 Auth/Quota → 短冷却（1min）
        let until = p.runtime().backoff_until.unwrap();
        let now = crate::types::now_unix();
        assert!(until <= now + 61, "ShareExpired 属 Other 档 → 1 分钟冷却");
        server.abort();
    }

    // ---- 非夸克链接：fail fast ----

    #[tokio::test]
    async fn non_share_link_rejected() {
        let (base, server) = spawn_mock(MockState::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let p = QuarkProvider::with_base("quark", dir.path().join("cookie.json"), base);
        p.set_cookie("__pus=valid".into(), String::new())
            .await
            .unwrap();
        let err = p
            .submit(&share_source("https://example.com/file.zip"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a quark share link"));
        server.abort();
    }

    #[test]
    fn name_and_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let p = QuarkProvider::new("quark", dir.path().join("c.json"));
        assert_eq!(p.name(), "quark");
        let caps = p.capabilities();
        assert!(caps.contains(&Capability::OfflineCache));
        assert!(caps.contains(&Capability::UrlRefresh));
    }
}
