//! `LatencyFirst` strategy: pick the available key with the lowest observed TTFT.
//!
//! v1 skeleton: there is no `LatencyTracker` wired into the routing crate yet,
//! so this strategy falls back to "first available". The eventual design (per
//! plan ADR-009) is to maintain a per-key TTFT histogram and pick the key with
//! the lowest P50. TODO(v2): add a `LatencyTracker` (likely sibling of
//! `BreakerStore`) and plumb it through `PickCtx` or the strategy struct.

use super::*;

pub struct LatencyFirst;

impl Strategy for LatencyFirst {
    fn name(&self) -> &'static str {
        "latency_first"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        b: &BreakerStore,
        _ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        // TODO(v2): pick lowest P50 TTFT. For now, first-available.
        candidates
            .iter()
            .find(|k| b.get_or_create(&k.key_id).is_available())
    }
}
