//! pricing_strategy_snapshot reader.
//!
//! Production: Go side periodically writes per-(model, vendor) pricing rows
//! into Redis / EntityStore; Rust reads them through the trait. v1 ships an
//! in-memory test impl for unit tests.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct PricingSnapshot {
    pub base_input_micro_per_token: u64,
    pub base_output_micro_per_token: u64,
    pub last_updated_ms: u64,
}

#[async_trait]
pub trait PricingSnapshotReader: Send + Sync {
    async fn get(&self, model_id: &str, vendor_id: &str) -> Option<PricingSnapshot>;
}

#[derive(Default)]
pub struct InMemoryPricingReader {
    map: Mutex<HashMap<(String, String), PricingSnapshot>>,
}

impl InMemoryPricingReader {
    pub fn insert(&self, model: &str, vendor: &str, s: PricingSnapshot) {
        self.map.lock().insert((model.into(), vendor.into()), s);
    }
}

#[async_trait]
impl PricingSnapshotReader for InMemoryPricingReader {
    async fn get(&self, m: &str, v: &str) -> Option<PricingSnapshot> {
        self.map.lock().get(&(m.into(), v.into())).copied()
    }
}
