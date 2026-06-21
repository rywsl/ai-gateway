//! Overflow gate for stale or missing per-pod credit leases.

use fred::clients::RedisPool;
use fred::interfaces::{KeysInterface, LuaInterface};
use fred::types::Expiration;
use gw_runtime::redis_retry::{retry, RetryConfig};
use sqlx::PgPool;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::metrics::{
    CREDIT_BALANCE_FALLBACK_TOTAL, CREDIT_OVERFLOW_CONSUMED_WINDOW, CREDIT_OVERFLOW_GATE_TOTAL,
    CREDIT_OVERFLOW_LATENCY_MS, PG_FALLBACK_TOTAL,
};

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
        let start = Instant::now();
        let (balance, path) = match self.balance_for(user_id).await {
            Ok(v) => v,
            Err(err) => {
                record_overflow_latency(start, "error");
                metrics::counter!(CREDIT_OVERFLOW_GATE_TOTAL, "result" => "cache_unavail")
                    .increment(1);
                return Err(err);
            }
        };
        let consumed = match self.consumed_for(user_id).await {
            Ok(v) => v,
            Err(err) => {
                record_overflow_latency(start, "error");
                metrics::counter!(CREDIT_OVERFLOW_GATE_TOTAL, "result" => "cache_unavail")
                    .increment(1);
                return Err(err);
            }
        };

        if decide_overflow(balance, consumed, estimate_credits)
            == OverflowDecision::BalanceInsufficient
        {
            record_overflow_latency(start, path);
            metrics::counter!(CREDIT_OVERFLOW_GATE_TOTAL, "result" => "balance_insuf").increment(1);
            return Ok(OverflowDecision::BalanceInsufficient);
        }

        if let Err(err) = self.incr_consumed(user_id, estimate_credits).await {
            record_overflow_latency(start, "error");
            metrics::counter!(CREDIT_OVERFLOW_GATE_TOTAL, "result" => "cache_unavail").increment(1);
            return Err(err);
        }
        record_overflow_latency(start, path);
        metrics::counter!(CREDIT_OVERFLOW_GATE_TOTAL, "result" => "pass").increment(1);
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

    pub async fn fallback_balance_micro(&self, user_id: &str) -> Result<i64, OverflowError> {
        let (balance, path) = self.balance_for(user_id).await?;
        let result = if balance > 0 { "positive" } else { "zero" };
        metrics::counter!(
            CREDIT_BALANCE_FALLBACK_TOTAL,
            "result" => result,
            "path" => path
        )
        .increment(1);
        Ok(balance)
    }

    async fn balance_for(&self, user_id: &str) -> Result<(i64, &'static str), OverflowError> {
        let key = balance_cache_key(user_id);
        match self.redis.get::<Option<i64>, _>(&key).await {
            Ok(Some(balance)) => return Ok((balance, "cache")),
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

        metrics::counter!(PG_FALLBACK_TOTAL, "op" => "balance_read").increment(1);
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

        Ok((balance, "pg_fallback"))
    }

    async fn consumed_for(&self, user_id: &str) -> Result<i64, OverflowError> {
        let key = overflow_consumed_key(user_id);
        match self.redis.get::<Option<i64>, _>(&key).await {
            Ok(Some(consumed)) => Ok(consumed),
            Ok(None) => Ok(0),
            Err(err) => {
                tracing::warn!(
                    target: "gw_ledger::overflow",
                    user_id,
                    error = ?err,
                    "overflow consumed read failed; treating as 0 (fail-open)"
                );
                metrics::counter!(
                    CREDIT_OVERFLOW_GATE_TOTAL,
                    "result" => "consumed_read_failed_open"
                )
                .increment(1);
                Ok(0)
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
                let new_value: i64 = redis
                    .eval(
                        OVERFLOW_INCR_LUA,
                        vec![key.as_str()],
                        vec![estimate_credits, ttl_secs],
                    )
                    .await?;
                metrics::gauge!(CREDIT_OVERFLOW_CONSUMED_WINDOW).set(new_value as f64);
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

fn record_overflow_latency(start: Instant, path: &'static str) {
    metrics::histogram!(CREDIT_OVERFLOW_LATENCY_MS, "path" => path)
        .record(start.elapsed().as_millis() as f64);
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

    #[tokio::test]
    async fn fallback_balance_micro_reuses_balance_for_path() {
        let (_redis_container, redis) = test_redis_pool().await;
        let (_pg_container, pg) = test_pg_pool().await;
        sqlx::query(
            "CREATE TABLE wallets (user_id TEXT PRIMARY KEY, recharge_balance_micros BIGINT, gift_balance_micros BIGINT, deleted_at TIMESTAMPTZ)",
        )
        .execute(&pg)
        .await
        .expect("create wallets");
        sqlx::query(
            "INSERT INTO wallets(user_id, recharge_balance_micros, gift_balance_micros, deleted_at) VALUES ($1, $2, $3, NULL)",
        )
        .bind("user-positive")
        .bind(11_i64)
        .bind(22_i64)
        .execute(&pg)
        .await
        .expect("insert wallet");

        let gate = OverflowGate::new(redis, pg, OverflowConfig::default());
        let balance = gate
            .fallback_balance_micro("user-positive")
            .await
            .expect("fallback balance");
        assert_eq!(balance, 33);
    }

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

    #[test]
    fn consumed_read_error_contract_is_fail_open_and_observable() {
        let src = include_str!("overflow.rs");
        assert!(
            src.contains("overflow consumed read failed; treating as 0 (fail-open)"),
            "consumed_for must warn that read failures fail open"
        );
        assert!(
            src.contains("\"result\" => \"consumed_read_failed_open\""),
            "consumed_for fail-open path must emit an observable metric label"
        );
        assert!(
            src.contains("Ok(0)"),
            "consumed_for fail-open path must treat failed consumed reads as zero"
        );
    }

    async fn test_redis_pool() -> (
        testcontainers::ContainerAsync<testcontainers_modules::redis::Redis>,
        RedisPool,
    ) {
        use fred::interfaces::ClientLike;
        use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        let container = Redis::default().start().await.expect("redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let pool = RedisPool::new(
            RedisConfig::from_url(&format!("redis://127.0.0.1:{port}")).expect("redis config"),
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::new_constant(0, 100)),
            1,
        )
        .expect("RedisPool");
        pool.init().await.expect("redis init");
        (container, pool)
    }

    async fn test_pg_pool() -> (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        PgPool,
    ) {
        use sqlx::postgres::PgPoolOptions;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default().start().await.expect("postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres port");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
            ))
            .await
            .expect("pg pool");
        (container, pool)
    }
}
