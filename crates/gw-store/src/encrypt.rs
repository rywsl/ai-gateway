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
pub struct KekCache(Arc<HashMap<String, [u8; 32]>>);

impl KekCache {
    pub fn new(map: HashMap<String, [u8; 32]>) -> Self {
        Self(Arc::new(map))
    }
    pub fn empty() -> Self {
        Self(Arc::new(HashMap::new()))
    }
    pub(crate) fn get(&self, kid: &str) -> Option<&[u8; 32]> {
        self.0.get(kid)
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct EncryptedKey {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub auth_tag: Vec<u8>,
    pub kek_kid: String,
}

pub fn decrypt(enc: &EncryptedKey, cache: &KekCache) -> anyhow::Result<SecretString> {
    let kek = cache
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

/// FOR TESTS ONLY: encrypts plain into an EncryptedKey using the provided KEK.
/// Do not call from production code — production data must come through KMS.
/// This function is exposed for integration tests; future versions may gate
/// this behind a `test-utils` cargo feature.
#[doc(hidden)]
pub fn encrypt_for_test_only(plain: &str, kek: [u8; 32]) -> EncryptedKey {
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
