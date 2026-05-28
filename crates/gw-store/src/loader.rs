//! Entity cold-load chain (ADR-016): Redis → PgSQL → write-back to Redis.
//!
//! `load_entity` is generic over the decoded value type and parameterised by
//! the namespace (key prefix), the row-fetch SQL, and a `decode_fn` that
//! materialises the JSON payload into the target type.

use std::sync::Arc;

use fred::interfaces::{HashesInterface, KeysInterface};
use fred::types::RedisMap;
use secrecy::SecretString;
use sqlx::{PgPool, Row};

use gw_core::entity::{
    ApiKeyEntry, AuthConfig, IpBlockEntry, JwksEntry, ManagementTokenEntry, Model, ModelGroup,
    OAuthCredentials, Pricing, ProxyNode, UserEntry, UserGroup, Vendor, VendorKey,
};
use gw_core::ids::{ApiKeyId, Group, KeyId, Kid, ModelName, UserGroupId, UserId, VendorId};
use gw_core::versioned::Versioned;

use super::redis_client::RedisCtx;
use crate::encrypt::{decrypt, EncryptedKey, KekCache};

#[derive(Clone)]
pub struct LoaderCtx {
    pub redis: RedisCtx,
    pub pg: PgPool,
    pub kek: KekCache,
}

/// Generic entity load. `namespace` selects the Redis key prefix and TTL.
pub async fn load_entity<T, F>(
    ctx: &LoaderCtx,
    namespace: &str,
    id: &str,
    sql: &str,
    decode_fn: F,
) -> anyhow::Result<Versioned<T>>
where
    T: 'static + Send + Sync,
    F: Fn(&serde_json::Value) -> anyhow::Result<T>,
{
    let key = format!("entity:{namespace}:{id}");
    let client = ctx.redis.pool.next();

    // L1: Redis hash {v, enc, d}
    let hash: std::collections::HashMap<String, String> = client.hgetall(&key).await?;
    if !hash.is_empty() {
        let v: u64 = hash.get("v").and_then(|s| s.parse().ok()).unwrap_or(0);
        let d = hash.get("d").ok_or_else(|| {
            anyhow::anyhow!("missing payload field 'd' in entity:{namespace}:{id}")
        })?;
        let parsed: serde_json::Value = serde_json::from_str(d)?;
        let value = decode_fn(&parsed)?;
        return Ok(Versioned {
            version: v,
            value: Arc::new(value),
        });
    }

    // L2: PgSQL
    let row: Option<(serde_json::Value, i64)> =
        sqlx::query_as(sql).bind(id).fetch_optional(&ctx.pg).await?;
    let (payload, version) =
        row.ok_or_else(|| anyhow::anyhow!("not found in pg: {namespace}:{id}"))?;
    let value = decode_fn(&payload)?;
    let v = version as u64;

    // L3: write-back to Redis (with TTL).
    let payload_s = payload.to_string();
    let map: RedisMap = vec![
        ("v".to_string(), v.to_string()),
        ("enc".to_string(), "json".to_string()),
        ("d".to_string(), payload_s),
    ]
    .into_iter()
    .collect();
    let _: Result<(), _> = client.hset(&key, map).await;
    let ttl = ttl_for(namespace);
    let _: Result<(), _> = client.expire(&key, ttl as i64).await;

    Ok(Versioned {
        version: v,
        value: Arc::new(value),
    })
}

fn ttl_for(ns: &str) -> u64 {
    match ns {
        "user" | "user_group" | "apikey" | "ipblock" | "management_token" => 300,
        "vendor" | "vendor_key" | "model_group" | "pricing" | "model" | "proxy_node" => 1800,
        "jwks" | "auth_config" => 3600,
        _ => 300,
    }
}

