//! `WeightedRr` (Smooth Weighted Round-Robin, Nginx-style).
//!
//! Per `(ModelName, KeyId)` we keep a `current_weight: AtomicI32`. Each pick:
//!   1. For every candidate `i`: `current[i] += weight[i]`.
//!   2. Pick the `i` with the highest `current[i]` among available keys.
//!   3. Subtract `total_weight` from the winner's `current[i]`.
//!
//! State lives in a `DashMap`, so concurrent picks may interleave the
//! read-modify-write phases for the same model. The result is still
//! eventually-consistent SWRR (no key is starved indefinitely).

use super::*;
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy};
use dashmap::DashMap;
use gw_core::ids::{KeyId, ModelName};
use std::sync::atomic::{AtomicI32, Ordering};

pub struct WeightedRr {
    /// `current_weight` per `(model, key)`.
    pub current: DashMap<(ModelName, KeyId), AtomicI32>,
}

impl Default for WeightedRr {
    fn default() -> Self {
        Self::new()
    }
}

impl WeightedRr {
    pub fn new() -> Self {
        Self {
            current: DashMap::new(),
        }
    }

    pub fn reset_key(&self, key_id: &KeyId) {
        for entry in self.current.iter() {
            if &entry.key().1 == key_id {
                entry.value().store(0, Ordering::Relaxed);
            }
        }
    }

    fn weight_of(k: &Arc<VendorKey>) -> i32 {
        // `weight` is `u32`; clamp to i32::MAX to avoid wrap.
        k.weight.min(i32::MAX as u32) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_key_zeros_accumulated_current_for_all_models() {
        let rr = WeightedRr::new();
        let key = KeyId::from("vk-reset");
        rr.current
            .insert((ModelName::from("m1"), key.clone()), AtomicI32::new(42));
        rr.current
            .insert((ModelName::from("m2"), key.clone()), AtomicI32::new(-7));
        rr.current.insert(
            (ModelName::from("m3"), KeyId::from("other")),
            AtomicI32::new(11),
        );

        rr.reset_key(&key);

        assert_eq!(
            rr.current
                .get(&(ModelName::from("m1"), key.clone()))
                .unwrap()
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            rr.current
                .get(&(ModelName::from("m2"), key))
                .unwrap()
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            rr.current
                .get(&(ModelName::from("m3"), KeyId::from("other")))
                .unwrap()
                .load(Ordering::Relaxed),
            11
        );
    }
}

impl Strategy for WeightedRr {
    fn name(&self) -> &'static str {
        "weighted_rr"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        if c.is_empty() {
            return None;
        }

        let total: i32 = c.iter().map(Self::weight_of).sum();
        if total <= 0 {
            // All weights zero — degenerate; fall back to the first available.
            return c.iter().find(|k| h.is_available(&k.key_id, ctx.api_format));
        }

        // Phase 1: bump every current[i] by weight[i].
        for k in c {
            let cur = self
                .current
                .entry((ctx.model.clone(), k.key_id.clone()))
                .or_insert_with(|| AtomicI32::new(0));
            cur.fetch_add(Self::weight_of(k), Ordering::Relaxed);
        }

        // Phase 2: pick the highest current[i] among available keys.
        let mut best_idx: Option<usize> = None;
        let mut best_val: i32 = i32::MIN;
        for (i, k) in c.iter().enumerate() {
            if !h.is_available(&k.key_id, ctx.api_format) {
                if let Some(cur) = self.current.get(&(ctx.model.clone(), k.key_id.clone())) {
                    cur.store(0, Ordering::Relaxed);
                }
                continue;
            }
            let cur = self
                .current
                .entry((ctx.model.clone(), k.key_id.clone()))
                .or_insert_with(|| AtomicI32::new(0));
            let v = cur.load(Ordering::Relaxed);
            if v > best_val {
                best_val = v;
                best_idx = Some(i);
            }
        }

        let i = best_idx?;
        // Phase 3: subtract total weight from the winner.
        if let Some(cur) = self.current.get(&(ctx.model.clone(), c[i].key_id.clone())) {
            cur.fetch_sub(total, Ordering::Relaxed);
        }
        Some(&c[i])
    }
}

impl ScoringPolicy for WeightedRr {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH_ONLY
    }

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        let cur = self
            .current
            .entry((ctx.model.clone(), features.key.key_id.clone()))
            .or_insert_with(|| AtomicI32::new(0));
        -(cur.load(Ordering::Relaxed) as f64)
    }
}
