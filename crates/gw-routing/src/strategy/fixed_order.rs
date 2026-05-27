//! `FixedOrder` strategy: sort candidates by descending `priority`, pick the
//! first one that is still available (per `KeyHealthStore::is_available`).

use super::*;
use crate::pipeline::policies::FixedOrderPolicy;
use crate::pipeline::{DimMask, ScoringPolicy};

pub struct FixedOrder;

impl Strategy for FixedOrder {
    fn name(&self) -> &'static str {
        "fixed_order"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        crate::pipeline::execute(self, candidates, h, f, ctx)
    }
}

impl ScoringPolicy for FixedOrder {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask {
        FixedOrderPolicy.needed_dims(ctx)
    }

    fn score(&self, features: &crate::pipeline::CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        FixedOrderPolicy.score(features, ctx)
    }
}
