//! MockProvider：内存实现（§13 RemoteProvider），可注入配额/backoff/认证/直链配置。

use crate::types::{
    link_expired, ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId,
    ResolvedRemoteFile,
};
use parking_lot::Mutex;
use smart_dl_core::types::{Capability, DownloadSource};
use std::collections::HashMap;
use std::sync::Arc;

struct MockTask {
    status: ProviderStatus,
    /// status() 调用次数（自动推进 Queued→Downloading→Ready）。
    status_calls: u32,
    /// 第几次 submit（1=首次，≥2=resubmit）。
    submit_seq: u32,
    /// 延迟 Ready 截止时刻（submit 时刻 + ready_delay；None = 旧行为自动推进）。
    /// Bug B 复现能力：模拟真实云盘「离线下载数分钟」的协调器等待窗。
    ready_at: Option<std::time::Instant>,
}

struct MockState {
    enabled: bool,
    authenticated: bool,
    quota_remaining: u64,
    concurrency_limit: u32,
    busy: u32,
    backoff_until: Option<u64>,
    last_error: Option<String>,
    /// 首次 submit 的 resolve 文件。
    files: Vec<ResolvedRemoteFile>,
    /// resubmit（第 ≥2 次 submit）的 resolve 文件。
    resubmit_files: Vec<ResolvedRemoteFile>,
    /// refresh_links 提供的更新 URL → resolve 时替换 files 的 url。
    update_urls: Option<Vec<String>>,
    /// refresh_links 是否已调用（update_urls 只在刷新后生效）。
    refreshed: bool,
    /// 下一次 submit 直接 Failed（fail_next_submits 注入）。
    fail_next: bool,
    /// 延迟 Ready 时长（>0 时 submit 后该时长内 status=Downloading，到期 Ready；
    /// 0 = 旧行为：status_calls 自动推进）。Bug B 复现能力，缺省 0 不影响既有测试。
    ready_delay: std::time::Duration,
    next_id: u64,
    tasks: HashMap<ProviderTaskId, MockTask>,
}

/// 内存 Mock Provider：submit 后状态自动推进，resolve 返回配置的直链。
#[derive(Clone)]
pub struct MockProvider {
    name: String,
    state: Arc<Mutex<MockState>>,
}

impl MockProvider {
    pub fn new(name: &str) -> Self {
        MockProvider {
            name: name.to_string(),
            state: Arc::new(Mutex::new(MockState {
                enabled: true,
                authenticated: true,
                quota_remaining: u64::MAX,
                concurrency_limit: 2,
                busy: 0,
                backoff_until: None,
                last_error: None,
                files: vec![],
                resubmit_files: vec![],
                update_urls: None,
                refreshed: false,
                fail_next: false,
                ready_delay: ready_delay_from_env(),
                next_id: 1,
                tasks: HashMap::new(),
            })),
        }
    }

    pub fn with_quota(self, q: u64) -> Self {
        self.state.lock().quota_remaining = q;
        self
    }

    pub fn disabled(self) -> Self {
        self.state.lock().enabled = false;
        self
    }

    pub fn unauthenticated(self) -> Self {
        self.state.lock().authenticated = false;
        self
    }

    pub fn with_backoff(self, until_unix: u64) -> Self {
        self.state.lock().backoff_until = Some(until_unix);
        self
    }

    pub fn with_concurrency(self, n: u32) -> Self {
        self.state.lock().concurrency_limit = n;
        self
    }

    /// 延迟 Ready：submit 后 `secs` 秒内 status=Downloading，到期 Ready。
    /// 0 = 旧行为（status_calls 自动推进 Queued→Downloading→Ready）。
    /// Bug B 免配额复现钥匙：用慢速 Mock 拉开协调器 poll_ready 等待窗，
    /// 等效真实云盘「离线下载数分钟」，不消耗账号配额。
    pub fn with_ready_delay_secs(self, secs: u64) -> Self {
        self.state.lock().ready_delay = std::time::Duration::from_secs(secs);
        self
    }

    pub fn with_files(self, files: Vec<ResolvedRemoteFile>) -> Self {
        self.state.lock().files = files;
        self
    }

    /// resubmit 轮次的 resolve 文件（新直链）。
    pub fn set_resubmit_files(&self, files: Vec<ResolvedRemoteFile>) {
        self.state.lock().resubmit_files = files;
    }

    /// update_sources 携带的新 URL（refresh_links 输出，resolve 时替换 files url）。
    pub fn set_update_urls(&self, urls: Vec<String>) {
        self.state.lock().update_urls = Some(urls);
    }

