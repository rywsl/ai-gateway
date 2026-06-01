//! Stream consumer (ADR-007) — XREADGROUP poll + invalidate-on-event.
//!
//! On boot we create two consumer groups (`events:entity` / `events:counter`)
//! anchored at `$` so a fresh pod only sees events published after it started.
//! The cold-load chain in `EntityStore::*_or_load` covers the gap.
//!
//! Each event carries `{ns, id}` (entity stream) or `{ns, ...}` (counter
//! stream). For entity events we **invalidate** the matching cache entry; the
//! next `*_or_load` call will cold-load the latest version. v1 ignores `v`
//! (the version hint) — invalidate-then-reload is simpler than version-aware
//! upsert and matches Moka's strong read-after-invalidate consistency. Counter
//! events are routed to the registered [`counter_dispatch::CounterHandler`]
//! (installed at boot by `gw-ledger`); no handler → benign warn-and-drop.

use super::entity_store::EntityStore;
use super::redis_client::RedisCtx;
use fred::interfaces::StreamsInterface;
use fred::types::{RedisValue, XReadResponse};
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
    async fn ensure_groups(&self) -> Result<(), anyhow::Error> {
        let client = self.redis.pool.next();
        // mkstream=true so the stream is created if missing; tolerate BUSYGROUP.
        let _: Result<(), _> = client
            .xgroup_create(ENTITY_STREAM, &*self.instance_id, "$", true)
            .await;
        let _: Result<(), _> = client
            .xgroup_create(COUNTER_STREAM, &*self.instance_id, "$", true)
            .await;
        Ok(())
    }

    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        crate::managed_loop::Supervisor::spawn(self)
    }

    async fn apply(&self, stream: &str, fields: &HashMap<String, String>) {
        if stream == COUNTER_STREAM {
            let outcome = crate::counter_dispatch::dispatch(fields);
            match outcome {
                crate::counter_dispatch::DispatchOutcome::UnknownNs => {
                    tracing::debug!(
                        target: "gw_store::stream_consumer",
                        ns = ?fields.get("ns"),
                        "events:counter: unknown ns; skipping"
                    );
                }
                crate::counter_dispatch::DispatchOutcome::InvalidFields => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        fields = ?fields,
                        "events:counter: missing/invalid required fields"
                    );
                }
                crate::counter_dispatch::DispatchOutcome::NoHandler => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        "events:counter: no handler registered (gw-ledger handler not installed)"
                    );
                }
                crate::counter_dispatch::DispatchOutcome::Handled => {}
            }
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
        apply_entity_event(&self.store, ns, id).await;
    }
}

/// Dispatch a single `events:entity` invalidation against an [`EntityStore`].
///
/// Extracted from [`StreamConsumer::apply`] so the in-process tests
/// (and any future direct caller) can exercise the per-namespace match
/// without a live Redis connection or consumer struct.
pub(crate) async fn apply_entity_event(store: &Arc<EntityStore>, ns: &str, id: &str) {
    tracing::debug!(
        target: "gw_store::stream_consumer",
        ns = %ns,
        id = %id,
        "invalidating entity"
    );

    // G6 / ADR-G6-1: drop the Redis L1 entry before Moka invalidate so
    // multi-pod deployments don't retain stale entries up to the L1 TTL.
    // Failure is non-fatal — Moka invalidate still runs below.
    if let Some(redis) = store.loader_redis() {
        let redis_key = format!("entity:{ns}:{id}");
        let client = redis.pool.next();
        use fred::interfaces::KeysInterface;
        if let Err(err) = client.del::<i64, _>(&redis_key).await {
            tracing::warn!(
                target: "gw_store::stream_consumer",
                ns = %ns,
                id = %id,
                key = %redis_key,
                error = ?err,
                "events:entity invalidate: redis DEL failed; falling back to Moka invalidate only"
            );
        }
    }

    match ns {
        "user" => store.user.invalidate(&UserId::from(id)).await,
        "user_group" => store.user_group.invalidate(&UserGroupId::from(id)).await,
        "apikey" => store.apikey.invalidate(&ApiKeyId::from(id)).await,
        "vendor" => store.vendor.invalidate(&VendorId::from(id)).await,
        "vendor_key" => store.invalidate_vendor_key(&KeyId::from(id)).await,
        "model_group" => store.model_group.invalidate(&Group::from(id)).await,
        "pricing" => store.pricing.invalidate(&ModelName::from(id)).await,
        "model" => store.model.invalidate(&ModelName::from(id)).await,
        "auth_config" => {
            store.auth_config.invalidate(&id.to_string()).await;
            if id == "global" {
                match store.auth_config_rebuild_hook() {
                    Some(hook) => hook(store.clone(), id.to_string()).await,
                    None => tracing::debug!(
                        target: "gw_store::stream_consumer",
                        id = %id,
                        "auth_config rebuild hook not registered; cache invalidated only"
                    ),
                }
            }
        }
        "jwks" => store.jwks.invalidate(&Kid::from(id)).await,
        "ipblock" => store.ipblock.invalidate(&id.to_string()).await,
        "proxy_node" => {
            store.proxy_node.invalidate(&id.to_string()).await;
        }
        "management_token" => {
            store.invalidate_mgmt_token(id).await;
        }
        "model_mapping" => {
            // id format: "alias|protocol", with protocol matching ProtocolKind's
            // snake_case serde tag ("openai" | "anthropic" | "gemini").
            if let Some((alias, proto)) = id.split_once('|') {
                let p = match proto {
                    "openai" => Some(gw_core::protocol::ProtocolKind::OpenAi),
                    "anthropic" => Some(gw_core::protocol::ProtocolKind::Anthropic),
                    "gemini" => Some(gw_core::protocol::ProtocolKind::Gemini),
                    _ => None,
                };
                if let Some(p) = p {
                    store
                        .model_mapping
                        .invalidate(&(alias.to_string(), p))
                        .await;
                } else {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        id = %id,
                        proto = %proto,
                        "model_mapping event with unknown protocol; skipping"
                    );
                }
            } else {
                tracing::warn!(
                    target: "gw_store::stream_consumer",
                    id = %id,
                    "model_mapping event id missing 'alias|protocol' separator; skipping"
                );
            }
        }
        other => {
            tracing::warn!(
                target: "gw_store::stream_consumer",
                ns = %other,
                "unknown entity namespace; skipping"
            );
        }
    }
}

