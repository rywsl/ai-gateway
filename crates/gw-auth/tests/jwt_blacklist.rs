use async_trait::async_trait;
use gw_auth::jwt_blacklist::{check_revoked, BlacklistError, BlacklistFailMode, JwtBlacklist};
use std::sync::Arc;

struct AlwaysOk;
struct AlwaysFail;
struct Hit;

#[async_trait]
impl JwtBlacklist for AlwaysOk {
    async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> {
        Ok(false)
    }
    async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> {
        Ok(())
    }
}
#[async_trait]
impl JwtBlacklist for AlwaysFail {
    async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> {
        Err(BlacklistError::Backend("redis down".into()))
    }
    async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> {
        Err(BlacklistError::Backend("redis down".into()))
    }
}
#[async_trait]
impl JwtBlacklist for Hit {
    async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> {
        Ok(true)
    }
    async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> {
        Ok(())
    }
}

#[tokio::test]
async fn fail_closed_treats_redis_down_as_revoked() {
    let bl: Arc<dyn JwtBlacklist> = Arc::new(AlwaysFail);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailClosed).await);
}

#[tokio::test]
async fn fail_open_treats_redis_down_as_clean() {
    let bl: Arc<dyn JwtBlacklist> = Arc::new(AlwaysFail);
    assert!(!check_revoked(&*bl, "jti1", BlacklistFailMode::FailOpen).await);
}

#[tokio::test]
async fn hit_means_revoked_regardless_of_mode() {
    let bl: Arc<dyn JwtBlacklist> = Arc::new(Hit);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailClosed).await);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailOpen).await);
}

#[tokio::test]
async fn miss_means_not_revoked() {
    let bl: Arc<dyn JwtBlacklist> = Arc::new(AlwaysOk);
    assert!(!check_revoked(&*bl, "jti1", BlacklistFailMode::FailClosed).await);
    assert!(!check_revoked(&*bl, "jti1", BlacklistFailMode::FailOpen).await);
}
