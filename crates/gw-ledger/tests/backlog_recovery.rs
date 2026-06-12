use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::backlog::{BacklogRecovery, BillingBacklog};
use gw_ledger::dlq::{DlqEnvelope, DlqLayer};
use gw_ledger::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
use gw_runtime::spill_io::{SpillRoot, SpillRootConfig};
use std::sync::Arc;
use tempfile::TempDir;

fn make_record() -> UsageRecord {
    UsageRecord {
        ts_ms: 1,
        request_id: "req-bl-1".into(),
        trace_id: "trace-bl-1".into(),
        user_id: UserId::from("u-bl"),
        child_id: None,
        api_key_id: None,
        client_protocol: "openai".into(),
        upstream_protocol: "openai".into(),
        model: ModelName::from("gpt-4o"),
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
        usage_provenance: UsageProvenance::default(),
        latency_ms: 10,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 100,
        estimate_pricing_version: 7,
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        operation_id: None,
        pod_id: "pod-a".into(),
        credits_consumed: None,
        coef_table_version: None,
    }
}

#[tokio::test]
async fn recovery_loads_and_quarantines() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1024 * 1024,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    // Valid file — two envelopes.
    let env = DlqEnvelope::new(make_record(), "xadd failed", 0, DlqLayer::Disk, 0);
    let line = serde_json::to_vec(&env).unwrap();
    let mut data = Vec::new();
    data.extend_from_slice(&line);
    data.push(b'\n');
    data.extend_from_slice(&line);
    data.push(b'\n');
    tokio::fs::write(tmp.path().join("dlq.0.jsonl"), data)
        .await
        .unwrap();

    // Corrupt file.
    tokio::fs::write(tmp.path().join("dlq.1.jsonl"), b"not-json\n")
        .await
        .unwrap();

    let backlog = BillingBacklog::new();
    let recovery = BacklogRecovery::new(root);
    let loaded = recovery.load_into(&backlog).await.unwrap();

    // Two valid envelopes from the one good file.
    assert_eq!(loaded, 2);
    assert_eq!(backlog.pending_count(), 2);

    // Corrupt file moved to corrupt/ subdir.
    assert!(tmp.path().join("corrupt").join("dlq.1.jsonl").exists());

    // Valid file was deleted after successful replay.
    assert!(!tmp.path().join("dlq.0.jsonl").exists());
}

#[tokio::test]
async fn dump_then_recover_round_trip() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1024 * 1024,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    let backlog1 = BillingBacklog::new();
    for _ in 0..5 {
        backlog1
            .push(DlqEnvelope::new(make_record(), "x", 0, DlqLayer::Disk, 0))
            .await;
    }
    let dumped = backlog1.dump_to_shutdown_file(&root).await.unwrap();
    assert_eq!(dumped, 5);

    let backlog2 = BillingBacklog::new();
    let loaded = BacklogRecovery::new(root.clone())
        .load_into(&backlog2)
        .await
        .unwrap();
    assert_eq!(loaded, 5);
    assert_eq!(backlog2.pending_count(), 5);
}
