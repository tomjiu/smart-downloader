#![cfg(feature = "bt")]

//! TD-2 Step 3 验收（`docs/design/ENGINE-TRAIT-SPLIT.md`，bt feature：
//! 真 BtEngine + 真 HttpEngine）：扩展访问器接线（`bt()`/`http()` →
//! `Some`）与方法本体直达（BT/HTTP 语义方法的**唯一**入口在扩展 trait——
//! Step 2 双侧并存期的 UFCS 消歧已随主 trait 收缩消失，常规调用语法恢复）。

use smart_dl_btcore::dl_engine::BtEngine;
use smart_dl_core::types::{BtSessionPatch, DownloadEngine, EngineError};
use std::sync::Arc;

fn bt() -> BtEngine {
    BtEngine::new(
        std::env::temp_dir().as_path(),
        None,
        0,
        0,
        false,
        false,
        false,
        false,
        false,
        "allow",
        &[],
        0.0,
        0,
    )
    .expect("bt engine")
}

#[tokio::test]
async fn bt_engine_exposes_bt_ext_as_sole_face() {
    let engine: Arc<dyn DownloadEngine> = Arc::new(bt());
    // 能力询问：BT 引擎 → bt() Some（BtEngineExt 面可达）；http() None（默认）
    assert!(engine.bt().is_some(), "BtEngine 必须暴露 bt() 扩展面");
    assert!(engine.http().is_none(), "BtEngine 不应暴露 http() 扩展面");

    let ext = engine.bt().expect("bt ext");
    // 空补丁 = 全 None 不调整 → 三组 changed 均 false → 仅提交快照 → Ok。
    ext.apply_bt_session(BtSessionPatch::default())
        .await
        .expect("空补丁应 Ok（无调整即无副作用）");

    // 错误路径：不存在任务 → 引擎覆写同款 NotFound。
    let miss = ext.force_recheck(&"no-such-task".to_string()).await;
    assert!(
        matches!(miss, Err(EngineError::NotFound)),
        "扩展面错误路径（收缩后唯一入口）: {miss:?}"
    );
}

#[tokio::test]
async fn http_engine_exposes_http_ext_as_sole_face() {
    let engine: Arc<dyn DownloadEngine> =
        Arc::new(smart_dl_httpdl::HttpEngine::new(reqwest::Client::new()));
    assert!(engine.http().is_some(), "HttpEngine 必须暴露 http() 扩展面");
    assert!(engine.bt().is_none(), "HttpEngine 不应暴露 bt() 扩展面");

    let ext = engine.http().expect("http ext");
    let miss = ext.set_task_proxy(&"no-such-task".to_string(), None).await;
    assert!(
        matches!(miss, Err(EngineError::NotFound)),
        "扩展面错误路径（收缩后唯一入口）: {miss:?}"
    );
}
