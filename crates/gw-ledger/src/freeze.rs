//! FreezeGuard — release-only RAII handle for the per-user `frozen_micro`
//! counter (ADR-004 + C-γ §ADR-Cγ-2).
//!
//! Two impls live here:
//!
//! - [`RequestFreeze`] — used by `gw-proxy` for one freeze per request.
//!   Constructed before upstream send; released (idempotent) by the
//!   `logging` filter once the request is settled, or by `Drop` on a
//!   cancellation path.
//! - [`TaskFreeze`] — placeholder for the task path. The real release
//!   (`HINCRBY task_frozen_micro -frozen`, atomic with `entity:task` status
//!   transition and `ZREM tasks:leases`) is implemented by `gw-task-runtime`
//!   in Phase 6. The 1b stub does nothing on release; constructing one is
//!   currently used only by type-system smoke tests.
//!
//! What Phase 1b deliberately does NOT do (deferred to Phase 5/6):
//!
//! - Async `HINCRBY counter:user:<uid>:frozen_micro` to Redis (C-γ
//!   §ADR-Cγ-3): in-process atomic only for now.
//! - Lua-driven Task release path (C-γ Phase 6).
//! - Writing `deducted_micro`: **never** — that's Go's exclusive write
//!   per C-γ §ADR-Cγ-1.

use gw_core::ids::UserId;
use gw_store::deduction::{DEDUCT, DeductionState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// Categorises a freeze for telemetry and (Phase 6) routing in the task
/// settle path.
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
    fn release(&self);
}

/// Per-request freeze. Mutates the in-process `DeductionState.frozen_micro`
/// only — Redis sync is Phase 5 (C-γ §ADR-Cγ-3) work.
pub struct RequestFreeze {
    pub uid: UserId,
    pub(crate) freeze_amount: u64,
    pub(crate) released: AtomicBool,
    pub(crate) state: Arc<DeductionState>,
}

impl RequestFreeze {
    /// Reserve `amount` micro-units against the user's `frozen_micro`.
    /// The returned guard MUST be released (explicitly or via `Drop`).
    // TODO(memory): DEDUCT entries persist for the process lifetime; periodic
    // GC of zero-state entries belongs to a follow-up under C-γ.
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
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}

/// Phase 1b placeholder. The real implementation in Phase 6 calls a Lua
/// script that atomically updates `counter:user:<uid>:task_frozen_micro`,
/// the `entity:task:<id>` status field, and `tasks:leases`.
///
/// In 1b this exists only so the trait has two impls and so dependent
/// crates can name the type. Construction is restricted to `gw-task-runtime`
/// in Phase 6 — there is no public constructor here.
pub struct TaskFreeze {
    pub uid: UserId,
    pub(crate) freeze_amount: u64,
    pub(crate) released: AtomicBool,
}

impl FreezeGuard for TaskFreeze {
    fn kind(&self) -> FreezeKind {
        FreezeKind::Task
    }

    fn freeze_amount(&self) -> u64 {
        self.freeze_amount
    }

    fn release(&self) {
        // Phase 1b: no-op. Phase 6 wires the Lua-driven release path
        // (atomic settle: ZREM lease + HSET task status + HINCRBY
        // task_frozen_micro -frozen) here.
        let _ = self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Drop for TaskFreeze {
    fn drop(&mut self) {
        // Phase 1b: release() is a no-op, so this is harmless today.
        // Phase 6 wires the real Lua-driven rollback in `release()` and
        // this Drop becomes the cancellation safety net.
        self.release();
    }
}
