use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;

/// Snapshot of the 4-tier markup chain plus its scalar product.
///
/// The four tier fields are populated by `merge_permissions`; only `product`
/// carries the real value initially. Per-tier values are wired through
/// separately.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkupChainSnapshot {
    pub user_group_markup: f32,
    pub api_key_markup: f32,
    pub vendor_markup: f32,
    pub model_markup: f32,
    /// Pre-computed product of all applicable markup tiers — the value Go
    /// needs to (re-)compute cost. Satisfies the invariant
    /// `product == user_group_markup * api_key_markup * vendor_markup * model_markup`.
    pub product: f32,
}

impl MarkupChainSnapshot {
    /// Multiplicative identity (all 1.0). Used by tests and by
    /// `merge_permissions` before the per-tier values are wired through.
    pub fn identity() -> Self {
        Self {
            user_group_markup: 1.0,
            api_key_markup: 1.0,
            vendor_markup: 1.0,
            model_markup: 1.0,
            product: 1.0,
        }
    }
}

impl Default for MarkupChainSnapshot {
    /// Multiplicative identity (all 1.0). Equivalent to `identity()`.
    fn default() -> Self {
        Self::identity()
    }
}

#[derive(Clone, Debug)]
pub struct MergedPermissions {
    pub user_id: UserId,
    pub effective_user_id: UserId,
    pub client_ip: IpAddr,
    pub allowed_models: HashSet<ModelName>,
    pub allowed_channels: HashSet<KeyId>,
    pub allowed_groups: HashSet<Group>,
    pub forced_model_group: Option<Group>,
    pub rpm_limit: u32,
    pub tpm_limit: u32,
    pub max_concurrent_streams: u32,
    pub markup_chain: MarkupChainSnapshot,
}
