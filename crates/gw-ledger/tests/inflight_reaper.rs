//! T0 emit-lag watchdog tests. Requires Redis.

use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};

async fn make_pool() -> RedisPool {
    let url =
        std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let cfg = RedisConfig::from_url(&url).expect("parse url");
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
#[ignore = "requires Redis"]
async fn reaper_fires_metric_on_stale_settled_key() {
    let pool = make_pool().await;
    let inst = format!("inst-reaper-{}", std::process::id());
    let key = format!("billing:inflight:{}:req-stale:0", inst);
    // Pre-write a key with ts_settled_ms 90s ago
    let stale_ts =
        (time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000) - 90_000;
    let fields: Vec<(&str, String)> = vec![
        ("state", "settled".to_string()),
        ("ts_settled_ms", stale_ts.to_string()),
    ];
    let _: () = pool.hset(&key, fields).await.unwrap();
    let _: () = pool.expire(&key, 86_400).await.unwrap();

    let count = gw_ledger::inflight_reaper::run_once(&pool, &inst, 60)
        .await
        .expect("run_once");
    assert_eq!(count, 1, "expected 1 stale key detected");

    // Key MUST remain (forensics requirement per spec §5.4.2)
    let exists: bool = pool.exists(&key).await.unwrap();
    assert!(exists, "reaper must not delete the key");

    // Cleanup
    let _: () = pool.del(&[key]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn reaper_skips_unsettled_keys() {
    let pool = make_pool().await;
    let inst = format!("inst-reaper-unset-{}", std::process::id());
    let key = format!("billing:inflight:{}:req-active:0", inst);
    // Key with only ts_sent_ms — not yet settled
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000 - 90_000;
    let fields: Vec<(&str, String)> = vec![
        ("state", "sent".to_string()),
        ("ts_sent_ms", now_ms.to_string()),
    ];
    let _: () = pool.hset(&key, fields).await.unwrap();
    let _: () = pool.expire(&key, 86_400).await.unwrap();

    let count = gw_ledger::inflight_reaper::run_once(&pool, &inst, 60)
        .await
        .expect("run_once");
    assert_eq!(count, 0, "unsettled keys must not trigger T0");

    let _: () = pool.del(&[key]).await.unwrap();
}
