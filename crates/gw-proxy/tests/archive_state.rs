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
