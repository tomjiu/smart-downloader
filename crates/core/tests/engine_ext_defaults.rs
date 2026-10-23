//! TD-2 Step 2 验收（`docs/design/ENGINE-TRAIT-SPLIT.md`）：扩展访问器
//! 默认 `None` 语义——未实现扩展的引擎（一切非 BT/HTTP 语义引擎，及
//! daemon 测试替身 FakeEngine / 本 MockEngine 形态）访问器必须返回
//! `None`，调用方 `engine.bt().ok_or(...)?` 即得到能力询问结果。

mod mocks;

use mocks::mock_engine::MockEngine;
use smart_dl_core::types::{Capability, DownloadEngine, EngineKind};

#[test]
fn default_accessors_are_none_for_non_extension_engines() {
    let mock = MockEngine::new("mock-ftp", EngineKind::Ftp, vec![Capability::Range]);
    let engine: &dyn DownloadEngine = &mock;
    assert!(
        engine.bt().is_none(),
        "非 BT 语义引擎 bt() 必须为 None（默认）"
    );
    assert!(
        engine.http().is_none(),
        "非 HTTP 语义引擎 http() 必须为 None（默认）"
    );
}

/// 能力询问形状预演：泛型调用方不持有具体类型，只经 `dyn` 面询问——
/// 这正是 Step 3 daemon 调用点迁移后的调用形状（kind 判断消失）。
#[test]
fn capability_probe_shape_via_dyn() {
    let mock = MockEngine::new("mock-http", EngineKind::Http, vec![Capability::Http]);
    let engines: Vec<&dyn DownloadEngine> = vec![&mock];
    let bt_count = engines.iter().filter(|e| e.bt().is_some()).count();
    let http_count = engines.iter().filter(|e| e.http().is_some()).count();
    // MockEngine 未实现扩展（Step 2 不要求测试替身接入）→ 双零。
    // 真 BtEngine/HttpEngine 的 Some 路径由 daemon/tests/engine_ext_accessors.rs
    // (bt feature 下真引擎) 覆盖。
    assert_eq!(bt_count, 0);
    assert_eq!(http_count, 0);
}
