//! XunleiProvider：迅雷云盘渠道，实现 RemoteProvider。

use crate::types::{ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile};
use crate::xunlei::auth::{load as load_auth, save as save_auth, AuthState};
use crate::xunlei::client::Client;
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
        // 骨架：真实 PLAY API 取链待端点确认后补
        let _ = id;
        Err(ProviderError::Other("resolve not yet implemented (endpoint pending)".into()))
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
}
