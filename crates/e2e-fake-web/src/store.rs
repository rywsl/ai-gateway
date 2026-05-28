//! `Store` — the thin Postgres + Redis facade the fake control-plane uses
//! to mutate the same state shape the gateway's hot path reads.
//!
//! Hard invariant: this crate must not depend on any gw-* crate that
//! carries the gateway's domain types (gw-store, gw-auth, gw-ledger,
//! gw-codec, gw-proxy, gw-gateway). We only use sqlx + fred directly
//! and serialize whatever JSON payload the test harness hands us.

use crate::events;
use fred::clients::RedisPool;
use fred::interfaces::{KeysInterface, StreamsInterface};
use futures::StreamExt;
use std::sync::Arc;

/// Allowlist of entity tables the fake web is permitted to upsert/delete.
///
/// `table` is interpolated directly into SQL (Postgres does not allow a
/// table name as a bind parameter), so we must reject anything not in
/// this fixed set. The mapping to invalidation `ns` lives in
/// [`ns_for_table`].
const ENTITY_TABLES: &[&str] = &[
    "users",
    "user_groups",
    "api_keys",
    "vendors",
    "vendor_keys",
    "model_groups",
    "pricings",
    "jwks",
    "ip_blocks",
    "models",
    // T4-B: B2 ManagementTokenEntry persistence (migration 0007). Consumed
    // by the gateway via `EntityStore::mgmt_token_or_load`. ns = "management_token"
    // (see `ns_for_table`).
    "management_tokens",
    // `auth_configs` is owned by the fake control-plane (gateway TODO
    // table not yet in migrations). No consumer ns — same treatment as
    // `models` — so we skip emitting invalidation for it. It IS a real
    // entity the harness writes via `put_entity`/`delete_entity` (the JWT
    // scenarios upsert it), but it is deliberately EXCLUDED from
    // [`TRUNCATE_TABLES`] — see the `reset()` doc-comment for why.
    "auth_configs",
];

/// Subset of [`ENTITY_TABLES`] that `reset()` is allowed to TRUNCATE.
///
/// This is `ENTITY_TABLES` minus `auth_configs`. The single global
/// `auth_configs` row is seeded ONCE by Postgres initdb
/// (`tests/e2e/seed_auth_config.sql`) and read by the gateway at boot;
/// it is owned by the boot lifecycle, not by any individual test. If
/// `reset()` truncated it, the first per-test reset would drop the row
/// and a later gateway restart (e.g. the restart-recovery scenario)
/// would re-read `auth_config_or_load("global")`, find nothing, and wire
/// the legacy `None` JWT-validator branch — turning every subsequent
/// `GW_AUTH_JWT_INVALID_ISS/_AUD` into a generic `GW_AUTH_INVALID_JWT`.
/// So `reset()` PRESERVES `auth_configs` across resets; the JWT
/// scenarios that need a different config still overwrite it explicitly
/// via `put_entity` (which remains in the `ENTITY_TABLES` allowlist).
const TRUNCATE_TABLES: &[&str] = &[
    "users",
    "user_groups",
    "api_keys",
    "vendors",
    "vendor_keys",
    "model_groups",
    "pricings",
    "jwks",
    "ip_blocks",
    "models",
    // T4-B: management tokens are per-test fixtures, same lifecycle as
    // api_keys — truncate on reset so a previous test's mt_* ids don't
    // bleed into the next.
    "management_tokens",
];

/// Map an entity table name to the `ns` field emitted on `events:entity`.
///
/// The gateway's consumer switches on a fixed vocabulary that is NOT the
/// table name (e.g. `users` → `user`, `api_keys` → `apikey`). `models`
/// has no consumer namespace; the gateway ignores unknown ns values, so
/// we simply skip emitting for it.
pub(crate) fn ns_for_table(table: &str) -> Option<&'static str> {
    match table {
        "users" => Some("user"),
        "user_groups" => Some("user_group"),
        "api_keys" => Some("apikey"),
        "vendors" => Some("vendor"),
        "vendor_keys" => Some("vendor_key"),
        "model_groups" => Some("model_group"),
        "pricings" => Some("pricing"),
        "jwks" => Some("jwks"),
        "ip_blocks" => Some("ipblock"),
        // T4-B: ns the gateway's stream_consumer dispatches to
        // `EntityStore::invalidate_mgmt_token`.
        "management_tokens" => Some("management_token"),
        // `models` has no consumer ns — skip emit.
        "models" => None,
        // The gateway reads `auth_configs` row "global" once at boot;
        // there is no stream consumer, so no invalidation event.
        "auth_configs" => None,
        _ => None,
    }
}

fn validate_table(table: &str) -> anyhow::Result<()> {
    if ENTITY_TABLES.contains(&table) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("table '{table}' not in entity allowlist"))
    }
}

pub struct Store {
    pub pg: sqlx::PgPool,
    pub redis: Arc<RedisPool>,
}

impl Store {
    pub fn new(pg: sqlx::PgPool, redis: Arc<RedisPool>) -> Self {
        Self { pg, redis }
    }

