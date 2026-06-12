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
    let file = root
        .create("test", NonZeroUsize::new(1).unwrap())
        .await
        .unwrap();
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
    let file = root
        .create("test", NonZeroUsize::new(4).unwrap())
        .await
        .unwrap();
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
    let file = Arc::new(
        root.create("concurrent", NonZeroUsize::new(4).unwrap())
            .await
            .unwrap(),
    );

    let mut joins = Vec::new();
    for w in 0..16 {
        let f = file.clone();
        joins.push(tokio::spawn(async move {
            for i in 0..100u32 {
                f.append(format!("{w}-{i}\n").as_bytes()).await.unwrap();
            }
        }));
    }
    for j in joins {
        j.await.unwrap();
    }
    file.flush().await.unwrap();

    let total_lines: usize = file
        .read_all()
        .await
        .unwrap()
        .iter()
        .map(|s| String::from_utf8_lossy(s).lines().count())
        .sum();
    assert_eq!(total_lines, 16 * 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_unlinks_shard_files() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root
        .create("drop_test", NonZeroUsize::new(3).unwrap())
        .await
        .unwrap();
    for i in 0..30u32 {
        file.append(format!("{i}\n").as_bytes()).await.unwrap();
    }
    file.flush().await.unwrap();

    let paths: Vec<_> = file.shard_paths().to_vec();
    for p in &paths {
        assert!(p.exists(), "shard {} should exist after writes", p.display());
    }

    drop(file);
    // Sync Drop completes the unlink before returning — no sleep needed.
    for p in &paths {
        assert!(
            !p.exists(),
            "shard {} should be unlinked after drop",
            p.display()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_tolerates_missing_shards() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root
        .create("missing_test", NonZeroUsize::new(2).unwrap())
        .await
        .unwrap();
    file.append(b"data\n").await.unwrap();
    file.flush().await.unwrap();
    let paths: Vec<_> = file.shard_paths().to_vec();

    // Simulate LRU racing ahead: unlink shard 0 BEFORE drop.
    tokio::fs::remove_file(&paths[0]).await.unwrap();
    assert!(!paths[0].exists());

    drop(file);

    // Sync Drop completes both NotFound (shard 0, already removed) and
    // successful unlink (shard 1) before returning.
    assert!(!paths[0].exists());
    assert!(!paths[1].exists(), "shard 1 should be cleaned by Drop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_coexists_with_lru_enforcer() {
    use std::time::Duration;
    let tmp = TempDir::new().unwrap();
    // Very small quota so any payload immediately triggers LRU eviction.
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1, // forces LRU to clean everything every tick
        buffer_size: 4096,
    }));
    let enforcer = root.clone().spawn_quota_enforcer(Duration::from_millis(20));

    let file = root
        .create("lru_race", NonZeroUsize::new(2).unwrap())
        .await
        .unwrap();
    file.append(b"x".repeat(8192).as_slice()).await.unwrap();
    file.flush().await.unwrap();

    // Let LRU run a few ticks while file is still live (it cannot remove open
    // BufWriter-held files on every OS, but should at minimum not crash).
    tokio::time::sleep(Duration::from_millis(80)).await;

    drop(file);

    // After drop, give both Drop and LRU time to settle. Either path may have
    // removed the files first — assert no panic, no double-remove failure.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop the enforcer; the test ends.
    enforcer.abort();

    // Whether files exist or not is OS-dependent (BufWriter may have flushed
    // before LRU ran). The invariant we assert is "no panic and no error
    // counter increment from double-remove". This is a panic-smoke-test,
    // not a behavioral-correctness test — a completely broken Drop (e.g. one
    // that does nothing) would also reach this line. The behavioral contract
    // is verified by `drop_unlinks_shard_files` and
    // `drop_tolerates_missing_shards` above.
    // metrics counter check would require a metrics recorder hook; instead we
    // confirm the assertion that we reached this line without panic.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_blocks_until_unlink_completes() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file1 = root
        .create("same_key", NonZeroUsize::new(1).unwrap())
        .await
        .unwrap();
    file1.append(b"aaaa").await.unwrap();
    file1.flush().await.unwrap();
    let path = file1.shard_paths()[0].clone();
    assert!(path.exists());

    drop(file1);
    // Sync Drop guarantee: by the time drop() returns, the file is gone.
    assert!(!path.exists(), "Drop must unlink synchronously before returning");

    // Now create a new SpillFile at the same path — it must start fresh.
    let file2 = root
        .create("same_key", NonZeroUsize::new(1).unwrap())
        .await
        .unwrap();
    file2.append(b"bbbb").await.unwrap();
    file2.flush().await.unwrap();
    let data = file2.read_all().await.unwrap();
    let concat: Vec<u8> = data.into_iter().flatten().collect();
    assert_eq!(concat, b"bbbb", "second SpillFile must not contain first's bytes");
}
