//! Chaos-mode flag + weighted-pick helper (E2E soak, §4.2/§4.3).
//!
//! Flag is **boot-time only** — orchestrator restarts the container to flip
//! it. `set_enabled_for_test` exists so unit tests can drive both modes
//! within one process; production paths only read via [`enabled`].

use std::sync::atomic::{AtomicBool, Ordering};

use rand::distributions::{Distribution, WeightedIndex};
use rand::thread_rng;

use crate::scenario::{ResponseSpec, WeightedVariant};

static CHAOS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Read `FAKE_UPSTREAM_CHAOS` from the process env and store into the global.
/// Recognised truthy values: `on`, `1`, `true` (case-sensitive — keep it
/// boring; docker compose env passthrough is exact).
pub fn init_from_env() {
    let on = std::env::var("FAKE_UPSTREAM_CHAOS")
        .map(|s| matches!(s.as_str(), "on" | "1" | "true"))
        .unwrap_or(false);
    CHAOS_ENABLED.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    CHAOS_ENABLED.load(Ordering::Relaxed)
}

/// Test-only setter. Production callers MUST use [`init_from_env`].
#[doc(hidden)]
pub fn set_enabled_for_test(on: bool) {
    CHAOS_ENABLED.store(on, Ordering::SeqCst);
}

/// Pick a variant by weight. Caller must guarantee `variants` is non-empty
/// (validator rejects empty at admin/scenario time).
pub fn pick_weighted(variants: &[WeightedVariant]) -> ResponseSpec {
    debug_assert!(
        !variants.is_empty(),
        "validator must have rejected empty variants"
    );
    if variants.len() == 1 {
        return (*variants[0].spec).clone();
    }
    let weights = variants.iter().map(|v| v.weight.max(1));
    let dist = WeightedIndex::new(weights).expect("all weights >= 1 after max");
    let idx = dist.sample(&mut thread_rng());
    (*variants[idx].spec).clone()
}

/// Like [`pick_weighted`] but also returns the chosen variant index so the
/// caller can attribute the hit to a per-variant counter.
pub fn pick_weighted_indexed(variants: &[WeightedVariant]) -> (usize, ResponseSpec) {
    debug_assert!(
        !variants.is_empty(),
        "validator must have rejected empty variants"
    );
    if variants.len() == 1 {
        return (0, (*variants[0].spec).clone());
    }
    let weights = variants.iter().map(|v| v.weight.max(1));
    let dist = WeightedIndex::new(weights).expect("all weights >= 1 after max");
    let idx = dist.sample(&mut thread_rng());
    (idx, (*variants[idx].spec).clone())
}
