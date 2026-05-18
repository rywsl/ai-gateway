//! Redis pool wrapper (fred 9.x).

use fred::clients::RedisPool;
use fred::interfaces::ClientLike;
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
use std::sync::Arc;

#[derive(Clone)]
pub struct RedisCtx {
    pub pool: Arc<RedisPool>,
}

impl RedisCtx {
    pub async fn connect(url: &str, pool_size: usize) -> anyhow::Result<Self> {
        let cfg = RedisConfig::from_url(url)?;
        let pool = RedisPool::new(
            cfg,
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::default()),
            pool_size,
        )?;
        pool.init().await?;
        Ok(Self { pool: Arc::new(pool) })
    }
}
