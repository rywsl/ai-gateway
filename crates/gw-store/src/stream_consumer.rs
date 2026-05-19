//! Stream consumer (ADR-007) — XREADGROUP poll + invalidate-on-event.
//!
//! On boot we create two consumer groups (`events:entity` / `events:counter`)
//! anchored at `$` so a fresh pod only sees events published after it started.
//! The cold-load chain in `EntityStore::*_or_load` covers the gap.
//!
//! Each event carries `{ns, id}` (entity stream) or `{scope, id, field, delta}`
//! (counter stream). For entity events we **invalidate** the matching cache
//! entry; the next `*_or_load` call will cold-load the latest version. v1
//! ignores `v` (the version hint) — invalidate-then-reload is simpler than
//! version-aware upsert and matches Moka's strong read-after-invalidate
//! consistency. Counter events are observability-only in v1 and are dropped
//! after ACK.

use super::entity_store::EntityStore;
use super::redis_client::RedisCtx;
use fred::interfaces::StreamsInterface;
use fred::types::XReadResponse;
use gw_core::ids::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

pub struct StreamConsumer {
    pub redis: RedisCtx,
    pub store: Arc<EntityStore>,
    pub instance_id: String,
}

const ENTITY_STREAM: &str = "events:entity";
const COUNTER_STREAM: &str = "events:counter";
const BLOCK_MS: u64 = 200;
const COUNT_PER_POLL: u64 = 64;

impl StreamConsumer {
    pub async fn start(self: Arc<Self>) -> anyhow::Result<JoinHandle<()>> {
        let client = self.redis.pool.next();
        // mkstream=true so the stream is created if missing; tolerate BUSYGROUP.
        let _: Result<(), _> = client
            .xgroup_create(ENTITY_STREAM, &*self.instance_id, "$", true)
            .await;
        let _: Result<(), _> = client
            .xgroup_create(COUNTER_STREAM, &*self.instance_id, "$", true)
            .await;
        let me = self.clone();
        let h = tokio::spawn(async move {
            me.consume_loop().await;
        });
        Ok(h)
    }

    async fn consume_loop(&self) {
        let consumer_name = format!("{}-consumer", self.instance_id);
        loop {
            let client = self.redis.pool.next();
            // XReadResponse<K1, I, K2, V> = HashMap<K1, Vec<(I, HashMap<K2, V>)>>
            let read: Result<XReadResponse<String, String, String, String>, _> = client
                .xreadgroup_map(
                    &*self.instance_id,
                    &consumer_name,
                    Some(COUNT_PER_POLL),
                    Some(BLOCK_MS),
                    // noack=false: we want entries delivered into the pending-entries list so
                    // our explicit xack below confirms processing. Flipping this to true would
                    // drop the PEL safety net.
                    false,
                    vec![ENTITY_STREAM, COUNTER_STREAM],
                    vec![">", ">"],
                )
                .await;

            let entries = match read {
                Ok(map) => map,
                Err(e) => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        error = %e,
                        "xreadgroup failed; backing off"
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };

            for (stream, stream_entries) in entries {
                for (entry_id, fields) in stream_entries {
                    self.apply(&stream, &fields).await;
                    // ACK regardless of apply outcome — a malformed event
                    // should not block forward progress. `apply` itself logs.
                    let ack: Result<u64, _> = client
                        .xack(stream.as_str(), &*self.instance_id, entry_id.clone())
                        .await;
                    if let Err(e) = ack {
                        tracing::warn!(
                            target: "gw_store::stream_consumer",
                            error = %e,
                            stream = %stream,
                            entry_id = %entry_id,
                            "xack failed; will be redelivered"
                        );
                    }
                }
            }
        }
    }

    async fn apply(&self, stream: &str, fields: &HashMap<String, String>) {
        if stream == COUNTER_STREAM {
            // v1: counter events are observability-only; no-op.
            return;
        }
        // entity events: expect {ns, id}.
        let ns = fields.get("ns").map(String::as_str);
        let id = fields.get("id").map(String::as_str);
        let (ns, id) = match (ns, id) {
            (Some(n), Some(i)) => (n, i),
            _ => {
                tracing::warn!(
                    target: "gw_store::stream_consumer",
                    fields = ?fields,
                    "entity event missing ns/id; skipping"
                );
                return;
            }
        };
        tracing::debug!(
            target: "gw_store::stream_consumer",
            ns = %ns,
            id = %id,
            "invalidating entity"
        );
        match ns {
            "user" => self.store.user.invalidate(&UserId::from(id)).await,
            "user_group" => {
                self.store
                    .user_group
                    .invalidate(&UserGroupId::from(id))
                    .await
            }
            "apikey" => self.store.apikey.invalidate(&ApiKeyId::from(id)).await,
            "vendor" => self.store.vendor.invalidate(&VendorId::from(id)).await,
            "vendor_key" => self.store.vendor_key.invalidate(&KeyId::from(id)).await,
            "model_group" => self.store.model_group.invalidate(&Group::from(id)).await,
            "pricing" => self.store.pricing.invalidate(&ModelName::from(id)).await,
            "jwks" => self.store.jwks.invalidate(&Kid::from(id)).await,
            "ipblock" => self.store.ipblock.invalidate(&id.to_string()).await,
            other => {
                tracing::warn!(
                    target: "gw_store::stream_consumer",
                    ns = %other,
                    "unknown entity namespace; skipping"
                );
            }
        }
    }
}
