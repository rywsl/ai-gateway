//! FreezeGuard — RAII handle for the three-tier deduction state (ADR-004).
//!
//! On construction we bump `frozen_micro` on the per-user `DeductionState`. The
//! caller MUST call [`FreezeGuard::settle`] with the actual cost when the
//! request completes; this moves the (capped) amount from `frozen` into
//! `deducted`. If the guard is dropped without settle (panic, cancellation,
//! early return) the destructor rolls the freeze back and emits a leak metric
//! so we can spot bugs in the proxy orchestration layer.

use gw_core::ids::UserId;
use gw_store::deduction::{DEDUCT, DeductionState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

pub struct FreezeGuard {
    pub uid: UserId,
    pub freeze_amount: u64,
    pub settled: AtomicBool,
    pub state: Arc<DeductionState>,
}

impl FreezeGuard {
    /// Reserve `amount` micro-units against the user's balance. The returned
    /// guard MUST be settled (or dropped) so the freeze is released.
    // TODO(memory): DEDUCT entries persist for the process lifetime; consider periodic GC of zero-state entries from a background task in gw-gateway.
    pub fn freeze(uid: UserId, amount: u64) -> Self {
        let state = DEDUCT
            .entry(uid.clone())
            .or_insert_with(|| {
                Arc::new(DeductionState {
                    base_micro: AtomicI64::new(0),
                    deducted_micro: AtomicI64::new(0),
                    frozen_micro: AtomicI64::new(0),
                })
            })
            .clone();
        state
            .frozen_micro
            .fetch_add(amount as i64, Ordering::AcqRel);
        Self {
            uid,
            freeze_amount: amount,
            settled: AtomicBool::new(false),
            state,
        }
    }

    /// Idempotently settle the freeze: clear `freeze_amount` from `frozen` and
    /// add `actual_micro` to `deducted`. Safe to call once; subsequent calls
    /// (and the destructor) become no-ops.
    pub fn settle(&self, actual_micro: u64) {
        // Race-free idempotency: only the winner proceeds with state mutation.
        if self
            .settled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Atomic ops below can't realistically panic — settled flag is committed.
        self.state
            .frozen_micro
            .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
        self.state
            .deducted_micro
            .fetch_add(actual_micro as i64, Ordering::AcqRel);
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if !self.settled.load(Ordering::Acquire) {
            self.state
                .frozen_micro
                .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}
