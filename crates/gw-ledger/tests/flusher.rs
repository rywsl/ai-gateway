//! Usage flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::{KeysInterface, StreamsInterface};
use fred::types::{RedisValue, XReadResponse};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::flusher::{ClearKey, LedgerItem};
use gw_ledger::inflight::{InflightLedger, InflightRegistration};
use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::Ledger;
use gw_runtime::spill_io::{SpillRoot, SpillRootConfig};
use gw_runtime::ShutdownToken;
use gw_store::redis_client::RedisCtx;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

fn make_spill_root() -> (TempDir, Arc<SpillRoot>) {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 16 * 1024 * 1024,
        buffer_size: 4096,
    }));
    (tmp, root)
}

const DLQ_SHARDS: usize = 4;

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
        attempted_keys: Vec::new(),
    }
}

fn make_item(req_id: &str) -> LedgerItem {
    let mut record = make_record();
    record.request_id = req_id.to_string();
    LedgerItem {
        record,
        clear: None,
    }
}

async fn read_usage_records(redis: &RedisCtx) -> Vec<UsageRecord> {
    let client = redis.pool.next();
    let raw: RedisValue = client
        .xread(None::<u64>, None::<u64>, "usage:records", "0-0")
        .await
        .unwrap();
    let response: XReadResponse<String, String, String, String> =
        raw.into_xread_response().unwrap();
    response
        .into_iter()
        .flat_map(|(_stream, entries)| entries.into_iter())
        .filter_map(|(_entry_id, fields)| {
            fields
                .get("payload")
                .and_then(|payload| serde_json::from_str::<UsageRecord>(payload).ok())
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn batch_flush_writes_usage_stream_no_balance_decr() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let (_tmp, spill_root) = make_spill_root();
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        "test-instance".to_string(),
        1024,
        2,
        Duration::from_millis(100),
        spill_root,
        DLQ_SHARDS,
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

/// Must-deliver overflow: when the primary mpsc channel is full, `emit`
/// routes through the non-blocking primary overflow worker rather than
/// dropping or performing synchronous XADD on the request tail.
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn emit_routes_to_overflow_when_channel_full() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();

    let (_tmp, spill_root) = make_spill_root();
    let (ledger, _primary_rx_held, _overflow_handle) = Ledger::for_test_with_overflow(
        redis.clone(),
        "test-instance".to_string(),
        1,
        64,
        spill_root,
        std::num::NonZeroUsize::new(DLQ_SHARDS).unwrap(),
        ShutdownToken::new(),
    );

    ledger.emit(make_item("req-fill")).await;
    ledger.emit(make_item("req-overflow")).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let records = read_usage_records(&redis).await;
    assert!(
        records.iter().any(|r| r.request_id == "req-overflow"),
        "overflow record must reach usage:records; got {:?}",
        records.iter().map(|r| &r.request_id).collect::<Vec<_>>()
    );
}

/// Overflow-full fallback: when both the primary channel and the overflow
/// channel cannot accept a record, `emit` returns without awaiting capacity
/// and the record is routed through the DLQ / emergency-disk / backlog
/// cascade. The inflight key for the targeted record is NOT deleted (no
/// `usage:records` XADD occurred).
///
/// Saturation strategy: deterministic channel closure. We cancel the
/// shutdown token before emitting and await the overflow worker's exit so
/// that `overflow_rx` is dropped — at that point `primary_overflow.try_enqueue`
/// returns `Closed` for every subsequent emit. This is functionally
/// equivalent to the overflow-full branch for the must-deliver guarantee
/// (both end in `route_ledger_item_to_dlq` with the inflight key preserved),
/// and avoids the nondeterminism of racing the worker's 50 ms flush tick
/// with back-to-back `emit` calls at `overflow_capacity = 1`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn overflow_full_routes_to_dlq_without_del() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-overflow-full".to_string();

    // 1. Register an inflight key for the targeted record up front.
    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    inflight
        .register(&InflightRegistration {
            request_id: "req-overdrop".to_string(),
            attempt_index: 0,
            user_id: "u".to_string(),
            vendor_key_id: "vk".to_string(),
            freeze_amount: 0,
            prompt_estimate_tokens: 0,
        })
        .await
        .unwrap();
    let key = InflightLedger::test_key(&instance, "req-overdrop", 0);
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        1,
        "inflight key registered before emit"
    );

    // 2. Construct the ledger with primary_cap=1, overflow_cap=1, then
    //    immediately cancel the worker so overflow_rx is dropped on exit.
    let (_tmp, spill_root) = make_spill_root();
    let spill_dir = _tmp.path().to_path_buf();
    let shutdown = ShutdownToken::new();
    let (ledger, _primary_rx_held, overflow_handle) = Ledger::for_test_with_overflow(
        redis.clone(),
        instance.clone(),
        1,
        1,
        spill_root,
        NonZeroUsize::new(DLQ_SHARDS).unwrap(),
        shutdown.clone(),
    );
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), overflow_handle).await;

    // 3. Emit the targeted record with `clear: Some`. Primary is full
    //    (held rx), overflow is Closed (worker dropped rx) → route_to_dlq.
    let mut rec = make_record();
    rec.request_id = "req-overdrop".to_string();
    ledger
        .emit(LedgerItem {
            record: rec,
            clear: Some(ClearKey {
                request_id: "req-overdrop".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    // 4. Allow any disk I/O to settle.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 5. Hard guarantee: inflight key NOT deleted (no usage:records XADD).
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        1,
        "inflight key must NOT be DEL'd when emit falls through to DLQ cascade"
    );

    // 6. Hard guarantee: the record reached a downstream fallback. We
    //    expect the emergency-disk path (retry_tx is closed because
    //    `for_test_with_overflow` drops `_retry_rx`; disk append succeeds
    //    against the tempdir). The BillingBacklog is only touched if disk
    //    also fails, which we don't simulate here.
    let dlq_files: Vec<_> = std::fs::read_dir(&spill_dir)
        .expect("spill root exists")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("dlq."))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !dlq_files.is_empty(),
        "expected at least one dlq.* emergency file in spill root after overflow-full fallback"
    );
}

