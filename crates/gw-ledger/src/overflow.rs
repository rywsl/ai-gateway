//! Overflow gate for stale or missing per-pod credit leases.

use fred::clients::RedisPool;
use fred::interfaces::{KeysInterface, LuaInterface};
use fred::types::Expiration;
use gw_runtime::redis_retry::{retry, RetryConfig};
use sqlx::PgPool;
use std::time::Duration;
use thiserror::Error;

pub const BALANCE_CACHE_PREFIX: &str = "credit:balance_cache";
pub const OVERFLOW_CONSUMED_PREFIX: &str = "credit:overflow_consumed";
pub const DEFAULT_WINDOW_TTL: Duration = Duration::from_secs(30);

pub const FETCH_BALANCE_SQL: &str = r#"
SELECT COALESCE(recharge_balance_micros, 0) + COALESCE(gift_balance_micros, 0)
FROM wallets
WHERE user_id = $1 AND deleted_at IS NULL
"#;

const OVERFLOW_INCR_LUA: &str = r#"
local new_value = redis.call('INCRBY', KEYS[1], ARGV[1])
if tonumber(new_value) == tonumber(ARGV[1]) then
  redis.call('EXPIRE', KEYS[1], ARGV[2])
end
return new_value
"#;

#[derive(Clone, Debug)]
pub struct OverflowConfig {
    pub window_ttl: Duration,
    pub balance_cache_ttl: Duration,
    pub retry: RetryConfig,
}

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            window_ttl: DEFAULT_WINDOW_TTL,
            balance_cache_ttl: DEFAULT_WINDOW_TTL,
            retry: RetryConfig {
                op_name: "overflow_gate".to_string(),
                max_retries: 5,
                initial_backoff: Duration::from_millis(100),
                multipliers: vec![1.0, 1.0, 2.0, 4.0, 8.0],
                jitter: 0.0,
                total_deadline: Duration::from_secs(5),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowDecision {
    Allow,
    BalanceInsufficient,
}

#[derive(Error, Debug)]
pub enum OverflowError {
    #[error("redis error: {0}")]
    Redis(#[from] fred::error::RedisError),
    #[error("pg error: {0}")]
    Pg(#[from] sqlx::Error),
    #[error("balance cache and fallback balance source unavailable")]
    CacheUnavailable,
}

#[derive(Clone)]
pub struct OverflowGate {
    redis: RedisPool,
    pg: PgPool,
    cfg: OverflowConfig,
}

impl OverflowGate {
    pub fn new(redis: RedisPool, pg: PgPool, cfg: OverflowConfig) -> Self {
        Self { redis, pg, cfg }
    }

    pub async fn check(
        &self,
        user_id: &str,
        estimate_credits: i64,
    ) -> Result<OverflowDecision, OverflowError> {
        let balance = self.balance_for(user_id).await?;
        let consumed = self.consumed_for(user_id).await?;

        if decide_overflow(balance, consumed, estimate_credits)
            == OverflowDecision::BalanceInsufficient
        {
            return Ok(OverflowDecision::BalanceInsufficient);
        }

        self.incr_consumed(user_id, estimate_credits).await?;
        Ok(OverflowDecision::Allow)
    }

    pub async fn record_actual(
        &self,
        user_id: &str,
        reserved_credits: i64,
        actual_credits: i64,
    ) -> Result<(), OverflowError> {
        let Some(delta) = record_actual_delta(reserved_credits, actual_credits) else {
            return Ok(());
        };
        self.incr_consumed(user_id, delta).await
    }

    async fn balance_for(&self, user_id: &str) -> Result<i64, OverflowError> {
        let key = balance_cache_key(user_id);
        match self.redis.get::<Option<i64>, _>(&key).await {
            Ok(Some(balance)) => return Ok(balance),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    target: "gw_ledger::overflow",
                    user_id,
                    error = ?err,
                    "balance cache read failed; falling back to postgres"
                );
            }
        }

        let balance = retry(&self.cfg.retry, || {
            let pg = self.pg.clone();
            let user_id = user_id.to_string();
            async move { fetch_balance_micro(&pg, &user_id).await }
        })
        .await
        .map_err(OverflowError::Pg)?;

        let cache_ttl = self.cfg.balance_cache_ttl.as_secs().max(1) as i64;
        if let Err(err) = self
            .redis
            .set::<(), _, _>(&key, balance, Some(Expiration::EX(cache_ttl)), None, false)
            .await
        {
            tracing::warn!(
                target: "gw_ledger::overflow",
                user_id,
                error = ?err,
                "balance cache write failed"
            );
        }

        Ok(balance)
    }

    async fn consumed_for(&self, user_id: &str) -> Result<i64, OverflowError> {
        let key = overflow_consumed_key(user_id);
        match self.redis.get::<Option<i64>, _>(&key).await {
            Ok(Some(consumed)) => Ok(consumed),
            Ok(None) => Ok(0),
            Err(err) => {
                tracing::error!(
                    target: "gw_ledger::overflow",
                    user_id,
                    error = ?err,
                    "overflow consumed read failed"
                );
                Err(OverflowError::CacheUnavailable)
            }
        }
    }

    async fn incr_consumed(
        &self,
        user_id: &str,
        estimate_credits: i64,
    ) -> Result<(), OverflowError> {
        let key = overflow_consumed_key(user_id);
        let ttl_secs = self.cfg.window_ttl.as_secs().max(1) as i64;
        retry(&self.cfg.retry, || {
            let redis = self.redis.clone();
            let key = key.clone();
            async move {
                let _: i64 = redis
                    .eval(
                        OVERFLOW_INCR_LUA,
                        vec![key.as_str()],
                        vec![estimate_credits, ttl_secs],
                    )
                    .await?;
                Ok::<(), fred::error::RedisError>(())
            }
        })
        .await
        .map_err(|err| {
            tracing::error!(
                target: "gw_ledger::overflow",
                user_id,
                error = ?err,
                "overflow consumed increment failed"
            );
            OverflowError::CacheUnavailable
        })
    }
}

pub async fn fetch_balance_micro(pool: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(FETCH_BALANCE_SQL)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map(|v| v.unwrap_or(0))
}

pub fn balance_cache_key(user_id: &str) -> String {
    format!("{BALANCE_CACHE_PREFIX}:{user_id}")
}

pub fn overflow_consumed_key(user_id: &str) -> String {
    format!("{OVERFLOW_CONSUMED_PREFIX}:{user_id}")
}

pub fn decide_overflow(balance: i64, consumed: i64, estimate: i64) -> OverflowDecision {
    if estimate <= 0 || balance.saturating_sub(consumed) >= estimate {
        OverflowDecision::Allow
    } else {
        OverflowDecision::BalanceInsufficient
    }
}

pub fn record_actual_delta(reserved_credits: i64, actual_credits: i64) -> Option<i64> {
    let delta = actual_credits.saturating_sub(reserved_credits);
    (delta > 0).then_some(delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_keys_match_hard_mode_contract() {
        assert_eq!(balance_cache_key("u_1"), "credit:balance_cache:u_1");
        assert_eq!(overflow_consumed_key("u_1"), "credit:overflow_consumed:u_1");
    }

    #[test]
    fn pg_fallback_query_uses_wallet_split_balance_columns() {
        assert!(FETCH_BALANCE_SQL.contains("recharge_balance_micros"));
        assert!(FETCH_BALANCE_SQL.contains("gift_balance_micros"));
        assert!(!FETCH_BALANCE_SQL.contains(" balance_micro "));
        assert!(FETCH_BALANCE_SQL.contains("deleted_at IS NULL"));
    }

    #[test]
    fn overflow_decision_requires_remaining_balance() {
        assert_eq!(
            decide_overflow(10_000, 2_000, 8_000),
            OverflowDecision::Allow
        );
        assert_eq!(
            decide_overflow(10_000, 2_001, 8_000),
            OverflowDecision::BalanceInsufficient
        );
        assert_eq!(
            decide_overflow(0, 0, 1),
            OverflowDecision::BalanceInsufficient
        );
    }

    #[test]
    fn incr_lua_uses_keys_for_cluster_routing_and_expires_only_first_set() {
        assert!(OVERFLOW_INCR_LUA.contains("INCRBY', KEYS[1], ARGV[1]"));
        assert!(OVERFLOW_INCR_LUA.contains("EXPIRE', KEYS[1], ARGV[2]"));
        assert!(OVERFLOW_INCR_LUA.contains("new_value) == tonumber(ARGV[1])"));
    }

    #[test]
    fn record_actual_delta_only_counts_actual_over_reserved() {
        assert_eq!(record_actual_delta(1000, 1500), Some(500));
        assert_eq!(record_actual_delta(1000, 1000), None);
        assert_eq!(record_actual_delta(1000, 500), None);
        assert_eq!(record_actual_delta(-10, 5), Some(15));
    }
}
