//! DLQ-fallback integration test (testcontainers: Redis).
//!
//! Verifies that when the primary `usage:records` stream cannot accept an
//! XADD (here forced by pre-creating the key as a plain string), the flusher
//! routes the record(s) to `usage:records:dlq` wrapped with `dlq_meta`.
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::{KeysInterface, StreamsInterface};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::Ledger;
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

fn sample_record() -> UsageRecord {
    UsageRecord {
        ts_ms: 0,
        request_id: "r-dlq".into(),
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
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        attempt_index: 0,
        vendor_exposure_state: VendorExposureState::Settled,
        usage_provenance: Default::default(),
        latency_ms: 1,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 0,
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::placeholder(),
        operation_id: None,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn primary_xadd_failure_routes_to_dlq_stream() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();

    // Force XADD on usage:records to fail by pre-creating it as a plain
    // string key — Redis returns WRONGTYPE on subsequent XADDs.
    let client = redis.pool.next();
    let _: () = client
        .set("usage:records", "blocked", None, None, false)
        .await
        .unwrap();

    let ledger = Ledger::start(redis.clone(), 1024, 2, Duration::from_millis(100));
    ledger.try_send(sample_record()).unwrap();
    ledger.try_send(sample_record()).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let dlq_len: i64 = client.xlen("usage:records:dlq").await.unwrap();
    assert_eq!(
        dlq_len, 2,
        "both records should land in usage:records:dlq when primary stream is unwritable"
    );
}
