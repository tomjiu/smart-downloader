//! M2: 路由矩阵（用户指定 4 例 + 边界）。
//! Magnet/Torrent→bt；Http→http；Ftp→ftp（未注册 → FeatureDisabled）；Ed2k→Failed。

mod mocks;

use base64::Engine;
use mocks::mock_engine::MockEngine;
use smart_dl_core::registry::{EngineRegistry, RoutingError};
use smart_dl_core::types::DownloadSource;
use std::sync::Arc;

fn registry_with(bt: MockEngine, http: MockEngine, ftp: Option<MockEngine>) -> EngineRegistry {
    let mut reg = EngineRegistry::new();
    reg.register(Arc::new(bt)).unwrap();
    reg.register(Arc::new(http)).unwrap();
    if let Some(f) = ftp {
        reg.register(Arc::new(f)).unwrap();
    }
    reg
}

#[test]
fn magnet_routes_to_bt() {
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    assert_eq!(
        reg.select(&DownloadSource::Magnet("magnet:?xt=urn:btih:abc".into()))
            .unwrap(),
        "bt"
    );
}

#[test]
fn torrent_file_routes_to_bt() {
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    assert_eq!(
        reg.select(&DownloadSource::TorrentFile(
            b"d4:infod4:name1:ad12:piece lengthi1e5:pieces1:ae".to_vec()
        ))
        .unwrap(),
        "bt"
    );
}

#[test]
fn http_routes_to_http() {
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    assert_eq!(
        reg.select(&DownloadSource::Http {
            url: "https://example.com/f.bin".into(),
            headers: vec![],
            auth: None,
            backup_url: None,
        })
        .unwrap(),
        "http"
    );
}

#[test]
fn thunder_routes_to_http_after_decode() {
    // thunder:// = base64("AA" + url + "ZZ")
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    let inner = b"AAhttps://example.com/f.binZZ";
    let thunder = format!(
        "thunder://{}",
        base64::engine::general_purpose::STANDARD.encode(inner)
    );
    assert_eq!(
        reg.select(&DownloadSource::Thunder(thunder)).unwrap(),
        "http"
    );
}

#[test]
fn ftp_routes_to_ftp_when_registered() {
    let reg = registry_with(
        MockEngine::bt(),
        MockEngine::http(),
        Some(MockEngine::ftp()),
    );
    assert_eq!(
        reg.select(&DownloadSource::Ftp {
            url: "ftp://example.com/f.bin".into(),
            user: "u".into(),
            pass: "p".into(),
        })
        .unwrap(),
        "ftp"
    );
}

#[test]
fn ftp_routes_to_feature_disabled_when_missing() {
    // feature 关闭（ftp 引擎未注册）→ 路由失败
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    let r = reg.select(&DownloadSource::Ftp {
        url: "ftp://example.com/f.bin".into(),
        user: "u".into(),
        pass: "p".into(),
    });
    assert!(matches!(r, Err(RoutingError::FeatureDisabled(_))));
}

#[test]
fn ed2k_routes_to_failed() {
    let reg = registry_with(MockEngine::bt(), MockEngine::http(), None);
    let r = reg.select(&DownloadSource::Ed2k("ed2k://|file|x|1|h=abc|/".into()));
    assert!(matches!(r, Err(RoutingError::Unsupported(ref s)) if s == "ed2k"));
}

#[test]
fn no_engine_at_all_is_no_engine_error() {
    let reg = EngineRegistry::new();
    let r = reg.select(&DownloadSource::Http {
        url: "https://example.com/f.bin".into(),
        headers: vec![],
        auth: None,
        backup_url: None,
    });
    assert!(matches!(r, Err(RoutingError::NoEngineForSource)));
}

#[test]
fn duplicate_registration_is_rejected() {
    let mut reg = EngineRegistry::new();
    let bt = Arc::new(MockEngine::bt());
    reg.register(bt.clone()).unwrap();
    let r = reg.register(Arc::new(MockEngine::bt()));
    assert!(r.is_err(), "重复 id 注册应被拒绝");
}
