//! Usage flusher — single background task that batches `UsageRecord`s from
//! an mpsc channel and writes them to Redis in a single pipelined `XADD`
//! batch.
//!
//! Per integration spec §2 表 A A6 + C-γ §ADR-Cγ-6: the flusher only
//! appends to `usage:records`. Balance decrement is Go's job (it consumes
//! `usage:records`, computes cost, writes `events:counter`). Rust no
//! longer touches `counter:user:*:balance_micro` here.
//!
//! Batching cap is set by `batch_size`; an interval timer flushes any
//! partial buffer so latency stays bounded even at low QPS.
//!
//! **Must-deliver semantic**: `emit` is the production billing path — on a
//! full mpsc channel it falls back to a synchronous direct XADD rather than
//! dropping; `try_send` retains the original drop-on-full behaviour for
//! callers that explicitly opt out of the blocking fallback.

use crate::usage::UsageRecord;
use fred::interfaces::{KeysInterface, StreamsInterface};
use fred::types::{XCap, XCapKind, XCapTrim};
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

const USAGE_STREAM: &str = "usage:records";
const DLQ_STREAM: &str = "usage:records:dlq";
const DLQ_MAXLEN: u64 = 100_000;

/// Channel-internal envelope: the wire `UsageRecord` plus an optional
/// inflight clear instruction. The flusher (or `sync_xadd` bypass) DELs
/// `billing:inflight:<instance>:<request_id>:<attempt>` after — and only
/// after — the corresponding `record` is XADD-confirmed on
/// `usage:records`. See `docs/superpowers/specs/2026-05-29-billing-record-durability-design.md` §3.2 (DEL rule).
#[derive(Clone, Debug)]
pub struct LedgerItem {
    pub record: UsageRecord,
    pub clear: Option<ClearKey>,
}

#[derive(Clone, Debug)]
pub struct ClearKey {
    pub request_id: String,
    pub attempt_index: u32,
}

pub struct Ledger {
    pub tx: mpsc::Sender<LedgerItem>,
    redis: RedisCtx,
    instance_id: String,
}

impl Ledger {
    pub fn start(
        redis: RedisCtx,
        instance_id: String,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<LedgerItem>(channel_capacity);
        let worker_redis = redis.clone();
        let worker_instance = instance_id.clone();
        tokio::spawn(async move {
            let mut buf: Vec<LedgerItem> = Vec::with_capacity(batch_size);
            let mut ticker = interval(flush_interval);
            loop {
                tokio::select! {
                    maybe_item = rx.recv() => {
                        match maybe_item {
                            Some(item) => {
                                buf.push(item);
                                if buf.len() >= batch_size {
                                    flush(&worker_redis, &worker_instance, &mut buf).await;
                                }
                            }
                            None => {
                                if !buf.is_empty() {
                                    flush(&worker_redis, &worker_instance, &mut buf).await;
                                }
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() {
                            flush(&worker_redis, &worker_instance, &mut buf).await;
                        }
                    }
                }
            }
        });
        Self {
            tx,
            redis,
            instance_id,
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Must-deliver billing emit path. Attempts the mpsc fast path first; on a
    /// full channel falls back to a synchronous direct XADD (bypass), routing
    /// to the DLQ if even the bypass fails. Use this for all production billing
    /// emits so that backpressure never silently drops revenue records.
    pub async fn emit(&self, item: LedgerItem) {
        match self.tx.try_send(item) {
            Ok(()) => {
                metrics::counter!("gw_billing_emit_path_total", "path" => "channel").increment(1);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
                metrics::counter!("gw_billing_mpsc_bypass_total").increment(1);
                match sync_xadd(&self.redis, &item_back.record).await {
                    Ok(()) => {
                        // XADD durably landed — apply the DEL rule.
                        if let Some(ck) = &item_back.clear {
                            sync_del_inflight(&self.redis, &self.instance_id, ck).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "must-deliver sync XADD bypass failed; routing to DLQ");
                        let payload = serde_json::to_string(&item_back.record)
                            .unwrap_or_else(|_| "{}".to_string());
                        let wrapped = wrap_for_dlq(&payload, "sync_xadd_bypass_failed", 0);
                        if dlq_xadd(&self.redis, wrapped).await.is_err() {
                            metrics::counter!("gateway_usage_records_lost_total").increment(1);
                        }
                        if item_back.clear.is_some() {
                            metrics::counter!(
                                "gw_billing_inflight_clear_total",
                                "outcome" => "skipped_dlq"
                            )
                            .increment(1);
                        }
                    }
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                metrics::counter!("gw_billing_emit_path_total", "path" => "closed").increment(1);
                tracing::error!("usage record channel closed; flusher dead");
            }
        }
    }

    /// Non-blocking enqueue. On a full channel we drop the record and bump
    /// the `gateway_usage_channel_full_total` counter — the alternative
    /// (blocking the proxy request task) would propagate Redis stalls into
    /// upstream latency. Prefer `emit` for production billing paths.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, item: LedgerItem) -> Result<(), ()> {
        self.tx.try_send(item).map_err(|_| {
            metrics::counter!("gateway_usage_channel_full_total").increment(1);
        })
    }
}

/// Direct single-record XADD to `usage:records`, used as the must-deliver
/// bypass when the mpsc channel is full.
async fn sync_xadd(redis: &RedisCtx, rec: &UsageRecord) -> Result<(), String> {
    let payload = serde_json::to_string(rec).map_err(|e| format!("serialize: {e}"))?;
    let client = redis.pool.next();
    client
        .xadd::<(), _, _, _, _>(USAGE_STREAM, false, None::<()>, "*", ("payload", payload))
        .await
        .map_err(|e| format!("xadd: {e}"))
}

/// DEL a single inflight key after a successful sync_xadd bypass. Best-effort:
/// failure is warned + counted but never propagated (record durability has
/// already been confirmed at the caller; key residue is harmless and
/// reconciler-recoverable).
async fn sync_del_inflight(redis: &RedisCtx, instance_id: &str, ck: &ClearKey) {
    let key = format!(
        "billing:inflight:{instance_id}:{}:{}",
        ck.request_id, ck.attempt_index
    );
    let client = redis.pool.next();
    match client.del::<(), _>(&[key.as_str()]).await {
        Ok(_) => {
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "ok").increment(1);
        }
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, key, "sync inflight DEL failed");
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "del_failed")
                .increment(1);
        }
    }
}

