//! `Sticky` strategy: route every request with the same `conversation_id` to
//! the same key, refreshing the binding's TTL on each hit.
//!
//! TTL: 30 minutes. Entries past the TTL are recomputed on next access.
//! Without a `conversation_id`, falls back to the first available candidate.

use super::*;
use dashmap::DashMap;
use gw_core::ids::KeyId;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const STICKY_TTL: Duration = Duration::from_secs(30 * 60);

pub struct Sticky {
    /// `conversation_id → (key_id, inserted_at)`.
    ///
    /// TODO(v2): periodic `sweep_expired()` or migrate to `moka::Cache` with TTL.
    /// Today this `DashMap` grows monotonically — entries past `STICKY_TTL` are
    /// re-bound on access but never evicted, so abandoned conversations leak.
    /// Background sweeping requires a task driver not yet wired in this crate;
    /// the gw-gateway binary in Phase 5 will own the sweep loop.
    pub bindings: DashMap<String, (KeyId, Instant)>,
}

impl Default for Sticky {
    fn default() -> Self {
        Self::new()
    }
}

impl Sticky {
    pub fn new() -> Self {
        Self {
            bindings: DashMap::new(),
        }
    }

    fn hash_conv(conv: &str, n: usize) -> usize {
        let mut hasher = Sha256::new();
        hasher.update(conv.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        let h = u64::from_le_bytes(bytes);
        (h % n as u64) as usize
    }
}

impl Strategy for Sticky {
    fn name(&self) -> &'static str {
        "sticky"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        if c.is_empty() {
            return None;
        }
        let Some(conv) = ctx.conversation_id else {
            // No conversation context: fall back to first available.
            return c
                .iter()
                .find(|k| h.is_available(&k.key_id, ctx.api_format));
        };

        // Look up an existing fresh binding.
        let now = Instant::now();
        if let Some(entry) = self.bindings.get(conv) {
            let (kid, ts) = entry.value();
            if now.duration_since(*ts) < STICKY_TTL {
                if let Some(k) = c.iter().find(|k| &k.key_id == kid) {
                    if h.is_available(&k.key_id, ctx.api_format) {
                        return Some(k);
                    }
                }
                // Bound key is gone or unavailable; fall through to rebind.
            }
        }

        // Compute a fresh binding over the available set.
        let available: Vec<&Arc<VendorKey>> = c
            .iter()
            .filter(|k| h.is_available(&k.key_id, ctx.api_format))
            .collect();
        if available.is_empty() {
            return None;
        }
        let idx = Self::hash_conv(conv, available.len());
        let chosen = available[idx];
        self.bindings
            .insert(conv.to_string(), (chosen.key_id.clone(), now));
        c.iter().find(|k| k.key_id == chosen.key_id)
    }
}
