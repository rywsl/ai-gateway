//! AES-256-GCM key encryption (ADR-017).
//!
//! Vendor api_key plaintext is encrypted at rest in PostgreSQL with a KEK keyed
//! by `kek_kid`. The cache holds the KEK material in process memory.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct KekCache(pub Arc<HashMap<String, [u8; 32]>>);

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct EncryptedKey {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub auth_tag: Vec<u8>,
    pub kek_kid: String,
}

pub fn decrypt(enc: &EncryptedKey, cache: &KekCache) -> anyhow::Result<SecretString> {
    let kek = cache
        .0
        .get(&enc.kek_kid)
        .ok_or_else(|| anyhow::anyhow!("kek not found: {}", enc.kek_kid))?;
    let cipher = Aes256Gcm::new(kek.into());
    let nonce = Nonce::from_slice(&enc.nonce);
    let mut combined = enc.ciphertext.clone();
    combined.extend_from_slice(&enc.auth_tag);
    let pt = cipher
        .decrypt(nonce, combined.as_ref())
        .map_err(|_| anyhow::anyhow!("decrypt failed"))?;
    let s = String::from_utf8(pt).map_err(|_| anyhow::anyhow!("utf8"))?;
    Ok(SecretString::from(s))
}

/// Test-only helper for round-trip tests. Not gated to `#[cfg(test)]` because
/// integration tests under `tests/` compile against the crate's public API as
/// a separate binary and would not see `cfg(test)` items.
pub fn encrypt_for_test(plain: &str, kek: [u8; 32]) -> EncryptedKey {
    use aes_gcm::aead::{AeadCore, OsRng};
    let cipher = Aes256Gcm::new(&kek.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plain.as_bytes()).unwrap();
    // aes-gcm 0.10 returns ciphertext || tag (last 16 bytes are the tag).
    let split_at = ct.len() - 16;
    let (cipher_only, tag) = ct.split_at(split_at);
    EncryptedKey {
        ciphertext: cipher_only.to_vec(),
        nonce: nonce.to_vec(),
        auth_tag: tag.to_vec(),
        kek_kid: "test".to_string(),
    }
}
