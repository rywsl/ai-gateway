//! Deterministic RNG seeded by SHA-256(key_id) — used to pick per-VendorKey
//! identity fields from the profile pools so the same key always projects the
//! same impersonation identity (matches Aether's per-ProviderAPIKey fingerprint).

use rand::{rngs::StdRng, RngCore, SeedableRng};
use sha2::{Digest, Sha256};

/// Build a deterministic 64-bit-seeded `StdRng` from any byte-like seed.
pub fn seeded_rng(seed: impl AsRef<[u8]>) -> StdRng {
    let digest = Sha256::digest(seed.as_ref());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    StdRng::from_seed(bytes)
}

/// Generate a cryptographic random hex string of `bytes` random bytes
/// (producing `bytes * 2` hex characters). Used by the Codex
/// `x-codex-turn-state` fallback and by `TurnStateCache` for fresh tokens.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::seeded_rng;
    use rand::RngCore;

    #[test]
    fn same_seed_produces_identical_stream() {
        let mut a = seeded_rng("vk-abc");
        let mut b = seeded_rng("vk-abc");
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seed_produces_different_stream() {
        let mut a = seeded_rng("vk-abc");
        let mut b = seeded_rng("vk-def");
        let mut diverged = false;
        for _ in 0..16 {
            if a.next_u64() != b.next_u64() {
                diverged = true;
                break;
            }
        }
        assert!(diverged, "different seeds must diverge within 16 draws");
    }
}
