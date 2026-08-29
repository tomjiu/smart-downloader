//! XunleiProvider：迅雷云盘渠道，实现 RemoteProvider。

use crate::types::{ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile};
use crate::xunlei::auth::{load as load_auth, save as save_auth, AuthState};
use crate::xunlei::client::Client;
use crate::xunlei::device::DeviceAuthFlow;
use smart_dl_core::types::{Capability, DownloadSource};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

pub struct XunleiProvider {
    name: String,
    client: Client,
    auth: Arc<AsyncMutex<Option<AuthState>>>,
    token_path: PathBuf,
    /// 本地任务句柄：ProviderTaskId → 云端 (task_id, file_id)。
    tasks: Arc<AsyncMutex<HashMap<ProviderTaskId, CloudTaskHandle>>>,
    /// 自动冷却：失败后该时刻前不参与 fallback 选择。
    backoff_until: Arc<Mutex<Option<std::time::Instant>>>,
}

/// 云端任务句柄。
#[derive(Clone, Debug)]
struct CloudTaskHandle {
    /// 离线任务 id（/drive/v1/tasks 轮询用；submit 响应可能不给）。
    cloud_task_id: String,
    /// 云盘文件 id（resolve 取直链用）。
    file_id: String,
}

impl XunleiProvider {
    pub fn new(name: &str, token_path: PathBuf) -> Self {
        let auth = load_auth(&token_path);
        XunleiProvider {
            name: name.to_string(),
            client: Client::new(),
            auth: Arc::new(AsyncMutex::new(auth)),
            token_path,
            tasks: Arc::new(AsyncMutex::new(HashMap::new())),
            backoff_until: Arc::new(Mutex::new(None)),
        }
    }

    /// 从登录态取 user_id（若已加载）；未登录或解析失败返回 None。
    pub async fn user_id(&self) -> Option<String> {
        let guard = self.auth.lock().await;
        guard.as_ref().map(|s| s.user_id.clone())
    }

    /// 设置冷却截止时刻（失败后自动降级）。
    fn set_backoff(&self, duration: std::time::Duration) {
        let mut guard = self.backoff_until.lock().unwrap();
        *guard = Some(std::time::Instant::now() + duration);
    }

    /// 清除冷却（操作成功时调用）。
    fn clear_backoff(&self) {
        let mut guard = self.backoff_until.lock().unwrap();
        *guard = None;
    }

