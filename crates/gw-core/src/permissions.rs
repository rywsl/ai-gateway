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
    /// Account/user-group tier multiplier. Until the cross-process schema grows
    /// a separate user/account field, this carries user, parent, and user-group
    /// markup combined so Go can still validate the four-field product.
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
    /// Api-key-tier model whitelist regex — direct copy from `ApiKeyEntry`,
    /// not 5-tier propagated. Evaluated by gw-proxy (Task 4).
    pub model_whitelist_regex: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MergedPermissions propagates `model_whitelist_regex` directly from
    /// ApiKeyEntry — it is NOT 5-tier merged.
    #[test]
    fn merged_permissions_model_whitelist_regex_from_api_key() {
        let perm = MergedPermissions {
            user_id: UserId::from("u1"),
            effective_user_id: UserId::from("u1"),
            client_ip: "127.0.0.1".parse().unwrap(),
            allowed_models: HashSet::new(),
            allowed_channels: HashSet::new(),
            allowed_groups: HashSet::new(),
            forced_model_group: None,
            rpm_limit: 0,
            tpm_limit: 0,
            max_concurrent_streams: 0,
            markup_chain: MarkupChainSnapshot::identity(),
            model_whitelist_regex: Some("^gpt-4.*".to_string()),
        };
        assert_eq!(perm.model_whitelist_regex.as_deref(), Some("^gpt-4.*"));
    }

    /// MergedPermissions with no api-key has `model_whitelist_regex: None`.
    #[test]
    fn merged_permissions_model_whitelist_regex_none_when_no_api_key() {
        let perm = MergedPermissions {
            user_id: UserId::from("u1"),
            effective_user_id: UserId::from("u1"),
            client_ip: "127.0.0.1".parse().unwrap(),
            allowed_models: HashSet::new(),
            allowed_channels: HashSet::new(),
            allowed_groups: HashSet::new(),
            forced_model_group: None,
            rpm_limit: 0,
            tpm_limit: 0,
            max_concurrent_streams: 0,
            markup_chain: MarkupChainSnapshot::identity(),
            model_whitelist_regex: None,
        };
        assert!(perm.model_whitelist_regex.is_none());
    }
}
