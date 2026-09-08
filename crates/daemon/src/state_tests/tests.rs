//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
#![cfg(test)]

use super::canonical_http_url;

#[test]
fn strips_token_param_keeps_others() {
    let c = canonical_http_url("https://host/a?token=abc&x=1&y=2");
    assert_eq!(c, "https://host/a?x=1&y=2");
}

#[test]
fn strips_cloud_signing_family() {
    let c = canonical_http_url(
        "https://host/a?X-Amz-Signature=deadbeef&X-Amz-Date=20260101&sig=zz&expires=999999",
    );
    assert_eq!(c, "https://host/a");
}

#[test]
fn no_token_url_unchanged() {
    let raw = "https://host/a?x=1";
    assert_eq!(canonical_http_url(raw), raw);
}

#[test]
fn only_token_difference_collides() {
    let a = canonical_http_url("https://host/f?token=aaa&v=1");
    let b = canonical_http_url("https://host/f?v=1&token=bbb");
    assert_eq!(a, b);
}

#[test]
fn invalid_url_passthrough() {
    assert_eq!(canonical_http_url("not a url"), "not a url");
}

#[test]
fn fragment_and_path_unaffected() {
    let c = canonical_http_url("https://host/dir/file.bin?token=x&keep=1#frag");
    assert_eq!(c, "https://host/dir/file.bin?keep=1#frag");
}