    /// Provision tables that the gateway expects to read but does not
    /// create itself. Currently just `auth_configs` (gateway TODO — not
    /// in migrations 0001-0004; the control-plane owns this table).
    ///
    /// Idempotent: uses `CREATE TABLE IF NOT EXISTS`.
    pub async fn ensure_schema(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS auth_configs (
                id TEXT PRIMARY KEY,
                payload JSONB NOT NULL,
                version BIGINT NOT NULL DEFAULT 0,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                deleted_at TIMESTAMPTZ NULL
            )",
        )
        .execute(&self.pg)
        .await?;
        Ok(())
    }

    /// Upsert an entity row by id, bump version, then publish an
    /// invalidation event on `events:entity` (unless the table has no
    /// consumer ns).
    ///
    /// We also DEL the gateway's Redis L1 cache key (`entity:{ns}:{id}`)
    /// just before emitting, so the gateway's next cold load (after
    /// Moka invalidation) hits PG and not a stale Redis hash. Without
    /// this, in-place mutations of an already-cached id are invisible
    /// for the L1 TTL (e.g. 3600s for `jwks`) — the gateway's loader
    /// (gw-store/src/loader.rs::load_entity) reads L1 first and never
    /// checks version against PG. Required for the JWKS rotation
    /// scenario.
    pub async fn put_entity(
        &self,
        table: &str,
        id: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        validate_table(table)?;
        // Table name is allowlist-validated above and cannot be a bind
        // parameter in PostgreSQL DDL/DML. id + payload are bind params.
        let sql = format!(
            "INSERT INTO {table} (id, payload, version) VALUES ($1, $2, 0) \
             ON CONFLICT (id) DO UPDATE SET payload = EXCLUDED.payload, \
             version = {table}.version + 1, updated_at = now(), deleted_at = NULL"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(&payload)
            .execute(&self.pg)
            .await?;
        if let Some(ns) = ns_for_table(table) {
            // Drop the gateway's Redis L1 cache for this entity so the
            // next cold load (after Moka invalidation) re-reads from PG.
            let l1_key = format!("entity:{ns}:{id}");
            let _: i64 = self.redis.next().del(l1_key).await?;
            events::emit_invalidate(&self.redis, ns, id).await?;
        }
        Ok(())
    }

    /// Soft-delete an entity row and publish an invalidation event.
    ///
    /// Also DEL the gateway's Redis L1 cache key so the next cold load
    /// (post-Moka invalidation) hits PG. See `put_entity` for rationale.
    pub async fn delete_entity(&self, table: &str, id: &str) -> anyhow::Result<()> {
        validate_table(table)?;
        let sql =
            format!("UPDATE {table} SET deleted_at = now(), version = version + 1 WHERE id = $1");
        sqlx::query(&sql).bind(id).execute(&self.pg).await?;
        if let Some(ns) = ns_for_table(table) {
            let l1_key = format!("entity:{ns}:{id}");
            let _: i64 = self.redis.next().del(l1_key).await?;
            events::emit_invalidate(&self.redis, ns, id).await?;
        }
        Ok(())
    }

    /// SET `counter:{scope}:{id}:{field}` to the given i64.
    pub async fn set_counter(
        &self,
        scope: &str,
        id: &str,
        field: &str,
        value: i64,
    ) -> anyhow::Result<()> {
        let key = format!("counter:{scope}:{id}:{field}");
        let _: () = self
            .redis
            .next()
            .set(&key, value, None, None, false)
            .await?;
        Ok(())
    }

    /// Convenience: SET `counter:user:{user_id}:balance_micro`.
    pub async fn set_balance(&self, user_id: &str, micros: i64) -> anyhow::Result<()> {
        self.set_counter("user", user_id, "balance_micro", micros)
            .await
    }

    /// SET an arbitrary fully-qualified counter key (used for the
    /// `daily_tokens:{day}` family whose suffix is a date, not a plain
    /// field segment).
    pub async fn set_raw_counter(&self, key: &str, value: i64) -> anyhow::Result<()> {
        let _: () = self.redis.next().set(key, value, None, None, false).await?;
        Ok(())
    }

    /// T4-B: SET `mgmt_token:blacklist:{token_id}` with TTL, simulating the
    /// Go-side revoke surface. The gateway's `RedisJwtBlacklist::check_mgmt`
    /// probes exactly this key, fail-closed; SET-then-request makes a
    /// post-cache revocation take effect immediately (entity Moka cache is
    /// bypassed for the blacklist check).
    pub async fn set_mgmt_blacklist(&self, token_id: &str, ttl_secs: i64) -> anyhow::Result<()> {
        let key = format!("mgmt_token:blacklist:{token_id}");
        let _: () = self
            .redis
            .next()
            .set(
                key,
                "1",
                Some(fred::types::Expiration::EX(ttl_secs.max(1))),
                None,
                false,
            )
            .await?;
        Ok(())
    }

    /// XLEN over an arbitrary stream. Returns 0 for an empty/missing stream.
    pub async fn xlen(&self, stream: &str) -> anyhow::Result<i64> {
        let v: i64 = self.redis.next().xlen(stream).await?;
        Ok(v)
    }

    /// Full-state reset for per-test isolation.
    ///
    /// IMPORTANT — why this is NOT `FLUSHALL`:
    ///
    /// The gateway's `StreamConsumer` (crates/gw-store/src/stream_consumer.rs)
    /// creates its `events:entity` and `events:counter` consumer groups
    /// EXACTLY ONCE at boot (`xgroup_create` with `mkstream=true`,
    /// stream_consumer.rs:40-45) and never recreates them. If we
    /// `FLUSHALL` the Redis between tests, the streams AND the consumer
    /// groups are destroyed; `consume_loop` then spins forever on
    /// `NOGROUP No such key 'events:entity' or consumer group ...` with a
    /// 500 ms backoff (stream_consumer.rs:77-86) and entity-cache
    /// invalidation is permanently dead for the gateway process lifetime.
    /// That breaks every invalidation-dependent scenario (cache
    /// invalidation, JWKS rotation, ...).
    ///
    /// So we do SELECTIVE Redis cleanup that PRESERVES `events:entity`
    /// and `events:counter` (and therefore the gateway's consumer groups),
    /// and deletes only the harness-owned keys:
    ///
    /// 1. PG: TRUNCATE every entity table with `RESTART IDENTITY CASCADE`,
    ///    EXCEPT `auth_configs`. The single global `auth_configs` row is
    ///    seeded once by Postgres initdb (`tests/e2e/seed_auth_config.sql`)
    ///    and loaded by the gateway at boot — it is owned by the boot
    ///    lifecycle, not by any test. Truncating it on the first reset
    ///    would leave a later gateway restart with no JWT validator
    ///    config; so `reset()` preserves it (table list = [`TRUNCATE_TABLES`],
    ///    which is [`ENTITY_TABLES`] minus `auth_configs`). JWT scenarios
    ///    that need a different config still overwrite it via `put_entity`.
    /// 2. Redis: DEL every `counter:*` key (per-user balances + daily
    ///    token windows; prefix from `gw-store/src/counter_store.rs`).
    /// 3. Redis: DEL every `entity:*` key — the gateway's L1 Redis-side
    ///    write-through cache for entity rows
    ///    (`gw-store/src/loader.rs::load_entity` writes
    ///    `entity:{ns}:{id}` hashes with TTL). Deleting these forces a
    ///    cold PG load on the next access, in lockstep with the
    ///    invalidation event a subsequent `put_entity` emits — so even if
    ///    a same-id row reappears, the gateway can't serve a stale L1.
    /// 4. Redis: DEL the usage streams (`usage:records`,
    ///    `usage:records:dlq`, `usage:detailed`). These are harness/
    ///    observability streams, not invalidation streams; the gateway
    ///    does not maintain consumer groups on them.
    ///
    /// We deliberately do NOT touch `events:entity` / `events:counter`,
    /// for the reason above. The drain cursor reset (Mutex<String> = "0-0")
    /// is owned by the HTTP handler in `AppState`.
    ///
    /// Failure semantics: the steps are not transactional across PG+Redis. A
    /// Redis error after the PG TRUNCATE leaves Redis half-cleaned; this is
    /// acceptable for a test fake — the next test's seed/assert fails loudly
    /// rather than silently, so a botched reset can't masquerade as a pass.
    pub async fn reset(&self) -> anyhow::Result<()> {
        // 1. PG truncate (one statement → atomic; can't half-truncate).
        //    Table list = `TRUNCATE_TABLES` = `ENTITY_TABLES` minus
        //    `auth_configs` — the boot-seeded global auth_config row is
        //    preserved so gateway-restart scenarios keep their JWT
        //    validator config (see the doc-comment above + `TRUNCATE_TABLES`).
        let tables = TRUNCATE_TABLES.join(", ");
        let sql = format!("TRUNCATE {tables} RESTART IDENTITY CASCADE");
        sqlx::query(&sql).execute(&self.pg).await?;

        // 2-3. SELECTIVE Redis DEL by prefix. We deliberately enumerate
        //      the harness-owned prefixes instead of `FLUSHALL` so the
        //      gateway's `events:entity` / `events:counter` streams and
        //      their consumer groups survive — see doc above. Fred 9's
        //      `KeysInterface` does not expose plain `KEYS pattern`, so
        //      we use `scan_buffered` and collect ourselves. The e2e
        //      Redis is small (one process, one DB) so buffering all
        //      matching keys is fine.
        let client = self.redis.next();
        for pattern in ["counter:*", "entity:*", "mgmt_token:blacklist:*"] {
            let mut keys: Vec<fred::types::RedisKey> = Vec::new();
            let mut stream = client.scan_buffered(pattern, Some(200), None);
            while let Some(k) = stream.next().await {
                keys.push(k?);
            }
            if !keys.is_empty() {
                let _: i64 = client.del(keys).await?;
            }
        }

        // 4. Harness-owned streams (NOT events:*). The gateway has no
        //    consumer group on these, so DEL is safe.
        for stream in ["usage:records", "usage:records:dlq", "usage:detailed"] {
            let _: i64 = client.del(stream).await?;
        }

        Ok(())
    }
}
