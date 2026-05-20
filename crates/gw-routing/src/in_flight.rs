//! Per-key in-flight counter + RAII guard.
//!
//! Extracted from `gw-routing::breaker::InFlightGuard` per K-A ADR-KA-8.
//! Concerns:
//!
//! - `InFlightStore` owns a `DashMap<KeyId, Arc<InFlightCounter>>`.
//! - `InFlightCounter` is a single `AtomicU32`.
//! - `InFlightGuard` increments on `new`, decrements on `Drop`.
//!
//! Two consumers in `gw-routing`:
//! - `strategy::load_balance::LoadBalance` reads `count` for composite score.
//! - `candidates::get_candidate_keys` drops candidates whose `count` exceeds
//!   the per-key cap.
//!
//! Health concerns (circuit / cooldown / sliding window) live in
//! `gw-health::KeyHealthStore`. The two stores are independent.
//!
//! Memory note: `InFlightStore::count` calls `get_or_create`, so a query
//! for a key that has never had any in-flight requests still allocates an
//! entry (count=0). Key space is bounded by the number of configured
//! vendor keys, so this is acceptable; no eviction sweep is needed in v1.

use dashmap::DashMap;
use gw_core::ids::KeyId;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct InFlightCounter {
    pub count: AtomicU32,
}

impl Default for InFlightCounter {
    fn default() -> Self {
        Self {
            count: AtomicU32::new(0),
        }
    }
}

#[derive(Default)]
pub struct InFlightStore {
    pub by_key: DashMap<KeyId, Arc<InFlightCounter>>,
}

impl InFlightStore {
    pub fn get_or_create(&self, k: &KeyId) -> Arc<InFlightCounter> {
        self.by_key
            .entry(k.clone())
            .or_insert_with(|| Arc::new(InFlightCounter::default()))
            .clone()
    }

    pub fn count(&self, k: &KeyId) -> u32 {
        self.get_or_create(k).count.load(Ordering::Relaxed)
    }
}

/// RAII increment of an [`InFlightCounter`]. Incremented on `new`,
/// decremented on `Drop`. `gw-proxy` holds this on `ReqCtx` so the
/// counter releases on every termination path (success, error, cancel).
pub struct InFlightGuard {
    counter: Arc<InFlightCounter>,
}

impl InFlightGuard {
    /// Bump the counter on construction; the matching decrement runs on
    /// `Drop`.
    ///
    /// Ordering: `Relaxed` is sufficient — the counter is a best-effort
    /// concurrency hint for `LoadBalance::pick` and `candidates::*` (both
    /// read with `Relaxed`). Stale observations are tolerated; no other
    /// memory accesses depend on this counter being globally visible
    /// before/after the RMW. Stronger orderings would buy nothing because
    /// no reader issues a paired `Acquire`.
    pub fn new(counter: Arc<InFlightCounter>) -> Self {
        counter.count.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.count.fetch_sub(1, Ordering::Relaxed);
    }
}
