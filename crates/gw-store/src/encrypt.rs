//! AES-256-GCM key encryption (ADR-017).
//!
//! Vendor api_key plaintext is encrypted at rest in PostgreSQL with a KEK keyed
//! by `kek_kid`. The cache holds the KEK material in process memory.
//!
//! KEK rotation (G-α.6 / SE3) follows a dual-read state machine:
//!
//! ```text
//! Healthy { active: K_old }
//!   --enter_dual_read(K_new)-->
//! DualRead { active: K_new, reencrypting: false }
//!   --mark_reencrypting()-->
//! DualRead { active: K_new, reencrypting: true }
//!   --drop_old_kek(K_old)-->
//! Healthy { active: K_new }
//! ```
//!
//! Constraints: at most 2 KEKs concurrent, never drop the only remaining KEK,
//! `active_kid` must always be in the resident set.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use arc_swap::ArcSwap;
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct KekCache {
    inner: Arc<KekCacheInner>,
}

impl Default for KekCache {
    fn default() -> Self {
        Self::empty()
    }
}

struct KekCacheInner {
    keks: HashMap<String, [u8; 32]>,
    active_kid: ArcSwap<String>,
    state: ArcSwap<KekState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KekState {
    Healthy,
    DualRead { reencrypting: bool },
}

impl KekCache {
    /// Construct a cache from a KEK map. The active kid is the first key
    /// returned by the map's iterator (HashMap iteration order is unspecified
    /// but stable for a single map). Use [`KekCache::with_active`] to pin a
    /// specific active kid.
    pub fn new(map: HashMap<String, [u8; 32]>) -> Self {
        Self::with_active(map, None)
    }

    /// Construct a cache and pin the active kid. If `active` is `None`, the
    /// first key in the map (iteration order) becomes active; if the map is
    /// empty the active kid is the empty string.
    pub fn with_active(map: HashMap<String, [u8; 32]>, active: Option<&str>) -> Self {
        let active = active
            .map(String::from)
            .or_else(|| map.keys().next().cloned())
            .unwrap_or_default();
        Self {
            inner: Arc::new(KekCacheInner {
                keks: map,
                active_kid: ArcSwap::from_pointee(active),
                state: ArcSwap::from_pointee(KekState::Healthy),
            }),
        }
    }

    pub fn empty() -> Self {
        Self::new(HashMap::new())
    }

    pub(crate) fn get(&self, kid: &str) -> Option<&[u8; 32]> {
        self.inner.keks.get(kid)
    }

    pub fn active_kid(&self) -> String {
        self.inner.active_kid.load_full().as_ref().clone()
    }

    pub fn state(&self) -> KekState {
        self.inner.state.load_full().as_ref().clone()
    }

    /// Enter dual-read: add new KEK to the resident set, switch active to new.
    /// Refuses if already in DualRead (max 2 KEKs concurrent).
    pub fn enter_dual_read(&mut self, new_kid: String, new_kek: [u8; 32]) -> anyhow::Result<()> {
        if matches!(self.state(), KekState::DualRead { .. }) {
            anyhow::bail!("already in dual-read");
        }
        if self.inner.keks.contains_key(&new_kid) {
            anyhow::bail!("kid {new_kid} already resident");
        }
        let mut new_map = self.inner.keks.clone();
        new_map.insert(new_kid.clone(), new_kek);
        self.inner = Arc::new(KekCacheInner {
            keks: new_map,
            active_kid: ArcSwap::from_pointee(new_kid),
            state: ArcSwap::from_pointee(KekState::DualRead {
                reencrypting: false,
            }),
        });
        Ok(())
    }

    /// Mark the dual-read state as actively re-encrypting (informational flag
    /// for observers; does not gate any internal decisions).
    pub fn mark_reencrypting(&self) {
        self.inner
            .state
            .store(Arc::new(KekState::DualRead { reencrypting: true }));
    }

    /// Drop an old KEK and return to Healthy. Refuses unless we're in
    /// DualRead, refuses to drop the only remaining KEK, and refuses to drop
    /// the currently-active KEK.
    pub fn drop_old_kek(&mut self, old_kid: &str) -> anyhow::Result<()> {
        if !matches!(self.state(), KekState::DualRead { .. }) {
            anyhow::bail!("not in dual-read");
        }
        let active = self.active_kid();
        if old_kid == active {
            anyhow::bail!("refusing to drop active kid {old_kid}");
        }
        let mut new_map = self.inner.keks.clone();
        new_map.remove(old_kid);
        if new_map.is_empty() {
            anyhow::bail!("cannot drop the only kek");
        }
        if !new_map.contains_key(&active) {
            anyhow::bail!("active_kid not in remaining set");
        }
        self.inner = Arc::new(KekCacheInner {
            keks: new_map,
            active_kid: ArcSwap::from_pointee(active),
            state: ArcSwap::from_pointee(KekState::Healthy),
        });
        Ok(())
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
