//! `[protection]` config table. Rust-side only — TTL/threshold/overcommit
//! live in Go, not here, per spec §9.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProtectionConfig {
    /// Kill switch. Hard-mode protection is enabled by default.
    pub enabled: bool,
    /// Clock-skew tolerance window (ms) when checking `now > expires_at`.
    pub lease_grace_ms: u64,
    /// Fallback `max_tokens` ceiling for credit estimation when the request
    /// has no explicit `max_tokens` and the model's entity config also lacks
    /// a default. Increments `gw_credit_default_ceiling_used_total`.
    pub default_output_ceiling_tokens: u32,
    /// How long Go should treat a Rust-refreshed user as active.
    pub active_registration_ttl_secs: u64,
    /// Minimum per-user interval before Rust enqueues another refresh.
    pub active_registration_refresh_min_interval_secs: u64,
    /// Bounded worker channel size. Queue overflow never rejects admitted requests.
    pub active_registration_queue_capacity: usize,
}

impl Default for ProtectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lease_grace_ms: 30_000,
            default_output_ceiling_tokens: 8192,
            active_registration_ttl_secs: 300,
            active_registration_refresh_min_interval_secs: 30,
            active_registration_queue_capacity: 8192,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enables_hard_mode_protection() {
        let cfg = ProtectionConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 30_000);
        assert_eq!(cfg.default_output_ceiling_tokens, 8192);
        assert_eq!(cfg.active_registration_ttl_secs, 300);
        assert_eq!(cfg.active_registration_refresh_min_interval_secs, 30);
        assert_eq!(cfg.active_registration_queue_capacity, 8192);
    }

    #[test]
    fn deserialize_partial_overrides_keep_defaults() {
        let toml_text = r#"
            default_output_ceiling_tokens = 4096
            active_registration_refresh_min_interval_secs = 7
        "#;
        let cfg: ProtectionConfig = toml::from_str(toml_text).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 30_000);
        assert_eq!(cfg.default_output_ceiling_tokens, 4096);
        assert_eq!(cfg.active_registration_ttl_secs, 300);
        assert_eq!(cfg.active_registration_refresh_min_interval_secs, 7);
        assert_eq!(cfg.active_registration_queue_capacity, 8192);
    }
}
