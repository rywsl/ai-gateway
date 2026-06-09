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
        let expired_record = self
            .records
            .get(dimension)
            .filter(|record| record.expires_at <= now)
            .map(|record| record.clone());

        if let Some(record) = expired_record {
            self.remove_expired_if(dimension, &record, now);
            return None;
        }

        self.records.get(dimension).map(|record| record.clone())
    }

    pub fn invalidate_vendor_key(&self, vendor_key_id: &KeyId) -> usize {
        let candidates: Vec<_> = self
            .records
            .iter()
            .filter(|record| &record.vendor_key_id == vendor_key_id)
            .map(|record| (record.key().clone(), record.value().clone()))
            .collect();

        candidates
            .iter()
            .filter(|(dimension, record)| self.remove_if_current_matches(dimension, record))
            .count()
    }

    pub fn sweep_expired(&self, now: Instant) -> usize {
        let candidates: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.expires_at <= now)
            .map(|record| (record.key().clone(), record.value().clone()))
            .collect();

        candidates
            .iter()
            .filter(|(dimension, record)| self.remove_if_current_matches(dimension, record))
            .count()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn remove_expired_if(
        &self,
        dimension: &AffinityDimension,
        expired_record: &AffinityRecord,
        now: Instant,
    ) -> bool {
        if expired_record.expires_at > now {
            return false;
        }

        self.remove_if_current_matches(dimension, expired_record)
    }

    fn remove_if_current_matches(
        &self,
        dimension: &AffinityDimension,
        expected_record: &AffinityRecord,
    ) -> bool {
        self.records
            .remove_if(dimension, |_, current_record| {
                current_record == expected_record
            })
            .is_some()
    }
}

impl Default for AffinityManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(30 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_key(id: &str) -> ApiKeyId {
        ApiKeyId::from(id)
    }

    fn vendor_key(id: &str) -> KeyId {
        KeyId::from(id)
    }

    #[test]
    fn stale_expired_snapshot_does_not_remove_refreshed_record() {
        let now = Instant::now();
        let manager = AffinityManager::new(Duration::from_secs(10));
        let dimension = AffinityDimension::ApiKey(api_key("ak-race"));

        manager.record_hit(dimension.clone(), vendor_key("vk-old"), now);
        let expired_snapshot = manager
            .records
            .get(&dimension)
            .expect("old record is present")
            .clone();

        manager.record_hit(
            dimension.clone(),
            vendor_key("vk-new"),
            now + Duration::from_secs(5),
        );

        assert!(!manager.remove_expired_if(
            &dimension,
            &expired_snapshot,
            now + Duration::from_secs(10)
        ));
        assert_eq!(
            manager
                .get(&dimension, now + Duration::from_secs(10))
                .expect("refreshed record remains")
                .vendor_key_id,
            vendor_key("vk-new")
        );
    }

    #[test]
    fn conditional_vendor_invalidation_reports_only_actual_removals() {
        let now = Instant::now();
        let manager = AffinityManager::new(Duration::from_secs(30));
        let dimension = AffinityDimension::ApiKey(api_key("ak-race"));

        manager.record_hit(dimension.clone(), vendor_key("vk-old"), now);
        let stale_snapshot = manager
            .records
            .get(&dimension)
            .expect("old record is present")
            .clone();

        manager.record_hit(
            dimension.clone(),
            vendor_key("vk-new"),
            now + Duration::from_secs(1),
        );

        assert!(!manager.remove_if_current_matches(&dimension, &stale_snapshot));
        assert_eq!(
            manager
                .get(&dimension, now + Duration::from_secs(2))
                .expect("refreshed record remains")
                .vendor_key_id,
            vendor_key("vk-new")
        );
    }
}
