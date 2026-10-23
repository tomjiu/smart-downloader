//! TD-2 Step 3 最小契约 canary（`docs/design/ENGINE-TRAIT-SPLIT.md`）。
//!
//! 一个**只实现 9 个必需方法**的引擎必须能通过编译——若有人向
//! `DownloadEngine` 新增无默认体的必需方法，本文件在编译期即失败，
//! 阻止主 trait 面在无人察觉时膨胀（40 → 15 的收缩成果防回潮）。
//!
//! 契约纪律：
//! - 通用可选能力（限速/顺序下载/全局代理）= 主 trait 默认实现；
//! - BT 语义（26 方法）= `BtEngineExt`；HTTP 语义（任务级代理）= `HttpEngineExt`；
//! - 新引擎只实现它能诚实提供的能力面。

use async_trait::async_trait;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, EngineError, EngineKind, EngineStatus, EngineTaskId,
};

/// 除 9 个必需方法外零覆写——「最小引擎」的存在性证明。
struct MinimalEngine;

#[async_trait]
impl DownloadEngine for MinimalEngine {
    fn id(&self) -> &str {
        "minimal"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::Http
    }

    fn capabilities(&self) -> Vec<Capability> {
        Vec::new()
    }

    async fn add(&self, _task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn pause(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn resume(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn status(&self, _id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn remove(&self, _id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }
}

/// 最小引擎的扩展访问器为默认 `None`（未声明任何扩展面）。impl 块本身
/// 即门禁：新增必需方法会让本文件编译失败（core 测试无异步运行时，
/// async 默认方法的 `Unsupported` 形状由引擎侧测试矩阵覆盖）。
#[test]
fn minimal_engine_meets_contract_with_defaults() {
    let e = MinimalEngine;
    let engine: &dyn DownloadEngine = &e;
    assert!(engine.bt().is_none(), "未实现 BtEngineExt → bt() 默认 None");
    assert!(
        engine.http().is_none(),
        "未实现 HttpEngineExt → http() 默认 None"
    );
}
