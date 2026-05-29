//! `[protection]` config table. Rust-side only — TTL/threshold/overcommit
//! live in Go, not here, per spec §9.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProtectionConfig {
    /// Kill switch. When false, Rust does not subscribe to `protected_lease`
    /// ns and all requests run the legacy `frozen_micro` soft-defense path.
    pub enabled: bool,
    /// Clock-skew tolerance window (ms) when checking `now > expires_at`.
    pub lease_grace_ms: u64,
    /// Fallback `max_tokens` ceiling for credit estimation when the request
    /// has no explicit `max_tokens` and the model's entity config also lacks
    /// a default. Increments `gw_credit_default_ceiling_used_total`.
    pub default_output_ceiling_tokens: u32,
}

impl Default for ProtectionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lease_grace_ms: 5_000,
            default_output_ceiling_tokens: 8192,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disables_protection() {
        let cfg = ProtectionConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 5_000);
        assert_eq!(cfg.default_output_ceiling_tokens, 8192);
    }

    #[test]
    fn deserialize_partial_overrides_keep_defaults() {
        let toml_text = r#"
            enabled = true
        "#;
        let cfg: ProtectionConfig = toml::from_str(toml_text).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 5_000);
        assert_eq!(cfg.default_output_ceiling_tokens, 8192);
    }
}
