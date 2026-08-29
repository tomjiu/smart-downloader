//! M2: CanonicalId / ContentIdentity 身份模型（§7 + D33/D34）。
//! serde 往返；v1 仅 InfoHash/SingleFile 两态。

use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity, Validator};

#[test]
fn content_identity_infohash_serde_roundtrip() {
    let ci = ContentIdentity::InfoHash([0xAA; 20]);
    let json = serde_json::to_string(&ci).unwrap();
    let back: ContentIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(ci, back);
}

#[test]
fn content_identity_single_file_serde_roundtrip() {
    let ci = ContentIdentity::SingleFile {
        size: 12345,
        etag: Some("\"abc\"".into()),
        sha256: None,
        backup_md5: None,
    };
    let json = serde_json::to_string(&ci).unwrap();
    let back: ContentIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(ci, back);
}

#[test]
fn canonical_id_serde_roundtrip() {
    let c = CanonicalId {
        kind: CanonicalKind::Http,
        identity: "https://a.com/f?v=1".into(),
        validator: Some(Validator::SizeAndEtag(9, "\"x\"".into())),
        token_sensitive: false,
    };
    let json = serde_json::to_string(&c).unwrap();
    let back: CanonicalId = serde_json::from_str(&json).unwrap();
    assert_eq!(c, back);
    // 旧数据缺 token_sensitive 字段 → default false 兼容
    let legacy = r#"{"kind":"Http","identity":"i","validator":null}"#;
    let back: CanonicalId = serde_json::from_str(legacy).unwrap();
    assert!(!back.token_sensitive);
}

#[test]
fn validator_serde_roundtrip() {
    for v in [Validator::Size(1), Validator::SizeAndEtag(2, "e".into())] {
        let json = serde_json::to_string(&v).unwrap();
        let back: Validator = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}

#[test]
fn canonical_id_distinguishes_kind_and_identity() {
    let bt = CanonicalId {
        kind: CanonicalKind::Bt,
        identity: "abc".into(),
        validator: None,
        token_sensitive: false,
    };
    let tf = CanonicalId {
        kind: CanonicalKind::TorrentFile,
        identity: "abc".into(),
        validator: None,
        token_sensitive: false,
    };
    assert_ne!(bt, tf, "不同 kind 同 identity 不应相等");
}
