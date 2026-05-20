//! Cost-first strategy.
//!
//! v1 limitation: VendorKey has no embedded cost/markup field; true cost
//! resolution requires async lookup into `gw_store::EntityStore.pricing`,
//! which is too expensive on the per-candidate hot path. This v1 implementation
//! delegates to a priority-DESCENDING sort (same direction as FixedOrder) as a
//! coarse proxy — operators are expected to set higher `priority` on cheaper
//! key pools.
//!
//! TODO(v2): consult Pricing.model_markup and sort ascending by true cost.

use super::*;

pub struct CostFirst;

impl Strategy for CostFirst {
    fn name(&self) -> &'static str {
        "cost_first"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let mut sorted: Vec<_> = candidates.iter().enumerate().collect();
        // priority desc (higher = preferred / cheaper-tier-by-convention)
        sorted.sort_by_key(|(_, k)| -k.priority);
        for (i, _) in &sorted {
            if h.is_available(&candidates[*i].key_id, ctx.api_format) {
                return Some(&candidates[*i]);
            }
        }
        None
    }
}
