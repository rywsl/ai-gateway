//! Sticky token cache for Codex `x-codex-turn-state`. Keyed by
//! `(key_id, conversation_id)`, TTL configurable (spec default 5 min).

use moka::future::Cache;
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
                let entry = self
                    .inner
                    .entry(k)
                    .or_insert_with(async { crate::impersonate::rng::random_hex(32) })
                    .await;
                let event = if entry.is_fresh() { "miss" } else { "hit" };
                metrics::counter!("gw_egress_identity_cache_total", "kind" => "turn_state", "event" => event).increment(1);
                entry.into_value()
            }
            None => crate::impersonate::rng::random_hex(32),
        }
    }
}
