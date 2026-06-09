use bytes::Bytes;
use gw_tunnel_client::target_filter::{set_whitelist, ResetGuard};
use gw_tunnel_client::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};
use gw_tunnel_server::{spawn, TunnelServerConfig};
use secrecy::SecretString;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn http_echo_upstream() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(
            String::from_utf8_lossy(&buf[..n]).starts_with("GET /hello HTTP/1.1"),
            "unexpected request: {}",
            String::from_utf8_lossy(&buf[..n])
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
async fn owned_server_roundtrips_http_bytes() {
    let _guard = ResetGuard::new();
    let (upstream_port, upstream_task) = http_echo_upstream().await;
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
            TunnelEvent::Error(_) => panic!("unexpected error event"),
        }
    }
    assert!(String::from_utf8_lossy(&response).contains("200 OK"));
    assert!(String::from_utf8_lossy(&response).ends_with("world"));
    upstream_task.await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn owned_server_accepts_matching_auth() {
    let (upstream_port, upstream_task) = http_echo_upstream().await;
    let server = spawn(TunnelServerConfig {
        auth_token: Some("secret".to_owned()),
        allowed_targets: vec![("127.0.0.1".to_owned(), upstream_port)],
        allow_private_targets: true,
        ..TunnelServerConfig::default()
    })
    .await
    .unwrap();
    let t = client(server.local_addr, Some("secret")).await;
    let mut stream = t
        .open_stream(&format!("127.0.0.1:{upstream_port}"))
        .await
        .unwrap();
    stream
        .send_data(Bytes::from_static(
            b"GET /hello HTTP/1.1\r\nhost: x\r\n\r\n",
        ))
        .await
        .unwrap();
    assert!(matches!(stream.recv().await.unwrap(), TunnelEvent::Data(_)));
    upstream_task.abort();
    server.shutdown().await;
}

#[tokio::test]
async fn owned_server_rejects_bad_auth() {
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

#[tokio::test]
async fn owned_server_rejects_target_denied_without_tcp_connect() {
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
        matches!(err, TunnelError::Remote(msg) if msg.contains("target") || msg.contains("blocked"))
    );
    server.shutdown().await;
}

#[tokio::test]
async fn owned_server_enforces_stream_cap() {
    let server = spawn(TunnelServerConfig {
        max_streams_per_conn: 1,
        allowed_targets: vec![("198.51.100.10".to_owned(), 443)],
        allow_private_targets: true,
        ..TunnelServerConfig::default()
    })
    .await
    .unwrap();
    let t = client(server.local_addr, None).await;
    let _first = t.open_stream("198.51.100.10:443").await.unwrap();
    let mut second = t.open_stream("198.51.100.10:443").await.unwrap();
    let err = second.recv().await.expect_err("stream cap should error");
    assert!(matches!(err, TunnelError::Remote(msg) if msg.contains("stream cap")));
    server.shutdown().await;
}
