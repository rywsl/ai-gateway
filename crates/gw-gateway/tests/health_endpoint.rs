use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn start_server() -> (u16, tokio::task::JoinHandle<anyhow::Result<()>>) {
    // Bind ephemerally to find a free port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // Release; the actual server will rebind on the same port.
    let h = tokio::spawn(gw_gateway::health::run_health_server(port));
    tokio::time::sleep(Duration::from_millis(50)).await;
    (port, h)
}

async fn fetch(port: u16, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = s.read(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn livez_still_works() {
    let (port, h) = start_server().await;
    let resp = fetch(port, "/livez").await;
    assert!(resp.starts_with("HTTP/1.1 200"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn admin_required_for_circuits() {
    let (port, h) = start_server().await;
    let resp = fetch(port, "/healthz/circuits").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn admin_required_for_key() {
    let (port, h) = start_server().await;
    let resp = fetch(port, "/healthz/key/some-id").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn admin_required_for_resilience() {
    let (port, h) = start_server().await;
    let resp = fetch(port, "/healthz/resilience").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}
