//! Counter store — runtime counters (rpm/tpm/balance/usage) in Redis.
//!
//! Keys follow the convention `counter:{scope}:{id}:{field}`.

use super::redis_client::RedisCtx;
use fred::interfaces::KeysInterface;
use gw_core::ids::UserId;
use gw_runtime::redis_retry::{retry, RetryConfig};
use std::time::Duration;

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
    /// T-W3-17 (WS-3 bounded memory):
    /// Previously, EXPIRE failure was swallowed with `let _: Result<(), _> =
    /// client.expire(...)`. INCRBY had already succeeded, so swallowing meant
    /// a transient Redis blip left the key with TTL == -1 (PERSIST) — i.e. it
    /// accumulated forever (1 key/user/day). The one-shot migration script
    /// `scripts/one-shot/migrate-daily-tokens-ttl.py` (T-W3-18) cleans up the
    /// historic mess; this retry loop prevents new instances.
    ///
    /// EXPIRE failure is still non-fatal for the caller (daily quota is
    /// observational, not billing), so we log + metric and return `Ok(v)`
    /// with the post-INCRBY counter value. INCRBY failure still returns
    /// `Err` (caller's existing contract).
    pub async fn incrby_with_expire(
        &self,
        key: &str,
        delta: i64,
        ttl_secs: i64,
    ) -> anyhow::Result<i64> {
        let client = self.redis.pool.next();
        let v: i64 = client.incr_by(key, delta).await?;

        let retry_cfg = RetryConfig {
            op_name: "counter_store::expire".to_string(),
            max_retries: 3,
            initial_backoff: Duration::from_millis(50),
            // 4 attempts total (initial + 3 retries) with backoffs 50ms,
            // 100ms, 200ms — bounded by total_deadline (1s).
            multipliers: vec![1.0, 2.0, 4.0],
            jitter: 0.2,
            total_deadline: Duration::from_secs(1),
        };

        let expire_result: Result<bool, fred::error::RedisError> =
            retry(&retry_cfg, || async {
                // `KeysInterface::expire` returns `Result<bool, RedisError>`
                // (true when the key existed and the TTL was set). A `false`
                // return is expected when the key was concurrently evicted
                // between INCRBY and EXPIRE; treat it as success. Only
                // transport / protocol errors are retryable here.
                client.expire(key, ttl_secs).await
            })
            .await;

        if let Err(err) = expire_result {
            // INCRBY already returned the authoritative counter value; do not
            // propagate the EXPIRE error. Use a static low-cardinality label
            // (per WS-3 REV-FINAL-V7 / T-W3-21: no per-tenant / per-request_id
            // labels on the metric).
            let key_prefix = key_prefix_label(key);
            metrics::counter!(
                "gw_counter_store_expire_fail_total",
                "key_prefix" => key_prefix,
            )
            .increment(1);
            tracing::warn!(
                target: "gw_store::counter_store",
                key_prefix,
                ttl_secs,
                err = ?err,
                "EXPIRE retry exhausted; counter retains its existing TTL"
            );
        }

        Ok(v)
    }
}

/// G3 (ADR-G3-1) — key shape for the per-user, per-UTC-day token counter.
/// Centralised so both the `request_filter` pre-check and the `logging`
/// post-increment cannot diverge.
pub fn daily_tokens_key(uid: &UserId, day_utc: &str) -> String {
    format!("counter:user:{}:daily_tokens:{}", uid.as_ref(), day_utc)
}

/// Static, low-cardinality label for `gw_counter_store_expire_fail_total`.
///
/// Bounded to the known `counter:*` namespaces; anything else collapses to
/// `"other"` so a malformed key (or future namespace) cannot inflate the
/// metric's cardinality. See WS-3 REV-FINAL-V7 / T-W3-21.
fn key_prefix_label(key: &str) -> &'static str {
    let Some(after_counter) = key.strip_prefix("counter:") else {
        return "other";
    };
    let scope = after_counter.split(':').next().unwrap_or("");
    match scope {
        "user" if key.contains(":daily_tokens:") => "user_daily_tokens",
        "user" => "user_other",
        "api_key" => "api_key",
        "vendor" => "vendor",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_prefix_label_collapses_to_static_set() {
        assert_eq!(
            key_prefix_label("counter:user:u-123:daily_tokens:2026-06-18"),
            "user_daily_tokens",
        );
        assert_eq!(
            key_prefix_label("counter:user:u-123:rpm:60s"),
            "user_other",
        );
        assert_eq!(
            key_prefix_label("counter:api_key:k-9:rpm:60s"),
            "api_key",
        );
        assert_eq!(key_prefix_label("counter:vendor:v-1:tpm"), "vendor");
        assert_eq!(key_prefix_label("counter:other:x:y"), "other");
        assert_eq!(key_prefix_label("not_a_counter_key"), "other");
    }
}
