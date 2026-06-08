//! FreezeGuard — release-only RAII handle for the per-user `frozen_micro`
//! counter (ADR-004 + C-γ §ADR-Cγ-2).
//!
//! Two impls live here:
//!
//! - [`RequestFreeze`] — used by `gw-proxy` for one freeze per request.
//!   Constructed before upstream send; released (idempotent) by the
//!   `logging` filter once the request is settled, or by `Drop` on a
//!   cancellation path.
//! - [`TaskFreeze`] — used by the task path. Release dispatches to an
//!   injected async releaser that atomically removes the lease, marks the task
//!   released, and decrements the task frozen counter.
//!
//! This module never writes `deducted_micro`: Go is the exclusive writer per
//! C-γ §ADR-Cγ-1.

use gw_core::ids::UserId;
use gw_store::deduction::{DeductionState, DEDUCT};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

/// Categorises a freeze for telemetry and routing in the task settle path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreezeKind {
    Request,
    Task,
}

/// Common contract for both per-request and per-task freezes.
///
/// `release` is idempotent and called both explicitly (by the proxy
/// `logging` filter / worker settle) and via `Drop` on cancellation paths.
pub trait FreezeGuard: Send + Sync {
    fn kind(&self) -> FreezeKind;
    fn freeze_amount(&self) -> u64;
    fn uid(&self) -> &UserId;
    fn release(&self);
}

/// Outcome of a task freeze release operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// The frozen amount was successfully released.
    Released,
    /// The task had already been released (idempotent).
    AlreadyReleased,
}

/// Trait for releasing a previously frozen task balance.
#[async_trait::async_trait]
pub trait TaskFreezeRelease: Send + Sync {
    /// Release `frozen_amount` for the given user and task.
    ///
    /// Returns [`ReleaseOutcome::Released`] on first release,
    /// [`ReleaseOutcome::AlreadyReleased`] on repeated calls (idempotent).
    async fn release(
        &self,
        uid: &UserId,
        task_id: &str,
        frozen_amount: u64,
    ) -> Result<ReleaseOutcome, anyhow::Error>;
}

/// Per-request freeze. Mutates the in-process `DeductionState.frozen_micro`
/// only — Redis sync is handled via C-γ §ADR-Cγ-3.
pub struct RequestFreeze {
    pub(crate) uid: UserId,
    pub(crate) freeze_amount: u64,
    pub(crate) released: AtomicBool,
    pub(crate) state: Arc<DeductionState>,
}

impl RequestFreeze {
    /// Reserve `amount` micro-units against the user's `frozen_micro`.
    /// The returned guard MUST be released (explicitly or via `Drop`).
    pub fn new(uid: UserId, amount: u64) -> Self {
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
        crate::flusher::enqueue_frozen_delta(&uid, amount as i64);
        Self {
            uid,
            freeze_amount: amount,
            released: AtomicBool::new(false),
            state,
        }
    }
}

impl FreezeGuard for RequestFreeze {
    fn kind(&self) -> FreezeKind {
        FreezeKind::Request
    }

    fn freeze_amount(&self) -> u64 {
        self.freeze_amount
    }

    fn uid(&self) -> &UserId {
        &self.uid
    }

    fn release(&self) {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.state
            .frozen_micro
            .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
        crate::flusher::enqueue_frozen_delta(&self.uid, -(self.freeze_amount as i64));
    }
}

impl Drop for RequestFreeze {
    fn drop(&mut self) {
        // Use the same CAS pattern as release() so the released flag is
        // symmetric across both rollback paths. swap returns the previous
        // value; if it was already true, release() ran first and we skip.
        if !self.released.swap(true, Ordering::AcqRel) {
            self.state
                .frozen_micro
                .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
            crate::flusher::enqueue_frozen_delta(&self.uid, -(self.freeze_amount as i64));
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}

/// Per-task freeze. Release is delegated to the injected task runtime releaser
/// so the synchronous guard API can remain shared with request freezes.
pub struct TaskFreeze {
    pub(crate) uid: UserId,
    pub(crate) task_id: String,
    pub(crate) freeze_amount: u64,
    pub(crate) released: AtomicBool,
    pub(crate) releaser: Option<Arc<dyn TaskFreezeRelease>>,
}

impl TaskFreeze {
    pub fn new(
        uid: UserId,
        task_id: impl Into<String>,
        amount: u64,
        releaser: Option<Arc<dyn TaskFreezeRelease>>,
    ) -> Self {
        Self {
            uid,
            task_id: task_id.into(),
            freeze_amount: amount,
            released: AtomicBool::new(false),
            releaser,
        }
    }
}

impl FreezeGuard for TaskFreeze {
    fn kind(&self) -> FreezeKind {
        FreezeKind::Task
    }

    fn freeze_amount(&self) -> u64 {
        self.freeze_amount
    }

    fn uid(&self) -> &UserId {
        &self.uid
    }

    fn release(&self) {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let Some(releaser) = self.releaser.clone() else {
            return;
        };
        let uid = self.uid.clone();
        let task_id = self.task_id.clone();
        let frozen_amount = self.freeze_amount;
        tokio::spawn(async move {
            if let Err(err) = releaser.release(&uid, &task_id, frozen_amount).await {
                tracing::warn!(
                    target: "gw_ledger",
                    error = %err,
                    uid = %uid,
                    task_id = %task_id,
                    "task freeze release failed"
                );
            }
        });
    }
}

impl Drop for TaskFreeze {
    fn drop(&mut self) {
        self.release();
    }
}
