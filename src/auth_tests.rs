use axum::http::{HeaderMap, HeaderValue};

use super::*;

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(*name, HeaderValue::from_static(value));
    }
    map
}

#[test]
fn accepts_bearer_and_cookie_tokens() {
    assert!(is_authorized(
        &headers(&[("authorization", "Bearer s3cret")]),
        "s3cret"
    ));
    assert!(is_authorized(
        &headers(&[("cookie", "theme=dark; synth_rc_token=s3cret")]),
        "s3cret"
    ));
}

#[test]
fn rejects_missing_and_wrong_tokens() {
    assert!(!is_authorized(&HeaderMap::new(), "s3cret"));
    assert!(!is_authorized(
        &headers(&[("authorization", "Bearer nope")]),
        "s3cret"
    ));
    assert!(!is_authorized(
        &headers(&[("authorization", "s3cret")]),
        "s3cret"
    ));
    assert!(!is_authorized(
        &headers(&[("cookie", "other=s3cret")]),
        "s3cret"
    ));
}

#[test]
fn constant_time_eq_matches_slice_equality() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
}

#[test]
fn origin_check_allows_native_and_same_origin_only() {
    assert!(is_same_origin_or_native(&headers(&[(
        "host",
        "rc.example"
    )])));
    assert!(is_same_origin_or_native(&headers(&[
        ("host", "rc.example"),
        ("origin", "https://rc.example"),
    ])));
    assert!(is_same_origin_or_native(&headers(&[
        ("host", "127.0.0.1:8787"),
        ("x-forwarded-host", "rc.example"),
        ("origin", "https://rc.example"),
    ])));
    assert!(!is_same_origin_or_native(&headers(&[
        ("host", "rc.example"),
        ("origin", "https://evil.example"),
    ])));
}

#[test]
fn generated_tokens_are_long_and_unique() {
    let a = generate_token();
    assert_eq!(a.len(), 64);
    assert_ne!(a, generate_token());
}
