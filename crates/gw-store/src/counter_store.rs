//! Counter store — runtime counters (rpm/tpm/balance/usage) in Redis.
//!
//! Keys follow the convention `counter:{scope}:{id}:{field}`.

use super::redis_client::RedisCtx;
use fred::interfaces::KeysInterface;
use gw_core::ids::UserId;

#[derive(Clone)]
pub struct CounterStore {
    pub redis: RedisCtx,
}

impl CounterStore {
    pub async fn get(&self, scope: &str, id: &str, field: &str) -> anyhow::Result<i64> {
        let key = format!("counter:{scope}:{id}:{field}");
        let v: Option<i64> = self.redis.pool.next().get(&key).await?;
        Ok(v.unwrap_or(0))
    }

    pub async fn incrby(
        &self,
        scope: &str,
        id: &str,
        field: &str,
        delta: i64,
    ) -> anyhow::Result<i64> {
        let key = format!("counter:{scope}:{id}:{field}");
        let v: i64 = self.redis.pool.next().incr_by(&key, delta).await?;
        Ok(v)
    }

    /// G3 — Read a raw, fully-qualified counter key (e.g. one built by
    /// [`daily_tokens_key`]). Returns 0 when the key is missing. The daily
    /// quota key suffix is a date and does not fit the 4-segment scope/id/field
    /// layout, so we expose a free-form read alongside the structured `get`.
    pub async fn get_str_key(&self, key: &str) -> anyhow::Result<i64> {
        let v: Option<i64> = self.redis.pool.next().get(key).await?;
        Ok(v.unwrap_or(0))
    }

    /// G3 — Atomic `INCRBY key delta` followed by `EXPIRE key ttl_secs`. Used
    /// from the logging filter to bump `counter:user:{eid}:daily_tokens:{day}`
    /// by the observed input+output tokens and pin a 48 h TTL so historical
    /// days are GC'd without an explicit sweeper.
    ///
    /// Both Redis commands are best-effort: a transient failure returns `Err`
    /// and the caller logs + drops it (daily quota is observational, not
    /// billing — billing lives in Go via `usage:records`).
    pub async fn incrby_with_expire(
        &self,
        key: &str,
        delta: i64,
        ttl_secs: i64,
    ) -> anyhow::Result<i64> {
        let client = self.redis.pool.next();
        let v: i64 = client.incr_by(key, delta).await?;
        let _: Result<(), _> = client.expire(key, ttl_secs).await;
        Ok(v)
    }
}

/// G3 (ADR-G3-1) — key shape for the per-user, per-UTC-day token counter.
/// Centralised so both the `request_filter` pre-check and the `logging`
/// post-increment cannot diverge.
pub fn daily_tokens_key(uid: &UserId, day_utc: &str) -> String {
    format!("counter:user:{}:daily_tokens:{}", uid.as_ref(), day_utc)
}
