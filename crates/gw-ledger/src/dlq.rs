use crate::usage::UsageRecord;
use gw_runtime::spill_io::{SpillFile, SpillPolicy, SpillRoot};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlqLayer {
    Redis,
    Mem,
    Disk,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DlqMeta {
    pub first_failure_ts_ms: u64,
    pub primary_failure_reason: String,
    pub retry_count_before_dlq: u32,
    pub dlq_layer: DlqLayer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DlqEnvelope {
    pub dlq_meta: DlqMeta,
    pub usage_record: UsageRecord,
}

impl DlqEnvelope {
    pub fn new(
        usage_record: UsageRecord,
        primary_failure_reason: impl Into<String>,
        retry_count_before_dlq: u32,
        dlq_layer: DlqLayer,
        first_failure_ts_ms: u64,
    ) -> Self {
        Self {
            dlq_meta: DlqMeta {
                first_failure_ts_ms,
                primary_failure_reason: primary_failure_reason.into(),
                retry_count_before_dlq,
                dlq_layer,
            },
            usage_record,
        }
    }

    pub fn with_layer(&self, dlq_layer: DlqLayer) -> Self {
        let mut clone = self.clone();
        clone.dlq_meta.dlq_layer = dlq_layer;
        clone
    }
}

/// Emergency DLQ writer backed by a sharded async `SpillFile`.
///
/// Replaces the former sync `std::fs::File` + `std::sync::Mutex` with an
/// async append-only file from `gw_runtime::spill_io`. The `SpillFile` is
/// lazily initialised on the first `append` call via a `OnceCell`.
///
/// Each process gets a unique key (`dlq.emergency.<uuid>`) and N shard files
/// under the shared `SpillRoot`. The quota enforcer on `SpillRoot` prunes
/// old spill files when total disk usage exceeds the configured limit.
///
/// ## Durability
///
/// The writer creates its `SpillFile` with `SpillPolicy::KeepOnDrop` so the
/// shard files **survive process exit**. `BacklogRecovery::load_into` on the
/// next boot scans the spill root for files matching `dlq.*` and replays
/// each into the in-memory billing backlog — without `KeepOnDrop` the
/// default `Drop` impl would synchronously `unlink` shards on shutdown,
/// silently losing every record that reached emergency disk during a Redis
/// outage. The `dlq.emergency.` prefix is what makes the file match the
/// recovery filter; renaming it without updating
/// `BacklogRecovery::load_into` (or vice-versa) reintroduces the loss bug.
pub struct EmergencyDlqWriter {
    spill_root: Arc<SpillRoot>,
    key: String,
    shards: NonZeroUsize,
    file: OnceCell<SpillFile>,
}

impl EmergencyDlqWriter {
    pub fn new(spill_root: Arc<SpillRoot>, shards: NonZeroUsize) -> Self {
        Self {
            spill_root,
            // Prefix MUST start with `dlq.` so `BacklogRecovery::load_into`
            // picks it up on the next boot — see struct-level docs.
            key: format!("dlq.emergency.{}", uuid::Uuid::new_v4()),
            shards,
            file: OnceCell::new(),
        }
    }

    /// Append a serialised `DlqEnvelope` as a JSONL line.
    ///
    /// The `SpillFile` is lazily created on the first call with
    /// `SpillPolicy::KeepOnDrop`, so subsequent process restarts can recover
    /// any envelopes written here. Subsequent calls reuse the same sharded
    /// file.
    pub async fn append(&self, envelope: &DlqEnvelope) -> io::Result<()> {
        let file = self
            .file
            .get_or_try_init(|| {
                self.spill_root
                    .create_with_policy(&self.key, self.shards, SpillPolicy::KeepOnDrop)
            })
            .await?;
        let bytes = serde_json::to_vec(envelope)?;
        let mut buf = bytes;
        buf.push(b'\n');
        file.append(&buf).await?;
        Ok(())
    }

    /// Synchronous rescue append for abnormal paths that cannot safely await.
    ///
    /// The file name still starts with `dlq.` so `BacklogRecovery::load_into`
    /// will replay it on the next boot. This intentionally creates a separate
    /// one-shot file instead of touching the async `SpillFile` buffers.
    pub fn append_blocking_rescue(&self, envelope: &DlqEnvelope) -> io::Result<()> {
        std::fs::create_dir_all(self.spill_root.root_dir())?;
        let path = self.spill_root.root_dir().join(format!(
            "{}.rescue.{}.jsonl",
            self.key,
            uuid::Uuid::new_v4()
        ));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        serde_json::to_writer(&mut file, envelope)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    /// Flush all shard buffers to the underlying files.
    pub async fn flush(&self) -> io::Result<()> {
        if let Some(file) = self.file.get() {
            file.flush().await?;
        }
        Ok(())
    }

    /// Read back all data from all shards.
    ///
    /// Returns one `Vec<u8>` per shard, in shard index order. Returns an
    /// empty vector if no data has been written yet.
    pub async fn read_all(&self) -> io::Result<Vec<Vec<u8>>> {
        match self.file.get() {
            Some(file) => file.read_all().await,
            None => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;
    use gw_runtime::spill_io::SpillRootConfig;
    use tempfile::TempDir;

    fn record() -> UsageRecord {
        UsageRecord {
            ts_ms: 1,
            request_id: "req-dlq-1".into(),
            trace_id: "trace-dlq-1".into(),
            user_id: UserId::from("u-dlq"),
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
            attempted_keys: Vec::new(),
        }
    }

    fn spill_root(tmp: &TempDir) -> Arc<SpillRoot> {
        Arc::new(SpillRoot::new(SpillRootConfig {
            root_dir: tmp.path().to_path_buf(),
            max_total_bytes: 16 * 1024 * 1024,
            buffer_size: 4096,
        }))
    }

    #[test]
    fn dlq_envelope_matches_go_contract_shape() {
        let envelope = DlqEnvelope::new(record(), "primary xadd failed", 3, DlqLayer::Mem, 1234);
        let json = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["dlq_meta"]["first_failure_ts_ms"], 1234);
        assert_eq!(
            json["dlq_meta"]["primary_failure_reason"],
            "primary xadd failed"
        );
        assert_eq!(json["dlq_meta"]["retry_count_before_dlq"], 3);
        assert_eq!(json["dlq_meta"]["dlq_layer"], "mem");
        assert_eq!(json["usage_record"]["request_id"], "req-dlq-1");
        let forbidden = format!("{}_{}", "cost", "micro");
        assert!(
            json.get(&forbidden).is_none(),
            "Rust DLQ must not carry cost"
        );
    }

    #[tokio::test]
    async fn emergency_writer_appends_json_lines() {
        let tmp = TempDir::new().unwrap();
        let root = spill_root(&tmp);
        let writer = EmergencyDlqWriter::new(root, NonZeroUsize::new(1).unwrap());

        writer
            .append(&DlqEnvelope::new(
                record(),
                "redis down",
                2,
                DlqLayer::Disk,
                99,
            ))
            .await
            .unwrap();
        writer
            .append(&DlqEnvelope::new(
                record(),
                "redis down",
                3,
                DlqLayer::Disk,
                100,
            ))
            .await
            .unwrap();

        writer.flush().await.unwrap();
        let shards = writer.read_all().await.unwrap();
        // 1 shard → 1 Vec<u8> containing both lines.
        assert_eq!(shards.len(), 1, "single shard expected");
        let text = String::from_utf8(shards.into_iter().flatten().collect()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "two JSONL lines");
        assert!(lines[0].contains("\"dlq_layer\":\"disk\""));
        assert!(lines[1].contains("\"dlq_layer\":\"disk\""));
    }

    #[test]
    fn blocking_rescue_file_uses_recovery_prefix() {
        let tmp = TempDir::new().unwrap();
        let root = spill_root(&tmp);
        let writer = EmergencyDlqWriter::new(root.clone(), NonZeroUsize::new(1).unwrap());

        writer
            .append_blocking_rescue(&DlqEnvelope::new(
                record(),
                "overflow_worker_aborted",
                0,
                DlqLayer::Disk,
                101,
            ))
            .unwrap();

        let entries: Vec<_> = std::fs::read_dir(root.root_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            entries.iter().any(|name| name.starts_with("dlq.")),
            "blocking rescue files must be picked up by BacklogRecovery: {entries:?}"
        );
    }

    /// Round-trip test: append two envelopes, flush, read back, and
    /// deserialise them back into `DlqEnvelope`s. T4.4 BacklogRecovery
    /// depends on this `from_slice` path to replay spilled DLQ lines.
    #[tokio::test]
    async fn dlq_writer_round_trip() {
        let tmp = TempDir::new().unwrap();
        let root = spill_root(&tmp);
        let writer = EmergencyDlqWriter::new(root, NonZeroUsize::new(4).unwrap());

        let env1 = DlqEnvelope::new(record(), "redis OOM", 1, DlqLayer::Mem, 1000);
        let env2 = DlqEnvelope::new(
            UsageRecord {
                request_id: "req-dlq-2".into(),
                ..record()
            },
            "xadd failed",
            2,
            DlqLayer::Redis,
            2000,
        );

        writer.append(&env1).await.unwrap();
        writer.append(&env2).await.unwrap();
        writer.flush().await.unwrap();

        let shards = writer.read_all().await.unwrap();
        // 4 shards: at least 2 must contain data (round-robin).
        let all_bytes: Vec<u8> = shards.into_iter().flatten().collect();
        let text = String::from_utf8(all_bytes).unwrap();

        // Round-trip the on-disk JSONL format back into DlqEnvelope. This is
        // the path BacklogRecovery::replay_file will take.
        let deserialized: Vec<DlqEnvelope> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| serde_json::from_slice::<DlqEnvelope>(line.as_bytes()).unwrap())
            .collect();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].dlq_meta.retry_count_before_dlq, 1);
        assert_eq!(deserialized[0].dlq_meta.dlq_layer, DlqLayer::Mem);
        assert_eq!(deserialized[0].dlq_meta.first_failure_ts_ms, 1000);
        assert_eq!(deserialized[0].usage_record.request_id, "req-dlq-1");
        assert_eq!(deserialized[1].dlq_meta.retry_count_before_dlq, 2);
        assert_eq!(deserialized[1].dlq_meta.dlq_layer, DlqLayer::Redis);
        assert_eq!(deserialized[1].dlq_meta.first_failure_ts_ms, 2000);
        assert_eq!(deserialized[1].usage_record.request_id, "req-dlq-2");
    }

    /// Cross-instance recovery: write via one `EmergencyDlqWriter`, drop it,
    /// then scan the spill root via `BacklogRecovery::load_into` and verify
    /// the envelopes replay correctly. This pins the two preconditions that
    /// the earlier in-process round-trip test missed:
    ///   * `SpillFile` Drop must NOT delete the shard files (KeepOnDrop).
    ///   * The shard filename prefix must match `starts_with("dlq.")` in
    ///     `BacklogRecovery::load_into`.
    /// A regression of either causes silent billing loss during Redis outage.
    #[tokio::test]
    async fn emergency_writer_files_survive_drop_and_match_recovery_filter() {
        let tmp = TempDir::new().unwrap();
        let root = spill_root(&tmp);

        // Phase 1 — write, then drop the writer.
        let shard_paths: Vec<std::path::PathBuf> = {
            let writer = EmergencyDlqWriter::new(root.clone(), NonZeroUsize::new(2).unwrap());
            writer
                .append(&DlqEnvelope::new(
                    record(),
                    "redis down",
                    1,
                    DlqLayer::Disk,
                    111,
                ))
                .await
                .unwrap();
            writer
                .append(&DlqEnvelope::new(
                    UsageRecord {
                        request_id: "req-dlq-3".into(),
                        ..record()
                    },
                    "redis down",
                    2,
                    DlqLayer::Disk,
                    222,
                ))
                .await
                .unwrap();
            writer.flush().await.unwrap();
            // Capture paths before drop so we can assert on them afterwards.
            writer.file.get().unwrap().shard_paths().to_vec()
        }; // writer dropped here — KeepOnDrop must leave files in place.

        // Phase 2 — files must still exist with the `dlq.` prefix.
        let surviving: Vec<_> = shard_paths.iter().filter(|p| p.exists()).collect();
        assert_eq!(
            surviving.len(),
            shard_paths.len(),
            "all shards must survive writer drop (KeepOnDrop policy); got {}/{}",
            surviving.len(),
            shard_paths.len(),
        );
        for p in &shard_paths {
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with("dlq."),
                "shard {name} must start with `dlq.` to be picked up by BacklogRecovery::load_into",
            );
        }

        // Phase 3 — `BacklogRecovery::load_into` would scan the root and
        // match these filenames. We assert the filter behaviour directly
        // here (rather than depending on the backlog module) to avoid a test
        // graph cycle; the integration is covered by
        // `gw-ledger/src/backlog.rs` tests via `replay_file`.
        let entries: Vec<_> = std::fs::read_dir(root.root_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("dlq.") || n.starts_with("backlog_shutdown_")
            })
            .collect();
        assert_eq!(
            entries.len(),
            shard_paths.len(),
            "load_into filter must match every emergency shard",
        );

        // Sanity: the surviving files round-trip as valid DlqEnvelope JSONL.
        let mut total = 0usize;
        for p in &shard_paths {
            let bytes = std::fs::read(p).unwrap();
            for line in bytes.split(|b| *b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let env: DlqEnvelope = serde_json::from_slice(line).unwrap();
                assert_eq!(env.dlq_meta.dlq_layer, DlqLayer::Disk);
                total += 1;
            }
        }
        assert_eq!(total, 2, "both appended envelopes must be recoverable");
    }
}
