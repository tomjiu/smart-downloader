//! FallbackCoordinator（§10/§13 + M2 FallbackPolicy 集成）：
//! Provider 选择（enabled ∧ authenticated ∧ quota>0 ∧ !backoff ∧ 并发<limit）；
//! 自动兜底决策（BT 进度 + 策略 → Auto/RequiresPauseFirst/ManualOnly）；
//! 兜底编排（submit → Ready → resolve → HttpSink 传输；直链过期 → update_sources(≤3)
//! → resubmit(≤2) → 超限 Failed）。BT 半成品文件由会话层保留，coordinator 从不删除。

use crate::mock::any_expired;
use crate::types::{link_expired, now_unix, ProviderError, ProviderTaskId, ResolvedRemoteFile};
use crate::RemoteProvider;
use smart_dl_core::ownership::{decide_auto_fallback, FallbackDecision, FallbackPolicy};
use smart_dl_core::task::DownloadTask;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// 传输抽象（M5 直链 → HttpEngine；daemon/M6 注入真实引擎，测试注入 mock/真引擎适配）。
#[async_trait::async_trait]
pub trait HttpSink: Send + Sync {
    /// 用直链 URL 传输一个文件到 dest_root（任务级：内部可复用 HttpEngine 任务）。
    async fn transfer(
        &self,
        task_id: &str,
        url: &str,
        dest_root: PathBuf,
        name: Option<String>,
    ) -> Result<(), SinkError>;
    /// 直链失效时换源（≤3 次）。
    async fn update_sources(&self, task_id: &str, urls: Vec<String>) -> Result<(), SinkError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkError {
    /// 传输中直链过期。
    Expired,
    /// 传输失败。
    Failed(String),
}

/// 兜底结果。
#[derive(Clone, Debug, PartialEq)]
pub struct FallbackOutcome {
    pub provider: String,
    pub provider_task: ProviderTaskId,
    /// 已传输（落位）的文件 rel_path 列表。
    pub transferred: Vec<String>,
}

/// 兜底协调器：Provider 列表 + FallbackPolicy。
pub struct FallbackCoordinator {
    providers: Vec<Arc<dyn RemoteProvider>>,
    policy: FallbackPolicy,
}

impl FallbackCoordinator {
    pub fn new(providers: Vec<Arc<dyn RemoteProvider>>, policy: FallbackPolicy) -> Self {
        FallbackCoordinator { providers, policy }
    }

    /// 选择 provider：enabled ∧ authenticated ∧ quota>0 ∧ !backoff ∧ 并发<limit。
    pub fn select_provider(&self) -> Option<String> {
        let now = now_unix();
        self.providers
            .iter()
            .filter(|p| {
                let rt = p.runtime();
                rt.enabled
                    && rt.authenticated
                    && rt.quota_remaining > 0
                    && rt.backoff_until.map(|b| b <= now).unwrap_or(true)
                    && rt.busy < rt.concurrency_limit
            })
            .map(|p| p.name().to_string())
            .next()
    }

    /// 自动兜底决策（M2 策略冻结：<0.5 允许；禁双份默认 → RequiresPauseFirst）。
    pub fn decide(&self, bt_progress: f64) -> FallbackDecision {
        decide_auto_fallback(bt_progress, &self.policy)
    }

    /// 刷新所有 provider 认证。
    pub async fn refresh_auth_all(&self) {
        for p in &self.providers {
            let _ = p.refresh_auth().await;
        }
    }

    /// 兜底编排：决策检查 → 选 provider → submit → Ready → resolve →
    /// 传输（直链过期 → update_sources ≤3 → resubmit ≤2 → 超限 Failed）。
    ///
    /// 探活失败自动降级：依次尝试所有可用 provider，单个 provider 在
    /// submit/poll/resolve/handle_links 任一步失败时自动切换到下一个，
    /// 不阻塞主下载链路。
    pub async fn begin_fallback(
        &self,
        task: &DownloadTask,
        bt_progress: f64,
        bt_paused: bool,
        sink: &dyn HttpSink,
    ) -> Result<FallbackOutcome, ProviderError> {
        match self.decide(bt_progress) {
            FallbackDecision::ManualOnly => return Err(ProviderError::ManualOnly),
            FallbackDecision::RequiresPauseFirst if !bt_paused => {
                return Err(ProviderError::RequiresPause)
            }
            _ => {}
        }
        let mut last_err = None;
        let mut tried: HashSet<String> = HashSet::new();
        loop {
            let name = match self.select_provider() {
                Some(n) => n,
                None => break,
            };
            if tried.contains(&name) {
                break;
            }
            let provider = self
                .providers
                .iter()
                .find(|p| p.name() == name)
                .expect("select_provider 返回的 name 必须在列表中");
            match self
                .try_provider_fallback(task, bt_paused, sink, provider)
                .await
            {
                Ok(outcome) => return Ok(outcome),
                Err(e) => {
                    last_err = Some(e);
                    tried.insert(name);
                    // 已尝试过的 provider 不再重入，避免 MockProvider 等未设置 backoff 的
                    // 实现导致同一 provider 死循环。
                }
            }
        }
        Err(last_err.unwrap_or(ProviderError::NoProvider))
    }

