//! Integration test for Task 3.6: Listener UDS round-trip.
//!
//! Wires up:
//!  1. A plain HTTP/1.1 mock upstream that echoes request headers as JSON.
//!  2. A `Listener` bound to a UDS socket path inside a tempdir.
//!  3. A hyper HTTP/1.1 client over a `UnixStream` that sends a request with
//!     `X-Egress-Target`, `X-Egress-Profile`, and several upstream headers.
//!
//! Asserts that the response is 200 OK and that the mock upstream received the
//! upstream headers verbatim (i.e., the Listener→Handler pipeline is intact).

mod common;
use common::mock_upstream;

use bytes::Bytes;
use gw_egress_impersonate::handle::EgressBind;
use gw_egress_impersonate::server::{Handler, Listener};
use http_body_util::{BodyExt, Empty};
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn uds_listener_accepts_and_round_trips() {
    // 1. Start plain-HTTP mock upstream.
    let upstream_addr = mock_upstream::start().await;
    let mock_port = upstream_addr.port();

    // 2. Bind Listener on a UDS socket in a tempdir.
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("egress.sock");

    // Build Handler with http:// scheme to reach the plain-HTTP mock.
    let handler = Arc::new(Handler::new_with_scheme(16, "http"));

    let listener = Listener::bind(EgressBind::Uds(sock_path.clone()), handler)
        .await
        .expect("Listener::bind should succeed");
    let handle = listener.spawn();

    // Give the listener task a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // 3. Connect a UnixStream to the socket and send an HTTP/1.1 request.
    let stream = tokio::net::UnixStream::connect(&sock_path)
        .await
        .expect("connect to UDS socket");
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake(io)
        .await
        .expect("HTTP/1.1 handshake over UDS");

    // Drive the connection in the background.
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let target = format!("127.0.0.1:{mock_port}");
    let req = Request::builder()
        .method("GET")
        .uri("/probe")
        .header("host", "localhost")
        .header("X-Egress-Target", &target)
        .header("X-Egress-Profile", "claude_code_chrome131")
        .header("User-Agent", "probe-ua")
        .header("Authorization", "Bearer x")
        .body(Empty::<Bytes>::new())
        .expect("valid request");

    let response = sender.send_request(req).await.expect("send request");

    // 4. Assert the response came back successfully.
    assert_eq!(
        response.status(),
        hyper::StatusCode::OK,
        "expected 200 from mock upstream via Listener"
    );

    // 5. Parse the echo body and check that upstream headers passed through.
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    let received: HashMap<String, String> =
        serde_json::from_slice(&body_bytes).expect("valid JSON echo from mock");

    assert_eq!(
        received.get("user-agent").map(String::as_str),
        Some("probe-ua"),
        "User-Agent must pass through to upstream; got: {received:?}"
    );
    assert_eq!(
        received.get("authorization").map(String::as_str),
        Some("Bearer x"),
        "Authorization must pass through to upstream; got: {received:?}"
    );
    // Internal egress control headers must NOT reach the upstream.
    assert!(
        !received.contains_key("x-egress-target"),
        "X-Egress-Target must be stripped; got: {received:?}"
    );
    assert!(
        !received.contains_key("x-egress-profile"),
        "X-Egress-Profile must be stripped; got: {received:?}"
    );

    // 6. Graceful shutdown.
    handle.shutdown().await;
}
