//! KEK material bootstrap.
//!
//! v1 supports a dev path only: the 32-byte KEK is supplied as 64 hex chars
//! via the `GW_KMS__KEK_HEX` env var (loaded into [`crate::config::KmsCfg`]).
//! Production wiring against a real KMS provider (AWS KMS, GCP KMS, Vault) is
//! a v2 task — the function returns a clear error if `kek_hex` is missing so
//! mis-configuration fails fast at startup rather than at first decrypt.

use crate::config::Config;
use anyhow::Context;
use gw_store::encrypt::KekCache;
use secrecy::ExposeSecret;
use std::collections::HashMap;

pub async fn init(cfg: &Config) -> anyhow::Result<KekCache> {
    let kek_hex = cfg
        .kms
        .kek_hex
        .as_ref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no KEK configured: set GW_KMS__KEK_HEX to a 64-char hex string \
                 (dev mode) or wire a production KMS provider"
            )
        })?;

    let raw = hex::decode(kek_hex.expose_secret())
        .context("GW_KMS__KEK_HEX is not valid hex")?;

    let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "GW_KMS__KEK_HEX must decode to exactly 32 bytes (got {})",
            raw.len()
        )
    })?;

    let mut map = HashMap::with_capacity(1);
    map.insert(cfg.kms.key_id.clone(), bytes);
    Ok(KekCache::new(map))
}
