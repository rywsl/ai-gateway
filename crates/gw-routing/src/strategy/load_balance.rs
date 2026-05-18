//! `LoadBalance` strategy: pick the available key with the lowest composite
//! score `α·in_flight + β·avg_latency + γ·error_rate`.
//!
//! v1 simplification: only `in_flight` is observed (α=1, β=γ=0). Latency and
//! error-rate inputs are stubbed pending a `LatencyTracker` (TODO Phase 4).

use super::*;
use std::sync::atomic::Ordering;

/// Weight for `in_flight` in the composite score.
pub const ALPHA_IN_FLIGHT: f32 = 1.0;
/// Weight for `avg_latency_ms`. Held at 0 in v1.
pub const BETA_LATENCY: f32 = 0.0;
/// Weight for `error_rate`. Held at 0 in v1.
pub const GAMMA_ERROR_RATE: f32 = 0.0;

pub struct LoadBalance;

impl Strategy for LoadBalance {
    fn name(&self) -> &'static str {
        "load_balance"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        b: &BreakerStore,
        _ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let mut best: Option<(f32, &'a Arc<VendorKey>)> = None;
        for k in c {
            let state = b.get_or_create(&k.key_id);
            if !state.is_available() {
                continue;
            }
            let in_flight = state.in_flight.load(Ordering::Relaxed) as f32;
            // v1: latency / error-rate not wired in; constants stay at 0.
            let score = ALPHA_IN_FLIGHT * in_flight;
            match best {
                Some((bs, _)) if bs <= score => {}
                _ => best = Some((score, k)),
            }
        }
        best.map(|(_, k)| k)
    }
}
