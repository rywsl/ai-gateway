//! Integration: emit a LedgerItem with `clear: Some(..)` and verify the
//! flusher DELs the inflight key after a successful XADD.
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger --test inflight_clear_via_flusher -- --ignored`.

use fred::interfaces::{KeysInterface, StreamsInterface};
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

const DLQ_SHARDS: usize = 4;

fn make_spill_root() -> (TempDir, Arc<SpillRoot>) {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 16 * 1024 * 1024,
        buffer_size: 4096,
    }));
    (tmp, root)
}

fn fresh_usage_record(req_id: &str, attempt: u32) -> UsageRecord {
    UsageRecord {
        ts_ms: 0,
        request_id: req_id.to_string(),
        trace_id: String::new(),
        user_id: UserId::from("u"),
        child_id: None,
        api_key_id: None,
        client_protocol: String::new(),
        upstream_protocol: String::new(),
        model: ModelName::from("m"),
        vendor_id: VendorId::from("v"),
        vendor_key_id: KeyId::from("k"),
        status: UsageStatus::Ok,
        error_code: None,
        http_status: 200,
        usage: UsageSnapshot::default(),
        attempt_index: attempt,
        vendor_exposure_state: VendorExposureState::Settled,
        usage_provenance: Default::default(),
        latency_ms: 0,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 0,
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
async fn flusher_dels_inflight_key_on_xadd_success() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-flush-del".to_string();

    // 1. Register an inflight hash for (req-X, attempt 0).
    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    let reg = InflightRegistration {
        request_id: "req-X".to_string(),
        attempt_index: 0,
        user_id: "u".to_string(),
        vendor_key_id: "vk".to_string(),
        freeze_amount: 0,
        prompt_estimate_tokens: 100,
    };
    inflight.register(&reg).await.unwrap();

    let key = InflightLedger::test_key(&instance, "req-X", 0);
    let exists_before: u64 = redis.pool.exists(&key).await.unwrap();
    assert_eq!(exists_before, 1, "inflight key registered");

    // 2. Start a Ledger with very short flush interval.
    let (_tmp, spill_root) = make_spill_root();
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        instance.clone(),
        64,
        4,
        Duration::from_millis(50),
        spill_root,
        DLQ_SHARDS,
        ShutdownToken::new(),
        Duration::from_secs(5),
    );

    // 3. Emit with clear: Some.
    let rec = fresh_usage_record("req-X", 0);
    ledger
        .emit(LedgerItem {
            record: rec,
            clear: Some(ClearKey {
                request_id: "req-X".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    // 4. Wait for the flusher tick (50 ms interval × 2 = generous).
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 5. Assert the record is in usage:records.
    let xlen: u64 = redis.pool.xlen("usage:records").await.unwrap();
    assert!(xlen >= 1, "record landed in usage:records");

    // 6. Assert the inflight key was DEL'd.
    let exists_after: u64 = redis.pool.exists(&key).await.unwrap();
    assert_eq!(exists_after, 0, "inflight key DEL'd after XADD success");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn full_bypass_sync_xadd_dels_inflight_key() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-bypass".to_string();

    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    inflight
        .register(&InflightRegistration {
            request_id: "req-bypass".to_string(),
            attempt_index: 0,
            user_id: "u".to_string(),
            vendor_key_id: "vk".to_string(),
            freeze_amount: 0,
            prompt_estimate_tokens: 0,
        })
        .await
        .unwrap();

    let key = InflightLedger::test_key(&instance, "req-bypass", 0);
    assert_eq!(redis.pool.exists::<u64, _>(&key).await.unwrap(), 1);

    // Force the channel to be Full immediately: capacity = 1, ticker disabled.
    let (_tmp, spill_root) = make_spill_root();
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        instance.clone(),
        1,
        1,
        Duration::from_secs(600), // ticker effectively disabled
        spill_root,
        DLQ_SHARDS,
        ShutdownToken::new(),
        Duration::from_secs(5),
    );
    // First emit fills the channel.
    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("filler", 0),
            clear: None,
        })
        .await;
    // Second emit must hit the Full branch → sync_xadd.
    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("req-bypass", 0),
            clear: Some(ClearKey {
                request_id: "req-bypass".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    // sync_xadd is awaited inside emit; no sleep needed for the bypass path.
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        0,
        "Full-branch sync_xadd → DEL"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn closed_branch_routes_through_sync_xadd_with_del() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-closed".to_string();

    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    inflight
        .register(&InflightRegistration {
            request_id: "req-closed".to_string(),
            attempt_index: 0,
            user_id: "u".to_string(),
            vendor_key_id: "vk".to_string(),
            freeze_amount: 0,
            prompt_estimate_tokens: 0,
        })
        .await
        .unwrap();

    let key = InflightLedger::test_key(&instance, "req-closed", 0);
    assert_eq!(redis.pool.exists::<u64, _>(&key).await.unwrap(), 1);

    // Construct a Ledger whose flusher task never starts: drop the rx
    // end immediately. Subsequent emits hit the Closed branch.
    let (tx, rx) = tokio::sync::mpsc::channel::<LedgerItem>(8);
    drop(rx); // channel is now Closed
    let (_tmp, spill_root) = make_spill_root();
    let ledger = Ledger::from_parts(
        tx,
        redis.clone(),
        instance.clone(),
        spill_root,
        NonZeroUsize::new(DLQ_SHARDS).unwrap(),
    );

    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("req-closed", 0),
            clear: Some(ClearKey {
                request_id: "req-closed".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    // sync_xadd is awaited inside emit; no sleep needed.
    let xlen: u64 = redis.pool.xlen("usage:records").await.unwrap();
    assert!(xlen >= 1);
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        0,
        "Closed branch sync_xadd → DEL"
    );
}
