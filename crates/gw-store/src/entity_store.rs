//! EntityStore — nine Moka caches keyed by ID newtypes, holding `Arc<Versioned<T>>`.
//!
//! Sized + TTL policy per entity class (ADR-006 / ADR-016):
//! - user / user_group / apikey / ipblock: 300s TTL
//! - vendor / vendor_key / model_group / pricing / proxy_node: 1800s TTL
//! - jwks: 3600s TTL
//!
//! Hot-path callers should use the `*_or_load` methods, which dedupe concurrent
//! loads via Moka's `try_get_with_by_ref` and fall through to the Redis→PG
//! cold-loader on cache miss when a [`LoaderCtx`] has been wired in.

use crate::loader::{
    load_apikey, load_apikey_by_hash, load_async_task_binding_index,
    load_async_task_binding_revision, load_async_task_endpoint_registry,
    load_async_task_profile_revision, load_auth_config, load_estimate_pricing, load_ipblock,
    load_jwks, load_mgmt_token, load_model, load_model_group, load_module_enabled, load_pricing,
    load_proxy_node, load_proxy_nodes_list, load_sensitive_filter_policy, load_user,
    load_user_group, load_vendor, load_vendor_key, LoaderCtx,
};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use moka::future::Cache;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;

pub type VendorKeyInvalidationHook = Arc<dyn Fn(&KeyId) + Send + Sync + 'static>;

/// Async rebuild hook for the hot-reloadable JWT validator (R7b). Registered
/// by `gw-gateway` bootstrap (which owns the `gw-auth::JwtValidator` type that
/// `gw-store` cannot name without a dependency cycle). Fired by the
/// `events:entity ns=auth_config id=global` arm to reload `AuthConfig.global`
/// and swap the live validator. The store is passed as an argument (not
/// captured) so the hook never forms an `Arc<EntityStore>` reference cycle.
pub type AuthConfigRebuildHook = Arc<
    dyn Fn(Arc<EntityStore>, String) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        + Send
        + Sync
        + 'static,
>;

/// Default per-cold-load deadline. Bootstrap can override this via
/// `EntityStore::with_loader_and_timeout` (wired from
/// `gw-gateway` `LoaderCfg::cold_load_timeout_ms`, default 2000 ms).
const DEFAULT_COLD_LOAD_TIMEOUT: Duration = Duration::from_secs(2);

/// Sentinel error used to drive Moka's `try_get_with_by_ref` Err arm without
/// caching the failure. The real diagnostic is emitted by
/// `with_cold_load_timeout` as a `tracing::warn`; this string is never user-
/// or log-facing on its own.
const COLD_LOAD_MISS_SENTINEL: &str = "cold-load miss (timeout or loader error); see warn log";

/// Cold-load timeout wrapper.
///
/// Wraps a loader future with `tokio::time::timeout(deadline, fut)`. On
/// timeout, increments `gateway_cold_load_timeout_total{ns}` and returns
/// `None`. On `Err(_)`, returns `None` and lets the loader's own
/// `tracing::warn` carry the underlying message. On `Ok(v)`, returns
/// `Some(v)`.
pub async fn with_cold_load_timeout<T>(
    ns: &'static str,
    deadline: Duration,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Option<T> {
    match timeout(deadline, fut).await {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            // Audit 2026-05-30 §1.12: a PoolTimedOut here is transient pool
            // contention, NOT a missing row — surface it distinctly (counted +
            // clearer log) so it is not silently read as "entity not found".
            // The caller still gets None, but the metric makes it observable.
            if matches!(
                e.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::PoolTimedOut)
            ) {
                metrics::counter!("gateway_pool_acquire_timeout_total", "ns" => ns).increment(1);
                tracing::warn!(
                    target: "gw_store::loader",
                    ns = %ns,
                    "cold loader hit a pool acquire timeout (transient contention, not a missing row)"
                );
            } else {
                tracing::warn!(
                    target: "gw_store::loader",
                    ns = %ns,
                    error = %e,
                    "cold loader returned error"
                );
            }
            None
        }
        Err(_elapsed) => {
            metrics::counter!(
                "gateway_cold_load_timeout_total",
                "ns" => ns,
            )
            .increment(1);
            tracing::warn!(
                target: "gw_store::loader",
                ns = %ns,
                deadline_ms = deadline.as_millis() as u64,
                "cold loader timed out"
            );
            None
        }
    }
}

