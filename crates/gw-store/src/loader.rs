//! Entity cold-load chain (ADR-016): Redis → PgSQL → write-back to Redis.
//!
//! `load_entity` is generic over the decoded value type and parameterised by
//! the namespace (key prefix), the row-fetch SQL, and a `decode_fn` that
//! materialises the JSON payload into the target type.

use super::encrypt::KekCache;
use super::redis_client::RedisCtx;
use fred::interfaces::{HashesInterface, KeysInterface};
use fred::types::RedisMap;
use gw_core::versioned::Versioned;
use sqlx::PgPool;
use std::sync::Arc;

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
        "jwks" => 3600,
        _ => 300,
    }
}