fn is_nogroup_error(e: &fred::error::RedisError) -> bool {
    e.to_string().contains("NOGROUP")
}

use crate::managed_loop::{ManagedLoop, Outcome, RecoverReason};

#[async_trait::async_trait]
impl ManagedLoop for StreamConsumer {
    fn name(&self) -> &'static str {
        "stream_consumer"
    }

    async fn ensure(&self) -> Result<(), anyhow::Error> {
        self.ensure_groups().await
    }

    async fn tick(&self) -> Outcome {
        let consumer_name = format!("{}-consumer", self.instance_id);
        let client = self.redis.pool.next();
        // Raw xreadgroup: RESP Null on BLOCK timeout is handled explicitly.
        let raw: Result<RedisValue, fred::error::RedisError> = client
            .xreadgroup(
                &*self.instance_id,
                &consumer_name,
                Some(COUNT_PER_POLL),
                Some(BLOCK_MS),
                false,
                vec![ENTITY_STREAM, COUNTER_STREAM],
                vec![">", ">"],
            )
            .await;

        let value = match raw {
            Ok(v) => v,
            Err(e) if is_nogroup_error(&e) => {
                return Outcome::Recover(RecoverReason::UpstreamResourceGone("NOGROUP"));
            }
            Err(_e) => {
                tracing::warn!(
                    target: "gw_store::stream_consumer",
                    error = %_e,
                    "xreadgroup failed; backing off"
                );
                return Outcome::Sleep(Duration::from_millis(500));
            }
        };

        if value.is_null() {
            return Outcome::Continue;
        }
        if let RedisValue::Array(ref a) = value {
            if a.is_empty() {
                return Outcome::Continue;
            }
        }

        let entries: XReadResponse<String, String, String, String> =
            match value.into_xread_response() {
                Ok(m) => m,
                Err(_e) => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        error = %_e,
                        "xreadgroup non-null reply failed to parse; backing off"
                    );
                    return Outcome::Sleep(Duration::from_millis(500));
                }
            };

        for (stream, stream_entries) in entries {
            for (entry_id, fields) in stream_entries {
                self.apply(&stream, &fields).await;
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
        Outcome::Continue
    }
}

/// Test-only helper: drive [`apply_entity_event`] against an [`EntityStore`]
/// without needing a live [`StreamConsumer`] or Redis connection.
#[doc(hidden)]
pub async fn apply_inline_for_test_helper(store: &Arc<EntityStore>, ns: &str, id: &str) {
    apply_entity_event(store, ns, id).await;
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use gw_core::entity::Model;
    use gw_core::versioned::Versioned;
    use std::sync::Arc;

    #[tokio::test]
    async fn apply_entity_event_invalidates_model() {
        let store = Arc::new(EntityStore::new());
        let model_name = ModelName::from("gpt-test");
        let model = Model {
            name: model_name.clone(),
            capabilities: vec!["chat".to_string()],
        };
        let versioned = Arc::new(Versioned {
            version: 1,
            value: Arc::new(model),
        });

        store.model.insert(model_name.clone(), versioned).await;
        assert!(
            store.model.get(&model_name).await.is_some(),
            "pre-condition: cache entry must be present before invalidation"
        );

        apply_entity_event(&store, "model", "gpt-test").await;

        assert!(
            store.model.get(&model_name).await.is_none(),
            "cache entry must be absent after invalidation"
        );
    }
}

