//! `RoundRobin` strategy: per-model atomic counter rotating through candidates.
//! Skips keys whose health is unavailable; falls back through up to `candidates.len()`
//! positions before giving up.

use super::*;
use dashmap::DashMap;
use gw_core::ids::ModelName;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RoundRobin {
    pub counters: DashMap<ModelName, AtomicUsize>,
}

impl Default for RoundRobin {
    fn default() -> Self {
        Self::new()
    }
}

impl RoundRobin {
    pub fn new() -> Self {
        Self {
            counters: DashMap::new(),
        }
    }
}

impl Strategy for RoundRobin {
    fn name(&self) -> &'static str {
        "round_robin"
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
        let counter = self
            .counters
            .entry(ctx.model.clone())
            .or_insert_with(|| AtomicUsize::new(0));
        for _ in 0..c.len() {
            let idx = counter.fetch_add(1, Ordering::Relaxed) % c.len();
            if h.is_available(&c[idx].key_id, ctx.api_format) {
                return Some(&c[idx]);
            }
        }
        None
    }
}