/// Wire shape of the `oauth` sub-object inside a `vendor_keys.payload` row.
/// Tokens are AES-256-GCM encrypted at rest; `load_vendor_key` decrypts them
/// into `OAuthCredentials` held only in process memory (G3 / T0).
#[derive(serde::Deserialize)]
struct OAuthCredentialsRow {
    access_token_enc: EncryptedKey,
    refresh_token_enc: EncryptedKey,
    /// Unix seconds timestamp (serde: integer).
    #[serde(with = "time::serde::timestamp")]
    access_expires_at: time::OffsetDateTime,
    token_endpoint: String,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    pub quota_project_id: Option<String>,
}

/// Wire shape of a `vendor_keys.payload` row. The plaintext `api_key` is held
/// encrypted on disk under `api_key_enc`; decryption happens at decode time so
/// the cache always stores the decrypted form.
#[derive(serde::Deserialize)]
struct VendorKeyRow {
    key_id: KeyId,
    vendor_id: VendorId,
    api_key_enc: EncryptedKey,
    #[serde(default)]
    accessible_models: Vec<ModelName>,
    #[serde(default)]
    model_mapping: std::collections::HashMap<ModelName, String>,
    #[serde(default)]
    accessible_endpoints: Vec<gw_core::protocol::ProtocolKind>,
    #[serde(default)]
    protocol_conversion: bool,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    weight: u32,
    #[serde(default)]
    rpm_limit: u32,
    #[serde(default)]
    tpm_limit: u32,
    #[serde(default)]
    max_usage_count: Option<u64>,
    #[serde(default)]
    proxy: Option<url::Url>,
    #[serde(default)]
    ip_allowlist: Option<Vec<String>>,
    #[serde(default)]
    expires_at: Option<time::OffsetDateTime>,
    status: gw_core::entity::KeyStatus,
    #[serde(default)]
    tags: Vec<String>,
    /// G13 — capabilities this channel advertises as required-to-route. Fed to
    /// the candidate `CapabilityFilter`, not the resolver. Defaults empty
    /// (non-restrictive).
    #[serde(default)]
    forced_capabilities: Vec<String>,
    /// G13 — if true, the channel only matches requests whose required-caps
    /// exactly equal `forced_capabilities`. Defaults false.
    #[serde(default)]
    exclusive_capability_match: bool,
    /// T2 — ADR-T-4. `None` means "follow global default" (Direct unless a
    /// node opts in).
    #[serde(default)]
    tunnel_preference: Option<gw_core::entity::TunnelPreference>,
    /// G3 / go-onboarding-gaps T0: optional OAuth credentials sub-object.
    /// Absent for all static-API-key rows.
    #[serde(default)]
    oauth: Option<OAuthCredentialsRow>,
    /// Per-VendorKey persisted impersonation identity snapshot.
    /// `None` for keys that have not yet used impersonate egress.
    #[serde(default)]
    identity_snapshot: Option<gw_core::impersonate::IdentitySnapshot>,
}

const USER_SQL: &str = "SELECT payload, version FROM users WHERE id = $1 AND deleted_at IS NULL";
const USER_GROUP_SQL: &str =
    "SELECT payload, version FROM user_groups WHERE id = $1 AND deleted_at IS NULL";
const API_KEY_SQL: &str = "SELECT jsonb_set(payload, '{signature_secret}', COALESCE(to_jsonb(signature_secret), 'null'::jsonb), true) AS payload, version FROM api_keys WHERE id = $1 AND deleted_at IS NULL";
const IP_BLOCK_SQL: &str =
    "SELECT payload, version FROM ip_blocks WHERE id = $1 AND deleted_at IS NULL";
const VENDOR_SQL: &str =
    "SELECT payload, version FROM vendors WHERE id = $1 AND deleted_at IS NULL";
const VENDOR_KEY_SQL: &str =
    "SELECT payload, version FROM vendor_keys WHERE id = $1 AND deleted_at IS NULL";
const MODEL_GROUP_SQL: &str =
    "SELECT payload, version FROM model_groups WHERE id = $1 AND deleted_at IS NULL";
const PRICING_SQL: &str =
    "SELECT payload, version FROM pricings WHERE id = $1 AND deleted_at IS NULL";