    /// 测试观察：当前占用并发。
    pub fn set_busy(&self, n: u32) {
        self.state.lock().busy = n;
    }

    /// 测试注入：下一次 submit 创建的任务直接进入 Failed（poll_ready 失败分支）。
    pub fn fail_next_submits(&self) {
        let mut st = self.state.lock();
        st.fail_next = true;
    }

    /// 测试观察：剩余配额。
    pub fn quota(&self) -> u64 {
        self.state.lock().quota_remaining
    }

    /// refresh_links：update_sources 用的新 URL 列表（None = 无新链接）。
    pub fn refresh_links_sync(
        &self,
        id: &ProviderTaskId,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        let mut st = self.state.lock();
        if !st.tasks.contains_key(id) {
            return Err(ProviderError::NotFound);
        }
        st.refreshed = true;
        Ok(st.update_urls.clone())
    }
}

#[async_trait::async_trait]
impl crate::RemoteProvider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::OfflineCache]
    }

    fn runtime(&self) -> ProviderRuntime {
        let st = self.state.lock();
        ProviderRuntime {
            enabled: st.enabled,
            authenticated: st.authenticated,
            quota_remaining: st.quota_remaining,
            concurrency_limit: st.concurrency_limit,
            busy: st.busy,
            backoff_until: st.backoff_until,
            last_error: st.last_error.clone(),
        }
    }

    async fn refresh_auth(&self) -> Result<(), ProviderError> {
        let mut st = self.state.lock();
        st.authenticated = true;
        Ok(())
    }

    async fn submit(&self, _source: &DownloadSource) -> Result<ProviderTaskId, ProviderError> {
        let mut st = self.state.lock();
        if !st.enabled {
            return Err(ProviderError::Other("disabled".into()));
        }
        if !st.authenticated {
            return Err(ProviderError::Auth);
        }
        if st.quota_remaining == 0 {
            return Err(ProviderError::Quota);
        }
        let id = format!("{}-{}", self.name, st.next_id);
        let seq = st.next_id as u32;
        st.next_id += 1;
        let initial = if st.fail_next {
            st.fail_next = false;
            ProviderStatus::Failed
        } else {
            ProviderStatus::Queued
        };
        if initial == ProviderStatus::Failed {
            st.backoff_until = Some(crate::types::now_unix() + 60);
        }
        let ready_at = if matches!(initial, ProviderStatus::Failed) {
            None
        } else {
            (st.ready_delay.as_secs() > 0).then(|| std::time::Instant::now() + st.ready_delay)
        };
        st.tasks.insert(
            id.clone(),
            MockTask {
                status: initial,
                status_calls: 0,
                submit_seq: seq,
                ready_at,
            },
        );
        Ok(id)
    }

    async fn status(&self, id: &ProviderTaskId) -> Result<ProviderStatus, ProviderError> {
        let mut st = self.state.lock();
        let t = st.tasks.get_mut(id).ok_or(ProviderError::NotFound)?;
        t.status_calls += 1;
        // Failed 是终态（fail_task 注入）——不自动推进
        if t.status == ProviderStatus::Failed {
            return Ok(ProviderStatus::Failed);
        }
        // 延迟 Ready 窗：到期前恒 Downloading，到期 Ready（Bug B 复现能力）。
        // 不走 status_calls 自动推进，保证长等待窗内判定稳定不翻转。
        if let Some(ra) = t.ready_at {
            if std::time::Instant::now() < ra {
                t.status = ProviderStatus::Downloading;
                return Ok(t.status);
            }
            t.status = ProviderStatus::Ready;
            return Ok(t.status);
        }
        t.status = match t.status_calls {
            1 => ProviderStatus::Queued,
            2 => ProviderStatus::Downloading,
            _ => ProviderStatus::Ready,
        };
        Ok(t.status)
    }

    async fn resolve(&self, id: &ProviderTaskId) -> Result<Vec<ResolvedRemoteFile>, ProviderError> {
        let st = self.state.lock();
        let t = st.tasks.get(id).ok_or(ProviderError::NotFound)?;
        if t.status != ProviderStatus::Ready {
            return Err(ProviderError::Other("task not ready".into()));
        }
        // 首次 submit → files；resubmit → resubmit_files（后者优先）
        let base = if t.submit_seq >= 2 && !st.resubmit_files.is_empty() {
            &st.resubmit_files
        } else {
            &st.files
        };
        // update_urls → 替换 url（refresh 后的新直链，全新有效期）
        let out: Vec<ResolvedRemoteFile> = match &st.update_urls {
            Some(urls) if !urls.is_empty() && st.refreshed => base
                .iter()
                .enumerate()
                .map(|(i, f)| ResolvedRemoteFile {
                    url: urls.get(i).cloned().unwrap_or_else(|| f.url.clone()),
                    expires_at: None, // 刷新即续期
                    ..f.clone()
                })
                .collect(),
            _ => base.clone(),
        };
        Ok(out)
    }

    async fn remove(&self, id: &ProviderTaskId) -> Result<(), ProviderError> {
        let mut st = self.state.lock();
        st.tasks.remove(id).ok_or(ProviderError::NotFound)?;
        Ok(())
    }

    async fn refresh_links(
        &self,
        id: &ProviderTaskId,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        self.refresh_links_sync(id)
    }
}

