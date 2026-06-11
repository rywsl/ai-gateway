use gw_runtime::spill_io::{SpillRoot, SpillRootConfig};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tempfile::TempDir;

fn root(tmp: &TempDir) -> Arc<SpillRoot> {
    Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 16 * 1024 * 1024,
        buffer_size: 4096,
    }))
}

#[tokio::test]
async fn single_shard_append_and_read_back() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root.create("test", NonZeroUsize::new(1).unwrap()).await.unwrap();
    file.append(b"hello\n").await.unwrap();
    file.append(b"world\n").await.unwrap();
    file.flush().await.unwrap();
    let all = file.read_all().await.unwrap();
    let concat: Vec<u8> = all.into_iter().flatten().collect();
    assert_eq!(concat, b"hello\nworld\n");
}

#[tokio::test]
async fn sharded_round_robin_distributes_writes() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root.create("test", NonZeroUsize::new(4).unwrap()).await.unwrap();
    for i in 0..40u32 {
        file.append(format!("{i}\n").as_bytes()).await.unwrap();
    }
    file.flush().await.unwrap();
    // 4 shards * 10 lines each
    let shards = file.read_all().await.unwrap();
    assert_eq!(shards.len(), 4);
    for s in &shards {
        let lines = String::from_utf8_lossy(s).lines().count();
        assert_eq!(lines, 10, "expected 10 lines per shard, got {lines}");
    }
}

#[tokio::test]
async fn concurrent_appends_no_data_loss() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = Arc::new(root.create("concurrent", NonZeroUsize::new(4).unwrap()).await.unwrap());

    let mut joins = Vec::new();
    for w in 0..16 {
        let f = file.clone();
        joins.push(tokio::spawn(async move {
            for i in 0..100u32 {
                f.append(format!("{w}-{i}\n").as_bytes()).await.unwrap();
            }
        }));
    }
    for j in joins { j.await.unwrap(); }
    file.flush().await.unwrap();

    let total_lines: usize = file.read_all().await.unwrap()
        .iter()
        .map(|s| String::from_utf8_lossy(s).lines().count())
        .sum();
    assert_eq!(total_lines, 16 * 100);
}
