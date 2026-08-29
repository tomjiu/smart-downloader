//! M2: Router 启发式路由（§10.2）：能力路由 + 热度阈值。

mod mocks;

use mocks::mock_engine::MockEngine;
use smart_dl_core::ownership::FallbackPolicy;
use smart_dl_core::registry::{EngineRegistry, RoutingError};
use smart_dl_core::router::{RouteDecision, Router};
use smart_dl_core::types::DownloadSource;
use std::sync::Arc;

fn router() -> Router {
    let mut reg = EngineRegistry::new();
    reg.register(Arc::new(MockEngine::bt())).unwrap();
    reg.register(Arc::new(MockEngine::http())).unwrap();
    reg.register(Arc::new(MockEngine::ftp())).unwrap();
    Router::new(reg, FallbackPolicy::default())
}

#[test]
fn cold_bt_routes_to_fallback() {
    let r = router();
    let d = r.route(
        &DownloadSource::Magnet("magnet:?xt=urn:btih:deadbeef".into()),
        2,
        0,
    );
    assert_eq!(d, RouteDecision::FallbackProvider);
}

#[test]
fn hot_bt_routes_to_bt_engine() {
    let r = router();
    let d = r.route(
        &DownloadSource::Magnet("magnet:?xt=urn:btih:deadbeef".into()),
        60,
        20,
    );
    assert_eq!(d, RouteDecision::Engine("bt".into()));
}

#[test]
fn warm_bt_routes_to_bt_engine() {
    let r = router();
    // score(20,3)=0.37 → Warm → BT（30s 无进展才兜底）
    let d = r.route(
        &DownloadSource::Magnet("magnet:?xt=urn:btih:deadbeef".into()),
        20,
        3,
    );
    assert_eq!(d, RouteDecision::Engine("bt".into()));
}

#[test]
fn http_routes_to_http_engine() {
    let r = router();
    let d = r.route(
        &DownloadSource::Http {
            url: "https://example.com/f.bin".into(),
            headers: vec![],
            auth: None,
            backup_url: None,
        },
        0,
        0,
    );
    assert_eq!(d, RouteDecision::Engine("http".into()));
}

#[test]
fn ed2k_routes_to_failed() {
    let r = router();
    let d = r.route(
        &DownloadSource::Ed2k("ed2k://|file|x|1|h=abc|/".into()),
        0,
        0,
    );
    assert!(matches!(d, RouteDecision::Failed(RoutingError::Unsupported(ref s)) if s == "ed2k"));
}

#[test]
fn unregistered_ftp_routes_to_feature_disabled() {
    let mut reg = EngineRegistry::new();
    reg.register(Arc::new(MockEngine::http())).unwrap();
    let r = Router::new(reg, FallbackPolicy::default());
    let d = r.route(
        &DownloadSource::Ftp {
            url: "ftp://example.com/f".into(),
            user: "u".into(),
            pass: "p".into(),
        },
        0,
        0,
    );
    assert!(matches!(d, RouteDecision::Failed(RoutingError::FeatureDisabled(ref s)) if s == "ftp"));
}
