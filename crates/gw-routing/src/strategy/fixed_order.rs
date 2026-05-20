//! `FixedOrder` strategy: sort candidates by descending `priority`, pick the
//! first one that is still available (per `KeyHealthStore::is_available`).

use super::*;

pub struct FixedOrder;

impl Strategy for FixedOrder {
    fn name(&self) -> &'static str {
        "fixed_order"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let mut sorted: Vec<_> = candidates.iter().enumerate().collect();
        // priority desc — `priority` field is "higher = more preferred" per gw-core::entity.
        sorted.sort_by_key(|(_, k)| -k.priority);
        for (i, _) in &sorted {
            if h.is_available(&candidates[*i].key_id, ctx.api_format) {
                return Some(&candidates[*i]);
            }
        }
        None
    }
}
