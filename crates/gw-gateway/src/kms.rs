//! KEK material bootstrap.
//!
//! The active 32-byte KEK is supplied as 64 hex chars via `GW_KMS__KEK_HEX`
//! (loaded into [`crate::config::KmsCfg`]). During a rotation window,
//! deployments may also supply `GW_KMS__PREVIOUS_KEK_HEX` and
//! `GW_KMS__PREVIOUS_KEY_ID`; both KEKs are loaded so ciphertext written under
//! either kid remains readable after a restart.
//!
//! Production wiring against a real KMS provider (AWS KMS, GCP KMS, Vault) is
//! a v2 task. Missing or malformed KEK material fails fast at startup rather
//! than at first decrypt.

use crate::config::{Config, KmsCfg};
use anyhow::Context;
use gw_store::encrypt::KekCache;
use secrecy::ExposeSecret;
use std::collections::HashMap;

pub async fn init(cfg: &Config) -> anyhow::Result<KekCache> {
    build_kek_cache(&cfg.kms)
}

fn build_kek_cache(kms: &KmsCfg) -> anyhow::Result<KekCache> {
    let kek_hex = kms.kek_hex.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "no KEK configured: set GW_KMS__KEK_HEX to a 64-char hex string \
                 (dev mode) or wire a production KMS provider"
        )
    })?;
    let active = decode_kek_hex("GW_KMS__KEK_HEX", kek_hex)?;

    match (&kms.previous_kek_hex, &kms.previous_key_id) {
        (None, None) => Ok(KekCache::with_active(
            HashMap::from([(kms.key_id.clone(), active)]),
            Some(&kms.key_id),
        )),
        (Some(_), None) => {
            anyhow::bail!(
                "GW_KMS__PREVIOUS_KEK_HEX requires GW_KMS__PREVIOUS_KEY_ID during KEK rotation"
            )
        }
        (None, Some(_)) => {
            anyhow::bail!(
                "GW_KMS__PREVIOUS_KEY_ID requires GW_KMS__PREVIOUS_KEK_HEX during KEK rotation"
            )
        }
        (Some(previous_hex), Some(previous_key_id)) => {
            if previous_key_id == &kms.key_id {
                anyhow::bail!("GW_KMS__PREVIOUS_KEY_ID must differ from GW_KMS__KEY_ID");
            }
            let previous = decode_kek_hex("GW_KMS__PREVIOUS_KEK_HEX", previous_hex)?;
            let mut cache = KekCache::with_active(
                HashMap::from([(previous_key_id.clone(), previous)]),
                Some(previous_key_id),
            );
            cache.enter_dual_read(kms.key_id.clone(), active)?;
            Ok(cache)
        }
    }
}

fn decode_kek_hex(env_name: &str, secret: &secrecy::SecretString) -> anyhow::Result<[u8; 32]> {
    let raw = hex::decode(secret.expose_secret())
        .with_context(|| format!("{env_name} is not valid hex"))?;

    raw.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "{env_name} must decode to exactly 32 bytes (got {})",
            raw.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_store::encrypt::KekState;
    use secrecy::SecretString;

    fn secret(hex: &str) -> SecretString {
        SecretString::from(hex.to_string())
    }

    fn cfg(active_id: &str, active_hex: &str) -> KmsCfg {
        KmsCfg {
            kek_hex: Some(secret(active_hex)),
            key_id: active_id.to_string(),
            previous_kek_hex: None,
            previous_key_id: None,
        }
    }

    #[test]
    fn build_kek_cache_single_active_kek() {
        let cache = build_kek_cache(&cfg("primary", &"ab".repeat(32))).unwrap();

        assert_eq!(cache.active_kid(), "primary");
        assert!(cache.contains_kid("primary"));
        assert_eq!(cache.resident_kid_count(), 1);
        assert_eq!(cache.state(), KekState::Healthy);
    }

    #[test]
    fn build_kek_cache_dual_read_with_previous_kek() {
        let mut kms = cfg("new", &"cd".repeat(32));
        kms.previous_key_id = Some("old".to_string());
        kms.previous_kek_hex = Some(secret(&"ab".repeat(32)));

        let cache = build_kek_cache(&kms).unwrap();

        assert_eq!(cache.active_kid(), "new");
        assert!(cache.contains_kid("old"));
        assert!(cache.contains_kid("new"));
        assert_eq!(cache.resident_kid_count(), 2);
        assert_eq!(
            cache.state(),
            KekState::DualRead {
                reencrypting: false
            }
        );
    }

    #[test]
    fn build_kek_cache_requires_previous_pair() {
        let mut only_hex = cfg("new", &"cd".repeat(32));
        only_hex.previous_kek_hex = Some(secret(&"ab".repeat(32)));
        assert!(build_kek_cache(&only_hex)
            .unwrap_err()
            .to_string()
            .contains("PREVIOUS_KEY_ID"));

        let mut only_id = cfg("new", &"cd".repeat(32));
        only_id.previous_key_id = Some("old".to_string());
        assert!(build_kek_cache(&only_id)
            .unwrap_err()
            .to_string()
            .contains("PREVIOUS_KEK_HEX"));
    }

    #[test]
    fn build_kek_cache_rejects_same_previous_and_active_kid() {
        let mut kms = cfg("same", &"cd".repeat(32));
        kms.previous_key_id = Some("same".to_string());
        kms.previous_kek_hex = Some(secret(&"ab".repeat(32)));

        assert!(build_kek_cache(&kms)
            .unwrap_err()
            .to_string()
            .contains("must differ"));
    }

    #[test]
    fn decode_kek_hex_rejects_wrong_length() {
        let err = decode_kek_hex("GW_KMS__KEK_HEX", &secret("abcd"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("exactly 32 bytes"));
    }
}
