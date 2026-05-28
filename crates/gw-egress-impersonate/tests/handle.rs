//! Integration test for Task 3.5: egress request Handler.
//!
//! The test wires up:
//!  1. A plain HTTP/1.1 mock upstream that echoes request headers as JSON.
//!  2. A `Handler` instance pointing at the mock via the `http` scheme
//!     (using `Handler::new_with_scheme("http")` — test-only constructor).
//!  3. A crafted hyper Request carrying X-Egress-Target, X-Egress-Profile,
//!     and upstream headers (Authorization, User-Agent).
//!
//! Asserts that internal headers are stripped before egress and that
//! upstream credential headers pass through verbatim.

mod common;
use common::mock_upstream;

use bytes::Bytes;
use gw_egress_impersonate::server::Handler;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use std::collections::HashMap;

#[tokio::test]
async fn handler_strips_internal_headers_and_forwards_upstream_headers() {
    // 1. Start mock upstream (plain HTTP).
    let upstream_addr = mock_upstream::start().await;

    // 2. Build Handler using http:// scheme so it can reach the plain-HTTP mock.
    let handler = Handler::new_with_scheme(16, "http");

    // 3. Build request with internal egress headers + upstream headers.
    let target = format!("{}:{}", upstream_addr.ip(), upstream_addr.port());
    let req = Request::builder()
        .method("GET")
        .uri("/echo")
        .header("X-Egress-Target", &target)
        .header("X-Egress-Profile", "claude_code_chrome131")
        // Upstream credential — must pass through.
        .header("Authorization", "Bearer test-token")
        // Upstream user-agent — must pass through.
        .header("User-Agent", "Mozilla/5.0 (forged)")
        .body(Full::new(Bytes::new()))
        .expect("valid request");

    // 4. Dispatch.
    let response = handler
        .handle(req)
        .await
        .expect("handle must not return Err");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "expected 200 from mock upstream"
    );

    // 5. Parse echo body.
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let recv: HashMap<String, String> =
        serde_json::from_slice(&body_bytes).expect("valid JSON echo");

    // Internal egress headers MUST be stripped before reaching the upstream.
    assert!(
        !recv.contains_key("x-egress-target"),
        "X-Egress-Target must be stripped; got: {recv:?}"
    );
    assert!(
        !recv.contains_key("x-egress-profile"),
        "X-Egress-Profile must be stripped; got: {recv:?}"
    );
    assert!(
        !recv.contains_key("x-egress-proxy"),
        "X-Egress-Proxy must be stripped; got: {recv:?}"
    );

    // Upstream credential headers MUST pass through unchanged.
    assert_eq!(
        recv.get("authorization").unwrap(),
        "Bearer test-token",
        "Authorization must pass through verbatim"
    );
    assert_eq!(
        recv.get("user-agent").unwrap(),
        "Mozilla/5.0 (forged)",
        "User-Agent must pass through verbatim"
    );
}

/// Missing X-Egress-Target → 400.
#[tokio::test]
async fn handler_returns_400_when_target_missing() {
    let handler = Handler::new(4);
    let req = Request::builder()
        .method("GET")
        .uri("/")
        .header("X-Egress-Profile", "openai_codex")
        .body(Full::new(Bytes::new()))
        .expect("valid request");

    let response = handler.handle(req).await.expect("no panic");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().contains_key("x-egress-error-class"));
}

/// Unknown X-Egress-Profile value → 400.
#[tokio::test]
async fn handler_returns_400_when_profile_unknown() {
    let handler = Handler::new(4);
    let req = Request::builder()
        .method("GET")
        .uri("/")
        .header("X-Egress-Target", "example.com:443")
        .header("X-Egress-Profile", "not_a_real_profile")
        .body(Full::new(Bytes::new()))
        .expect("valid request");

    let response = handler.handle(req).await.expect("no panic");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().contains_key("x-egress-error-class"));
}
