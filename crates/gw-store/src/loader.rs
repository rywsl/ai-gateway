//! Entity cold-load chain (ADR-016): Redis → PgSQL → write-back to Redis.
//!
//! `load_entity` is generic over the decoded value type and parameterised by
//! the namespace (key prefix), the row-fetch SQL, and a `decode_fn` that
//! materialises the JSON payload into the target type.

use std::sync::Arc;

use fred::interfaces::{HashesInterface, KeysInterface};
use fred::types::RedisMap;
use secrecy::SecretString;
use sqlx::PgPool;

use gw_core::entity::{
    ApiKeyEntry, AuthConfig, JwksEntry, ModelGroup, Pricing, UserEntry, UserGroup, Vendor,
    VendorKey,
};
use gw_core::ids::{ApiKeyId, Group, Kid, KeyId, ModelName, UserGroupId, UserId, VendorId};
use gw_core::versioned::Versioned;

use crate::encrypt::{decrypt, EncryptedKey, KekCache};
use super::redis_client::RedisCtx;

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
        let d = hash
            .get("d")
            .ok_or_else(|| anyhow::anyhow!("missing payload field 'd' in entity:{namespace}:{id}"))?;
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
        "user" | "user_group" | "apikey" | "ipblock" => 300,
        "vendor" | "vendor_key" | "model_group" | "pricing" => 1800,
        "jwks" | "auth_config" => 3600,
        _ => 300,
    }
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
}

const USER_SQL: &str = "SELECT payload, version FROM users WHERE id = $1 AND deleted_at IS NULL";
const USER_GROUP_SQL: &str =
    "SELECT payload, version FROM user_groups WHERE id = $1 AND deleted_at IS NULL";
const API_KEY_SQL: &str =
    "SELECT payload, version FROM api_keys WHERE id = $1 AND deleted_at IS NULL";
const VENDOR_SQL: &str =
    "SELECT payload, version FROM vendors WHERE id = $1 AND deleted_at IS NULL";
const VENDOR_KEY_SQL: &str =
    "SELECT payload, version FROM vendor_keys WHERE id = $1 AND deleted_at IS NULL";
const MODEL_GROUP_SQL: &str =
    "SELECT payload, version FROM model_groups WHERE id = $1 AND deleted_at IS NULL";
const PRICING_SQL: &str =
    "SELECT payload, version FROM pricings WHERE id = $1 AND deleted_at IS NULL";
const JWKS_SQL: &str = "SELECT payload, version FROM jwks WHERE id = $1 AND deleted_at IS NULL";
// TODO(T1-E): the `auth_configs` table does not yet exist in
// `migrations/0001_initial.sql`. Until it lands, this query errors at the PG
// layer, `with_cold_load_timeout` swallows the error into `None`, and
// `auth_config_or_load` returns `None` — which T1-E treats as "no AuthConfig"
// and falls back to permissive JWT defaults. Once the table is added, this
// constant needs no change.
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

pub async fn load_apikey(
    ctx: &LoaderCtx,
    id: &ApiKeyId,
) -> anyhow::Result<Versioned<ApiKeyEntry>> {
    load_entity(ctx, "apikey", id.as_ref(), API_KEY_SQL, |v| {
        serde_json::from_value::<ApiKeyEntry>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_vendor(ctx: &LoaderCtx, id: &VendorId) -> anyhow::Result<Versioned<Vendor>> {
    load_entity(ctx, "vendor", id.as_ref(), VENDOR_SQL, |v| {
        serde_json::from_value::<Vendor>(v.clone()).map_err(Into::into)
    })
    .await
}

pub async fn load_vendor_key(
    ctx: &LoaderCtx,
    id: &KeyId,
) -> anyhow::Result<Versioned<VendorKey>> {
    let kek = ctx.kek.clone();
    load_entity(ctx, "vendor_key", id.as_ref(), VENDOR_KEY_SQL, move |v| {
        let row: VendorKeyRow = serde_json::from_value(v.clone())?;
        let secret: SecretString = decrypt(&row.api_key_enc, &kek)?;
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

pub async fn load_pricing(
    ctx: &LoaderCtx,
    id: &ModelName,
) -> anyhow::Result<Versioned<Pricing>> {
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
/// loader is id-generic so per-tenant configs can be added later. See the
/// TODO on [`AUTH_CONFIG_SQL`]: the table is not in the v1 schema yet, so until
/// it lands this returns `Err` (swallowed to `None` by the store wrapper) and
/// T1-E falls back to permissive defaults.
pub async fn load_auth_config(ctx: &LoaderCtx, id: &str) -> anyhow::Result<Versioned<AuthConfig>> {
    load_entity(ctx, "auth_config", id, AUTH_CONFIG_SQL, |v| {
        serde_json::from_value::<AuthConfig>(v.clone()).map_err(Into::into)
    })
    .await
}

// Note: IpBlockEntry has no cold-loader-by-id; its consumer iterates the
// whole snapshot via `EntityStore::ipblock_snapshot`. v1 accepts an empty
// runtime list; preload-from-PG is a follow-up.
