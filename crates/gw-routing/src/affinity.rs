//! API-key to vendor-key affinity bindings.
//!
//! This manager is intentionally separate from strategy-local affinity such as
//! `Sticky` and `CacheAffinity`: it stores durable per-dimension routing hints
//! that a future proxy/auth wiring layer can consult across requests.

use dashmap::DashMap;
use gw_core::ids::{ApiKeyId, KeyId};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AffinityDimension {
    ApiKey(ApiKeyId),
    Custom { name: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffinityRecord {
    pub dimension: AffinityDimension,
    pub vendor_key_id: KeyId,
    pub expires_at: Instant,
}

#[derive(Debug)]
pub struct AffinityManager {
    ttl: Duration,
    records: DashMap<AffinityDimension, AffinityRecord>,
}

impl AffinityManager {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            records: DashMap::new(),
        }
    }

    pub fn record_hit(&self, dimension: AffinityDimension, vendor_key_id: KeyId, now: Instant) {
        let record = AffinityRecord {
            dimension: dimension.clone(),
            vendor_key_id,
            expires_at: now + self.ttl,
        };
        self.records.insert(dimension, record);
    }

    pub fn get(&self, dimension: &AffinityDimension, now: Instant) -> Option<AffinityRecord> {
        if self
            .records
            .get(dimension)
            .is_some_and(|record| record.expires_at <= now)
        {
            self.records.remove(dimension);
            return None;
        }

        self.records.get(dimension).map(|record| record.clone())
    }

    pub fn invalidate_vendor_key(&self, vendor_key_id: &KeyId) -> usize {
        let before = self.records.len();
        self.records
            .retain(|_, record| &record.vendor_key_id != vendor_key_id);
        before.saturating_sub(self.records.len())
    }

    pub fn sweep_expired(&self, now: Instant) -> usize {
        let before = self.records.len();
        self.records.retain(|_, record| record.expires_at > now);
        before.saturating_sub(self.records.len())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl Default for AffinityManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(30 * 60))
    }
}