const JWKS_SQL: &str = "SELECT payload, version FROM jwks WHERE id = $1 AND deleted_at IS NULL";
const MODEL_SQL: &str = "SELECT payload, version FROM models WHERE id = $1 AND deleted_at IS NULL";
const PROXY_NODE_SQL: &str =
    "SELECT payload, version FROM proxy_nodes WHERE id = $1 AND deleted_at IS NULL";
const PROXY_NODE_LIST_SQL: &str =
    "SELECT payload, version FROM proxy_nodes WHERE deleted_at IS NULL ORDER BY id";
const MGMT_TOKEN_SQL: &str =
    "SELECT payload, version FROM management_tokens WHERE id = $1 AND deleted_at IS NULL";
// `auth_configs` is created by `migrations/0006_auth_configs.sql` (G2,
// go-onboarding-gaps T1-A). When the `global` row is absent the SQL returns
// no rows; `with_cold_load_timeout` collapses other errors into `None`; the
// bootstrap warn path in gw-gateway::lib falls back to the legacy validator.
const AUTH_CONFIG_SQL: &str =
    "SELECT payload, version FROM auth_configs WHERE id = $1 AND deleted_at IS NULL";

pub async fn load_user(ctx: &LoaderCtx, id: &UserId) -> anyhow::Result<Versioned<UserEntry>> {
    load_entity(ctx, "user", id.as_ref(), USER_SQL, |v| {
        serde_json::from_value::<UserEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_user_group(
    ctx: &LoaderCtx,
    id: &UserGroupId,
) -> anyhow::Result<Versioned<UserGroup>> {
    load_entity(ctx, "user_group", id.as_ref(), USER_GROUP_SQL, |v| {
        serde_json::from_value::<UserGroup>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_apikey(ctx: &LoaderCtx, id: &ApiKeyId) -> anyhow::Result<Versioned<ApiKeyEntry>> {
    load_entity(ctx, "apikey", id.as_ref(), API_KEY_SQL, |v| {
        serde_json::from_value::<ApiKeyEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_ipblock(ctx: &LoaderCtx, id: &str) -> anyhow::Result<Versioned<IpBlockEntry>> {
    load_entity(ctx, "ipblock", id, IP_BLOCK_SQL, |v| {
        serde_json::from_value::<IpBlockEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_vendor(ctx: &LoaderCtx, id: &VendorId) -> anyhow::Result<Versioned<Vendor>> {
    load_entity(ctx, "vendor", id.as_ref(), VENDOR_SQL, |v| {
        serde_json::from_value::<Vendor>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_vendor_key(ctx: &LoaderCtx, id: &KeyId) -> anyhow::Result<Versioned<VendorKey>> {
    let kek = ctx.kek.clone();
    load_entity(ctx, "vendor_key", id.as_ref(), VENDOR_KEY_SQL, move |v| {
        let row: VendorKeyRow = serde_json::from_value(v.clone())?;
        let secret: SecretString = decrypt(&row.api_key_enc, &kek)?;
        let oauth = row
            .oauth
            .map(|o| {
                let access_token = decrypt(&o.access_token_enc, &kek)?;
                let refresh_token = decrypt(&o.refresh_token_enc, &kek)?;
                Ok::<OAuthCredentials, anyhow::Error>(OAuthCredentials {
                    access_token,
                    refresh_token,
                    access_expires_at: o.access_expires_at,
                    token_endpoint: o.token_endpoint,
                    scope: o.scope,
                    quota_project_id: o.quota_project_id,
                })
            })
            .transpose()?;
        Ok(VendorKey {
            key_id: row.key_id,
            vendor_id: row.vendor_id,
            api_key: secret,
            accessible_models: row.accessible_models,
            model_mapping: row.model_mapping,
            accessible_endpoints: row.accessible_endpoints,
            protocol_conversion: row.protocol_conversion,
            priority: row.priority,
            weight: row.weight,
            rpm_limit: row.rpm_limit,
            tpm_limit: row.tpm_limit,
            max_usage_count: row.max_usage_count,
            proxy: row.proxy,
            ip_allowlist: row.ip_allowlist,
            expires_at: row.expires_at,
            status: row.status,
            tags: row.tags,
            // G13 (T3-C): read the capability fields from the JSONB payload.
            forced_capabilities: row.forced_capabilities,
            exclusive_capability_match: row.exclusive_capability_match,
            tunnel_preference: row.tunnel_preference,
            oauth,
            identity_snapshot: row.identity_snapshot,
        })
    })
    .await
}

pub async fn load_model_group(
    ctx: &LoaderCtx,
    id: &Group,
) -> anyhow::Result<Versioned<ModelGroup>> {
    load_entity(ctx, "model_group", id.as_ref(), MODEL_GROUP_SQL, |v| {
        serde_json::from_value::<ModelGroup>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_pricing(ctx: &LoaderCtx, id: &ModelName) -> anyhow::Result<Versioned<Pricing>> {
    load_entity(ctx, "pricing", id.as_ref(), PRICING_SQL, |v| {
        serde_json::from_value::<Pricing>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_jwks(ctx: &LoaderCtx, id: &Kid) -> anyhow::Result<Versioned<JwksEntry>> {
    load_entity(ctx, "jwks", id.as_ref(), JWKS_SQL, |v| {
        serde_json::from_value::<JwksEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

/// Cold-load an [`AuthConfig`] (JWT/blacklist policy, G1/G2/G3). Mirrors
/// [`load_jwks`]: Redis L1 → PgSQL `auth_configs` L2 → Redis write-back.
///
/// The well-known id is `"global"` (T1-E reads `AuthConfig.global`), but the
/// loader is id-generic so per-tenant configs can be added later. The table is
/// created by `migrations/0006_auth_configs.sql`; when the `global` row is absent
/// the store wrapper collapses the empty result into `None` and the bootstrap
/// warn path in gw-gateway::lib falls back to the legacy validator.
pub async fn load_auth_config(ctx: &LoaderCtx, id: &str) -> anyhow::Result<Versioned<AuthConfig>> {
    load_entity(ctx, "auth_config", id, AUTH_CONFIG_SQL, |v| {
        serde_json::from_value::<AuthConfig>(v.clone()).map_err(Into::into)
    })
    .await
}

/// Cold-load a [`Model`] (G13 — capability metadata) by name. Mirrors
/// [`load_vendor_key`]: Redis L1 → PgSQL `models` L2 → Redis write-back.
/// T3-C's `CapabilityResolver` consumes the returned `capabilities` list.
pub async fn load_model(ctx: &LoaderCtx, name: &ModelName) -> anyhow::Result<Versioned<Model>> {
    load_entity(ctx, "model", name.as_ref(), MODEL_SQL, |v| {
        serde_json::from_value::<Model>(v.clone()).map_err(Into::into)
    })
    .await
}

/// Cold-load a [`ProxyNode`] (ADR-T-3) by id. Mirrors [`load_model`]:
/// Redis L1 → PgSQL `proxy_nodes` L2 → Redis write-back. 1800s TTL matching
/// `vendor_key`. T5-B's `TunnelStack` reads the per-id cache to resolve nodes.
pub async fn load_proxy_node(ctx: &LoaderCtx, id: &str) -> anyhow::Result<Versioned<ProxyNode>> {
    load_entity(ctx, "proxy_node", id, PROXY_NODE_SQL, |v| {
        serde_json::from_value::<ProxyNode>(v.clone()).map_err(Into::into)
    })
    .await
}

/// Whole-table scan for the live `proxy_nodes` list. Used by
/// `TunnelStack::reload_nodes` (ADR-T-3). The list is small in practice
/// (≤ low hundreds) so a single PG round-trip is fine; the ArcSwap inside
/// TunnelStack is what makes hot-path reads lock-free.
pub async fn load_proxy_nodes_list(ctx: &LoaderCtx) -> anyhow::Result<Vec<ProxyNode>> {
    let rows = sqlx::query(PROXY_NODE_LIST_SQL).fetch_all(&ctx.pg).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let payload: serde_json::Value = row.try_get("payload")?;
        let node: ProxyNode = serde_json::from_value(payload)?;
        out.push(node);
    }
    Ok(out)
}

/// Cold-load a [`ManagementTokenEntry`] (B2 / ADR-B2-1) by `token_id`. Mirrors
/// [`load_apikey`]: Redis L1 → PgSQL `management_tokens` L2 → Redis write-back
/// with the 300s TTL bucket (`ttl_for("management_token")`). The presented
/// bearer's SHA-256 is then constant-time compared against `entry.hash` by
/// request_filter (T3-A/B/C); this loader does not perform that compare.
pub async fn load_mgmt_token(
    ctx: &LoaderCtx,
    token_id: &str,
) -> anyhow::Result<Versioned<ManagementTokenEntry>> {
    load_entity(ctx, "management_token", token_id, MGMT_TOKEN_SQL, |v| {
        serde_json::from_value::<ManagementTokenEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

#[cfg(test)]
mod tunnel_pref_tests {
    use gw_core::entity::TunnelPreference;
    use serde_json::json;

    #[test]
    fn vendor_key_row_decodes_tunnel_pref_when_absent() {
        let pref: Option<TunnelPreference> = serde_json::from_value(json!(null)).unwrap();
        assert!(pref.is_none());
    }

    #[test]
    fn vendor_key_row_decodes_tunnel_pref_when_present() {
        let pref: TunnelPreference = serde_json::from_value(json!({
            "preferred_node_ids": ["node-eu", "node-us"],
            "require_tunnel": true,
            "prefer_low_latency": true
        }))
        .unwrap();
        assert_eq!(
            pref.preferred_node_ids.as_slice(),
            &["node-eu".to_string(), "node-us".to_string()]
        );
        assert!(pref.require_tunnel);
        assert!(pref.prefer_low_latency);
    }
}

#[cfg(test)]
mod oauth_row_tests {
    use super::VendorKeyRow;
    use serde_json::json;

    /// Builds a minimal EncryptedKey JSON value (fields as integer arrays,
    /// matching the on-disk JSONB shape of `gw_store::encrypt::EncryptedKey`).
    fn enc_key_json() -> serde_json::Value {
        json!({
            "ciphertext": [1, 2, 3, 4],
            "nonce": [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21],
            "auth_tag": [0xaa, 0xbb, 0xcc, 0xdd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            "kek_kid": "kid-1"
        })
    }

    #[test]
    fn vendor_key_row_oauth_field_roundtrip() {
        // access_expires_at is stored as a Unix seconds integer
        // (OAuthCredentialsRow uses `#[serde(with = "time::serde::timestamp")]`).
        let payload = json!({
            "key_id": "key-123",
            "vendor_id": "v-1",
            "api_key_enc": enc_key_json(),
            "status": "active",
            "oauth": {
                "access_token_enc": enc_key_json(),
                "refresh_token_enc": enc_key_json(),
                "access_expires_at": 1893456000_i64,
                "token_endpoint": "https://oauth.example.com/token",
                "scope": ["chat.completions"]
            }
        });
        let row: VendorKeyRow = serde_json::from_value(payload).unwrap();
        let oauth = row.oauth.expect("oauth should be Some");
        assert_eq!(oauth.token_endpoint, "https://oauth.example.com/token");
        assert_eq!(oauth.scope, vec!["chat.completions".to_string()]);
        // Pin the wire format: integer Unix seconds, NOT milliseconds and
        // NOT a default-time serde array — getting this wrong gives dates
        // a thousand times off without a compile error.
        assert_eq!(oauth.access_expires_at.unix_timestamp(), 1893456000);
    }

    #[test]
    fn vendor_key_row_oauth_omitted_defaults_to_none() {
        let payload = json!({
            "key_id": "key-456",
            "vendor_id": "v-2",
            "api_key_enc": enc_key_json(),
            "status": "active"
        });
        let row: VendorKeyRow = serde_json::from_value(payload).unwrap();
        assert!(row.oauth.is_none());
    }
}