    /// 剩余冷却时间（None = 无冷却）。
    fn backoff_remaining(&self) -> Option<std::time::Duration> {
        let guard = self.backoff_until.lock().unwrap();
        guard.and_then(|until| {
            let remaining = until.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                None
            } else {
                Some(remaining)
            }
        })
    }

    /// 按错误类型记录冷却（Auth 5 分钟 / Quota 1 小时 / 其他 1 分钟）。
    fn mark_failure(&self, err: &ProviderError) {
        let duration = match err {
            ProviderError::Auth => std::time::Duration::from_secs(300),
            ProviderError::Quota => std::time::Duration::from_secs(3600),
            _ => std::time::Duration::from_secs(60),
        };
        self.set_backoff(duration);
    }

    /// 执行异步操作，失败时自动记录冷却。
    async fn with_backoff<F, T>(&self, f: F) -> Result<T, ProviderError>
    where
        F: std::future::Future<Output = Result<T, ProviderError>>,
        T: std::fmt::Debug,
    {
        let result = f.await;
        if result.is_err() {
            self.mark_failure(result.as_ref().unwrap_err());
        }
        result
    }

    #[allow(dead_code)]
    /// 轻量级探活：检查登录态是否已加载且 access_token 未完全过期。
    async fn probe(&self) -> Result<(), ProviderError> {
        let guard = self.auth.try_lock().map(|g| g.clone()).unwrap_or(None);
        let state = guard.ok_or(ProviderError::Auth)?;
        let now = now_unix();
        if now >= state.access_token_expires_at {
            return Err(ProviderError::Other("access_token expired".into()));
        }
        Ok(())
    }

    /// 开始设备码登录：返回一个 `DeviceAuthFlow`，上层调用 `start(scope)` 拿二维码，
    /// 然后 `poll_once(state)` 轮询直到 `Done`（拿到 token）或 `Failed`。
    pub fn begin_device_login(&self) -> DeviceAuthFlow {
        DeviceAuthFlow::new(self.client.clone())
    }

    /// 把设备码登录拿到的 token 写入登录态并持久化。
    pub async fn store_login(&self, access_token: String, refresh_token: String) -> Result<(), ProviderError> {
        // 登录后需初始化 captcha_token 和 device_id。
        // device_id：若已有则复用，否则生成 32 位随机 hex（captcha_sign 只需 32 位）。
        // 注：完整 device_id 是 `wdi10.` + 64位hex（device-sign 流程），
        //     但 captcha_sign/取链只用到前 32 位，故这里按 32 位存储。
        let device_id = {
            let guard = self.auth.lock().await;
            guard.as_ref().map(|s| s.device_id.clone())
                .unwrap_or_else(generate_device_id)
        };
        let mut state = AuthState {
            access_token: access_token.clone(),
            refresh_token,
            device_id,
            captcha_token: String::new(),
            user_id: String::new(),
            access_token_expires_at: now_unix() + 43200, // 12h，实际以 token 响应为准
            captcha_token_expires_at: 0,
        };
        // 从 access_token（JWT sub）解析 user_id，captcha/init 需要。
        state.fill_user_id_from_token();
        // 拉取 captcha_token（带真实 meta + captcha_sign）。
        self.client.refresh_captcha(&mut state).await.map_err(|e| ProviderError::Other(e.to_string()))?;
        let mut guard = self.auth.lock().await;
        *guard = Some(state.clone());
        save_auth(&self.token_path, &state).map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::RemoteProvider for XunleiProvider {
    fn name(&self) -> &str { &self.name }
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::OfflineCache, Capability::UrlRefresh]
    }
    fn runtime(&self) -> ProviderRuntime {
        // 注意：这里使用 tokio::sync::Mutex，尝试 blocking_lock 在测试线程池中可能不可用；
        // 测试中通过 runtime_authenticated_false_when_no_auth 验证默认状态。
        // 生产环境应在已有运行时上下文中调用。
        let authenticated = self.auth.try_lock().map(|g| g.is_some()).unwrap_or(false);
        let backoff_until = self
            .backoff_remaining()
            .map(|d| now_unix() + d.as_secs());
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
        let mut guard = self.auth.lock().await;
        if let Some(state) = guard.as_mut() {
            if state.access_token_expiring(now_unix()) {
                self.client.refresh(state).await.map_err(|e| ProviderError::Other(e.to_string()))?;
            }
            if state.captcha_token_expiring(now_unix()) {
                self.client.refresh_captcha(state).await.map_err(|e| ProviderError::Other(e.to_string()))?;
            }
            let _ = save_auth(&self.token_path, state);
        }
        Ok(())
    }
    async fn submit(&self, source: &DownloadSource) -> Result<ProviderTaskId, ProviderError> {
        self.clear_backoff();
        self.with_backoff(async {
            self.refresh_auth().await?;
            // 离线提交（端点格式已由 verify_offline_submit.py 实测）：
            // POST /drive/v1/files {upload_type: UPLOAD_TYPE_URL, url:{url}}
            let (url, name) = match source {
                DownloadSource::Magnet(m) => (m.clone(), magnet_name(m)),
                DownloadSource::Http { url, .. } => (url.clone(), url_file_name(url)),
                DownloadSource::TorrentFile(_) | DownloadSource::Thunder(_)
                | DownloadSource::XunleiShare(_) | DownloadSource::Ftp { .. }
                | DownloadSource::Ed2k(_) => {
                    return Err(ProviderError::Other(
                        "v1 离线提交仅支持磁力/HTTP 链接（torrent 字节上传留后续）".into(),
                    ));
                }
            };
            let state = self.auth.lock().await.clone().ok_or(ProviderError::Auth)?;
            let resp = self.client.offline_submit(&state, &url, &name).await
                .map_err(|e| ProviderError::Other(e.to_string()))?;

            let pid = if resp.task_id.is_empty() {
                format!("{}-{}", self.name, now_unix())
            } else {
                format!("{}-{}", self.name, resp.task_id)
            };
            self.tasks.lock().await.insert(pid.clone(), CloudTaskHandle {
                cloud_task_id: resp.task_id,
                file_id: resp.file_id,
            });
            Ok(pid)
        })
        .await
    }
    async fn status(&self, id: &ProviderTaskId) -> Result<ProviderStatus, ProviderError> {
        self.clear_backoff();
        self.with_backoff(async {
            self.refresh_auth().await?;
            let handle = {
                let tasks = self.tasks.lock().await;
                tasks.get(id).cloned().ok_or(ProviderError::NotFound)?
            };
            if handle.cloud_task_id.is_empty() && handle.file_id.is_empty() {
                return Ok(ProviderStatus::Queued);
            }
            // Bug B 修复：此处原有一次重复 refresh_auth（入口已刷过）——每次都触发
            // save_auth 同步写盘，poll_ready 高频轮询下造成 fs 阻塞累积（运行时饿死）。
            let state = self.auth.lock().await.clone().ok_or(ProviderError::Auth)?;
            let tasks = self.client.offline_tasks(&state).await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            let (phase, discovered_file_id) = {
                let mut phase = String::new();
                let mut fid = String::new();
                for t in &tasks {
                    if (!handle.cloud_task_id.is_empty() && t.id == handle.cloud_task_id)
                        || (!handle.file_id.is_empty() && t.file_id == handle.file_id)
                    {
                        phase = t.phase.clone();
                        fid = t.file_id.clone();
                        break;
                    }
                }
                (phase, fid)
            };
            // 磁力提交响应里 file=null、file_id 为空——离线完成后才在任务列表中出现。
            // 此处回填句柄，resolve 才能取直链（F3.1 实测修复）。
            if !discovered_file_id.is_empty() && handle.file_id.is_empty() {
                let mut tasks = self.tasks.lock().await;
                if let Some(h) = tasks.get_mut(id) {
                    h.file_id = discovered_file_id.clone();
                }
            }
            let verdict = match phase.as_str() {
                p if p.contains("COMPLETE") => ProviderStatus::Ready,
                p if p.contains("FAIL") || p.contains("ERROR") => ProviderStatus::Failed,
                _ => ProviderStatus::Downloading,
            };
            Ok(verdict)
        })
        .await
        .map(|v| {
            if matches!(v, ProviderStatus::Ready | ProviderStatus::Failed) {
                self.clear_backoff();
            }
            v
        })
    }
    async fn resolve(&self, id: &ProviderTaskId) -> Result<Vec<ResolvedRemoteFile>, ProviderError> {
        // 取链前确保票新鲜（access 12h 自动续期 + captcha 过期重取），失败即 Auth 错。
        self.clear_backoff();
        self.with_backoff(async {
            self.refresh_auth().await?;
            let file_id = {
                let tasks = self.tasks.lock().await;
                tasks.get(id).map(|h| h.file_id.clone()).ok_or(ProviderError::NotFound)?
            };
            if file_id.is_empty() {
                return Err(ProviderError::Other("云端 file_id 尚未生成（任务未完成？）".into()));
            }
            let state = self.auth.lock().await.clone().ok_or(ProviderError::Auth)?;
            let play = self.client.resolve_link(&state, &file_id).await
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            let url = play.web_content_link;
            if url.is_empty() {
                return Err(ProviderError::Other("web_content_link empty".into()));
            }
            let size = play.size.or_else(|| url_query_u64(&url, "f")).unwrap_or(0);
            let expires_at = url_query_u64(&url, "e");
            Ok(vec![ResolvedRemoteFile {
                rel_path: if play.name.is_empty() { file_id } else { play.name },
                url,
                size,
                etag: None,
                expires_at,
            }])
        })
        .await
    }
    async fn remove(&self, id: &ProviderTaskId) -> Result<(), ProviderError> {
        self.tasks.lock().await.remove(id).ok_or(ProviderError::NotFound)?;
        Ok(())
    }
    async fn refresh_links(&self, id: &ProviderTaskId) -> Result<Option<Vec<String>>, ProviderError> {
        let _ = id;
        Ok(None)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// 从磁力链接提取展示名：`dn=` 参数优先，否则取 btih 末 8 位。
fn magnet_name(m: &str) -> String {
    for pair in m.split('&') {
        if let Some(dn) = pair.strip_prefix("dn=") {
            let decoded = urldecode(dn);
            if !decoded.is_empty() {
                return decoded;
            }
        }
    }
    let xt = m.split("btih:").nth(1).unwrap_or("unknown");
    let h = xt.split('&').next().unwrap_or("unknown");
    let tail: String = h.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
    format!("magnet-{tail}")
}

/// 从 HTTP URL 提取文件名（最后一段路径，去 query）。
fn url_file_name(url: &str) -> String {
    let no_query = url.split('?').next().unwrap_or(url);
    let seg = no_query.rsplit('/').next().unwrap_or("");
    let name = urldecode(seg);
    if name.is_empty() { format!("offline-{}", now_unix()) } else { name }
}

/// 极简 percent-decode（+ → 空格，%XX → 字节），足够覆盖 dn=/文件名场景。
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < bytes.len() + 1 && i + 2 < bytes.len() + 1 => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                if i + 2 < bytes.len() {
                    if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        out.push(h * 16 + l);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'%');
                i += 1;
            }
            b => { out.push(b); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 生成 32 位 hex 的 device_id（captcha_sign / 取链用）。
///
/// 已验证（test_random_device_e2e.py）：服务端对 device_id **不做 /risk 来源校验**，
/// 任意 32 位 hex 都能完成 captcha/init → drive/v1/files 全流程（HTTP 200 + 列出文件）。
/// 浏览器真实的 device_id 由 `XLDeviceSignUtil`（/risk 设备注册）生成，但那套流程
/// 对取链/列表并非必需，故这里本地随机生成即可。
fn generate_device_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    // 用纳秒低 64 位 + 地址随机化（ASLR），展开成 32 位 hex。
    let mut seed = nanos as u64;
    let aslr = &seed as *const _ as u64;
    seed ^= aslr;
    let mut hex = String::with_capacity(32);
    for _ in 0..4 {
        hex.push_str(&format!("{:016x}", seed));
        // 简单扰动（xorshift），避免重复段。
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed ^= aslr.rotate_left(13);
    }
    hex.truncate(32);
    hex
}

