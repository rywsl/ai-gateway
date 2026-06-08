use crate::usage::UsageRecord;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

#[derive(Clone, Debug, Serialize)]
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

pub struct EmergencyDlqWriter {
    root: PathBuf,
    file: Mutex<Option<File>>,
}

impl EmergencyDlqWriter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            file: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&self, envelope: &DlqEnvelope) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let path = self.root.join(format!(
                "gw-ledger-emergency-dlq-{}.jsonl",
                uuid::Uuid::new_v4()
            ));
            *guard = Some(OpenOptions::new().create(true).append(true).open(path)?);
        }
        let file = guard.as_mut().expect("file initialized above");
        serde_json::to_writer(&mut *file, envelope)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;

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

    #[test]
    fn emergency_writer_appends_json_lines() {
        let root = std::env::temp_dir().join(format!("gw-dlq-test-{}", uuid::Uuid::new_v4()));
        let writer = EmergencyDlqWriter::new(root.clone());

        writer
            .append(&DlqEnvelope::new(
                record(),
                "redis down",
                2,
                DlqLayer::Disk,
                99,
            ))
            .unwrap();
        writer
            .append(&DlqEnvelope::new(
                record(),
                "redis down",
                3,
                DlqLayer::Disk,
                100,
            ))
            .unwrap();

        let entries = std::fs::read_dir(&root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1, "one JSONL file per process is enough");
        let text = std::fs::read_to_string(entries[0].path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"dlq_layer\":\"disk\""));
    }
}
