#![cfg(feature = "bt")]

//! TD-2 Step 2 验收（`docs/design/ENGINE-TRAIT-SPLIT.md`，bt feature：
//! 真 BtEngine + 真 HttpEngine）：扩展访问器接线（`bt()`/`http()` → Some）
//! 与转发等价（经 `BtEngineExt`/`HttpEngineExt` 的调用到达主 trait 覆写的
//! 同一代码路径，行为逐字节不变）。

use smart_dl_btcore::dl_engine::BtEngine;
use smart_dl_core::types::{
    BtEngineExt, BtSessionPatch, DownloadEngine, EngineError, HttpEngineExt,
};
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
async fn bt_engine_exposes_bt_ext_and_forwards() {
    let engine: Arc<dyn DownloadEngine> = Arc::new(bt());
    // 能力询问：BT 引擎 → bt() Some（BtEngineExt 面可达）；http() None（默认）
    assert!(engine.bt().is_some(), "BtEngine 必须暴露 bt() 扩展面");
    assert!(engine.http().is_none(), "BtEngine 不应暴露 http() 扩展面");

    // 转发等价：经 BtEngineExt 调 apply_bt_session（空补丁 = 全 None 不调整
    // → 三组 changed 均 false → 仅提交快照 → Ok），与主 trait 覆写同一路径。
    // 注：Step 2 双侧并存期，同名方法在 dyn BtEngineExt 与其超 trait
    // DownloadEngine 上同时可见 → 方法调用语法歧义（E0034），须 UFCS 消歧；
    // Step 3 主 trait 收缩后同名方法唯一，常规调用语法恢复可用。
    let ext = engine.bt().expect("bt ext");
    BtEngineExt::apply_bt_session(ext, BtSessionPatch::default())
        .await
        .expect("空补丁应 Ok（无调整即无副作用）");

    // 转发等价（错误路径）：不存在任务 → 主 trait 覆写同款 NotFound。
    let miss = BtEngineExt::force_recheck(ext, &"no-such-task".to_string()).await;
    assert!(
        matches!(miss, Err(EngineError::NotFound)),
        "经扩展调用的错误路径应与主 trait 一致: {miss:?}"
    );
}

#[tokio::test]
async fn http_engine_exposes_http_ext_and_forwards() {
    let engine: Arc<dyn DownloadEngine> =
        Arc::new(smart_dl_httpdl::HttpEngine::new(reqwest::Client::new()));
    assert!(engine.http().is_some(), "HttpEngine 必须暴露 http() 扩展面");
    assert!(engine.bt().is_none(), "HttpEngine 不应暴露 bt() 扩展面");

    // 转发等价（错误路径）：不存在任务 set_task_proxy → 主 trait 覆写同款
    // NotFound。UFCS 消歧同上（Step 2 双侧并存期）。
    let ext = engine.http().expect("http ext");
    let miss = HttpEngineExt::set_task_proxy(ext, &"no-such-task".to_string(), None).await;
    assert!(
        matches!(miss, Err(EngineError::NotFound)),
        "经扩展调用的错误路径应与主 trait 一致: {miss:?}"
    );
}