fn dlq_cap() -> XCap {
    XCap::try_from((XCapKind::MaxLen, XCapTrim::AlmostExact, DLQ_MAXLEN))
        .expect("XCap::try_from infallible for (MaxLen, AlmostExact, u64)")
}

/// Wrap a serialized usage-record payload with `dlq_meta` for the DLQ stream.
///
/// The wrapper is a JSON object `{ "payload": <original>, "dlq_meta": { ... } }`
/// where `<original>` is the parsed original payload (so consumers can decode
/// it back into `UsageRecord`).
fn wrap_for_dlq(original_payload: &str, failure_reason: &str, retry_count: u32) -> String {
    let inner: serde_json::Value =
        serde_json::from_str(original_payload).unwrap_or(serde_json::Value::Null);
    let wrapped = serde_json::json!({
        "payload": inner,
        "dlq_meta": {
            "failure_reason": failure_reason,
            "retry_count": retry_count,
        },
    });
    wrapped.to_string()
}

/// Best-effort XADD to the DLQ stream. Returns Ok on success; on failure the
/// record is fully dropped and the caller bumps `gw_usage_records_lost_total`.
async fn dlq_xadd(redis: &RedisCtx, wrapped: String) -> Result<(), String> {
    let client = redis.pool.next();
    let r: Result<String, _> = client
        .xadd(DLQ_STREAM, false, dlq_cap(), "*", ("payload", wrapped))
        .await;
    match r {
        Ok(_) => {
            metrics::counter!("gw_usage_dlq_enqueued_total", "outcome" => "success").increment(1);
            Ok(())
        }
        Err(e) => {
            metrics::counter!("gw_usage_dlq_enqueued_total", "outcome" => "error").increment(1);
            Err(e.to_string())
        }
    }
}

