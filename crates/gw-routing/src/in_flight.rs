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
//! `InFlightStore::count(k)` returns 0 for an unknown key without creating
//! an entry. Entries are created only by `acquire`/`get_or_create` (the
//! actual in-flight tracking path). This avoids the failure mode where a
//! read-only `count` query for an arbitrary key would otherwise allocate.

use dashmap::DashMap;
use gw_core::ids::KeyId;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlightSnapshot {
    pub key_id: KeyId,
    pub count: u32,
}

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
        self.count_if_present(k)
    }

    pub fn count_if_present(&self, k: &KeyId) -> u32 {
        self.by_key
            .get(k)
            .map(|counter| counter.count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn snapshot_counts<'a, I>(&self, keys: I) -> Vec<InFlightSnapshot>
    where
        I: IntoIterator<Item = &'a KeyId>,
    {
        keys.into_iter()
            .map(|key_id| InFlightSnapshot {
                key_id: key_id.clone(),
                count: self.count_if_present(key_id),
            })
            .collect()
    }

    pub fn snapshot_existing(&self) -> Vec<InFlightSnapshot> {
        self.by_key
            .iter()
            .map(|entry| InFlightSnapshot {
                key_id: entry.key().clone(),
                count: entry.value().count.load(Ordering::Relaxed),
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_returns_zero_without_creating_entry() {
        let store = InFlightStore::default();
        let key = KeyId::from("k-never-acquired");
        // Read-only count of an unknown key.
        assert_eq!(store.count(&key), 0);
        // Critical: no entry was inserted.
        assert!(
            !store.by_key.contains_key(&key),
            "count() on unknown key must NOT create an entry"
        );
    }
}
