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

use crate::usage::UsageRecord;
use fred::interfaces::StreamsInterface;
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

const USAGE_STREAM: &str = "usage:records";

pub struct Ledger {
    pub tx: mpsc::Sender<UsageRecord>,
}

impl Ledger {
    pub fn start(
        redis: RedisCtx,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageRecord>(channel_capacity);
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
                                    flush(&redis, &mut buf).await;
                                }
                            }
                            None => {
                                if !buf.is_empty() { flush(&redis, &mut buf).await; }
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() { flush(&redis, &mut buf).await; }
                    }
                }
            }
        });
        Self { tx }
    }

    /// Non-blocking enqueue. On a full channel we drop the record and bump
    /// the `gateway_usage_channel_full_total` counter — the alternative
    /// (blocking the proxy request task) would propagate Redis stalls into
    /// upstream latency.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, rec: UsageRecord) -> Result<(), ()> {
        self.tx.try_send(rec).map_err(|_| {
            metrics::counter!("gateway_usage_channel_full_total").increment(1);
        })
    }
}

async fn flush(redis: &RedisCtx, buf: &mut Vec<UsageRecord>) {
    let batch_len = buf.len();
    let client = redis.pool.next();
    let pipe = client.pipeline();
    for rec in buf.drain(..) {
        let payload = match serde_json::to_string(&rec) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "gw_ledger", error = %e, "usage flush: serialize failed; record dropped");
                metrics::counter!("gateway_usage_serialize_errors_total").increment(1);
                continue;
            }
        };
        if let Err(e) = pipe
            .xadd::<(), _, _, _, _>(
                USAGE_STREAM,
                false,
                None::<()>,
                "*",
                ("payload", payload),
            )
            .await
        {
            tracing::warn!(target: "gw_ledger", error = %e, "usage flush: xadd queue failed");
            metrics::counter!("gateway_usage_queue_errors_total").increment(1);
        }
    }
    if let Err(e) = pipe.all::<()>().await {
        tracing::warn!(target: "gw_ledger", error = %e, batch_len, "usage flush RTT failed; records lost");
        metrics::counter!("gateway_usage_flush_errors_total").increment(1);
        metrics::counter!("gateway_usage_records_lost_total").increment(batch_len as u64);
    }
}
