//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
#![cfg(test)]

use super::{ct_eq, DaemonState};
use std::sync::Arc;

#[test]
fn ct_eq_matches_equality_semantics() {
    assert!(ct_eq("abc", "abc"));
    assert!(ct_eq("", ""));
    assert!(!ct_eq("abc", "abd"));
    assert!(!ct_eq("abc", "abC"));
    assert!(!ct_eq("abc", "abcd"));
    assert!(!ct_eq("abcd", "abc"));
    assert!(!ct_eq("", "a"));
    // 高熵长 token 等价性
    let t = "a1B2c3D4e5F6g7H8";
    assert!(ct_eq(t, t));
    assert!(!ct_eq(t, "a1B2c3D4e5F6g7H9"));
}

#[test]
fn verify_http_token_end_to_end() {
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let st = DaemonState::new(Arc::new(engine), vec![]).with_http_token(Some("s3cret".to_string()));
    assert!(st.verify_http_token(Some("Bearer s3cret")));
    // 前缀大小写敏感（Bearer 规范）
    assert!(!st.verify_http_token(Some("bearer s3cret")));
    assert!(!st.verify_http_token(Some("Bearer s3cretX")));
    assert!(!st.verify_http_token(Some("Bearer ")));
    assert!(!st.verify_http_token(Some("Basic s3cret")));
    assert!(!st.verify_http_token(None));
}