pub struct EntityStore {
    pub user: Cache<UserId, Arc<Versioned<UserEntry>>>,
    pub user_group: Cache<UserGroupId, Arc<Versioned<UserGroup>>>,
    pub apikey: Cache<ApiKeyId, Arc<Versioned<ApiKeyEntry>>>,
    pub vendor: Cache<VendorId, Arc<Versioned<Vendor>>>,
    pub vendor_key: Cache<KeyId, Arc<Versioned<VendorKey>>>,
    pub model_group: Cache<Group, Arc<Versioned<ModelGroup>>>,
    pub pricing: Cache<ModelName, Arc<Versioned<Pricing>>>,
    pub estimate_pricing: Cache<ModelName, Arc<Versioned<EstimatePricing>>>,
    pub jwks: Cache<Kid, Arc<Versioned<JwksEntry>>>,
    /// JWT/blacklist policy (G1/G2/G3). Keyed by config id (well-known id
    /// `"global"`). 3600s TTL, same as `jwks`. Populated by T1-E via
    /// [`EntityStore::auth_config_or_load`].
    pub auth_config: Cache<String, Arc<Versioned<AuthConfig>>>,
    /// Per-model entity (G13). 1800s TTL matching `vendor_key`. Consumed by
    /// T3-C's `CapabilityResolver` via [`EntityStore::model_or_load`].
    pub model: Cache<ModelName, Arc<Versioned<Model>>>,
    pub ipblock: Cache<String, Arc<Versioned<IpBlockEntry>>>,
    pub model_mapping: moka::future::Cache<
        (String, gw_core::protocol::ProtocolKind),
        std::sync::Arc<gw_core::versioned::Versioned<gw_core::entity::ModelMapping>>,
    >,
    /// Proxy-node entity cache (ADR-T-3). 1800s TTL, matching `vendor_key`.
    /// Populated by [`EntityStore::proxy_node_or_load`]; invalidated by the
    /// stream consumer on `proxy_node` namespace events (T1 / T5-B).
    pub proxy_node: Cache<String, Arc<Versioned<ProxyNode>>>,
    /// Published async-task profile revision cache. Go owns writes and emits
    /// `events:entity ns=async_task_profile_revision` for invalidation.
    pub async_task_profile_revision: Cache<String, Arc<Versioned<AsyncTaskProfileRevision>>>,
    /// Published async-task binding revision cache. Go owns writes and emits
    /// `events:entity ns=async_task_binding_revision` for invalidation.
    pub async_task_binding_revision: Cache<String, Arc<Versioned<AsyncTaskBindingRevision>>>,
    /// Async-task binding index keyed as `<global_model_id>:<task_kind>`.
    pub async_task_binding_index: Cache<String, Arc<Versioned<AsyncTaskBindingIndex>>>,
    /// Async-task route registry keyed by `"global"` for pre-codec endpoint
    /// admission.
    pub async_task_endpoint_registry: Cache<String, Arc<Versioned<AsyncTaskEndpointRegistry>>>,
    /// Management-token entity cache (B2 / ADR-B2-1). 300s TTL, matching
    /// `apikey` (auth-surface cache bucket). Populated by
    /// [`EntityStore::mgmt_token_or_load`]; invalidated by the stream consumer
    /// on `management_token` namespace events (T1-D).
    pub mgmt_token: Cache<String, Arc<Versioned<ManagementTokenEntry>>>,
    /// Go-owned module toggles keyed by bare module name (`apikey` for
    /// `module.apikey.enabled`). Missing/unreadable rows are treated as enabled.
    pub module_enabled: Cache<String, bool>,
    pub sensitive_filter_policy: Cache<String, Arc<Versioned<SensitiveFilterPolicy>>>,
    /// When `Some`, cache misses on `*_or_load` fall through to the cold loader.
    /// `None` for unit tests that pre-populate caches by hand.
    loader: Option<Arc<LoaderCtx>>,
    /// Per-cold-load deadline. If a loader future hasn't resolved by this
    /// duration, the miss returns `None` and bumps
    /// `gateway_cold_load_timeout_total{ns}`. Defaults to 2s.
    cold_load_timeout: Duration,
    vendor_key_invalidation_hooks: Vec<VendorKeyInvalidationHook>,
    auth_config_rebuild_hook: Option<AuthConfigRebuildHook>,
    compiled_sensitive_filter_policy:
        Mutex<Option<(u64, Arc<gw_runtime::CompiledSensitiveFilterPolicy>)>>,
}

