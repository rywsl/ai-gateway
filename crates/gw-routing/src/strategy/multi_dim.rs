use super::*;
use crate::pipeline::multi_dim::{DimWeights, MultiDimPolicy};
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy};

pub struct MultiDim {
    policy: MultiDimPolicy,
}

impl MultiDim {
    pub fn new(_name: impl Into<String>, weights: DimWeights) -> Self {
        Self {
            policy: MultiDimPolicy::new(weights),
        }
    }

    pub fn weights(&self) -> DimWeights {
        self.policy.weights()
    }
}

impl Strategy for MultiDim {
    fn name(&self) -> &'static str {
        "multi_dim"
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

impl ScoringPolicy for MultiDim {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask {
        self.policy.needed_dims(ctx)
    }

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        self.policy.score(features, ctx)
    }

    fn tiebreak(&self) -> crate::pipeline::TiebreakStrategy {
        self.policy.tiebreak()
    }
}
