//! Sticky token cache for Codex `x-codex-turn-state`. Keyed by
//! `(key_id, conversation_id)`, TTL configurable (spec default 5 min).

use moka::future::Cache;
use rand::RngCore;
use std::time::Duration;

pub struct TurnStateCache {
    inner: Cache<(String, String), String>,
}

impl TurnStateCache {
    pub fn new(ttl: Duration, capacity: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .build();
        Self { inner }
    }

    /// `None` conversation_id -> no cache binding (fresh token per call —
    /// matches "non-conversational request" semantics).
    pub async fn get_or_create(&self, key_id: &str, conversation_id: Option<&str>) -> String {
        match conversation_id {
            Some(cid) => {
                let k = (key_id.to_owned(), cid.to_owned());
                self.inner.get_with(k, async { random_hex(32) }).await
            }
            None => random_hex(32),
        }
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}
