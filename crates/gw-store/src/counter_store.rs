//! Counter store — runtime counters (rpm/tpm/balance/usage) in Redis.
//!
//! Keys follow the convention `counter:{scope}:{id}:{field}`.

use super::redis_client::RedisCtx;
use fred::interfaces::KeysInterface;

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
}