impl Default for EntityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityStore {
    pub fn new() -> Self {
        Self::build(None, DEFAULT_COLD_LOAD_TIMEOUT)
    }

    pub fn with_loader(ctx: LoaderCtx) -> Self {
        Self::with_loader_and_timeout(ctx, DEFAULT_COLD_LOAD_TIMEOUT)
    }

    pub fn with_loader_and_timeout(ctx: LoaderCtx, cold_load_timeout: Duration) -> Self {
        Self::build(Some(Arc::new(ctx)), cold_load_timeout)
    }

    fn build(loader: Option<Arc<LoaderCtx>>, cold_load_timeout: Duration) -> Self {
        Self {
            user: Cache::builder()
                .max_capacity(1_000_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            user_group: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            apikey: Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            vendor: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            vendor_key: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            model_group: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            pricing: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            estimate_pricing: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            jwks: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            auth_config: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            model: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            ipblock: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            model_mapping: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            proxy_node: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            async_task_profile_revision: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            async_task_binding_revision: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            async_task_binding_index: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            async_task_endpoint_registry: Cache::builder()
                .max_capacity(16)
                .time_to_live(Duration::from_secs(1800))
                .build(),
            mgmt_token: Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            module_enabled: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(300))
                .build(),
            sensitive_filter_policy: Cache::builder()
                .max_capacity(16)
                .time_to_live(Duration::from_secs(300))
                .build(),
            loader,
            cold_load_timeout,
            vendor_key_invalidation_hooks: Vec::new(),
            auth_config_rebuild_hook: None,
            compiled_sensitive_filter_policy: Mutex::new(None),
        }
    }

    pub fn on_vendor_key_invalidate(&mut self, hook: VendorKeyInvalidationHook) {
        self.vendor_key_invalidation_hooks.push(hook);
    }

    /// Register the hot-reload hook fired when `auth_config` (`global`) changes
    /// (R7b). One validator → one hook; a second registration replaces the
    /// first. Must be called before the store is wrapped in `Arc` (it takes
    /// `&mut self`), same as [`on_vendor_key_invalidate`].
    pub fn on_auth_config_rebuild(&mut self, hook: AuthConfigRebuildHook) {
        self.auth_config_rebuild_hook = Some(hook);
    }

    pub(crate) fn auth_config_rebuild_hook(&self) -> Option<AuthConfigRebuildHook> {
        self.auth_config_rebuild_hook.clone()
    }

    pub async fn invalidate_vendor_key(&self, key_id: &KeyId) {
        self.vendor_key.invalidate(key_id).await;
        for hook in &self.vendor_key_invalidation_hooks {
            hook(key_id);
        }
    }

    /// Walk every live `ipblock` entry; used by `gw-auth::ip_filter`.
    /// Snapshots into a `Vec` so callers don't have to thread a lifetime through
    /// the iterator. Moka's iterator is read-only and concurrent-safe.
    pub fn ipblock_snapshot(&self) -> Vec<(String, Arc<Versioned<IpBlockEntry>>)> {
        self.ipblock
            .iter()
            .map(|(k, v)| ((*k).clone(), v))
            .collect()
    }

