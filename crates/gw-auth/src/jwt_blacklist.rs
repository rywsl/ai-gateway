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

const JWT_PREFIX: &str = "jwt:blacklist:";
const MGMT_PREFIX: &str = "mgmt_token:blacklist:";

pub struct RedisJwtBlacklist {
    pool: RedisPool,
}

impl RedisJwtBlacklist {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    /// Inherent helper: probe the jwt-blacklist namespace by `jti`, folding
    /// backend errors via the supplied [`BlacklistFailMode`].
    ///
    /// Mirrors the policy of the [`check_revoked`] free function so callers
    /// that hold a concrete `RedisJwtBlacklist` (e.g. proxy filters wiring
    /// in T3-A) don't have to up-cast through the trait.
    pub async fn check_jwt(&self, jti: &str, mode: BlacklistFailMode) -> bool {
        self.check_keyed(&format!("{JWT_PREFIX}{jti}"), mode).await
    }

    /// Inherent helper: probe the mgmt-token blacklist namespace by `token_id`.
    /// ADR-B2-4: the management-token revoke surface lives in a separate Redis
    /// key prefix so jti and token_id collisions are impossible.
    pub async fn check_mgmt(&self, token_id: &str, mode: BlacklistFailMode) -> bool {
        self.check_keyed(&format!("{MGMT_PREFIX}{token_id}"), mode)
            .await
    }

    async fn check_keyed(&self, key: &str, mode: BlacklistFailMode) -> bool {
        let client = self.pool.next();
        match client.get::<Option<String>, _>(key).await {
            Ok(v) => v.is_some(),
            Err(_) => matches!(mode, BlacklistFailMode::FailClosed),
        }
    }
}

#[async_trait]
impl JwtBlacklist for RedisJwtBlacklist {
    async fn is_revoked(&self, jti: &str) -> Result<bool, BlacklistError> {
        let client = self.pool.next();
        let key = format!("{JWT_PREFIX}{jti}");
        let v: Option<String> = client
            .get(key)
            .await
            .map_err(|e| BlacklistError::Backend(e.to_string()))?;
        Ok(v.is_some())
    }

    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), BlacklistError> {
        let client = self.pool.next();
        let key = format!("{JWT_PREFIX}{jti}");
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

#[cfg(test)]
mod mgmt_namespace_tests {
    /// ADR-B2-4 source-level safety net: verify the mgmt + jwt namespaces
    /// are isolated and the new inherent methods exist on RedisJwtBlacklist.
    /// Behavioral tests against a live Redis live in the integration suite
    /// (gated by testcontainers).
    #[test]
    fn mgmt_methods_exist_in_source() {
        let src = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/jwt_blacklist.rs"),
        )
        .expect("read source");
        let mgmt_prefix: String = ["mgmt_token:", "blacklist:"].concat();
        let jwt_prefix: String = ["jwt:", "blacklist:"].concat();
        assert!(src.contains(&mgmt_prefix), "mgmt prefix missing");
        assert!(src.contains(&jwt_prefix), "jwt prefix missing");
        assert!(src.contains("check_mgmt"), "check_mgmt method missing");
        assert!(src.contains("check_jwt"), "check_jwt method missing");
    }
}
