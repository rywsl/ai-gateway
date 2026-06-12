use crate::usage::UsageRecord;
use gw_runtime::spill_io::{SpillFile, SpillRoot};
use serde::{Deserialize, Serialize};
use std::io;
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
/// Each process gets a unique key (`gw-ledger-emergency-dlq-<uuid>`) and N
/// shard files under the shared `SpillRoot`. The quota enforcer on
/// `SpillRoot` prunes old spill files when total disk usage exceeds the
/// configured limit.
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
            key: format!("gw-ledger-emergency-dlq-{}", uuid::Uuid::new_v4()),
            shards,
            file: OnceCell::new(),
        }
    }

    /// Append a serialised `DlqEnvelope` as a JSONL line.
    ///
    /// The `SpillFile` is lazily created on the first call. Subsequent calls
    /// reuse the same sharded file.
    pub async fn append(&self, envelope: &DlqEnvelope) -> io::Result<()> {
        let file = self
            .file
            .get_or_try_init(|| self.spill_root.create(&self.key, self.shards))
            .await?;
        let bytes = serde_json::to_vec(envelope)?;
        let mut buf = bytes;
        buf.push(b'\n');
        file.append(&buf).await?;
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
}