#[cfg(test)]
mod proxy_node_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use gw_core::entity::ProxyNode;
    use gw_core::versioned::Versioned;
    use std::sync::Arc;

    fn make_proxy_node(id: &str) -> ProxyNode {
        ProxyNode {
            id: id.to_string(),
            ws_url: "ws://127.0.0.1:9999/tunnel".parse().unwrap(),
            enabled: true,
            latency_ms: 0,
            allowed_provider_ids: vec![],
            allowed_vendor_key_ids: vec![],
            auth_token_enc: None,
        }
    }

    /// Pre-populate `store.proxy_node` with a fake `ProxyNode`, call
    /// `apply_entity_event`, and assert the cache key is then absent.
    #[tokio::test]
    async fn proxy_node_invalidation_removes_cache_entry() {
        let store = Arc::new(EntityStore::new());
        let node = make_proxy_node("node-1");
        let versioned = Arc::new(Versioned {
            version: 1,
            value: Arc::new(node),
        });

        // Pre-populate the cache.
        store
            .proxy_node
            .insert("node-1".to_string(), versioned)
            .await;
        assert!(
            store.proxy_node.get("node-1").await.is_some(),
            "pre-condition: cache entry must be present before invalidation"
        );

        // Fire the invalidation.
        apply_entity_event(&store, "proxy_node", "node-1").await;

        // The cache entry must be gone.
        assert!(
            store.proxy_node.get("node-1").await.is_none(),
            "cache entry must be absent after invalidation"
        );
    }
}

#[cfg(test)]
mod mgmt_token_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use gw_core::entity::{KeyStatus, ManagementTokenEntry, MgmtScope};
    use gw_core::ids::UserId;
    use gw_core::versioned::Versioned;
    use std::sync::Arc;

    fn make_mgmt_token(token_id: &str) -> ManagementTokenEntry {
        ManagementTokenEntry {
            token_id: token_id.to_string(),
            user_id: UserId::from("u-1"),
            scope: MgmtScope::AdminRead,
            hash: vec![0u8; 32],
            expires_at: None,
            status: KeyStatus::Active,
            display_name: "test-token".to_string(),
        }
    }

    /// Pre-populate `store.mgmt_token` with a fake entry, dispatch a
    /// `ns=management_token` event, and assert the cache entry is gone.
    /// Mirrors `proxy_node_invalidation_removes_cache_entry`.
    #[tokio::test]
    async fn apply_entity_event_invalidates_mgmt_token() {
        let store = Arc::new(EntityStore::new());
        let entry = make_mgmt_token("tk-1");
        let versioned = Arc::new(Versioned {
            version: 1,
            value: Arc::new(entry),
        });

        store.mgmt_token.insert("tk-1".to_string(), versioned).await;
        assert!(
            store.mgmt_token.get("tk-1").await.is_some(),
            "pre-condition: cache entry must be present before invalidation"
        );

        apply_entity_event(&store, "management_token", "tk-1").await;

        assert!(
            store.mgmt_token.get("tk-1").await.is_none(),
            "cache entry must be absent after invalidation"
        );
    }
}

#[cfg(test)]
mod vendor_key_hook_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_entity_store_invalidation_evicts() {
        let mut store_inner = EntityStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        store_inner.on_vendor_key_invalidate(Arc::new(move |key| {
            if key.as_ref() == "vk-evict-hook" {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let store = Arc::new(store_inner);

        apply_inline_for_test_helper(&store, "vendor_key", "vk-evict-hook").await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod auth_config_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn auth_config_global_event_fires_rebuild_hook() {
        let mut store_inner = EntityStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        store_inner.on_auth_config_rebuild(Arc::new(
            move |_store: Arc<EntityStore>,
                  id: String|
                  -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
                let observed = observed.clone();
                Box::pin(async move {
                    assert_eq!(id, "global", "rebuild hook must receive the event id");
                    observed.fetch_add(1, Ordering::SeqCst);
                })
            },
        ));
        let store = Arc::new(store_inner);

        apply_entity_event(&store, "auth_config", "global").await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "ns=auth_config id=global must fire the validator rebuild hook"
        );
    }

    #[tokio::test]
    async fn auth_config_non_global_event_does_not_rebuild() {
        let mut store_inner = EntityStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        store_inner.on_auth_config_rebuild(Arc::new(
            move |_store: Arc<EntityStore>,
                  _id: String|
                  -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
                let observed = observed.clone();
                Box::pin(async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                })
            },
        ));
        let store = Arc::new(store_inner);

        apply_entity_event(&store, "auth_config", "tenant-acme").await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "only id=global rebuilds the global validator"
        );
    }
}
