//! Usage flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::StreamsInterface;
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::usage::{UsageRecord, UsageSource, UsageStatus};
use gw_ledger::Ledger;
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn batch_flush_writes_usage_stream_no_balance_decr() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let ledger = Ledger::start(redis.clone(), 1024, 2, Duration::from_millis(100));
    let rec = UsageRecord {
        ts_ms: 0,
        request_id: "r1".into(),
        trace_id: "t".into(),
        user_id: UserId::from("u1"),
        child_id: None,
        api_key_id: None,
        client_protocol: "openai".into(),
        upstream_protocol: "openai".into(),
        model: ModelName::from("gpt-4"),
        vendor_id: VendorId::from("v1"),
        vendor_key_id: KeyId::from("k1"),
        status: UsageStatus::Ok,
        error_code: None,
        http_status: 200,
        usage: UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        usage_source: UsageSource::Upstream,
        retry_count: 0,
        latency_ms: 100,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 1500,
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::placeholder(),
        operation_id: None,
    };
    ledger.try_send(rec.clone()).unwrap();
    ledger.try_send(rec).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = redis.pool.next();
    // (1) Stream xlen reflects both records.
    let len: i64 = client.xlen("usage:records").await.unwrap();
    assert_eq!(len, 2);
    // (2) Critical invariant: Rust must not have touched balance_micro.
    use fred::interfaces::KeysInterface;
    let bal: Option<i64> = client.get("counter:user:u1:balance_micro").await.unwrap();
    assert!(
        bal.is_none(),
        "Rust must not write counter:user:*:balance_micro"
    );
}
