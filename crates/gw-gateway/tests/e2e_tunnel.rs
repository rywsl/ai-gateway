//! Ignored owned tunnel e2e tests.
//!
//! Run with:
//! `cargo test -p gw-gateway --test e2e_tunnel -- --ignored --nocapture`

use bytes::Bytes;
use gw_proxy::tunnel_stack::{TunnelPeerError, TunnelStack};
use gw_tunnel_client::cnexus::pool::PoolConfig;
use gw_tunnel_client::cnexus::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};
use gw_tunnel_client::resolver::ProxyNodeView;
use gw_tunnel_client::safe_dns::{set_override, OverrideGuard, SafeDnsResolver};
use gw_tunnel_client::target_filter::{set_whitelist, ResetGuard};
use gw_tunnel_server::{spawn, TunnelServerConfig};
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn http_echo_upstream(path: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(
            request.starts_with(&format!("GET {path} HTTP/1.1")),
            "unexpected request: {request}"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nworld")
            .await
            .unwrap();
    });
    (port, task)
}

async fn client(addr: std::net::SocketAddr, token: Option<&str>) -> WsTransport {
    WsTransport::connect(TransportConfig {
        url: format!("ws://{addr}/").parse().unwrap(),
        auth_token: token.map(|s| SecretString::from(s.to_owned())),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        max_streams_per_conn: 8,
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "W8 owned tunnel e2e; opens local TCP listeners"]
async fn e2e_owned_server_roundtrips_http_bytes() {
    let _guard = ResetGuard::new();
    let (upstream_port, upstream_task) = http_echo_upstream("/hello").await;
    set_whitelist(vec![("127.0.0.1".to_owned(), upstream_port)]);
    let server = spawn(TunnelServerConfig {
        allowed_targets: vec![("127.0.0.1".to_owned(), upstream_port)],
        allow_private_targets: true,
        ..TunnelServerConfig::default()
    })
    .await
    .unwrap();

    let t = client(server.local_addr, None).await;
    let mut stream = t
        .open_stream(&format!("127.0.0.1:{upstream_port}"))
        .await
        .unwrap();
    stream
        .send_data(Bytes::from_static(
            b"GET /hello HTTP/1.1\r\nhost: example.test\r\n\r\n",
        ))
        .await
        .unwrap();

    let mut response = Vec::new();
    loop {
        match stream.recv().await.unwrap() {
            TunnelEvent::Data(bytes) => response.extend_from_slice(&bytes),
            TunnelEvent::Close => break,
            TunnelEvent::Error(msg) => panic!("unexpected tunnel error: {msg}"),
        }
    }
    let response = String::from_utf8_lossy(&response);
    assert!(response.contains("200 OK"));
    assert!(response.ends_with("world"));
    upstream_task.await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "W8 owned tunnel e2e; opens local TCP listeners"]
async fn e2e_tunnel_stack_reports_no_live_node() {
    let _guard = ResetGuard::new();
    let _dns_guard = OverrideGuard::new();
    set_override("tunnel-live.example", vec!["1.1.1.1".parse().unwrap()]);
    set_whitelist(vec![("tunnel-live.example".to_owned(), 443)]);
    let stack = TunnelStack::new(
        vec![ProxyNodeView {
            id: "node-no-live".to_owned(),
            ws_url: "ws://127.0.0.1:9/".parse().unwrap(),
            auth_token: None,
            proxy_url: None,
            enabled: true,
            latency_ms: 0,
            allowed_provider_ids: vec!["openai".to_owned()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        }],
        Arc::new(SafeDnsResolver::cloudflare_default()),
        PoolConfig::default(),
        TransportConfig::default(),
    );

    let err = stack
        .peer_for_node("node-no-live", "tunnel-live.example", 443)
        .await
        .expect_err("closed local port should not produce a live peer");
    assert!(matches!(err, TunnelPeerError::NoLive { .. }));
}

#[tokio::test]
#[ignore = "W8 owned tunnel e2e; opens local TCP listeners"]
async fn e2e_owned_server_rejects_target_denied() {
    let server = spawn(TunnelServerConfig {
        allowed_targets: vec![("example.com".to_owned(), 443)],
        allow_private_targets: true,
        ..TunnelServerConfig::default()
    })
    .await
    .unwrap();
    let t = client(server.local_addr, None).await;
    let mut stream = t.open_stream("127.0.0.1:9").await.unwrap();
    let err = stream.recv().await.expect_err("denied target should error");
    assert!(
        matches!(err, TunnelError::Remote(msg) if msg.contains("target") || msg.contains("denied") || msg.contains("blocked"))
    );
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "W8 owned tunnel e2e; opens local TCP listeners"]
async fn e2e_owned_server_rejects_auth_failure() {
    let server = spawn(TunnelServerConfig {
        auth_token: Some("secret".to_owned()),
        ..TunnelServerConfig::default()
    })
    .await
    .unwrap();
    let t = client(server.local_addr, Some("wrong")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!t.is_alive(), "transport should close after auth failure");
    server.shutdown().await;
}