async fn flush(redis: &RedisCtx, instance_id: &str, buf: &mut Vec<LedgerItem>) {
    // Drain into parallel vectors: (payload, clear_opt). Per spec
    // `docs/superpowers/specs/2026-05-29-billing-record-durability-design.md`
    // §3.2 (DEL rule), the flusher only DELs the inflight key after the
    // matching record's XADD on `usage:records` is confirmed; DLQ / serialize-
    // fail / queue-fail paths leave the key alone so the reconciler can
    // surface them via CrashFallback.
    let mut payloads: Vec<String> = Vec::with_capacity(buf.len());
    let mut clears: Vec<Option<ClearKey>> = Vec::with_capacity(buf.len());
    for item in buf.drain(..) {
        match serde_json::to_string(&item.record) {
            Ok(p) => {
                payloads.push(p);
                clears.push(item.clear);
            }
            Err(e) => {
                tracing::warn!(target: "gw_ledger", error = %e, "usage flush: serialize failed; record dropped");
                metrics::counter!("gateway_usage_serialize_errors_total").increment(1);
                // Serialize-fail path: drop record AND drop clear (no DEL —
                // key lingers, reconciler will emit CrashFallback later).
                // Consistent with the DEL rule (no XADD success ⇒ no DEL).
            }
        }
    }
    if payloads.is_empty() {
        return;
    }

    let batch_len = payloads.len();
    let client = redis.pool.next();
    let pipe = client.pipeline();
    // Track per-record queue failures so we can DLQ those individually.
    let mut queue_failed: Vec<(usize, String, String)> = Vec::new();
    let mut queued_indices: Vec<usize> = Vec::with_capacity(payloads.len());
    for (idx, payload) in payloads.iter().enumerate() {
        if let Err(e) = pipe
            .xadd::<(), _, _, _, _>(
                USAGE_STREAM,
                false,
                None::<()>,
                "*",
                ("payload", payload.clone()),
            )
            .await
        {
            tracing::warn!(target: "gw_ledger", error = %e, "usage flush: xadd queue failed");
            metrics::counter!("gateway_usage_queue_errors_total").increment(1);
            queue_failed.push((idx, payload.clone(), e.to_string()));
        } else {
            queued_indices.push(idx);
        }
    }
    // RTT: execute the pipelined batch.
    let batch_result = pipe.all::<()>().await;

    // Collect inflight keys to DEL — only those whose XADD success is confirmed.
    let mut to_clear: Vec<String> = Vec::new();
    match batch_result {
        Ok(_) => {
            for idx in &queued_indices {
                if let Some(ck) = &clears[*idx] {
                    to_clear.push(format!(
                        "billing:inflight:{instance_id}:{}:{}",
                        ck.request_id, ck.attempt_index
                    ));
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, batch_len, "usage flush RTT failed; routing to DLQ");
            metrics::counter!("gateway_usage_flush_errors_total").increment(1);
            // Drain ALL queued records to DLQ.
            let reason = format!("batch_rtt: {e}");
            for idx in &queued_indices {
                let wrapped = wrap_for_dlq(&payloads[*idx], &reason, 0);
                if dlq_xadd(redis, wrapped).await.is_err() {
                    metrics::counter!("gateway_usage_records_lost_total").increment(1);
                }
                // No DEL — DLQ path leaves inflight key for reconciler.
                if clears[*idx].is_some() {
                    metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "skipped_dlq").increment(1);
                }
            }
        }
    }

    // Per-record queue failures (independent of batch RTT).
    for (idx, payload, reason) in queue_failed {
        let wrapped = wrap_for_dlq(&payload, &format!("xadd_queue: {reason}"), 0);
        if dlq_xadd(redis, wrapped).await.is_err() {
            metrics::counter!("gateway_usage_records_lost_total").increment(1);
        }
        if clears[idx].is_some() {
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "skipped_dlq")
                .increment(1);
        }
    }

    // Execute the DEL pipeline for confirmed-XADD records with clear: Some.
    if !to_clear.is_empty() {
        let del_client = redis.pool.next();
        let del_pipe = del_client.pipeline();
        let mut del_queued: usize = 0;
        for key in &to_clear {
            if let Err(e) = del_pipe.del::<(), _>(&[key.as_str()]).await {
                tracing::warn!(target: "gw_ledger", error = %e, key, "inflight DEL queue failed");
                metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "del_failed")
                    .increment(1);
            } else {
                del_queued += 1;
            }
        }
        if del_queued > 0 {
            match del_pipe.all::<()>().await {
                Ok(_) => {
                    metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "ok")
                        .increment(del_queued as u64);
                }
                Err(e) => {
                    tracing::warn!(target: "gw_ledger", error = %e, count = del_queued, "inflight DEL batch RTT failed");
                    metrics::counter!(
                        "gw_billing_inflight_clear_total",
                        "outcome" => "del_failed"
                    )
                    .increment(del_queued as u64);
                    // Harmless: keys linger, reconciler/T0 surfaces them.
                }
            }
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

    fn dummy_record(req_id: &str) -> UsageRecord {
        UsageRecord {
            ts_ms: 0,
            request_id: req_id.to_string(),
            trace_id: String::new(),
            user_id: UserId::from("u0"),
            child_id: None,
            api_key_id: None,
            client_protocol: String::new(),
            upstream_protocol: String::new(),
            model: ModelName::from("m"),
            vendor_id: VendorId::from("v"),
            vendor_key_id: KeyId::from("k"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot::default(),
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: UsageProvenance::default(),
            latency_ms: 0,
            ttft_ms: None,
            is_stream: false,
            freeze_amount_micro: 0,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::identity(),
            operation_id: None,
        }
    }

    #[test]
    fn ledger_item_constructs_with_and_without_clear() {
        let rec = dummy_record("req-1");
        let with_clear = LedgerItem {
            record: rec.clone(),
            clear: Some(ClearKey {
                request_id: "req-1".to_string(),
                attempt_index: 0,
            }),
        };
        let without_clear = LedgerItem {
            record: rec,
            clear: None,
        };
        assert!(with_clear.clear.is_some());
        assert!(without_clear.clear.is_none());
        assert_eq!(with_clear.record.request_id, "req-1");
    }
}
