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
//!
//! ## Sharing semantics
//!
//! `KekCache` is `Clone`, where every clone shares one `Arc<KekCacheInner>`
//! that wraps a single `ArcSwap<KekSnapshot>`. Rotation methods publish a new
//! snapshot atomically — `&self` is sufficient and every existing clone sees
//! the update on its next read. The bootstrap path distributes clones into
//! `LoaderCtx.kek`, the OAuth refresh worker, and the tunnel auth writer; all
//! observe the same `keks` map / `active_kid` / `state`.
//!
//! Concurrent callers of `enter_dual_read` / `drop_old_kek` are not internally
//! serialised — the load → mutate → store sequence is not CAS-protected. KEK
//! rotation in practice is driven by a single KMS control loop, so this
//! one-controller invariant is upheld externally. Multiple concurrent
//! `mark_reencrypting()` callers are safe (single `store` of a fresh snapshot
//! per call).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use arc_swap::ArcSwap;
use secrecy::{ExposeSecret, SecretString};
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

/// Internal shared state. The single `ArcSwap` lets `KekCache::clone()` share
/// state by `Arc<KekCacheInner>` aliasing while still allowing atomic
/// publication of new snapshots from any clone.
struct KekCacheInner {
    snapshot: ArcSwap<KekSnapshot>,
}