/// 供 coordinator 区分"直链是否失效"的便捷判定（mock 也可用）。
pub(crate) fn any_expired(files: &[ResolvedRemoteFile], now: u64) -> bool {
    files.iter().any(|f| link_expired(f, now))
}

/// 解析 `SMARTDL_MOCK_READY_DELAY_SECS`（Bug B 复现环境变量）：非负整数秒；
/// 缺失/非法 → 0（旧行为：秒级 Ready）。独立纯函数便于单测（避免测试进程
/// 内改环境变量的并行竞态）。
pub fn parse_ready_delay_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0)
}

/// 从进程环境读取延迟 Ready 秒数（MockProvider::new 装配时调用一次）。
fn ready_delay_from_env() -> std::time::Duration {
    let v = std::env::var("SMARTDL_MOCK_READY_DELAY_SECS").ok();
    std::time::Duration::from_secs(parse_ready_delay_secs(v.as_deref()))
}

#[cfg(test)]
mod ready_delay_tests {
    use super::*;
    use crate::RemoteProvider;

    #[test]
    fn parse_ready_delay_env_semantics() {
        assert_eq!(parse_ready_delay_secs(None), 0, "缺省 = 0 = 旧行为");
        assert_eq!(parse_ready_delay_secs(Some("")), 0, "空串 = 旧行为");
        assert_eq!(parse_ready_delay_secs(Some("abc")), 0, "非法 = 旧行为");
        assert_eq!(parse_ready_delay_secs(Some("-5")), 0, "负数 = 旧行为");
        assert_eq!(parse_ready_delay_secs(Some("0")), 0, "显式 0 = 旧行为");
        assert_eq!(parse_ready_delay_secs(Some("240")), 240);
        assert_eq!(parse_ready_delay_secs(Some(" 30 ")), 30, "容忍空白");
    }

    #[tokio::test]
    async fn zero_delay_keeps_legacy_autopromote() {
        // with_ready_delay_secs(0) 显式等价缺省：status_calls 自动推进不变
        let mp = crate::mock::MockProvider::new("m").with_ready_delay_secs(0);
        use crate::types::ProviderStatus;
        let id = mp
            .submit(&smart_dl_core::types::DownloadSource::Http {
                url: "https://example.com/a".into(),
                headers: vec![],
                auth: None,
                backup_url: None,
            })
            .await
            .unwrap();
        assert_eq!(mp.status(&id).await.unwrap(), ProviderStatus::Queued);
        assert_eq!(mp.status(&id).await.unwrap(), ProviderStatus::Downloading);
        assert_eq!(mp.status(&id).await.unwrap(), ProviderStatus::Ready);
    }

    #[tokio::test]
    async fn delayed_ready_holds_downloading_until_deadline() {
        let mp = crate::mock::MockProvider::new("m").with_ready_delay_secs(1);
        use crate::types::ProviderStatus;
        let id = mp
            .submit(&smart_dl_core::types::DownloadSource::Http {
                url: "https://example.com/b".into(),
                headers: vec![],
                auth: None,
                backup_url: None,
            })
            .await
            .unwrap();
        // 窗内多次判定恒 Downloading（不随调用次数翻转）
        for _ in 0..3 {
            assert_eq!(
                mp.status(&id).await.unwrap(),
                ProviderStatus::Downloading,
                "延迟窗内应稳定 Downloading"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // 到期 → Ready，且 resolve 放行
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(mp.status(&id).await.unwrap(), ProviderStatus::Ready);
        assert!(mp.resolve(&id).await.is_ok(), "到期后 resolve 应可用");
    }
}
