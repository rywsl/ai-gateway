//! Stream consumer (ADR-007) — XREADGROUP poll + invalidate-on-event.
//!
//! On boot we create two consumer groups (`events:entity` /
//! `events:counter:pod:<instance_id>`) anchored at `$` so a fresh pod only sees
//! events published after it started.
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
const COUNTER_STREAM_PREFIX: &str = "events:counter:pod";
const BLOCK_MS: u64 = 200;
const COUNT_PER_POLL: u64 = 64;
const PEL_MIN_IDLE_MS: u64 = 30_000;
const PEL_RECLAIM_COUNT: u64 = 32;

pub fn counter_stream_for_instance(instance_id: &str) -> String {
    format!("{COUNTER_STREAM_PREFIX}:{instance_id}")
}

impl StreamConsumer {
    async fn ensure_groups(&self) -> Result<(), anyhow::Error> {
        let client = self.redis.pool.next();
        let counter_stream = self.counter_stream();
        // mkstream=true so the stream is created if missing; tolerate BUSYGROUP.
        let _: Result<(), _> = client
            .xgroup_create(ENTITY_STREAM, &*self.instance_id, "$", true)
            .await;
        let _: Result<(), _> = client
            .xgroup_create(counter_stream.as_str(), &*self.instance_id, "$", true)
            .await;
        Ok(())
    }

    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        crate::managed_loop::Supervisor::spawn_with_registry(self)
    }

    async fn apply(&self, stream: &str, fields: &HashMap<String, String>) {
        if stream == self.counter_stream() {
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

    fn counter_stream(&self) -> String {
        counter_stream_for_instance(&self.instance_id)
    }

    async fn reclaim_pending(
        &self,
        client: &fred::clients::RedisClient,
        consumer_name: &str,
        stream: &str,
    ) -> Result<usize, fred::error::RedisError> {
        #[allow(clippy::type_complexity)]
        let (_cursor, entries): (String, Vec<(String, HashMap<String, String>)>) = client
            .xautoclaim_values(
                stream,
                &*self.instance_id,
                consumer_name,
                PEL_MIN_IDLE_MS,
                "0-0",
                Some(PEL_RECLAIM_COUNT),
                false,
            )
            .await?;

        for (entry_id, fields) in &entries {
            self.apply(stream, fields).await;
            let ack: Result<u64, _> = client
                .xack(stream, &*self.instance_id, entry_id.clone())
                .await;
            if let Err(e) = ack {
                tracing::warn!(
                    target: "gw_store::stream_consumer",
                    error = %e,
                    stream = %stream,
                    entry_id = %entry_id,
                    "xack failed after xautoclaim; will be reclaimed again"
                );
            }
        }
        Ok(entries.len())
    }

    async fn reclaim_pending_streams(
        &self,
        client: &fred::clients::RedisClient,
        consumer_name: &str,
        counter_stream: &str,
    ) {
        for stream in [ENTITY_STREAM, counter_stream] {
            match self.reclaim_pending(client, consumer_name, stream).await {
                Ok(0) => {}
                Ok(count) => {
                    metrics::counter!(
                        "gw_stream_consumer_xautoclaim_total",
                        "stream" => stream.to_string()
                    )
                    .increment(count as u64);
                }
                Err(e) if is_nogroup_error(&e) => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        error = %e,
                        stream = %stream,
                        "xautoclaim saw missing group; ensure will recreate"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        target: "gw_store::stream_consumer",
                        error = %e,
                        stream = %stream,
                        "xautoclaim failed; backing off to normal reads"
                    );
                }
            }
        }
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
        "estimate_pricing" => {
            store
                .estimate_pricing
                .invalidate(&ModelName::from(id))
                .await
        }
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
        "config" => {
            if let Some(module_name) = module_name_from_config_id(id) {
                store.invalidate_module_enabled(module_name).await;
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
        "async_task_profile_revision" => {
            store.invalidate_async_task_profile_revision(id).await;
        }
        "async_task_binding_revision" => {
            store.invalidate_async_task_binding_revision(id).await;
        }
        "async_task_binding_index" => {
            store.invalidate_async_task_binding_index(id).await;
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

fn module_name_from_config_id(id: &str) -> Option<&str> {
    let name = id.strip_prefix("module.")?.strip_suffix(".enabled")?;
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod counter_stream_tests {
    use super::*;

    #[test]
    fn counter_stream_is_scoped_to_instance_id() {
        assert_eq!(
            counter_stream_for_instance("pod-a"),
            "events:counter:pod:pod-a"
        );
    }

    #[test]
    fn stream_consumer_reclaims_pel_with_xautoclaim() {
        let src = include_str!("stream_consumer.rs");
        let production_src = src
            .split("#[cfg(test)]")
            .next()
            .expect("production section");

        assert!(production_src.contains("xautoclaim_values"));
        assert!(production_src.contains("PEL_MIN_IDLE_MS"));
        assert!(production_src.contains("PEL_RECLAIM_COUNT"));
        assert!(production_src.contains("self.apply(stream, fields).await"));
        assert!(production_src.contains(".xack(stream, &*self.instance_id"));
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
        let counter_stream = self.counter_stream();
        self.reclaim_pending_streams(client, &consumer_name, counter_stream.as_str())
            .await;
        // Raw xreadgroup: RESP Null on BLOCK timeout is handled explicitly.
        let raw: Result<RedisValue, fred::error::RedisError> = client
            .xreadgroup(
                &*self.instance_id,
                &consumer_name,
                Some(COUNT_PER_POLL),
                Some(BLOCK_MS),
                false,
                vec![ENTITY_STREAM, counter_stream.as_str()],
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
mod estimate_pricing_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use gw_core::entity::EstimatePricing;
    use gw_core::ids::ModelName;
    use gw_core::versioned::Versioned;
    use std::sync::Arc;

    #[tokio::test]
    async fn apply_entity_event_invalidates_estimate_pricing() {
        let store = Arc::new(EntityStore::new());
        let model_name = ModelName::from("gpt-test");
        let versioned = Arc::new(Versioned {
            version: 1,
            value: Arc::new(EstimatePricing {
                model: model_name.clone(),
                conservative_per_token_micro: 7,
                safety_factor: 1.0,
                default_max_tokens: 4096,
                hard_cap_tokens: 200_000,
            }),
        });

        store
            .estimate_pricing
            .insert(model_name.clone(), versioned)
            .await;
        assert!(
            store.estimate_pricing.get(&model_name).await.is_some(),
            "pre-condition: cache entry must be present before invalidation"
        );

        apply_entity_event(&store, "estimate_pricing", "gpt-test").await;

        assert!(
            store.estimate_pricing.get(&model_name).await.is_none(),
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
            proxy_url: None,
            enabled: true,
            latency_ms: 0,
            allowed_provider_ids: vec![],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
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
mod async_task_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use gw_core::entity::{
        AsyncTaskBindingIndex, AsyncTaskBindingRevision, AsyncTaskProfileRevision,
    };
    use gw_core::versioned::Versioned;
    use std::sync::Arc;

    fn versioned<T>(value: T) -> Arc<Versioned<T>> {
        Arc::new(Versioned {
            version: 1,
            value: Arc::new(value),
        })
    }

    #[tokio::test]
    async fn apply_entity_event_invalidates_async_task_profile_revision() {
        let store = Arc::new(EntityStore::new());
        store
            .async_task_profile_revision
            .insert(
                "profile-rev-1".to_string(),
                versioned(AsyncTaskProfileRevision {
                    id: "profile-rev-1".to_string(),
                    profile_id: "profile-1".to_string(),
                    version: 1,
                    schema_version: "v1".to_string(),
                    task_kind: "image".to_string(),
                    profile: serde_json::json!({"name": "image default"}),
                    template_hash: "sha256:profile".to_string(),
                    published_by: "admin".to_string(),
                    published_at: 1_700_000_000,
                }),
            )
            .await;

        apply_entity_event(&store, "async_task_profile_revision", "profile-rev-1").await;

        assert!(store
            .async_task_profile_revision
            .get("profile-rev-1")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn apply_entity_event_invalidates_async_task_binding_revision() {
        let store = Arc::new(EntityStore::new());
        store
            .async_task_binding_revision
            .insert(
                "binding-rev-1".to_string(),
                versioned(AsyncTaskBindingRevision {
                    id: "binding-rev-1".to_string(),
                    binding_id: "binding-1".to_string(),
                    version: 1,
                    profile_id: "profile-1".to_string(),
                    profile_revision_id: "profile-rev-1".to_string(),
                    profile_version: 1,
                    global_model_id: "gpt-image".to_string(),
                    task_kind: "image".to_string(),
                    provider_id: "p1".to_string(),
                    target_snapshot: serde_json::json!({"provider_id": "p1"}),
                    provider_model_id: "pm1".to_string(),
                    endpoint_id: "endpoint-1".to_string(),
                    api_format: "openai".to_string(),
                    provider_key_id: "key-1".to_string(),
                    overrides_snapshot: serde_json::json!({}),
                    effective_template_snapshot: serde_json::json!({"request": {}}),
                    effective_template_hash: "sha256:binding".to_string(),
                    specificity_key: "image|gpt-image|pm1|*|*|*".to_string(),
                    priority: 10,
                    published_by: "admin".to_string(),
                    published_at: 1_700_000_000,
                }),
            )
            .await;

        apply_entity_event(&store, "async_task_binding_revision", "binding-rev-1").await;

        assert!(store
            .async_task_binding_revision
            .get("binding-rev-1")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn apply_entity_event_invalidates_async_task_binding_index() {
        let store = Arc::new(EntityStore::new());
        store
            .async_task_binding_index
            .insert(
                "gpt-image:image".to_string(),
                versioned(AsyncTaskBindingIndex {
                    global_model_id: "gpt-image".to_string(),
                    task_kind: "image".to_string(),
                    binding_revision_ids: vec!["binding-rev-1".to_string()],
                    endpoint_registry: vec![serde_json::json!({
                        "method": "POST",
                        "path": "/v1/images/generations"
                    })],
                }),
            )
            .await;

        apply_entity_event(&store, "async_task_binding_index", "gpt-image:image").await;

        assert!(store
            .async_task_binding_index
            .get("gpt-image:image")
            .await
            .is_none());
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

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::entity_store::EntityStore;
    use std::sync::Arc;

    #[test]
    fn module_name_from_config_id_accepts_module_enabled_keys() {
        assert_eq!(
            module_name_from_config_id("module.apikey.enabled"),
            Some("apikey")
        );
        assert_eq!(
            module_name_from_config_id("module.billing.enabled"),
            Some("billing")
        );
    }

    #[test]
    fn module_name_from_config_id_rejects_other_config_keys() {
        assert_eq!(module_name_from_config_id("module..enabled"), None);
        assert_eq!(module_name_from_config_id("module.apikey.disabled"), None);
        assert_eq!(module_name_from_config_id("auth.global"), None);
    }

    #[tokio::test]
    async fn config_namespace_is_accepted() {
        let store = Arc::new(EntityStore::new());

        apply_entity_event(&store, "config", "module.apikey.enabled").await;
    }

    #[tokio::test]
    async fn config_event_invalidates_module_enabled_cache() {
        let store = Arc::new(EntityStore::new());
        store
            .module_enabled
            .insert("apikey".to_string(), false)
            .await;
        assert_eq!(store.module_enabled.get("apikey").await, Some(false));

        apply_entity_event(&store, "config", "module.apikey.enabled").await;

        assert_eq!(store.module_enabled.get("apikey").await, None);
    }

    #[tokio::test]
    async fn non_module_config_event_leaves_module_enabled_cache() {
        let store = Arc::new(EntityStore::new());
        store
            .module_enabled
            .insert("apikey".to_string(), false)
            .await;

        apply_entity_event(&store, "config", "auth.global").await;

        assert_eq!(store.module_enabled.get("apikey").await, Some(false));
    }
}
