//! Usage flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::StreamsInterface;
use fred::types::{RedisValue, XReadResponse};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::flusher::LedgerItem;
use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::Ledger;
use gw_runtime::spill_io::{SpillRoot, SpillRootConfig};
use gw_runtime::ShutdownToken;
use gw_store::redis_client::RedisCtx;
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