    /// 单 provider 兜底编排（供 begin_fallback 重试循环调用）。
    async fn try_provider_fallback(
        &self,
        task: &DownloadTask,
        _bt_paused: bool,
        sink: &dyn HttpSink,
        provider: &Arc<dyn RemoteProvider>,
    ) -> Result<FallbackOutcome, ProviderError> {
        let mut ptid = provider.submit(&task.source).await?;
        poll_ready(provider, &mut ptid).await?;
        let files = provider.resolve(&ptid).await?;
        let transferred = self
            .handle_links(task, provider, &mut ptid, files, sink)
            .await?;
        Ok(FallbackOutcome {
            provider: provider.name().to_string(),
            provider_task: ptid,
            transferred,
        })
    }

    /// 传输 + 过期恢复（update_sources ≤3 → resubmit ≤2 → RetriesExhausted）。
    /// 返回已传输文件 rel_path 列表。
    async fn handle_links(
        &self,
        task: &DownloadTask,
        provider: &Arc<dyn RemoteProvider>,
        ptid: &mut ProviderTaskId,
        mut files: Vec<ResolvedRemoteFile>,
        sink: &dyn HttpSink,
    ) -> Result<Vec<String>, ProviderError> {
        let mut transferred: Vec<String> = Vec::new();
        let mut updates: u32 = 0;
        let mut resubmits: u32 = 0;

        loop {
            let now = now_unix();
            let files_ok = !files.iter().any(|f| link_expired(f, now));
            if files_ok {
                let mut all_ok = true;
                for f in &files {
                    let tid = format!("{}-{}", task.id, f.rel_path);
                    match sink
                        .transfer(
                            &tid,
                            &f.url,
                            task.dest_root.clone(),
                            Some(f.rel_path.clone()),
                        )
                        .await
                    {
                        Ok(()) => {
                            transferred.push(f.rel_path.clone())
                        }
                        // 传输中直链过期 → 进入恢复流
                        Err(SinkError::Expired) => {
                            all_ok = false;
                            break;
                        }
                        Err(SinkError::Failed(e)) => return Err(ProviderError::Other(e)),
                    }
                }
                if all_ok {
                    return Ok(transferred);
                }
            }

            // 直链失效恢复：先 update_sources（≤3）
            if updates < 3 {
                updates += 1;
                match provider.refresh_links(ptid).await {
                    Ok(Some(urls)) if !urls.is_empty() => {
                        sink.update_sources(&task.id, urls)
                            .await
                            .map_err(|e| match e {
                                SinkError::Failed(s) => ProviderError::Other(s),
                                SinkError::Expired => ProviderError::Expired,
                            })?;
                        files = provider.resolve(ptid).await?;
                        continue;
                    }
                    _ => {}
                }
            }

            // 仍失效 → resubmit（≤2）
            if resubmits < 2 {
                resubmits += 1;
                let _ = provider.remove(ptid).await;
                *ptid = provider.submit(&task.source).await?;
                poll_ready(provider, ptid).await?;
                files = provider.resolve(ptid).await?;
                continue;
            }

            return Err(ProviderError::RetriesExhausted);
        }
    }
}

/// 轮询 Provider 任务到 Ready（自动推进 Queued→Downloading→Ready）。
///
/// 真实云盘离线下载耗时秒级到分钟级：指数退避轮询（0.5s 起步，封顶 5s），
/// 总上限 10 分钟；MockProvider 秒级 Ready 不受影响。
async fn poll_ready(
    provider: &Arc<dyn RemoteProvider>,
    ptid: &mut ProviderTaskId,
) -> Result<(), ProviderError> {
    use crate::types::ProviderStatus;
    const TOTAL_CAP: std::time::Duration = std::time::Duration::from_secs(600);
    let started = std::time::Instant::now();
    let mut interval = std::time::Duration::from_millis(500);
    let mut next_probe = std::time::Instant::now();
    let mut last_verdict: Option<std::string::String> = None;
    while std::time::Instant::now() < started + TOTAL_CAP {
        if std::time::Instant::now() >= next_probe {
            match provider.status(ptid).await? {
                ProviderStatus::Ready => {
                    return Ok(());
                }
                ProviderStatus::Failed => {
                    return Err(ProviderError::Other("provider task failed".into()))
                }
                other => {
                    let v = format!("{other:?}");
                    if last_verdict.as_deref() != Some(v.as_str()) {
                        last_verdict = Some(v);
                    }
                }
            }
            next_probe = std::time::Instant::now() + interval;
            interval = (interval * 2).min(std::time::Duration::from_secs(5));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(ProviderError::Other(
        "provider task not ready (offline download still in progress?)".into(),
    ))
}

/// 供测试/外部检查直链有效性的便捷函数。
pub fn files_all_valid(files: &[ResolvedRemoteFile], now: u64) -> bool {
    !any_expired(files, now)
}
