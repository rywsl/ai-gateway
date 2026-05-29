//! ModelCoefCache — RCU-style versioned cache mapping model → per-dim
//! credit coefficient. Updated via Go push on `events:counter ns=coef_table`
//! + `ns=coef_table_commit`. See spec §5.1.2, §5.1.3, §6.2.
//!
//! All values are unitless `credits / token`. Rust does NOT translate to
//! USD micro or any currency — that is a strict boundary (spec §2).

use arc_swap::ArcSwap;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    Thinking,
    MultimodalInput,
}

impl Dim {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "input" => Some(Dim::Input),
            "output" => Some(Dim::Output),
            "cache_read" => Some(Dim::CacheRead),
            "cache_creation" => Some(Dim::CacheCreation),
            "thinking" => Some(Dim::Thinking),
            "multimodal_input" => Some(Dim::MultimodalInput),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelCoefs {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub thinking: u64,
    pub multimodal_input: u64,
}

#[derive(Clone, Debug)]
pub struct CoefSnapshot {
    pub version: u64,
    pub by_model: HashMap<String, ModelCoefs>,
}

impl CoefSnapshot {
    pub fn empty() -> Self {
        Self {
            version: 0,
            by_model: HashMap::new(),
        }
    }

    pub fn get(&self, model_id: &str) -> Option<&ModelCoefs> {
        self.by_model.get(model_id)
    }
}

/// Staging area for an in-progress version push. Entries accumulate as
/// `coef_table` events arrive; `coef_table_commit` swaps the staged snapshot
/// into `COEF_CACHE` once `model_count` is satisfied.
#[derive(Debug, Default)]
pub struct StagingState {
    pub entries: HashMap<String, ModelCoefs>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Activated,
    Incomplete { staged: u32, expected: u32 },
    Stale { committed: u64, active: u64 },
}

pub static COEF_CACHE: Lazy<ArcSwap<CoefSnapshot>> =
    Lazy::new(|| ArcSwap::new(Arc::new(CoefSnapshot::empty())));

pub static COEF_STAGING: Lazy<DashMap<u64, StagingState>> = Lazy::new(DashMap::new);

/// Test-only mutex that serializes any test mutating the process-global
/// `COEF_CACHE` / `COEF_STAGING`. Exposed `pub(crate)` so cross-module tests
/// (e.g. `counter_handler::tests::coef_table_event_then_commit_swaps_snapshot`)
/// can take the same lock and avoid racing with `coef::tests`.
#[cfg(test)]
pub(crate) static TEST_COEF_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Apply a single coef entry to the staging area for `version`.
/// Idempotent: re-applying the same (version, model_id, dim) overwrites.
pub fn apply_coef_entry(version: u64, model_id: &str, dim: Dim, coef: u64) {
    let mut staging = COEF_STAGING.entry(version).or_default();
    let coefs = staging.entries.entry(model_id.to_string()).or_default();
    match dim {
        Dim::Input => coefs.input = coef,
        Dim::Output => coefs.output = coef,
        Dim::CacheRead => coefs.cache_read = coef,
        Dim::CacheCreation => coefs.cache_creation = coef,
        Dim::Thinking => coefs.thinking = coef,
        Dim::MultimodalInput => coefs.multimodal_input = coef,
    }
}

/// Commit the staged version. Activates if `staged model count ≥ expected`
/// AND `version > active version`. Returns the outcome for caller telemetry.
pub fn apply_coef_commit(version: u64, model_count: u32) -> CommitOutcome {
    let active_version = COEF_CACHE.load().version;
    if version <= active_version {
        // Stale commit (already moved on). Drop staging entry.
        COEF_STAGING.remove(&version);
        return CommitOutcome::Stale {
            committed: version,
            active: active_version,
        };
    }

    let staging = match COEF_STAGING.remove(&version) {
        Some((_, s)) => s,
        None => StagingState::default(),
    };
    let staged_count = staging.entries.len() as u32;
    if staged_count < model_count {
        // Put back so a later commit with the right count can succeed.
        COEF_STAGING.insert(version, staging);
        return CommitOutcome::Incomplete {
            staged: staged_count,
            expected: model_count,
        };
    }

    let new_snapshot = CoefSnapshot {
        version,
        by_model: staging.entries,
    };
    COEF_CACHE.store(Arc::new(new_snapshot));
    CommitOutcome::Activated
}

/// Lookup the active snapshot. Returns the `Arc` directly so callers can hold
/// a stable reference even across an RCU swap.
pub fn current_snapshot() -> Arc<CoefSnapshot> {
    COEF_CACHE.load_full()
}

/// Lookup a specific model in a specific historical version. Required by
/// `LeaseGuard.settle_actual` which must use the lease's coef_version even if
/// the active version has moved on. Returns `None` if that version is neither
/// active nor in staging.
pub fn lookup(version: u64, model_id: &str) -> Option<ModelCoefs> {
    let active = COEF_CACHE.load();
    if active.version == version {
        return active.by_model.get(model_id).copied();
    }
    COEF_STAGING
        .get(&version)
        .and_then(|s| s.entries.get(model_id).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_COEF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();
        guard
    }

    #[test]
    fn empty_snapshot_has_version_zero() {
        let _g = reset();
        let snap = CoefSnapshot::empty();
        assert_eq!(snap.version, 0);
        assert!(snap.by_model.is_empty());
        assert!(snap.get("gpt-4").is_none());
    }

    #[test]
    fn apply_entry_then_commit_swaps_snapshot() {
        let _g = reset();
        apply_coef_entry(7, "model-a", Dim::Input, 100);
        apply_coef_entry(7, "model-a", Dim::Output, 300);
        apply_coef_entry(7, "model-b", Dim::Input, 50);
        assert_eq!(COEF_CACHE.load().version, 0);

        let outcome = apply_coef_commit(7, 2);
        assert_eq!(outcome, CommitOutcome::Activated);
        assert_eq!(COEF_CACHE.load().version, 7);
        assert_eq!(
            COEF_CACHE.load().get("model-a"),
            Some(&ModelCoefs {
                input: 100,
                output: 300,
                ..Default::default()
            })
        );
    }

    #[test]
    fn commit_with_incomplete_staging_keeps_old() {
        let _g = reset();
        apply_coef_entry(9, "model-a", Dim::Input, 100);
        let outcome = apply_coef_commit(9, 2);
        assert_eq!(outcome, CommitOutcome::Incomplete { staged: 1, expected: 2 });
        assert_eq!(COEF_CACHE.load().version, 0);
    }

    #[test]
    fn stale_commit_for_older_version_is_dropped() {
        let _g = reset();
        COEF_CACHE.store(Arc::new(CoefSnapshot {
            version: 10,
            by_model: HashMap::new(),
        }));
        apply_coef_entry(5, "model-a", Dim::Input, 100);
        let outcome = apply_coef_commit(5, 1);
        assert_eq!(outcome, CommitOutcome::Stale { committed: 5, active: 10 });
        assert_eq!(COEF_CACHE.load().version, 10);
    }

    #[test]
    fn lookup_finds_active_version() {
        let _g = reset();
        apply_coef_entry(11, "m", Dim::Input, 7);
        apply_coef_commit(11, 1);
        assert_eq!(
            lookup(11, "m"),
            Some(ModelCoefs { input: 7, ..Default::default() })
        );
    }

    #[test]
    fn lookup_finds_staging_version() {
        let _g = reset();
        apply_coef_entry(20, "m", Dim::Input, 5);
        // Not committed yet; lookup of staged version still returns the entry.
        assert_eq!(
            lookup(20, "m"),
            Some(ModelCoefs { input: 5, ..Default::default() })
        );
    }

    #[test]
    fn lookup_unknown_version_returns_none() {
        let _g = reset();
        assert!(lookup(999, "anything").is_none());
    }

    #[test]
    fn re_apply_same_dim_overwrites() {
        let _g = reset();
        apply_coef_entry(30, "m", Dim::Input, 10);
        apply_coef_entry(30, "m", Dim::Input, 99);
        apply_coef_commit(30, 1);
        assert_eq!(lookup(30, "m").unwrap().input, 99);
    }
}