/// 从 URL 查询串里取一个 u64 参数（如 f=size, e=expires）。
fn url_query_u64(url: &str, key: &str) -> Option<u64> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            if let Some(v) = kv.next() {
                if let Ok(n) = v.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteProvider;
    #[test]
    fn reports_name_and_capabilities() {
        let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent_test.json"));
        assert_eq!(p.name(), "xunlei");
        assert!(p.capabilities().contains(&Capability::OfflineCache));
        assert!(p.capabilities().contains(&Capability::UrlRefresh));
    }
    #[test]
    fn runtime_authenticated_false_when_no_auth() {
        let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent_test.json"));
        assert!(!p.runtime().authenticated);
    }
    #[test]
    fn url_query_parses_size_and_expires() {
        let url = "https://vod.xunlei.com/download/?fid=x&f=27471387&e=1787156621&g=abc";
        assert_eq!(url_query_u64(url, "f"), Some(27471387));
        assert_eq!(url_query_u64(url, "e"), Some(1787156621));
        assert_eq!(url_query_u64(url, "g"), None); // g 非数字
        assert_eq!(url_query_u64(url, "nonexistent"), None);
    }

    #[test]
    fn magnet_name_prefers_dn_param() {
        assert_eq!(magnet_name("magnet:?xt=urn:btih:ABCDEF1234567890&dn=My%20Movie.mp4"), "My Movie.mp4");
    }

    #[test]
    fn magnet_name_falls_back_to_btih_tail() {
        let n = magnet_name("magnet:?xt=urn:btih:000000000000000000000000deadbeef");
        assert_eq!(n, "magnet-deadbeef");
        assert_eq!(magnet_name("magnet:?xt=urn:btih:X"), "magnet-X");
    }

    #[test]
    fn url_file_name_extracts_last_segment() {
        assert_eq!(url_file_name("https://example.com/a/b/file.zip?token=x"), "file.zip");
        assert_eq!(url_file_name("https://example.com/file.zip"), "file.zip");
        // 裸域名 → 最后一段即主机名
        assert_eq!(url_file_name("https://example.com"), "example.com");
        // 以 / 结尾 → 空段回退时间戳前缀
        assert!(url_file_name("https://example.com/").starts_with("offline-"));
    }

    #[test]
    fn urldecode_handles_percent_and_plus() {
        assert_eq!(urldecode("a%20b+c"), "a b c");
        assert_eq!(urldecode("%E4%B8%AD%E6%96%87"), "中文");
        assert_eq!(urldecode("plain"), "plain");
        assert_eq!(urldecode("bad%2"), "bad%2"); // 截断的 % 不 panic
    }

    // ---- 探活 + 自动降级 ----

    #[tokio::test]
    async fn probe_ok_when_auth_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("auth.json");
        let state = AuthState {
            access_token: "test".into(),
            refresh_token: "test".into(),
            device_id: "test".into(),
            captcha_token: "test".into(),
            user_id: "test".into(),
            access_token_expires_at: now_unix() + 3600,
            captcha_token_expires_at: 0,
        };
        save_auth(&token_path, &state).unwrap();
        let p = XunleiProvider::new("xunlei", token_path);
        assert!(p.probe().await.is_ok());
    }

    #[tokio::test]
    async fn probe_err_when_no_auth() {
        let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent.json"));
        assert!(p.probe().await.is_err());
    }

    #[tokio::test]
    async fn probe_err_when_token_expired() {
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("auth.json");
        let state = AuthState {
            access_token: "test".into(),
            refresh_token: "test".into(),
            device_id: "test".into(),
            captcha_token: "test".into(),
            user_id: "test".into(),
            access_token_expires_at: now_unix() - 10,
            captcha_token_expires_at: 0,
        };
        save_auth(&token_path, &state).unwrap();
        let p = XunleiProvider::new("xunlei", token_path);
        let err = p.probe().await.unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[tokio::test]
    async fn submit_sets_backoff_on_error() {
        use smart_dl_core::types::DownloadSource;
        let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent.json"));
        let source = DownloadSource::Http {
            url: "https://example.com/file.zip".into(),
            headers: vec![],
            auth: None,
            backup_url: None,
        };
        let res = p.submit(&source).await;
        assert!(res.is_err());
        // backoff 应已写入（哪怕只有几毫秒）
        let rt = p.runtime();
        assert!(rt.backoff_until.is_some(), "submit 失败应触发 backoff");
    }

    #[tokio::test]
    async fn runtime_reflects_backoff_countdown() {
        let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent.json"));
        // 先无冷却
        assert!(p.runtime().backoff_until.is_none());
        // 手动注入冷却
        p.set_backoff(std::time::Duration::from_secs(300));
        let rt = p.runtime();
        assert!(rt.backoff_until.is_some());
        // 冷却结束后应清空
        // 这里不实际等 5 分钟，直接 clear 验证逻辑
        p.clear_backoff();
        assert!(p.runtime().backoff_until.is_none());
    }
}
