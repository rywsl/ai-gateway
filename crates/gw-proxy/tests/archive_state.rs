use gw_proxy::ctx::{ArchiveCaps, ArchiveState};
use gw_runtime::{SpillRoot, SpillRootConfig};
use std::sync::Arc;
use tempfile::TempDir;

fn caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps {
        soft_cap_bytes: soft,
        hard_cap_bytes: hard,
    }
}

#[tokio::test]
async fn stays_in_memory_under_soft_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();
    let mut a = ArchiveState::default();
    let small = vec![0u8; 1024];
    a.append_chunk(&small, &caps(4096, 8192), &root, "req1", "upstream")
        .await;
    assert!(matches!(a, ArchiveState::InMemory(_)));
    assert_eq!(a.len(), 1024);
}

#[tokio::test]
async fn spills_to_disk_past_soft_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();
    let mut a = ArchiveState::default();
    let chunk = vec![1u8; 4096];
    a.append_chunk(&chunk, &caps(2048, 8192), &root, "req2", "upstream")
        .await;
    assert!(matches!(a, ArchiveState::Spilled { .. }));
    let read_back = a.read_for_emit().await.unwrap();
    assert_eq!(read_back.len(), 4096);
}

#[tokio::test]
async fn truncates_past_hard_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();
    let mut a = ArchiveState::default();
    let chunk = vec![2u8; 5000];
    a.append_chunk(&chunk, &caps(2048, 4096), &root, "req3", "upstream")
        .await;
    a.append_chunk(&chunk, &caps(2048, 4096), &root, "req3", "upstream")
        .await;
    assert!(a.is_truncated());
    assert!(a.dropped() > 0);
}

#[tokio::test]
async fn reset_response_attempt_clears_archive_accumulators() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    // Simulate attempt-1 archive: write into both archive_*_response_body
    // until they spill, capture their shard paths.
    let mut upstream = ArchiveState::default();
    let mut downstream = ArchiveState::default();
    let chunk = vec![1u8; 4096];
    upstream
        .append_chunk(&chunk, &caps(2048, 8192), &root, "req-test", "upstream")
        .await;
    downstream
        .append_chunk(&chunk, &caps(2048, 8192), &root, "req-test", "downstream")
        .await;
    assert!(matches!(upstream, ArchiveState::Spilled { .. }));
    assert!(matches!(downstream, ArchiveState::Spilled { .. }));

    // Assert each accumulator is non-empty before reset.
    assert!(upstream.len() > 0);
    assert!(downstream.len() > 0);

    // The actual reset call: assign default. This is exactly what the
    // production code change in gateway.rs will do.
    upstream = ArchiveState::default();
    downstream = ArchiveState::default();

    // Post-reset: both are InMemory and empty.
    assert!(matches!(upstream, ArchiveState::InMemory(_)));
    assert!(matches!(downstream, ArchiveState::InMemory(_)));
    assert_eq!(upstream.len(), 0);
    assert_eq!(downstream.len(), 0);

    // Sync Drop completes the unlink before the default-assignment returns —
    // no sleep needed. Assert no spill shard files remain.
    let remaining: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        remaining.is_empty(),
        "expected all archive shards to be unlinked after reset, found: {remaining:?}"
    );
}

#[tokio::test]
async fn attempt_2_archive_contains_no_attempt_1_bytes() {
    // Verifies the core behavioral property: after `reset_response_attempt_state`
    // reassigns the archive accumulators to `ArchiveState::default()`, a fresh
    // attempt-2 write must contain ONLY attempt-2 bytes — no contamination from
    // attempt-1 data. This is the data-isolation guarantee the audit identified.
    //
    // Both attempts use the SAME request_id, reflecting the real production
    // scenario: a response-side failover retry reuses the request's id, so
    // attempt-2's spill file path collides with attempt-1's. The sync Drop
    // contract guarantees attempt-1's shard is unlinked before attempt-2's
    // `SpillRoot::create` opens its `O_CREAT | O_APPEND` handle, preventing
    // on-disk byte contamination across retries.
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    let mut upstream = ArchiveState::default();
    let mut downstream = ArchiveState::default();

    // Attempt 1: write a distinctive 0xAA pattern, forcing spill.
    let attempt_1 = vec![0xAAu8; 4096];
    upstream
        .append_chunk(&attempt_1, &caps(2048, 8192), &root, "req-iso", "upstream")
        .await;
    downstream
        .append_chunk(&attempt_1, &caps(2048, 8192), &root, "req-iso", "downstream")
        .await;
    assert!(matches!(upstream, ArchiveState::Spilled { .. }));
    assert!(matches!(downstream, ArchiveState::Spilled { .. }));

    // Reset: this is exactly what `reset_response_attempt_state` does in
    // production on response-side failover.
    upstream = ArchiveState::default();
    downstream = ArchiveState::default();

    // Attempt 2: write a different distinctive 0xBB pattern.
    let attempt_2 = vec![0xBBu8; 4096];
    upstream
        .append_chunk(&attempt_2, &caps(2048, 8192), &root, "req-iso", "upstream")
        .await;
    downstream
        .append_chunk(&attempt_2, &caps(2048, 8192), &root, "req-iso", "downstream")
        .await;

    // Read back attempt-2 archives and assert NO 0xAA bytes leaked in.
    let upstream_bytes = upstream.read_for_emit().await.unwrap();
    let downstream_bytes = downstream.read_for_emit().await.unwrap();

    assert!(
        !upstream_bytes.iter().any(|&b| b == 0xAA),
        "upstream attempt-2 archive contains 0xAA bytes from attempt-1 (contamination)"
    );
    assert!(
        upstream_bytes.iter().all(|&b| b == 0xBB),
        "upstream attempt-2 archive must contain only 0xBB bytes"
    );
    assert!(
        !downstream_bytes.iter().any(|&b| b == 0xAA),
        "downstream attempt-2 archive contains 0xAA bytes from attempt-1 (contamination)"
    );
    assert!(
        downstream_bytes.iter().all(|&b| b == 0xBB),
        "downstream attempt-2 archive must contain only 0xBB bytes"
    );
}
