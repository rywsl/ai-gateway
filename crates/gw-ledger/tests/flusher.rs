//! Billing flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::{KeysInterface, StreamsInterface};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_ledger::Ledger;
use gw_ledger::billing::{BillingRecord, BillingStatus, UsageSource};
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn batch_flush_writes_billing_stream_and_decrs_balance() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let ledger = Ledger::start(redis.clone(), 1024, 2, Duration::from_millis(100));
    let rec = BillingRecord {
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
        status: BillingStatus::Ok,
        error_code: None,
        http_status: 200,
        input_tokens: 100,
        output_tokens: 50,
        cached_tokens: None,
        cost_micro: 1000,
        freeze_amount_micro: 1500,
        markup_product: 1.0,
        usage_source: UsageSource::Upstream,
        retry_count: 0,
        latency_ms: 100,
        ttft_ms: None,
        is_stream: false,
    };
    ledger.try_send(rec.clone()).unwrap();
    ledger.try_send(rec).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = redis.pool.next();
    let bal: i64 = client
        .get("counter:user:u1:balance_micro")
        .await
        .unwrap();
    assert_eq!(bal, -2000);
    let len: i64 = client.xlen("billing:records").await.unwrap();
    assert_eq!(len, 2);
}