/// Atomic-replacement bundle of the three fields that move together during a
/// KEK rotation. Replacing all three in one `store` removes the visibility
/// window where readers could see a fresh `active_kid` against a stale `keks`
/// map (which would surface as `kek not found: <new_kid>`).
struct KekSnapshot {
    keks: Arc<HashMap<String, [u8; 32]>>,
    active_kid: String,
    state: KekState,
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
                snapshot: ArcSwap::from_pointee(KekSnapshot {
                    keks: Arc::new(map),
                    active_kid: active,
                    state: KekState::Healthy,
                }),
            }),
        }
    }

    pub fn empty() -> Self {
        Self::new(HashMap::new())
    }

    /// Look up a KEK by kid. Returns an owned copy because the cache may
    /// atomically swap the underlying map at any time (a borrow would tie the
    /// caller's lifetime to the load guard, which is incompatible with the
    /// `ArcSwap` snapshot pattern). 32 bytes is cheap to copy.
    pub(crate) fn get(&self, kid: &str) -> Option<[u8; 32]> {
        self.inner.snapshot.load().keks.get(kid).copied()
    }

    pub fn active_kid(&self) -> String {
        self.inner.snapshot.load().active_kid.clone()
    }

    pub fn state(&self) -> KekState {
        self.inner.snapshot.load().state.clone()
    }

    /// Enter dual-read: add new KEK to the resident set, switch active to new.
    /// Refuses if already in DualRead (max 2 KEKs concurrent). Publishes
    /// atomically so every `KekCache` clone sees the new snapshot on its next
    /// `get()` / `active_kid()`.
    pub fn enter_dual_read(&self, new_kid: String, new_kek: [u8; 32]) -> anyhow::Result<()> {
        let cur = self.inner.snapshot.load_full();
        if matches!(cur.state, KekState::DualRead { .. }) {
            anyhow::bail!("already in dual-read");
        }
        if cur.keks.contains_key(&new_kid) {
            anyhow::bail!("kid {new_kid} already resident");
        }
        let mut new_map: HashMap<String, [u8; 32]> = (*cur.keks).clone();
        new_map.insert(new_kid.clone(), new_kek);
        self.inner.snapshot.store(Arc::new(KekSnapshot {
            keks: Arc::new(new_map),
            active_kid: new_kid,
            state: KekState::DualRead {
                reencrypting: false,
            },
        }));
        Ok(())
    }

    /// Mark the dual-read state as actively re-encrypting (informational flag
    /// for observers; does not gate any internal decisions).
    pub fn mark_reencrypting(&self) {
        let cur = self.inner.snapshot.load_full();
        self.inner.snapshot.store(Arc::new(KekSnapshot {
            keks: cur.keks.clone(),
            active_kid: cur.active_kid.clone(),
            state: KekState::DualRead { reencrypting: true },
        }));
    }

    /// Drop an old KEK and return to Healthy. Refuses unless we're in
    /// DualRead, refuses to drop the only remaining KEK, and refuses to drop
    /// the currently-active KEK. Publishes atomically.
    pub fn drop_old_kek(&self, old_kid: &str) -> anyhow::Result<()> {
        let cur = self.inner.snapshot.load_full();
        if !matches!(cur.state, KekState::DualRead { .. }) {
            anyhow::bail!("not in dual-read");
        }
        let active = cur.active_kid.clone();
        if old_kid == active {
            anyhow::bail!("refusing to drop active kid {old_kid}");
        }
        let mut new_map: HashMap<String, [u8; 32]> = (*cur.keks).clone();
        new_map.remove(old_kid);
        if new_map.is_empty() {
            anyhow::bail!("cannot drop the only kek");
        }
        if !new_map.contains_key(&active) {
            anyhow::bail!("active_kid not in remaining set");
        }
        self.inner.snapshot.store(Arc::new(KekSnapshot {
            keks: Arc::new(new_map),
            active_kid: active,
            state: KekState::Healthy,
        }));
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

/// AES-256-GCM nonce length. `Nonce::from_slice` panics on a mismatched slice
/// (`assert_eq!` inside `GenericArray::from_slice`), so a corrupted DB row or
/// an out-of-contract Go encrypt would otherwise DoS a Pingora worker.
const AES_GCM_NONCE_LEN: usize = 12;

pub fn decrypt(enc: &EncryptedKey, cache: &KekCache) -> anyhow::Result<SecretString> {
    let kek = cache
        .get(&enc.kek_kid)
        .ok_or_else(|| anyhow::anyhow!("kek not found: {}", enc.kek_kid))?;
    if enc.nonce.len() != AES_GCM_NONCE_LEN {
        anyhow::bail!(
            "invalid nonce length: expected {AES_GCM_NONCE_LEN}, got {}",
            enc.nonce.len()
        );
    }
    let cipher = Aes256Gcm::new(&kek.into());
    let nonce = Nonce::from_slice(&enc.nonce);
    let mut combined = enc.ciphertext.clone();
    combined.extend_from_slice(&enc.auth_tag);
    let pt = cipher
        .decrypt(nonce, combined.as_ref())
        .map_err(|_| anyhow::anyhow!("decrypt failed"))?;
    let s = String::from_utf8(pt).map_err(|_| anyhow::anyhow!("utf8"))?;
    Ok(SecretString::from(s))
}

pub fn encrypt_with_active_kek(
    plain: &SecretString,
    cache: &KekCache,
) -> anyhow::Result<EncryptedKey> {
    use aes_gcm::aead::{AeadCore, OsRng};

    let kid = cache.active_kid();
    let kek = cache
        .get(&kid)
        .ok_or_else(|| anyhow::anyhow!("active kek not found: {kid}"))?;
    let cipher = Aes256Gcm::new(&kek.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plain.expose_secret().as_bytes())
        .map_err(|_| anyhow::anyhow!("encrypt failed"))?;
    let split_at = ct
        .len()
        .checked_sub(16)
        .ok_or_else(|| anyhow::anyhow!("ciphertext too short"))?;
    let (ciphertext, auth_tag) = ct.split_at(split_at);
    Ok(EncryptedKey {
        ciphertext: ciphertext.to_vec(),
        nonce: nonce.to_vec(),
        auth_tag: auth_tag.to_vec(),
        kek_kid: kid,
    })
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

#[cfg(test)]
mod production_encrypt_tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn encrypt_with_active_kek_roundtrips_secret() {
        let kek = [7u8; 32];
        let cache = KekCache::with_active(
            std::collections::HashMap::from([("active".to_string(), kek)]),
            Some("active"),
        );
        let secret = secrecy::SecretString::from("access-token-1");
        let encrypted = encrypt_with_active_kek(&secret, &cache).unwrap();
        assert_eq!(encrypted.kek_kid, "active");
        let decrypted = decrypt(&encrypted, &cache).unwrap();
        assert_eq!(decrypted.expose_secret(), "access-token-1");
    }

    /// Wrong-length nonce must produce an `Err`, never panic — otherwise a
    /// single corrupted DB row crashes the Pingora worker that touches it.
    #[test]
    fn decrypt_rejects_short_nonce_without_panic() {
        let cache = KekCache::with_active(
            std::collections::HashMap::from([("k".to_string(), [0u8; 32])]),
            Some("k"),
        );
        let bad = EncryptedKey {
            ciphertext: vec![1, 2, 3],
            nonce: vec![0u8; 8], // wrong length
            auth_tag: vec![0u8; 16],
            kek_kid: "k".to_string(),
        };
        let err = decrypt(&bad, &cache).expect_err("must not panic");
        assert!(
            err.to_string().contains("invalid nonce length"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decrypt_rejects_long_nonce_without_panic() {
        let cache = KekCache::with_active(
            std::collections::HashMap::from([("k".to_string(), [0u8; 32])]),
            Some("k"),
        );
        let bad = EncryptedKey {
            ciphertext: vec![1, 2, 3],
            nonce: vec![0u8; 16],
            auth_tag: vec![0u8; 16],
            kek_kid: "k".to_string(),
        };
        let err = decrypt(&bad, &cache).expect_err("must not panic");
        assert!(
            err.to_string().contains("invalid nonce length"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decrypt_rejects_empty_nonce_without_panic() {
        let cache = KekCache::with_active(
            std::collections::HashMap::from([("k".to_string(), [0u8; 32])]),
            Some("k"),
        );
        let bad = EncryptedKey {
            ciphertext: vec![1, 2, 3],
            nonce: vec![],
            auth_tag: vec![0u8; 16],
            kek_kid: "k".to_string(),
        };
        let err = decrypt(&bad, &cache).expect_err("must not panic");
        assert!(
            err.to_string().contains("invalid nonce length"),
            "unexpected error: {err}"
        );
    }
}
