//! Stream consumer applies entity events to EntityStore (invalidate-on-event).
//! Requires Docker (testcontainers Redis).

use fred::interfaces::StreamsInterface;
use fred::types::XID;
use gw_core::entity::{UserEntry, UserStatus};
use gw_core::ids::UserId;
use gw_core::versioned::Versioned;
use gw_store::redis_client::RedisCtx;
use gw_store::stream_consumer::StreamConsumer;
use gw_store::EntityStore;
use std::sync::Arc;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn entity_event_invalidates_cache_entry() {
    let redis_node = Redis::default().start().await.unwrap();
    let redis_port = redis_node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
        .await
        .unwrap();

    let store = Arc::new(EntityStore::new());
    let uid = UserId::from("u-evt-1");
    store
        .user
        .insert(
            uid.clone(),
            Arc::new(Versioned::new(
                1,
                UserEntry {
                    user_id: uid.clone(),
                    parent_id: None,
                    tier: "free".into(),
                    status: UserStatus::Active,
                    allowed_models: vec![],
                    allowed_channels: vec![],
                    allowed_groups: vec![],
                    rpm_limit: 0,
                    tpm_limit: 0,
                    max_concurrent_streams: 100,
                    daily_quota_tokens: 0,
                    user_group_ids: vec![],
                    markup: 1.0,
                    debug_enabled: false,
                },
            )),
        )
        .await;
    assert!(store.user.get(&uid).await.is_some(), "seed");

    let consumer = Arc::new(StreamConsumer {
        redis: redis.clone(),
        store: store.clone(),
        instance_id: "test-instance".into(),
    });
    let _h = consumer.clone().start();

    // XADD an event to events:entity announcing user u-evt-1 changed.
    let client = redis.pool.next();
    let _: String = client
        .xadd(
            "events:entity",
            false,
            None::<()>,
            XID::Auto,
            vec![
                ("ns".to_string(), "user".to_string()),
                ("id".to_string(), "u-evt-1".to_string()),
                ("v".to_string(), "2".to_string()),
            ],
        )
        .await
        .unwrap();

    // Allow the consumer one poll cycle (default 200ms block); poll up to 2s.
    for _ in 0..20 {
        if store.user.get(&uid).await.is_none() {
            return; // PASS
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("stream consumer did not invalidate entity:user:u-evt-1 within 2s");
}
