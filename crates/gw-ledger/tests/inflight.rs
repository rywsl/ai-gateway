//! InflightLedger integration test. Requires Redis.

use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
use std::collections::HashMap;

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

fn test_registration(req_id: &str, attempt: u32) -> gw_ledger::inflight::InflightRegistration {
    gw_ledger::inflight::InflightRegistration {
        request_id: req_id.to_string(),
        attempt_index: attempt,
        user_id: "u-test".to_string(),
        vendor_key_id: "vk-test".to_string(),
        freeze_amount: 1234,
        prompt_estimate_tokens: 100,
    }
}

#[tokio::test]
#[ignore = "requires Redis (GW_TEST_REDIS_URL)"]
async fn register_writes_hash_with_expected_fields() {
    let pool = make_pool().await;
    let ledger = gw_ledger::inflight::InflightLedger::new(pool.clone(), "inst-test".to_string());
    let req = test_registration("r-reg-1", 0);
    ledger.register(&req).await.unwrap();
    let key = "billing:inflight:inst-test:r-reg-1:0";
    let hash: HashMap<String, String> = pool.hgetall(key).await.unwrap();
    assert_eq!(hash.get("state").map(String::as_str), Some("sent"));
    assert_eq!(hash.get("user_id").map(String::as_str), Some("u-test"));
    assert_eq!(
        hash.get("vendor_key_id").map(String::as_str),
        Some("vk-test")
    );
    assert_eq!(hash.get("freeze_amount").map(String::as_str), Some("1234"));
    assert_eq!(
        hash.get("prompt_estimate_tokens").map(String::as_str),
        Some("100")
    );
    assert!(hash.contains_key("ts_sent_ms"));
    let ttl: i64 = pool.ttl(key).await.unwrap();
    assert!(
        ttl > 86_000 && ttl <= 86_400,
        "expected ~24h TTL, got {}",
        ttl
    );
    // Cleanup
    let _: () = pool.del(&[key]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn clear_removes_key() {
    let pool = make_pool().await;
    let ledger = gw_ledger::inflight::InflightLedger::new(pool.clone(), "inst-test".to_string());
    let req = test_registration("r-clear", 0);
    ledger.register(&req).await.unwrap();
    ledger.clear("r-clear", 0).await.unwrap();
    let exists: bool = pool
        .exists("billing:inflight:inst-test:r-clear:0")
        .await
        .unwrap();
    assert!(!exists);
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn mark_settled_sets_state_and_ts() {
    let pool = make_pool().await;
    let ledger = gw_ledger::inflight::InflightLedger::new(pool.clone(), "inst-test".to_string());
    let req = test_registration("r-settled", 0);
    ledger.register(&req).await.unwrap();
    ledger
        .mark_settled("r-settled", 0, gw_ledger::inflight::SettleState::Settled)
        .await
        .unwrap();
    let hash: HashMap<String, String> = pool
        .hgetall::<HashMap<String, String>, _>("billing:inflight:inst-test:r-settled:0")
        .await
        .unwrap();
    assert_eq!(hash.get("state").map(String::as_str), Some("settled"));
    let ts: i64 = hash
        .get("ts_settled_ms")
        .expect("ts_settled_ms set")
        .parse()
        .unwrap();
    assert!(ts > 0);
    // Cleanup
    let _: () = pool
        .del(&["billing:inflight:inst-test:r-settled:0"])
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn mark_terminated_state_label_is_terminated() {
    let pool = make_pool().await;
    let ledger = gw_ledger::inflight::InflightLedger::new(pool.clone(), "inst-test".to_string());
    let req = test_registration("r-term", 0);
    ledger.register(&req).await.unwrap();
    ledger
        .mark_settled("r-term", 0, gw_ledger::inflight::SettleState::Terminated)
        .await
        .unwrap();
    let state: Option<String> = pool
        .hget("billing:inflight:inst-test:r-term:0", "state")
        .await
        .unwrap();
    assert_eq!(state.as_deref(), Some("terminated"));
    let _: () = pool
        .del(&["billing:inflight:inst-test:r-term:0"])
        .await
        .unwrap();
}

#[test]
fn settle_state_string_labels() {
    assert_eq!(
        gw_ledger::inflight::SettleState::Settled.as_str(),
        "settled"
    );
    assert_eq!(
        gw_ledger::inflight::SettleState::Terminated.as_str(),
        "terminated"
    );
}

#[test]
fn key_format() {
    // Verify key format without Redis
    let ledger = gw_ledger::inflight::InflightLedger::test_key("inst-1", "req-2", 3);
    assert_eq!(ledger, "billing:inflight:inst-1:req-2:3");
}
