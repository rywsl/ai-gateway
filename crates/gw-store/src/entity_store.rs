//! EntityStore — nine Moka caches keyed by ID newtypes, holding `Arc<Versioned<T>>`.
//!
//! Sized + TTL policy per entity class (ADR-006 / ADR-016):
//! - user / user_group / apikey / ipblock: 300s TTL
//! - vendor / vendor_key / model_group / pricing: 1800s TTL
//! - jwks: 3600s TTL

use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

pub struct EntityStore {
    pub user: Cache<UserId, Arc<Versioned<UserEntry>>>,
    pub user_group: Cache<UserGroupId, Arc<Versioned<UserGroup>>>,
    pub apikey: Cache<ApiKeyId, Arc<Versioned<ApiKeyEntry>>>,
    pub vendor: Cache<VendorId, Arc<Versioned<Vendor>>>,
    pub vendor_key: Cache<KeyId, Arc<Versioned<VendorKey>>>,
    pub model_group: Cache<Group, Arc<Versioned<ModelGroup>>>,
    pub pricing: Cache<ModelName, Arc<Versioned<Pricing>>>,
    pub jwks: Cache<Kid, Arc<Versioned<JwksEntry>>>,
    pub ipblock: Cache<String, Arc<Versioned<IpBlockEntry>>>,
}

impl Default for EntityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityStore {
    pub fn new() -> Self {
        Self {
            user: Cache::builder()
                .max_capacity(1_000_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            user_group: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            apikey: Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            vendor: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            vendor_key: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            model_group: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            pricing: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            jwks: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            ipblock: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
        }
    }

    /// Walk every live `ipblock` entry; used by `gw-auth::ip_filter`.
    /// Snapshots into a `Vec` so callers don't have to thread a lifetime through
    /// the iterator. Moka's iterator is read-only and concurrent-safe.
    pub fn ipblock_snapshot(&self) -> Vec<(String, Arc<Versioned<IpBlockEntry>>)> {
        self.ipblock
            .iter()
            .map(|(k, v)| ((*k).clone(), v))
            .collect()
    }
}
