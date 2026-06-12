//! Sharded async append-only spill files.
//!
//! Each `SpillFile` is backed by N shard files (`<base>.0.jsonl`,
//! `<base>.1.jsonl`, ...). Concurrent appends pick a shard via round-robin
//! `AtomicUsize::fetch_add(Relaxed)` and serialise into that shard's
//! `tokio::sync::Mutex<BufWriter<tokio::fs::File>>`. Locks are held only
//! across the syscall, never across business `.await`.

use std::io::{self};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct SpillRootConfig {
    pub root_dir: PathBuf,
    pub max_total_bytes: u64,
    pub buffer_size: usize,
}

impl Default for SpillRootConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("/data/cnexus/spill"),
            max_total_bytes: 10 * 1024 * 1024 * 1024, // 10 GiB
            buffer_size: 64 * 1024,
        }
    }
}

pub struct SpillRoot {
    config: SpillRootConfig,
}

impl SpillRoot {
    pub fn new(config: SpillRootConfig) -> Self {
        Self { config }
    }

    pub fn root_dir(&self) -> &Path {
        &self.config.root_dir
    }

    pub async fn ensure_dir(&self) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.config.root_dir).await
    }

    pub async fn create(&self, key: &str, shards: NonZeroUsize) -> io::Result<SpillFile> {
        self.ensure_dir().await?;
        let shard_n = shards.get();
        let mut shard_files = Vec::with_capacity(shard_n);
        let mut shard_paths = Vec::with_capacity(shard_n);
        for i in 0..shard_n {
            let path = self.config.root_dir.join(format!("{key}.{i}.jsonl"));
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&path)
                .await?;
            shard_files.push(Mutex::new(BufWriter::with_capacity(
                self.config.buffer_size,
                file,
            )));
            shard_paths.push(path);
        }
        let bytes_counter = metrics::counter!("gw_spill_bytes_total", "site" => key.to_string());
        Ok(SpillFile {
            base_key: key.to_string(),
            shard_paths,
            shards: shard_files,
            next_shard: AtomicUsize::new(0),
            bytes_counter,
        })
    }

    /// Background LRU enforcer task that prunes oldest files until total
    /// size fits below `max_total_bytes`. Returns its JoinHandle so the
    /// caller can register it with the ShutdownCoordinator / Supervisor.
    pub fn spawn_quota_enforcer(self: Arc<Self>, interval: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                if let Err(e) = self.enforce_quota_once().await {
                    metrics::counter!("gw_spill_quota_enforce_fail_total").increment(1);
                    tracing::warn!(target: "gw_runtime::spill_io", error = %e, "quota enforce iteration failed");
                }
            }
        })
    }

    async fn enforce_quota_once(&self) -> io::Result<()> {
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&self.config.root_dir).await?;
        let mut total: u64 = 0;
        while let Some(ent) = dir.next_entry().await? {
            let meta = match ent.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            total += meta.len();
            let mtime = meta.modified().ok();
            entries.push((mtime, ent.path(), meta.len()));
        }
        if total <= self.config.max_total_bytes {
            return Ok(());
        }
        entries.sort_by_key(|(t, _, _)| *t);
        let mut removed: u64 = 0;
        for (_, p, len) in entries {
            if total - removed <= self.config.max_total_bytes {
                break;
            }
            if tokio::fs::remove_file(&p).await.is_ok() {
                removed += len;
            }
        }
        Ok(())
    }
}

pub struct SpillFile {
    base_key: String,
    shard_paths: Vec<PathBuf>,
    shards: Vec<Mutex<BufWriter<File>>>,
    next_shard: AtomicUsize,
    bytes_counter: metrics::Counter,
}

impl SpillFile {
    pub fn key(&self) -> &str {
        &self.base_key
    }
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
    pub fn shard_paths(&self) -> &[PathBuf] {
        &self.shard_paths
    }

    pub async fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let n = self.shards.len();
        let idx = self.next_shard.fetch_add(1, Ordering::Relaxed) % n;
        let mut guard = self.shards[idx].lock().await;
        guard.write_all(bytes).await?;
        self.bytes_counter.increment(bytes.len() as u64);
        Ok(())
    }

    pub async fn flush(&self) -> io::Result<()> {
        for s in &self.shards {
            let mut g = s.lock().await;
            g.flush().await?;
        }
        Ok(())
    }

    pub async fn fsync(&self) -> io::Result<()> {
        for s in &self.shards {
            let mut g = s.lock().await;
            g.flush().await?;
            g.get_mut().sync_all().await?;
        }
        Ok(())
    }

    pub async fn read_all(&self) -> io::Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(self.shards.len());
        for (idx, s) in self.shards.iter().enumerate() {
            let mut g = s.lock().await;
            g.flush().await?;
            // Hold the lock across open+read so a concurrent append cannot
            // write to the BufWriter between our flush and the read.
            let mut f = File::open(&self.shard_paths[idx]).await?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).await?;
            out.push(buf);
        }
        Ok(out)
    }
}
