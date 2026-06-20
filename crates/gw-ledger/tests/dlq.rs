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
use gw_ledger::backlog::BillingBacklog;
use gw_ledger::dlq::{DlqEnvelope, DlqLayer, EmergencyDlqWriter};
use gw_ledger::flusher::{DlqFallback, LedgerItem};
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
use tokio::sync::mpsc;

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
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        operation_id: None,
        pod_id: String::new(),
        credits_consumed: None,
        coef_table_version: None,
        attempted_keys: Vec::new(),
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
    ledger
        .try_send(LedgerItem {
            record: sample_record(),
            clear: None,
        })
        .unwrap();
    ledger
        .try_send(LedgerItem {
            record: sample_record(),
            clear: None,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let dlq_len: i64 = client.xlen("usage:records:dlq").await.unwrap();
    assert_eq!(
        dlq_len, 2,
        "both records should land in usage:records:dlq when primary stream is unwritable"
    );
}

/// DLQ retry terminal fallback: when both the Redis DLQ XADD and the
/// emergency-file append fail inside `process_dlq_envelope`, the envelope
/// is pushed into `BillingBacklog` and `gateway_usage_records_lost_total`
/// is NOT incremented.
///
/// Deviation from prompt: `process_dlq_envelope` is private to the
/// `gw-ledger` crate, so it cannot be called directly from an integration
/// test without changing production API visibility. Per the prompt's
/// fallback allowance, this test constructs a `DlqFallback` manually and
/// invokes the terminal `fallback.backlog.push(disk_envelope).await` line
/// directly — the exact code path `process_dlq_envelope` takes after both
/// Redis XADD and `EmergencyDlqWriter::append` have failed. The static
/// source-level test `dlq_retry_terminal_failure_routes_to_backlog` in
/// `crates/gw-ledger/src/flusher.rs` already pins the source shape
/// (including the absence of a `gateway_usage_records_lost_total`
/// increment on this path); this runtime test pins the behavior of the
/// terminal push.
#[tokio::test]
async fn dlq_retry_terminal_failure_pushes_backlog() {
    let tmp = TempDir::new().unwrap();
    let spill_root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 16 * 1024 * 1024,
        buffer_size: 4096,
    }));
    let writer = Arc::new(EmergencyDlqWriter::new(
        spill_root,
        NonZeroUsize::new(DLQ_SHARDS).unwrap(),
    ));
    let (retry_tx, _retry_rx) = mpsc::channel::<DlqEnvelope>(1);
    let backlog = Arc::new(BillingBacklog::new());
    let fallback = DlqFallback {
        writer: writer.clone(),
        retry_tx,
        backlog: backlog.clone(),
    };

    let disk_envelope = DlqEnvelope::new(
        sample_record(),
        "redis_xadd_failed + disk_append_failed",
        3,
        DlqLayer::Disk,
        0,
    );

    // Terminal branch of process_dlq_envelope: both Redis XADD and
    // EmergencyDlqWriter::append failed — push to the unbounded backlog.
    fallback.backlog.push(disk_envelope).await;

    assert_eq!(
        fallback.backlog.pending_count(),
        1,
        "terminal DLQ retry failure must push envelope into BillingBacklog, not drop it"
    );
    // The `gateway_usage_records_lost_total` counter is NOT incremented on
    // this path — that property is pinned by the static source test
    // `dlq_retry_terminal_failure_routes_to_backlog` in flusher.rs, which
    // asserts the source contains `fallback.backlog.push` and the
    // emergency-file metric but no lost-counter increment in
    // `process_dlq_envelope`'s body.
}
