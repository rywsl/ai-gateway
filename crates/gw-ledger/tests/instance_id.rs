//! L0/L1/L2 instance_id resolver tests. Requires Redis (testcontainers/local).

use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, KeysInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};

async fn make_pool() -> RedisPool {
    let url =
        std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let cfg = RedisConfig::from_url(&url).expect("parse redis url");
    let pool = RedisPool::new(
        cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        1,
    )
    .expect("RedisPool");
    pool.init().await.expect("connect");
    pool
}

#[tokio::test]
#[ignore = "requires Redis (GW_TEST_REDIS_URL)"]
async fn cold_l2_generates_and_persists_uuid() {
    let pool = make_pool().await;
    let hostname = format!("host-cold-{}", std::process::id());
    let key = format!("gateway:instance:{}", hostname);
    let _: () = pool.del(&key).await.unwrap();
    // Clear env so we don't take L0 branch
    std::env::remove_var("GW_INSTANCE_ID");
    // Reset the OnceLock (test-only hook below)
    gw_ledger::instance_id::__reset_for_tests();

    let id = gw_ledger::instance_id::resolve(&pool, &hostname)
        .await
        .unwrap();
    assert!(
        uuid::Uuid::parse_str(&id).is_ok(),
        "expected UUID, got {}",
        id
    );
    let stored: String = pool.get(&key).await.unwrap();
    assert_eq!(stored, id);
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn l2_load_returns_existing() {
    let pool = make_pool().await;
    let hostname = format!("host-existing-{}", std::process::id());
    let key = format!("gateway:instance:{}", hostname);
    let _: () = pool
        .set(&key, "preloaded-instance-id", None, None, false)
        .await
        .unwrap();
    std::env::remove_var("GW_INSTANCE_ID");
    gw_ledger::instance_id::__reset_for_tests();

    let id = gw_ledger::instance_id::resolve(&pool, &hostname)
        .await
        .unwrap();
    assert_eq!(id, "preloaded-instance-id");
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn env_var_overrides_l2() {
    let pool = make_pool().await;
    let hostname = format!("host-env-{}", std::process::id());
    std::env::set_var("GW_INSTANCE_ID", "env-overridden");
    gw_ledger::instance_id::__reset_for_tests();

    let id = gw_ledger::instance_id::resolve(&pool, &hostname)
        .await
        .unwrap();
    assert_eq!(id, "env-overridden");
    let stored: String = pool
        .get::<String, _>(&format!("gateway:instance:{}", hostname))
        .await
        .unwrap();
    assert_eq!(stored, "env-overridden", "env L0 must sync into L2");
    // Cleanup
    std::env::remove_var("GW_INSTANCE_ID");
}

#[tokio::test]
async fn heartbeat_key_format() {
    let key = gw_ledger::instance_id::heartbeat_key("inst-x");
    assert_eq!(key, "gateway:instance:inst-x:heartbeat");
}
