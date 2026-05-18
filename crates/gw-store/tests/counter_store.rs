//! CounterStore integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without Docker.

use gw_store::counter_store::CounterStore;
use gw_store::redis_client::RedisCtx;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn incrby_returns_new_value() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 2)
        .await
        .unwrap();
    let cs = CounterStore { redis };
    let v1 = cs.incrby("user", "u1", "balance_micro", 1000).await.unwrap();
    assert_eq!(v1, 1000);
    let v2 = cs.incrby("user", "u1", "balance_micro", -300).await.unwrap();
    assert_eq!(v2, 700);
    let g = cs.get("user", "u1", "balance_micro").await.unwrap();
    assert_eq!(g, 700);
}
