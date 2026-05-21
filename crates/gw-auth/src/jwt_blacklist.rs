//! JWT blacklist 接口 + Redis 实现 + FailMode 决策。

use async_trait::async_trait;
use fred::clients::RedisPool;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;

#[derive(Debug, thiserror::Error)]
pub enum BlacklistError {
    #[error("backend: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, Copy)]
pub enum BlacklistFailMode {
    FailClosed,
    FailOpen,
}

impl From<gw_core::entity::BlacklistFailMode> for BlacklistFailMode {
    fn from(v: gw_core::entity::BlacklistFailMode) -> Self {
        match v {
            gw_core::entity::BlacklistFailMode::FailClosed => Self::FailClosed,
            gw_core::entity::BlacklistFailMode::FailOpen => Self::FailOpen,
        }
    }
}

#[async_trait]
pub trait JwtBlacklist: Send + Sync {
    async fn is_revoked(&self, jti: &str) -> Result<bool, BlacklistError>;
    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), BlacklistError>;
}

/// 高层入口：按 fail mode 折叠 backend error。
pub async fn check_revoked(
    bl: &dyn JwtBlacklist,
    jti: &str,
    mode: BlacklistFailMode,
) -> bool {
    match bl.is_revoked(jti).await {
        Ok(b) => b,
        Err(_) => matches!(mode, BlacklistFailMode::FailClosed),
    }
}

const KEY_PREFIX: &str = "auth:jwt_blacklist:";

pub struct RedisJwtBlacklist {
    pool: RedisPool,
}

impl RedisJwtBlacklist {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JwtBlacklist for RedisJwtBlacklist {
    async fn is_revoked(&self, jti: &str) -> Result<bool, BlacklistError> {
        let client = self.pool.next();
        let key = format!("{KEY_PREFIX}{jti}");
        let v: Option<String> = client
            .get(key)
            .await
            .map_err(|e| BlacklistError::Backend(e.to_string()))?;
        Ok(v.is_some())
    }

    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), BlacklistError> {
        let client = self.pool.next();
        let key = format!("{KEY_PREFIX}{jti}");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let ttl = until_unix_s.saturating_sub(now).max(1);
        let _: () = client
            .set(
                key,
                "1",
                Some(Expiration::EX(ttl as i64)),
                None,
                false,
            )
            .await
            .map_err(|e| BlacklistError::Backend(e.to_string()))?;
        Ok(())
    }
}
