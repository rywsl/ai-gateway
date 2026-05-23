use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Best-effort Redis connect for the health-endpoint integration tests. The
/// tests do not exercise the blacklist itself — they only assert the
/// 401-blanket admin response — but T3-Pre extended `run_health_server` to
/// take an `Arc<RedisJwtBlacklist>` so a real handle has to be constructed.
/// When Redis is unreachable (the default `cargo test --workspace` flow on a
/// host without docker), the helper returns `None` and the test caller
/// skips with `#[ignore]` semantics by short-circuiting.
async fn start_server() -> Option<(u16, tokio::task::JoinHandle<anyhow::Result<()>>)> {
    let redis_url =
        std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let redis = match gw_store::redis_client::RedisCtx::connect(&redis_url, 2).await {
        Ok(r) => r,
        Err(_) => return None,
    };

    // Bind ephemerally to find a free port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // Release; the actual server will rebind on the same port.

    let store: Arc<gw_store::EntityStore> = Arc::new(gw_store::EntityStore::new());
    let health: Arc<gw_health::KeyHealthStore> = Arc::new(gw_health::KeyHealthStore::default());
    let resilience: Arc<gw_proxy::resilience::ResilienceState> =
        Arc::new(gw_proxy::resilience::ResilienceState::default());
    let mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist> = Arc::new(
        gw_auth::jwt_blacklist::RedisJwtBlacklist::new((*redis.pool).clone()),
    );

    let state = gw_gateway::health::HealthState {
        key_health: health,
        resilience,
    };
    let h = tokio::spawn(gw_gateway::health::run_health_server(
        port,
        state,
        store,
        mgmt_blacklist,
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    Some((port, h))
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
#[ignore = "needs redis; T3-Pre widened the server signature"]
async fn livez_still_works() {
    let Some((port, h)) = start_server().await else {
        return;
    };
    let resp = fetch(port, "/livez").await;
    assert!(resp.starts_with("HTTP/1.1 200"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs redis; T3-Pre widened the server signature"]
async fn admin_required_for_circuits() {
    let Some((port, h)) = start_server().await else {
        return;
    };
    let resp = fetch(port, "/healthz/circuits").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs redis; T3-Pre widened the server signature"]
async fn admin_required_for_key() {
    let Some((port, h)) = start_server().await else {
        return;
    };
    let resp = fetch(port, "/healthz/key/some-id").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs redis; T3-Pre widened the server signature"]
async fn admin_required_for_resilience() {
    let Some((port, h)) = start_server().await else {
        return;
    };
    let resp = fetch(port, "/healthz/resilience").await;
    assert!(resp.starts_with("HTTP/1.1 401"));
    h.abort();
}
