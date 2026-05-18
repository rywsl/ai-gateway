//! Billing flusher — single background task that batches `BillingRecord`s
//! from an mpsc channel and writes them to Redis in a single pipeline.
//!
//! The flush emits two commands per record:
//!   1. `INCRBY counter:user:{uid}:balance_micro -cost_micro` — decrement balance.
//!   2. `XADD billing:records * payload <json>` — append the full record to the
//!      stream that downstream consumers (settlement, billing export) read from.
//!
//! Batching cap is set by `batch_size`; an interval timer flushes any partial
//! buffer so latency stays bounded even at low QPS.

use crate::billing::BillingRecord;
use fred::interfaces::{KeysInterface, StreamsInterface};
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

pub struct Ledger {
    pub tx: mpsc::Sender<BillingRecord>,
}

impl Ledger {
    /// Spawn the background flusher and return a `Ledger` whose `try_send`
    /// hands records to it. Dropping the returned `Ledger` (and any clones of
    /// `tx`) shuts the task down.
    pub fn start(
        redis: RedisCtx,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<BillingRecord>(channel_capacity);
        tokio::spawn(async move {
            let mut buf: Vec<BillingRecord> = Vec::with_capacity(batch_size);
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

    /// Non-blocking enqueue. On a full channel we drop the record and bump the
    /// `gateway_billing_channel_full_total` counter — the alternative
    /// (blocking the proxy request task) would propagate Redis stalls into
    /// upstream latency.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, rec: BillingRecord) -> Result<(), ()> {
        self.tx.try_send(rec).map_err(|_| {
            metrics::counter!("gateway_billing_channel_full_total").increment(1);
        })
    }
}

async fn flush(redis: &RedisCtx, buf: &mut Vec<BillingRecord>) {
    let batch_len = buf.len();
    let client = redis.pool.next();
    let pipe = client.pipeline();
    for rec in buf.drain(..) {
        let key = format!("counter:user:{}:balance_micro", rec.user_id);
        let payload = serde_json::to_string(&rec).unwrap_or_default();
        let delta = -(rec.cost_micro as i64);
        // Each pipeline op is async but only queues the command in memory;
        // the actual RTT happens in `pipe.all().await` below. If a queue step
        // fails (e.g., bad arg encoding from a future schema drift), log and
        // skip the matching xadd to keep balance and stream consistent.
        if let Err(e) = pipe.incr_by::<(), _>(&key, delta).await {
            tracing::warn!(target: "gw_ledger", error = %e, "billing flush: incr_by queue failed");
            metrics::counter!("gateway_billing_queue_errors_total").increment(1);
            continue;
        }
        if let Err(e) = pipe
            .xadd::<(), _, _, _, _>(
                "billing:records",
                false,
                None::<()>,
                "*",
                ("payload", payload),
            )
            .await
        {
            tracing::warn!(target: "gw_ledger", error = %e, "billing flush: xadd queue failed");
            metrics::counter!("gateway_billing_queue_errors_total").increment(1);
        }
    }
    if let Err(e) = pipe.all::<()>().await {
        tracing::warn!(target: "gw_ledger", error = %e, batch_len, "billing flush RTT failed; records lost");
        metrics::counter!("gateway_billing_flush_errors_total").increment(1);
        metrics::counter!("gateway_billing_records_lost_total").increment(batch_len as u64);
    }
}