/// Shutdown drain: when the supervisor's `ShutdownToken` is cancelled while
/// items are arriving on the overflow channel, late records still land in
/// `usage:records` within the constructor's 2-second drain budget.
///
/// Timing assumption: `for_test_with_overflow` hard-codes `drain_budget =
/// Duration::from_secs(2)` in `spawn_primary_overflow_worker`. We emit 8
/// records, cancel immediately, and await the worker handle with a 5-second
/// timeout (well above the 2-second drain budget + the 50 ms flush
/// interval slack).
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn shutdown_drain_flushes_late_overflow_items() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();

    let (_tmp, spill_root) = make_spill_root();
    let shutdown = ShutdownToken::new();
    let (ledger, _primary_rx_held, overflow_handle) = Ledger::for_test_with_overflow(
        redis.clone(),
        "test-inst-shutdown-drain".to_string(),
        1,
        64,
        spill_root,
        NonZeroUsize::new(DLQ_SHARDS).unwrap(),
        shutdown.clone(),
    );

    // Emit 8 records targeting req-drain-{i}. All land in overflow because
    // the primary rx is held unread (primary_cap = 1).
    for i in 0..8u32 {
        ledger.emit(make_item(&format!("req-drain-{i}"))).await;
    }

    // Cancel immediately and let the worker drain.
    shutdown.cancel();
    let drain_result =
        tokio::time::timeout(Duration::from_secs(5), overflow_handle).await;
    assert!(
        drain_result.is_ok(),
        "overflow worker must exit within 5 s of shutdown.cancel() (2 s drain budget + slack)"
    );

    // Allow a brief settle for any in-flight XADD pipelining.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let records = read_usage_records(&redis).await;
    for i in 0..8u32 {
        let target = format!("req-drain-{i}");
        assert!(
            records.iter().any(|r| r.request_id == target),
            "shutdown drain must deliver {target} to usage:records; got {:?}",
            records.iter().map(|r| &r.request_id).collect::<Vec<_>>()
        );
    }
}
