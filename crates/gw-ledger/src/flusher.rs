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
use fred::interfaces::StreamsInterface;
use fred::types::{XCap, XCapKind, XCapTrim};
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

const USAGE_STREAM: &str = "usage:records";
const DLQ_STREAM: &str = "usage:records:dlq";
const DLQ_MAXLEN: u64 = 100_000;

pub struct Ledger {
    pub tx: mpsc::Sender<UsageRecord>,
    redis: RedisCtx,
}

impl Ledger {
    pub fn start(
        redis: RedisCtx,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageRecord>(channel_capacity);
        let worker_redis = redis.clone();
        tokio::spawn(async move {
            let mut buf: Vec<UsageRecord> = Vec::with_capacity(batch_size);
            let mut ticker = interval(flush_interval);
            loop {
                tokio::select! {
                    maybe_rec = rx.recv() => {
                        match maybe_rec {
                            Some(rec) => {
                                buf.push(rec);
                                if buf.len() >= batch_size {
                                    flush(&worker_redis, &mut buf).await;
                                }
                            }
                            None => {
                                if !buf.is_empty() { flush(&worker_redis, &mut buf).await; }
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() { flush(&worker_redis, &mut buf).await; }
                    }
                }
            }
        });
        Self { tx, redis }
    }

    /// Must-deliver billing emit path. Attempts the mpsc fast path first; on a
    /// full channel falls back to a synchronous direct XADD (bypass), routing
    /// to the DLQ if even the bypass fails. Use this for all production billing
    /// emits so that backpressure never silently drops revenue records.
    pub async fn emit(&self, rec: UsageRecord) {
        match self.tx.try_send(rec) {
            Ok(()) => {
                metrics::counter!("gw_billing_emit_path_total", "path" => "channel").increment(1);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(rec_back)) => {
                metrics::counter!("gw_billing_mpsc_bypass_total").increment(1);
                if let Err(e) = sync_xadd(&self.redis, &rec_back).await {
                    tracing::error!(error = ?e, "must-deliver sync XADD bypass failed; routing to DLQ");
                    let payload = match serde_json::to_string(&rec_back) {
                        Ok(p) => p,
                        Err(_) => "{}".to_string(),
                    };
                    let wrapped = wrap_for_dlq(&payload, "sync_xadd_bypass_failed", 0);
                    if dlq_xadd(&self.redis, wrapped).await.is_err() {
                        metrics::counter!("gateway_usage_records_lost_total").increment(1);
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
    pub fn try_send(&self, rec: UsageRecord) -> Result<(), ()> {
        self.tx.try_send(rec).map_err(|_| {
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

async fn flush(redis: &RedisCtx, buf: &mut Vec<UsageRecord>) {
    // Serialize first so we can replay payloads to DLQ on batch failure.
    let mut payloads: Vec<String> = Vec::with_capacity(buf.len());
    for rec in buf.drain(..) {
        match serde_json::to_string(&rec) {
            Ok(p) => payloads.push(p),
            Err(e) => {
                tracing::warn!(target: "gw_ledger", error = %e, "usage flush: serialize failed; record dropped");
                metrics::counter!("gateway_usage_serialize_errors_total").increment(1);
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
    let mut queue_failed: Vec<(String, String)> = Vec::new();
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
            queue_failed.push((payload.clone(), e.to_string()));
        } else {
            queued_indices.push(idx);
        }
    }
    // RTT: execute the pipelined batch.
    let batch_result = pipe.all::<()>().await;
    if let Err(e) = batch_result {
        tracing::warn!(target: "gw_ledger", error = %e, batch_len, "usage flush RTT failed; routing to DLQ");
        metrics::counter!("gateway_usage_flush_errors_total").increment(1);
        // Drain ALL queued records to DLQ.
        let reason = format!("batch_rtt: {e}");
        for idx in &queued_indices {
            let wrapped = wrap_for_dlq(&payloads[*idx], &reason, 0);
            if dlq_xadd(redis, wrapped).await.is_err() {
                metrics::counter!("gateway_usage_records_lost_total").increment(1);
            }
        }
    }
    // Handle per-record queue failures (independent of batch RTT).
    for (payload, reason) in queue_failed {
        let wrapped = wrap_for_dlq(&payload, &format!("xadd_queue: {reason}"), 0);
        if dlq_xadd(redis, wrapped).await.is_err() {
            metrics::counter!("gateway_usage_records_lost_total").increment(1);
        }
    }
}
