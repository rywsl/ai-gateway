//! Sharded async append-only spill files.
//!
//! Each `SpillFile` is backed by N shard files (`<base>.0.jsonl`,
//! `<base>.1.jsonl`, ...). Concurrent appends pick a shard via round-robin
//! `AtomicUsize::fetch_add(Relaxed)` and serialise into that shard's
//! `tokio::sync::Mutex<BufWriter<tokio::fs::File>>`. Locks are held only
//! across the syscall, never across business `.await`.
//!
//! ## Drop policy
//!
//! Callers choose one of two on-drop behaviours:
//!
//! * [`SpillPolicy::DeleteOnDrop`] (default): synchronous unlink on Drop.
//!   Appropriate for ephemeral round-trip use (e.g. `detailed_archive`
//!   compression buffers) — the data has already been forwarded by the time
//!   Drop fires; the file is residue.
//! * [`SpillPolicy::KeepOnDrop`]: leave the shard files on disk. Required
//!   for cross-restart recovery — e.g. emergency DLQ files that the next
//!   process must replay via `BacklogRecovery`. The shared LRU enforcer
//!   (`SpillRoot::spawn_quota_enforcer`) still bounds total disk usage.

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

/// Whether [`SpillFile`] unlinks its shards when dropped. See module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpillPolicy {
    /// Synchronous best-effort `unlink` of every shard file on Drop.
    #[default]
    DeleteOnDrop,
    /// Leave shards on disk for an external consumer (e.g. cross-restart
    /// recovery scanner) to pick up. The LRU enforcer still applies.
    KeepOnDrop,
}

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
        self.create_with_policy(key, shards, SpillPolicy::DeleteOnDrop)
            .await
    }

    /// Create a sharded spill file with an explicit drop policy. Use
    /// [`SpillPolicy::KeepOnDrop`] for cross-restart recovery (emergency
    /// DLQ); callers without a recovery story should keep the default.
    pub async fn create_with_policy(
        &self,
        key: &str,
        shards: NonZeroUsize,
        policy: SpillPolicy,
    ) -> io::Result<SpillFile> {
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
            policy,
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
    policy: SpillPolicy,
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
    pub fn policy(&self) -> SpillPolicy {
        self.policy
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

impl Drop for SpillFile {
    fn drop(&mut self) {
        // For `KeepOnDrop` (cross-restart recovery: emergency DLQ files
        // must survive the writer being dropped on shutdown), leave shards
        // on disk so the next process can replay them via `BacklogRecovery`.
        // The shared LRU enforcer still bounds total disk usage.
        if matches!(self.policy, SpillPolicy::KeepOnDrop) {
            return;
        }
        // `DeleteOnDrop`: synchronous best-effort cleanup. `unlink(2)` is a
        // single-syscall metadata op (microseconds on local disk), so
        // blocking briefly in Drop is acceptable and avoids the TOCTOU race
        // that an async-spawn cleanup would have with same-`request_id`
        // retries: the next attempt's `SpillRoot::create` (which uses
        // `O_CREAT | O_APPEND`) would otherwise open the prior attempt's
        // file before our spawned unlink ran, causing on-disk byte
        // contamination across retries.
        //
        // Coexists with `SpillRoot::spawn_quota_enforcer`: this Drop is
        // timely, the LRU is fallback. Both paths tolerate `NotFound`
        // silently.
        let paths = std::mem::take(&mut self.shard_paths);
        for path in paths {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Already cleaned (LRU or external).
                }
                Err(e) => {
                    metrics::counter!("gw_spill_drop_cleanup_fail_total").increment(1);
                    tracing::warn!(
                        target: "gw_runtime::spill_io",
                        path = %path.display(),
                        error = %e,
                        "spill_file_drop_cleanup_failed"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn root(tmp: &TempDir) -> SpillRoot {
        SpillRoot::new(SpillRootConfig {
            root_dir: tmp.path().to_path_buf(),
            max_total_bytes: 1024 * 1024,
            buffer_size: 4096,
        })
    }

    #[tokio::test]
    async fn delete_on_drop_unlinks_shards() {
        let tmp = TempDir::new().unwrap();
        let r = root(&tmp);
        let paths: Vec<PathBuf> = {
            let f = r
                .create("ephemeral", NonZeroUsize::new(2).unwrap())
                .await
                .unwrap();
            f.append(b"line\n").await.unwrap();
            f.flush().await.unwrap();
            f.shard_paths().to_vec()
        }; // drop here
        for p in &paths {
            assert!(
                !p.exists(),
                "DeleteOnDrop must unlink shards on drop: {p:?}"
            );
        }
    }

    #[tokio::test]
    async fn keep_on_drop_leaves_shards() {
        let tmp = TempDir::new().unwrap();
        let r = root(&tmp);
        let paths: Vec<PathBuf> = {
            let f = r
                .create_with_policy(
                    "dlq.emergency.t1",
                    NonZeroUsize::new(2).unwrap(),
                    SpillPolicy::KeepOnDrop,
                )
                .await
                .unwrap();
            f.append(b"durable\n").await.unwrap();
            f.flush().await.unwrap();
            f.shard_paths().to_vec()
        }; // drop here
        for p in &paths {
            assert!(
                p.exists(),
                "KeepOnDrop must NOT unlink shards on drop: {p:?}"
            );
            let bytes = std::fs::read(p).unwrap();
            assert!(
                bytes.windows(b"durable".len()).any(|w| w == b"durable") || bytes.is_empty(),
                "shard {p:?} should contain at least the durable line or be the unwritten shard",
            );
        }
    }
}
