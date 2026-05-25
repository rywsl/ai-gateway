//! Startup crash-recovery reconciler. Spec §5.4.3.
//!
//! At gateway boot (after `instance_id::resolve`), scan
//! `billing:inflight:<self_instance_id>:*` for residuals from the previous
//! incarnation. Emit a `CrashFallback` UsageRecord (input=prompt_estimate,
//! output=0, all provenance=CrashFallback) and DEL each key.
//!
//! This is the ONLY place that emits fallback UsageRecords; the runtime
//! reaper (§5.4.2) only fires the T0 metric.

use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
use fred::clients::RedisPool;
use fred::interfaces::{HashesInterface, KeysInterface};
use fred::types::Scanner;
use futures::StreamExt;
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::{ProvenanceTag, UsageSnapshot};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// Reconcile own-instance residuals. Returns the count emitted.
///
/// `emit` is a callback to send the record to the ledger (typically
/// `Ledger::emit(rec)` wrapped). It's a callback rather than a direct
/// `Ledger` ref so the test fixture can capture without setting up a full
/// flusher.
pub async fn run<F>(pool: &RedisPool, instance_id: &str, emit: F) -> anyhow::Result<usize>
where
    F: Fn(UsageRecord) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>,
{
    let pattern = format!("billing:inflight:{}:*", instance_id);
    let mut recovered = 0usize;

    let client = pool.next();
    let mut stream = client.scan(pattern, Some(100), None);
    while let Some(result) = stream.next().await {
        let mut page = result?;
        if let Some(keys) = page.take_results() {
            for rkey in keys {
                let Some(key_str) = rkey.into_string() else {
                    continue;
                };
                match build_fallback_record(pool, &key_str).await {
                    Ok(Some(rec)) => {
                        if let Err(e) = emit(rec).await {
                            tracing::error!(
                                error = ?e,
                                key = %key_str,
                                "reconciler emit failed; leaving key for retry"
                            );
                            continue;
                        }
                        let _: () = pool.del::<(), _>(&[key_str.as_str()]).await?;
                        metrics::counter!(
                            "gw_billing_inflight_recovered_total",
                            "exposure_state" => "crash_fallback",
                        )
                        .increment(1);
                        recovered = recovered.saturating_add(1);
                    }
                    Ok(None) => {
                        tracing::warn!(key = %key_str, "reconciler skipped malformed key");
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, key = %key_str, "reconciler hash read failed");
                    }
                }
            }
        }
    }

    Ok(recovered)
}

async fn build_fallback_record(pool: &RedisPool, key: &str) -> anyhow::Result<Option<UsageRecord>> {
    let hash: HashMap<String, String> = pool.hgetall(key).await?;
    if hash.is_empty() {
        return Ok(None);
    }

    // Key shape: billing:inflight:<instance_id>:<request_id>:<attempt_index>
    // instance_id may contain colons, so we parse from the right.
    let parts: Vec<&str> = key.split(':').collect();
    let attempt_index: u32 = parts
        .last()
        .copied()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let request_id = parts
        .len()
        .checked_sub(2)
        .map(|i| parts[i])
        .unwrap_or("")
        .to_string();

    let prompt_estimate: u32 = hash
        .get("prompt_estimate_tokens")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ts_sent: u64 = hash
        .get("ts_sent_ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let provenance = UsageProvenance {
        input: ProvenanceTag::CrashFallback,
        output: ProvenanceTag::CrashFallback,
        cache_read: ProvenanceTag::CrashFallback,
        cache_creation: ProvenanceTag::CrashFallback,
        thinking: ProvenanceTag::CrashFallback,
        multimodal_input: ProvenanceTag::CrashFallback,
    };

    let usage = UsageSnapshot {
        input_tokens: prompt_estimate,
        output_tokens: 0,
        ..Default::default()
    };

    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    Ok(Some(UsageRecord {
        ts_ms: ts_sent,
        request_id,
        trace_id: String::new(),
        user_id: UserId::from(hash.get("user_id").cloned().unwrap_or_default().as_str()),
        child_id: None,
        api_key_id: None,
        client_protocol: String::new(),
        upstream_protocol: String::new(),
        model: ModelName::from(""),
        vendor_id: VendorId::from(""),
        vendor_key_id: KeyId::from(
            hash.get("vendor_key_id")
                .cloned()
                .unwrap_or_default()
                .as_str(),
        ),
        status: UsageStatus::Error,
        error_code: Some("crash_fallback".to_string()),
        http_status: 0,
        usage,
        attempt_index,
        vendor_exposure_state: VendorExposureState::CrashFallback,
        usage_provenance: provenance,
        freeze_amount_micro: hash
            .get("freeze_amount")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        latency_ms: 0,
        ttft_ms: None,
        is_stream: false,
        operation_id: None,
    }))
}
