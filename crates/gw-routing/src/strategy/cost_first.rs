//! `CostFirst` strategy: pick the cheapest available key.
//!
//! The plan calls for sorting by `key.markup` ascending, but `VendorKey` has no
//! direct markup field (markup lives on `Pricing`/`MergedPermissions`). v1
//! proxy: sort by `priority` ASCENDING — by convention the cheapest pool is
//! given the lowest priority tier. TODO(v2): wire `Pricing.model_markup`.

use super::*;

pub struct CostFirst;

impl Strategy for CostFirst {
    fn name(&self) -> &'static str {
        "cost_first"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        b: &BreakerStore,
        _ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let mut sorted: Vec<_> = candidates.iter().enumerate().collect();
        // Ascending priority: lowest first ≈ cheapest tier.
        sorted.sort_by_key(|(_, k)| k.priority);
        for (i, _) in &sorted {
            if b.get_or_create(&candidates[*i].key_id).is_available() {
                return Some(&candidates[*i]);
            }
        }
        None
    }
}
