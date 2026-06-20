use crate::flusher::LedgerItem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct PrimaryDelivery {
    tx: mpsc::Sender<LedgerItem>,
    pending: Arc<AtomicUsize>,
}

impl PrimaryDelivery {
    pub fn new(tx: mpsc::Sender<LedgerItem>) -> Self {
        Self {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    pub fn try_enqueue(
        &self,
        item: LedgerItem,
    ) -> Result<(), mpsc::error::TrySendError<LedgerItem>> {
        match self.tx.try_send(item) {
            Ok(()) => {
                let pending = self.pending.fetch_add(1, Ordering::AcqRel) + 1;
                metrics::gauge!("gw_billing_primary_overflow_pending").set(pending as f64);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn mark_drained(&self, count: usize) {
        if count == 0 {
            return;
        }
        let prev = self.pending.fetch_sub(count, Ordering::AcqRel);
        let next = prev.saturating_sub(count);
        metrics::gauge!("gw_billing_primary_overflow_pending").set(next as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flusher::{ClearKey, LedgerItem};
    use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;

    fn record(req_id: &str) -> UsageRecord {
        UsageRecord {
            ts_ms: 0,
            request_id: req_id.to_string(),
            trace_id: String::new(),
            user_id: UserId::from("u-primary-delivery"),
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
            pod_id: String::new(),
            credits_consumed: None,
            coef_table_version: None,
            attempted_keys: Vec::new(),
        }
    }

    fn item(req_id: &str) -> LedgerItem {
        LedgerItem {
            record: record(req_id),
            clear: Some(ClearKey {
                request_id: req_id.to_string(),
                attempt_index: 0,
            }),
        }
    }

    #[tokio::test]
    async fn try_enqueue_preserves_clear_and_tracks_pending() {
        let (tx, mut rx) = mpsc::channel(1);
        let delivery = PrimaryDelivery::new(tx);

        delivery.try_enqueue(item("req-ok")).unwrap();

        assert_eq!(delivery.pending(), 1);
        let got = rx.recv().await.unwrap();
        assert_eq!(got.record.request_id, "req-ok");
        assert!(got.clear.is_some());
        delivery.mark_drained(1);
        assert_eq!(delivery.pending(), 0);
    }

    #[tokio::test]
    async fn full_returns_owned_item_without_pending_increment() {
        let (tx, _rx) = mpsc::channel(1);
        let delivery = PrimaryDelivery::new(tx);
        delivery.try_enqueue(item("req-fill")).unwrap();

        let err = delivery.try_enqueue(item("req-full")).unwrap_err();

        match err {
            mpsc::error::TrySendError::Full(back) => {
                assert_eq!(back.record.request_id, "req-full");
                assert!(back.clear.is_some());
            }
            other => panic!("expected Full, got {other:?}"),
        }
        assert_eq!(delivery.pending(), 1);
    }

    #[tokio::test]
    async fn closed_returns_owned_item_without_pending_increment() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let delivery = PrimaryDelivery::new(tx);

        let err = delivery.try_enqueue(item("req-closed")).unwrap_err();

        match err {
            mpsc::error::TrySendError::Closed(back) => {
                assert_eq!(back.record.request_id, "req-closed");
                assert!(back.clear.is_some());
            }
            other => panic!("expected Closed, got {other:?}"),
        }
        assert_eq!(delivery.pending(), 0);
    }
}
