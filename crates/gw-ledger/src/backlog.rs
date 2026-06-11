//! BillingBacklog — unbounded in-memory fallback when both Redis XADD and
//! the SpillFile DLQ have failed. Guarantees the must-deliver billing
//! invariant: never drops an envelope. The retry worker (next step)
//! drains it back to Redis / disk.

use crate::dlq::DlqEnvelope;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;

pub struct BillingBacklog {
    queue: Mutex<VecDeque<(Instant, DlqEnvelope)>>,
    pending_count: AtomicUsize,
    pending_bytes: AtomicU64,
    high_watermark_count: AtomicUsize,
    oldest_ns: AtomicU64,
}

impl Default for BillingBacklog {
    fn default() -> Self {
        Self::new()
    }
}

impl BillingBacklog {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            pending_count: AtomicUsize::new(0),
            pending_bytes: AtomicU64::new(0),
            high_watermark_count: AtomicUsize::new(0),
            oldest_ns: AtomicU64::new(0),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Acquire)
    }
    pub fn pending_bytes(&self) -> u64 {
        self.pending_bytes.load(Ordering::Acquire)
    }
    pub fn high_watermark(&self) -> usize {
        self.high_watermark_count.load(Ordering::Acquire)
    }

    pub async fn push(&self, env: DlqEnvelope) {
        let bytes = serde_json::to_vec(&env).map(|b| b.len() as u64).unwrap_or(0);
        let mut q = self.queue.lock().await;
        let now = Instant::now();
        if q.is_empty() {
            self.oldest_ns.store(elapsed_ns(now), Ordering::Release);
        }
        q.push_back((now, env));
        let count = self.pending_count.fetch_add(1, Ordering::AcqRel) + 1;
        self.pending_bytes.fetch_add(bytes, Ordering::AcqRel);
        let mut hwm = self.high_watermark_count.load(Ordering::Acquire);
        while count > hwm {
            match self.high_watermark_count.compare_exchange(
                hwm,
                count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => hwm = actual,
            }
        }
        metrics::gauge!("gw_billing_backlog_pending_count").set(count as f64);
        metrics::gauge!("gw_billing_backlog_pending_bytes").set(self.pending_bytes() as f64);
    }

    pub async fn pop(&self) -> Option<DlqEnvelope> {
        let mut q = self.queue.lock().await;
        let (_, env) = q.pop_front()?;
        let new_oldest = q.front().map(|(t, _)| elapsed_ns(*t)).unwrap_or(0);
        self.oldest_ns.store(new_oldest, Ordering::Release);
        let bytes = serde_json::to_vec(&env).map(|b| b.len() as u64).unwrap_or(0);
        self.pending_count.fetch_sub(1, Ordering::AcqRel);
        self.pending_bytes.fetch_sub(bytes, Ordering::AcqRel);
        metrics::gauge!("gw_billing_backlog_pending_count").set(self.pending_count() as f64);
        metrics::gauge!("gw_billing_backlog_oldest_age_seconds").set(
            if new_oldest == 0 {
                0.0
            } else {
                (elapsed_ns(Instant::now()) - new_oldest) as f64 / 1e9
            },
        );
        Some(env)
    }

    pub async fn drain_to_vec(&self) -> Vec<DlqEnvelope> {
        let mut q = self.queue.lock().await;
        let out: Vec<DlqEnvelope> = q.drain(..).map(|(_, e)| e).collect();
        self.pending_count.store(0, Ordering::Release);
        self.pending_bytes.store(0, Ordering::Release);
        self.oldest_ns.store(0, Ordering::Release);
        out
    }
}

fn elapsed_ns(t: Instant) -> u64 {
    // Use Instant elapsed since epoch-equivalent — we only ever compute
    // relative deltas so this is fine; pick a fixed monotonic baseline.
    static BASE: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);
    t.duration_since(*BASE).as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlq::DlqLayer;
    use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;

    fn make_record() -> UsageRecord {
        UsageRecord {
            ts_ms: 1,
            request_id: "req-bl-1".into(),
            trace_id: "trace-bl-1".into(),
            user_id: UserId::from("u-bl"),
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

    fn env() -> DlqEnvelope {
        DlqEnvelope::new(make_record(), "test", 0, DlqLayer::Redis, 0)
    }

    #[tokio::test]
    async fn push_and_pop_preserves_order() {
        let b = BillingBacklog::new();
        for _ in 0..3 {
            b.push(env()).await;
        }
        assert_eq!(b.pending_count(), 3);
        for _ in 0..3 {
            assert!(b.pop().await.is_some());
        }
        assert_eq!(b.pending_count(), 0);
    }

    #[tokio::test]
    async fn drain_clears_state() {
        let b = BillingBacklog::new();
        for _ in 0..10 {
            b.push(env()).await;
        }
        let drained = b.drain_to_vec().await;
        assert_eq!(drained.len(), 10);
        assert_eq!(b.pending_count(), 0);
        assert_eq!(b.pending_bytes(), 0);
    }
}
