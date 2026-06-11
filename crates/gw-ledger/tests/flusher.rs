//! Usage flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::StreamsInterface;
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::flusher::LedgerItem;
use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::Ledger;
use gw_runtime::ShutdownToken;
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

fn make_record() -> UsageRecord {
    UsageRecord {
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
        attempt_index: 0,
        vendor_exposure_state: VendorExposureState::Settled,
        usage_provenance: Default::default(),
        latency_ms: 100,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 1500,
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        operation_id: None,
        pod_id: String::new(),
        credits_consumed: None,
        coef_table_version: None,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn batch_flush_writes_usage_stream_no_balance_decr() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        "test-instance".to_string(),
        1024,
        2,
        Duration::from_millis(100),
        ShutdownToken::new(),
        Duration::from_secs(5),
    );
    let rec = make_record();
    ledger
        .try_send(LedgerItem {
            record: rec.clone(),
            clear: None,
        })
        .unwrap();
    ledger
        .try_send(LedgerItem {
            record: rec,
            clear: None,
        })
        .unwrap();
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

/// Must-deliver bypass: when the mpsc channel is full, `emit` writes via
/// synchronous XADD rather than dropping. Requires Docker (testcontainers Redis).
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn emit_bypasses_to_sync_xadd_when_channel_full() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();

    // capacity=1 so the channel fills after the first record.
    // The flusher worker has a 60-second flush interval — it won't drain
    // before we overflow the channel with rapid try_send calls.
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        "test-instance".to_string(),
        1,
        100,
        Duration::from_secs(60),
        ShutdownToken::new(),
        Duration::from_secs(5),
    );

    // Fill the channel: first try_send may succeed (capacity=1), subsequent
    // ones will hit Full. We don't care about success/failure here — we just
    // want the channel to be at capacity when emit is called.
    for _ in 0..5 {
        let _ = ledger.try_send(LedgerItem {
            record: make_record(),
            clear: None,
        });
    }

    // emit() must not drop the record: it should either enqueue it or bypass
    // via sync XADD. Give the channel a moment to drain before checking.
    ledger
        .emit(LedgerItem {
            record: make_record(),
            clear: None,
        })
        .await;

    // Allow the batch flusher a moment to drain whatever was queued normally.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify at least one record reached usage:records.
    let client = redis.pool.next();
    let len: i64 = client.xlen("usage:records").await.unwrap();
    assert!(
        len >= 1,
        "emit must deliver at least one record to usage:records; got {len}"
    );
}
