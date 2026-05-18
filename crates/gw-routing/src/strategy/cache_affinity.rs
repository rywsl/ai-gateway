//! `CacheAffinity` strategy: deterministically map a request fingerprint
//! (`model` plus optional message-prefix hash) to one candidate. Same
//! fingerprint → same key, maximizing upstream prompt-cache hit rate.
//!
//! Hash function: SHA-256 over `model_name || ":" || message_hash`, then take
//! the first 8 bytes as a little-endian `u64` and reduce modulo the count of
//! available candidates.

use super::*;
use sha2::{Digest, Sha256};

pub struct CacheAffinity;

impl Strategy for CacheAffinity {
    fn name(&self) -> &'static str {
        "cache_affinity"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        b: &BreakerStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        // Build a filtered slice of available candidates so the hash maps onto
        // a stable, contiguous index space.
        let available: Vec<&Arc<VendorKey>> = c
            .iter()
            .filter(|k| b.get_or_create(&k.key_id).is_available())
            .collect();
        if available.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        hasher.update(ctx.model.as_ref().as_bytes());
        hasher.update(b":");
        if let Some(h) = ctx.message_hash {
            hasher.update(h.to_le_bytes());
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        let h = u64::from_le_bytes(bytes);
        let idx = (h % available.len() as u64) as usize;

        // We need to return a reference into the original `c` slice (`&'a`),
        // not into our temporary `available` vec.
        let chosen_key_id = &available[idx].key_id;
        c.iter().find(|k| &k.key_id == chosen_key_id)
    }
}
