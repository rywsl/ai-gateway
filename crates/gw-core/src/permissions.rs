use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;

/// Snapshot of the 4-tier markup chain plus its scalar product.
///
/// Phase 1b: the four tier fields are populated as 1.0 placeholders by
/// `merge_permissions`; only `product` carries the real value (== the old
/// `markup_product`). Phase 5 wires the real per-tier values through.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkupChainSnapshot {
    pub user_group_markup: f32,
    pub api_key_markup: f32,
    pub vendor_markup: f32,
    pub model_markup: f32,
    /// Pre-computed product of all applicable markup tiers — the value Go
    /// needs to (re-)compute cost. In Phase 1b this equals the legacy
    /// `markup_product` scalar; the four per-tier fields above are populated
    /// in Phase 5 (Bm1) and will then satisfy the invariant
    /// `product == user_group_markup * api_key_markup * vendor_markup * model_markup`.
    pub product: f32,
}

impl MarkupChainSnapshot {
    /// All-1.0 placeholder. Used by tests and by `merge_permissions` before
    /// the per-tier values are plumbed through (Phase 5).
    pub fn placeholder() -> Self {
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
    /// Multiplicative identity (all 1.0). Equivalent to `placeholder()`.
    fn default() -> Self {
        Self::placeholder()
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
