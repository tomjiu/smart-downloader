//! XunleiProvider：迅雷云盘渠道，实现 RemoteProvider。

use crate::types::{ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile};
use crate::xunlei::auth::{load as load_auth, save as save_auth, AuthState};
use crate::xunlei::client::Client;
use crate::xunlei::device::DeviceAuthFlow;
use smart_dl_core::types::{Capability, DownloadSource};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct XunleiProvider {
    name: String,
    client: Client,
    auth: Arc<Mutex<Option<AuthState>>>,
    token_path: PathBuf,
    tasks: Arc<Mutex<HashMap<ProviderTaskId, String>>>,  // id -> file_id
}

impl XunleiProvider {
    pub fn new(name: &str, token_path: PathBuf) -> Self {
        let auth = load_auth(&token_path);
        XunleiProvider {
            name: name.to_string(),
            client: Client::new(),
            auth: Arc::new(Mutex::new(auth)),
            token_path,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 开始设备码登录：返回一个 `DeviceAuthFlow`，上层调用 `start(scope)` 拿二维码，
    /// 然后 `poll_once(state)` 轮询直到 `Done`（拿到 token）或 `Failed`。
    pub fn begin_device_login(&self) -> DeviceAuthFlow {
        DeviceAuthFlow::new(self.client.clone())
    }

    /// 把设备码登录拿到的 token 写入登录态并持久化。
    pub async fn store_login(&self, access_token: String, refresh_token: String) -> Result<(), ProviderError> {
        // 登录后需初始化 captcha_token（匿名可拿）和 device_id。
        // device_id：若已有则复用，否则生成（这里用固定占位，真实生成待 daemon 层提供）。
        let device_id = {
            let guard = self.auth.lock().await;
            guard.as_ref().map(|s| s.device_id.clone()).unwrap_or_else(|| format!("wdi10.{}", now_unix()))
        };
        let mut state = AuthState {
            access_token,
            refresh_token,
            device_id,
            captcha_token: String::new(),
            access_token_expires_at: now_unix() + 43200, // 12h，实际以 token 响应为准
            captcha_token_expires_at: 0,
        };
        // 拉取 captcha_token
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
        ProviderRuntime {
            enabled: true,
            authenticated,
            quota_remaining: u64::MAX,
            concurrency_limit: 2,
            busy: 0,
            backoff_until: None,
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
        // 骨架：真实离线提交逻辑待端点确认后补
        let id = format!("{}-{}", self.name, now_unix());
        let _ = source;
        self.tasks.lock().await.insert(id.clone(), "pending-file-id".into());
        Ok(id)
    }
    async fn status(&self, id: &ProviderTaskId) -> Result<ProviderStatus, ProviderError> {
        self.tasks.lock().await.get(id).map(|_| ProviderStatus::Ready).ok_or(ProviderError::NotFound)
    }
    async fn resolve(&self, id: &ProviderTaskId) -> Result<Vec<ResolvedRemoteFile>, ProviderError> {
        let file_id = self.tasks.lock().await.get(id).cloned().ok_or(ProviderError::NotFound)?;
        let state = self.auth.lock().await.clone().ok_or(ProviderError::Auth)?;
        let play = self.client.resolve_link(&state, &file_id).await.map_err(|e| ProviderError::Other(e.to_string()))?;
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
}