    pub async fn ipblock_or_load(&self, id: &str) -> Option<Arc<Versioned<IpBlockEntry>>> {
        if let Some(hit) = self.ipblock.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.to_owned();
        let deadline = self.cold_load_timeout;
        self.ipblock
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("ipblock", deadline, async move {
                    load_ipblock(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn user_or_load(&self, id: &UserId) -> Option<Arc<Versioned<UserEntry>>> {
        if let Some(hit) = self.user.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.user
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout(
                    "user",
                    deadline,
                    async move { load_user(&loader, &id2).await },
                )
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn user_group_or_load(&self, id: &UserGroupId) -> Option<Arc<Versioned<UserGroup>>> {
        if let Some(hit) = self.user_group.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.user_group
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("user_group", deadline, async move {
                    load_user_group(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn apikey_or_load(&self, id: &ApiKeyId) -> Option<Arc<Versioned<ApiKeyEntry>>> {
        if let Some(hit) = self.apikey.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.apikey
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("apikey", deadline, async move {
                    load_apikey(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn apikey_by_hash_or_load(
        &self,
        key_hash: &str,
    ) -> Option<Arc<Versioned<ApiKeyEntry>>> {
        let key_hash = key_hash.to_owned();
        let loader = self.loader.as_ref()?.clone();
        let deadline = self.cold_load_timeout;
        let loaded = with_cold_load_timeout("apikey", deadline, async move {
            load_apikey_by_hash(&loader, &key_hash).await
        })
        .await?;
        let key_id = loaded.value.key_id.clone();
        let entry = Arc::new(loaded);
        self.apikey.insert(key_id, entry.clone()).await;
        Some(entry)
    }

    pub async fn vendor_or_load(&self, id: &VendorId) -> Option<Arc<Versioned<Vendor>>> {
        if let Some(hit) = self.vendor.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.vendor
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("vendor", deadline, async move {
                    load_vendor(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn vendor_key_or_load(&self, id: &KeyId) -> Option<Arc<Versioned<VendorKey>>> {
        if let Some(hit) = self.vendor_key.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.vendor_key
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("vendor_key", deadline, async move {
                    load_vendor_key(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn model_group_or_load(&self, id: &Group) -> Option<Arc<Versioned<ModelGroup>>> {
        if let Some(hit) = self.model_group.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.model_group
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("model_group", deadline, async move {
                    load_model_group(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn pricing_or_load(&self, id: &ModelName) -> Option<Arc<Versioned<Pricing>>> {
        if let Some(hit) = self.pricing.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.pricing
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("pricing", deadline, async move {
                    load_pricing(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn estimate_pricing_or_load(
        &self,
        id: &ModelName,
    ) -> Option<Arc<Versioned<EstimatePricing>>> {
        if let Some(hit) = self.estimate_pricing.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.estimate_pricing
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("estimate_pricing", deadline, async move {
                    load_estimate_pricing(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn jwks_or_load(&self, id: &Kid) -> Option<Arc<Versioned<JwksEntry>>> {
        if let Some(hit) = self.jwks.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.clone();
        let deadline = self.cold_load_timeout;
        self.jwks
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout(
                    "jwks",
                    deadline,
                    async move { load_jwks(&loader, &id2).await },
                )
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    /// Resolve a [`Model`] (G13 — capability metadata) by name. L1 Moka →
    /// cold load via `load_model`. T3-C's `CapabilityResolver` reads
    /// `Model.capabilities`. Returns `None` on miss/timeout (empty
    /// capabilities then mean "no specific requirements" per ADR-G13-1).
    pub async fn model_or_load(&self, name: &ModelName) -> Option<Arc<Versioned<Model>>> {
        if let Some(hit) = self.model.get(name).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let name2 = name.clone();
        let deadline = self.cold_load_timeout;
        self.model
            .try_get_with_by_ref(name, async move {
                with_cold_load_timeout("model", deadline, async move {
                    load_model(&loader, &name2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    /// Resolve an [`AuthConfig`] (G1/G2/G3 — JWT/blacklist policy) by id.
    /// L1 Moka → cold load via `load_auth_config`. T1-E reads the well-known
    /// id `"global"`; the loader is id-generic so per-tenant configs can be
    /// added later. Returns `None` on miss/timeout (the `auth_configs` table
    /// is not yet in the v1 migrations, see `load_auth_config` for the
    /// follow-up note); callers fall back to permissive defaults.
    pub async fn auth_config_or_load(&self, id: &str) -> Option<Arc<Versioned<AuthConfig>>> {
        if let Some(hit) = self.auth_config.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.to_owned();
        let deadline = self.cold_load_timeout;
        self.auth_config
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("auth_config", deadline, async move {
                    load_auth_config(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    /// Resolve a [`ProxyNode`] (ADR-T-3) by id. L1 Moka → cold load via
    /// `load_proxy_node`. 1800s TTL matching `vendor_key`. Returns `None` on
    /// miss/timeout; callers treat that as "node not available".
    pub async fn proxy_node_or_load(&self, id: &str) -> Option<Arc<Versioned<ProxyNode>>> {
        if let Some(hit) = self.proxy_node.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.to_owned();
        let deadline = self.cold_load_timeout;
        self.proxy_node
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("proxy_node", deadline, async move {
                    load_proxy_node(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    /// Resolve a [`ManagementTokenEntry`] (B2 / ADR-B2-1) by `token_id`. L1
    /// Moka → cold load via `load_mgmt_token`. 300s TTL matching `apikey`
    /// (auth-surface bucket). Returns `None` on miss/timeout; callers
    /// (T3-A/B/C request_filter) treat that as "unknown token → 401".
    pub async fn mgmt_token_or_load(
        &self,
        token_id: &str,
    ) -> Option<Arc<Versioned<ManagementTokenEntry>>> {
        if let Some(hit) = self.mgmt_token.get(token_id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = token_id.to_owned();
        let deadline = self.cold_load_timeout;
        self.mgmt_token
            .try_get_with_by_ref(token_id, async move {
                with_cold_load_timeout("management_token", deadline, async move {
                    load_mgmt_token(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    /// Drop the cached entry for `token_id`. Called by the stream consumer
    /// when an `events:entity` `{ns=management_token,id=…}` event arrives
    /// (T1-D). Next `mgmt_token_or_load` will cold-load the new version.
    pub async fn invalidate_mgmt_token(&self, token_id: &str) {
        self.mgmt_token.invalidate(token_id).await;
    }

    pub async fn async_task_profile_revision_or_load(
        &self,
        id: &str,
    ) -> Option<Arc<Versioned<AsyncTaskProfileRevision>>> {
        if let Some(hit) = self.async_task_profile_revision.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.to_owned();
        let deadline = self.cold_load_timeout;
        self.async_task_profile_revision
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("async_task_profile_revision", deadline, async move {
                    load_async_task_profile_revision(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn invalidate_async_task_profile_revision(&self, id: &str) {
        self.async_task_profile_revision.invalidate(id).await;
    }

    pub async fn async_task_binding_revision_or_load(
        &self,
        id: &str,
    ) -> Option<Arc<Versioned<AsyncTaskBindingRevision>>> {
        if let Some(hit) = self.async_task_binding_revision.get(id).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let id2 = id.to_owned();
        let deadline = self.cold_load_timeout;
        self.async_task_binding_revision
            .try_get_with_by_ref(id, async move {
                with_cold_load_timeout("async_task_binding_revision", deadline, async move {
                    load_async_task_binding_revision(&loader, &id2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn invalidate_async_task_binding_revision(&self, id: &str) {
        self.async_task_binding_revision.invalidate(id).await;
    }

    pub async fn async_task_binding_index_or_load(
        &self,
        key: &str,
    ) -> Option<Arc<Versioned<AsyncTaskBindingIndex>>> {
        if let Some(hit) = self.async_task_binding_index.get(key).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let key2 = key.to_owned();
        let deadline = self.cold_load_timeout;
        self.async_task_binding_index
            .try_get_with_by_ref(key, async move {
                with_cold_load_timeout("async_task_binding_index", deadline, async move {
                    load_async_task_binding_index(&loader, &key2).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn invalidate_async_task_binding_index(&self, key: &str) {
        self.async_task_binding_index.invalidate(key).await;
    }

    pub async fn async_task_endpoint_registry_or_load(
        &self,
    ) -> Option<Arc<Versioned<AsyncTaskEndpointRegistry>>> {
        const KEY: &str = "global";
        if let Some(hit) = self.async_task_endpoint_registry.get(KEY).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let deadline = self.cold_load_timeout;
        self.async_task_endpoint_registry
            .try_get_with_by_ref(KEY, async move {
                with_cold_load_timeout("async_task_endpoint_registry", deadline, async move {
                    load_async_task_endpoint_registry(&loader, KEY).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn invalidate_async_task_endpoint_registry(&self, key: &str) {
        self.async_task_endpoint_registry.invalidate(key).await;
    }

    pub async fn module_enabled_or_load(&self, name: &str) -> bool {
        if let Some(hit) = self.module_enabled.get(name).await {
            return hit;
        }
        let Some(loader) = self.loader.as_ref().cloned() else {
            return true;
        };
        let name2 = name.to_owned();
        let deadline = self.cold_load_timeout;
        self.module_enabled
            .try_get_with_by_ref(name, async move {
                with_cold_load_timeout("module_enabled", deadline, async move {
                    load_module_enabled(&loader, &name2).await
                })
                .await
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .unwrap_or(true)
    }

    pub async fn invalidate_module_enabled(&self, name: &str) {
        self.module_enabled.invalidate(name).await;
    }

    pub async fn sensitive_filter_policy_or_load(
        &self,
    ) -> Option<Arc<Versioned<SensitiveFilterPolicy>>> {
        const KEY: &str = "global";
        if let Some(hit) = self.sensitive_filter_policy.get(KEY).await {
            return Some(hit);
        }
        let loader = self.loader.as_ref()?.clone();
        let deadline = self.cold_load_timeout;
        self.sensitive_filter_policy
            .try_get_with_by_ref(KEY, async move {
                with_cold_load_timeout("sensitive_filter_policy", deadline, async move {
                    load_sensitive_filter_policy(&loader, KEY).await
                })
                .await
                .map(Arc::new)
                .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
            })
            .await
            .ok()
    }

    pub async fn compiled_sensitive_filter_policy_or_compile(
        &self,
    ) -> Option<Arc<gw_runtime::CompiledSensitiveFilterPolicy>> {
        let policy = self.sensitive_filter_policy_or_load().await?;
        let mut slot = self.compiled_sensitive_filter_policy.lock().await;
        if let Some((version, compiled)) = slot.as_ref() {
            if *version == policy.version {
                return Some(compiled.clone());
            }
        }
        match gw_runtime::CompiledSensitiveFilterPolicy::compile(policy.value.as_ref()) {
            Ok(compiled) => {
                let compiled = Arc::new(compiled);
                *slot = Some((policy.version, compiled.clone()));
                Some(compiled)
            }
            Err(err) => {
                tracing::warn!(
                    target: "gw_store::sensitive_filter",
                    error = %err,
                    "compile failed; keeping last good sensitive filter policy"
                );
                slot.as_ref().map(|(_, compiled)| compiled.clone())
            }
        }
    }

    pub async fn invalidate_sensitive_filter_policy(&self, id: &str) {
        self.sensitive_filter_policy.invalidate(id).await;
        *self.compiled_sensitive_filter_policy.lock().await = None;
    }

    /// G6/T1-C: handle to the optional loader's Redis pool, used by the
    /// stream consumer to DEL the Redis L1 key before invalidating Moka.
    /// Also consumed by the proxy's `OAuthListener` (T2-B) to reverse-XADD
    /// `events:entity` on `OAuthSignal::RefreshFailed`. `None` for in-process
    /// tests built with `EntityStore::new()`.
    pub fn loader_redis(&self) -> Option<&crate::redis_client::RedisCtx> {
        self.loader.as_deref().map(|ctx| &ctx.redis)
    }

    /// Fetch the full live list of [`ProxyNode`]s from PG (bypasses the
    /// per-id cache). Used by `TunnelStack::reload_nodes` (ADR-T-3, T5-B)
    /// after a `proxy_node` invalidation event arrives on the entity stream.
    /// The caller ArcSwaps the returned `Vec` into the tunnel stack; hot-path
    /// reads are then lock-free.
    ///
    /// Returns `None` on timeout or loader error (caller should retain the
    /// previous snapshot and log the failure).
    pub async fn proxy_nodes_list_or_load(&self) -> Option<Vec<ProxyNode>> {
        let loader = self.loader.as_ref()?.clone();
        let deadline = self.cold_load_timeout;
        with_cold_load_timeout("proxy_node_list", deadline, async move {
            load_proxy_nodes_list(&loader).await
        })
        .await
    }
}
