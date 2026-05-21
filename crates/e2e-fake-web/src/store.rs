//! `Store` — the thin Postgres + Redis facade the fake control-plane uses
//! to mutate the same state shape the gateway's hot path reads.
//!
//! Hard invariant: this crate must not depend on any gw-* crate that
//! carries the gateway's domain types (gw-store, gw-auth, gw-ledger,
//! gw-codec, gw-proxy, gw-gateway). We only use sqlx + fred directly
//! and serialize whatever JSON payload the test harness hands us.

use crate::events;
use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, KeysInterface, StreamsInterface};
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
    // `auth_configs` is owned by the fake control-plane (gateway TODO
    // table not yet in migrations). No consumer ns — same treatment as
    // `models` — so we skip emitting invalidation for it.
    "auth_configs",
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
        Err(anyhow::anyhow!("table '{}' not in entity allowlist", table))
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
            events::emit_invalidate(&self.redis, ns, id).await?;
        }
        Ok(())
    }

    /// Soft-delete an entity row and publish an invalidation event.
    pub async fn delete_entity(&self, table: &str, id: &str) -> anyhow::Result<()> {
        validate_table(table)?;
        let sql =
            format!("UPDATE {table} SET deleted_at = now(), version = version + 1 WHERE id = $1");
        sqlx::query(&sql).bind(id).execute(&self.pg).await?;
        if let Some(ns) = ns_for_table(table) {
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

    /// XLEN over an arbitrary stream. Returns 0 for an empty/missing stream.
    pub async fn xlen(&self, stream: &str) -> anyhow::Result<i64> {
        let v: i64 = self.redis.next().xlen(stream).await?;
        Ok(v)
    }

    /// Full-state reset for per-test isolation:
    ///
    /// 1. `FLUSHALL` on Redis (NOT `FLUSHDB`) — the e2e Redis is
    ///    single-purpose, so wiping every key across every database
    ///    (counters, streams, consumer groups) is the simplest way to
    ///    guarantee a clean slate between tests.
    /// 2. `TRUNCATE` every entity table with `RESTART IDENTITY CASCADE` so
    ///    autoincrement sequences and any FK fanout reset too. `CASCADE`
    ///    is safe today because v1 entity tables have no inter-table FKs;
    ///    the clause is kept defensively for future migrations.
    ///
    /// Note: this deliberately SKIPS `events::emit_invalidate`. The Redis
    /// stream that carries those events is wiped in step 1, so there are
    /// no consumers left to notify — the gateway's in-memory caches are
    /// expected to be re-seeded by whatever the next test puts back via
    /// `put_entity` (which emits invalidations the normal way).
    ///
    /// Cursor reset for the drain loop is handled by the HTTP handler
    /// (it owns the `Mutex<String>` in `AppState`).
    pub async fn reset(&self) -> anyhow::Result<()> {
        // FLUSHALL is gated on fred's `i-server` feature, which is pulled
        // in by the default `i-std` set, so this just works.
        let _: () = self.redis.next().flushall(false).await?;

        // One statement so it's atomic and we can't half-truncate. The
        // table list is the same allowlist `ENTITY_TABLES` — keep them
        // in sync.
        let tables = ENTITY_TABLES.join(", ");
        let sql = format!("TRUNCATE {tables} RESTART IDENTITY CASCADE");
        sqlx::query(&sql).execute(&self.pg).await?;
        Ok(())
    }
}
