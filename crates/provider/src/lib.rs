//! 云兜底 Provider（M5 交付，设计 §13/D5/D10）：RemoteProvider trait + 运行态 +
//! MockProvider + FallbackCoordinator（M2 FallbackPolicy 集成）。
//! 选择 = enabled ∧ authenticated ∧ quota>0 ∧ !backoff ∧ 并发<limit（≤2，D24）；
//! 直链过期：update_sources(≤3) → resubmit(≤2) → 超限 Failed（计划 M5 link_expiry）。
//! 默认关（不自动烧配额）：只有显式传入已启用 Provider 才可选。

pub mod baidu;
pub mod coordinator;
pub mod mock;
pub mod quark;
pub mod types;
pub mod xunlei;

// B3-a：百度网盘分享免登录解析（dlink 直链待 B3-b 登录态）
// 命名说明：顶层 parse_share_link 已被 quark 占用，百度版别名导出
pub use baidu::share::parse_share_link as parse_baidu_share;
pub use baidu::{BaiduClient, BaiduError, BaiduShareFile, BaiduShareLink};
pub use coordinator::{FallbackCoordinator, FallbackOutcome, HttpSink, SinkError};
pub use mock::MockProvider;
// Task 5-d/T2：夸克网盘 Provider（分享链接 → 转存 → 直链）
pub use quark::{parse_share_link, QuarkProvider};
pub use types::{
    ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile,
};

use smart_dl_core::types::{Capability, DownloadSource};

/// 云 Provider（§13 契约）。
#[async_trait::async_trait]
pub trait RemoteProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Vec<Capability>;
    /// 运行态快照（D5）。
    fn runtime(&self) -> ProviderRuntime;
    /// 重新认证/刷新凭证。
    async fn refresh_auth(&self) -> Result<(), ProviderError>;
    /// 提交远程任务（把源交给云端缓存）。
    async fn submit(&self, source: &DownloadSource) -> Result<ProviderTaskId, ProviderError>;
    /// 任务状态（Queued→Downloading→Ready）。
    async fn status(&self, id: &ProviderTaskId) -> Result<ProviderStatus, ProviderError>;
    /// 直链解析（Ready 后）。
    async fn resolve(&self, id: &ProviderTaskId) -> Result<Vec<ResolvedRemoteFile>, ProviderError>;
    /// 取消/删除远程任务。
    async fn remove(&self, id: &ProviderTaskId) -> Result<(), ProviderError>;
    /// 直链失效时刷新出新 URL（None = 无新链接，交给 resubmit）。
    async fn refresh_links(
        &self,
        id: &ProviderTaskId,
    ) -> Result<Option<Vec<String>>, ProviderError>;

    /// 轻量级探活（默认假设存活；真实 Provider 按需重载）。
    async fn probe(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}
