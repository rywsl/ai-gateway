use crate::ctx::{OAuthRuntime, OAuthTokenResult};
use async_trait::async_trait;
use fred::interfaces::KeysInterface;
use fred::types::{Expiration, SetOptions};
use gw_core::ids::KeyId;
use gw_store::oauth_refresh::{OAuthWriteBack, OAuthWriteBackError, PgOAuthRefreshWriter};
use gw_store::redis_client::RedisCtx;
use gw_store::EntityStore;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub struct OAuthRuntimeCfg {
    pub refresh_skew_secs: i64,
    pub lock_ttl_ms: u64,
    pub request_timeout_ms: u64,
    pub wait_after_lock_lost_ms: u64,
}

pub fn should_refresh(now: OffsetDateTime, expires_at: OffsetDateTime, skew_secs: i64) -> bool {
    now >= expires_at - time::Duration::seconds(skew_secs)
}

pub fn lock_key(key_id: &str) -> String {
    format!("lock:oauth:refresh:{key_id}")
}

pub struct ParsedTokenResponse {
    pub access_token: SecretString,
    pub rotated_refresh_token: Option<SecretString>,
    pub expires_in_secs: i64,
    pub scope: Vec<String>,
}

#[derive(serde::Deserialize)]
struct TokenEndpointResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

pub fn parse_token_response(
    body: &[u8],
    previous_scope: &[String],
) -> anyhow::Result<ParsedTokenResponse> {
    let decoded: TokenEndpointResponse = serde_json::from_slice(body)?;
    let scope = decoded
        .scope
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| previous_scope.to_vec());
    Ok(ParsedTokenResponse {
        access_token: SecretString::from(decoded.access_token),
        rotated_refresh_token: decoded.refresh_token.map(SecretString::from),
        expires_in_secs: decoded.expires_in.unwrap_or(3600),
        scope,
    })
}

pub struct RustOAuthRuntime {
    store: Arc<EntityStore>,
    writer: Arc<PgOAuthRefreshWriter>,
    redis: RedisCtx,
    client: reqwest::Client,
    cfg: OAuthRuntimeCfg,
}

impl RustOAuthRuntime {
    pub fn new(
        store: Arc<EntityStore>,
        writer: Arc<PgOAuthRefreshWriter>,
        redis: RedisCtx,
        client: reqwest::Client,
        cfg: OAuthRuntimeCfg,
    ) -> Self {
        Self {
            store,
            writer,
            redis,
            client,
            cfg,
        }
    }

    async fn reload_token(
        &self,
        vendor_key_id: &KeyId,
        refreshed_vendor_key: bool,
    ) -> Result<OAuthTokenResult, String> {
        self.store.invalidate_vendor_key(vendor_key_id).await;
        let loaded = self
            .store
            .vendor_key_or_load(vendor_key_id)
            .await
            .ok_or_else(|| {
                format!(
                    "vendor key {} not found after oauth reload",
                    vendor_key_id.0
                )
            })?;
        let oauth =
            loaded.value.oauth.as_ref().ok_or_else(|| {
                format!("vendor key {} has no oauth credentials", vendor_key_id.0)
            })?;
        Ok(OAuthTokenResult {
            access_token: oauth.access_token.clone(),
            refreshed_vendor_key: refreshed_vendor_key.then(|| loaded.value.clone()),
            version: loaded.version,
        })
    }

    async fn acquire_lock(&self, vendor_key_id: &KeyId, request_id: &str) -> Result<bool, String> {
        let client = self.redis.pool.next();
        let result: Option<String> = client
            .set(
                lock_key(vendor_key_id.as_ref()),
                request_id,
                Some(Expiration::PX(self.cfg.lock_ttl_ms as i64)),
                Some(SetOptions::NX),
                false,
            )
            .await
            .map_err(|e| format!("oauth refresh lock failed: {e}"))?;
        Ok(result.is_some())
    }

    async fn refresh_with_provider(
        &self,
        endpoint: &str,
        refresh_token: &SecretString,
        previous_scope: &[String],
    ) -> Result<ParsedTokenResponse, String> {
        let send = self
            .client
            .post(endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.expose_secret()),
            ])
            .send();
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.cfg.request_timeout_ms.max(1)),
            send,
        )
        .await
        .map_err(|_| "oauth refresh provider request timed out".to_string())?
        .map_err(|e| format!("oauth refresh provider request failed: {e}"))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("oauth refresh provider body failed: {e}"))?;
        if !status.is_success() {
            return Err(format!("oauth refresh provider returned {status}"));
        }
        parse_token_response(&body, previous_scope)
            .map_err(|e| format!("oauth refresh provider response invalid: {e}"))
    }
}

#[async_trait]
impl OAuthRuntime for RustOAuthRuntime {
    async fn access_token_for(
        &self,
        vendor_key_id: &KeyId,
        request_id: &str,
    ) -> Result<OAuthTokenResult, String> {
        let loaded = self
            .store
            .vendor_key_or_load(vendor_key_id)
            .await
            .ok_or_else(|| format!("vendor key {} not found", vendor_key_id.0))?;
        let oauth =
            loaded.value.oauth.as_ref().ok_or_else(|| {
                format!("vendor key {} has no oauth credentials", vendor_key_id.0)
            })?;

        let now = OffsetDateTime::now_utc();
        if !should_refresh(now, oauth.access_expires_at, self.cfg.refresh_skew_secs) {
            return Ok(OAuthTokenResult {
                access_token: oauth.access_token.clone(),
                refreshed_vendor_key: None,
                version: loaded.version,
            });
        }

        if !self.acquire_lock(vendor_key_id, request_id).await? {
            metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "lock_lost")
                .increment(1);
            tokio::time::sleep(std::time::Duration::from_millis(
                self.cfg.wait_after_lock_lost_ms,
            ))
            .await;
            return self.reload_token(vendor_key_id, true).await;
        }

        let parsed = match self
            .refresh_with_provider(
                &oauth.token_endpoint,
                &oauth.refresh_token,
                oauth.scope.as_slice(),
            )
            .await
        {
            Ok(parsed) => parsed,
            Err(err) => {
                metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "provider_error")
                    .increment(1);
                return Err(err);
            }
        };

        let expires_at = now + time::Duration::seconds(parsed.expires_in_secs.max(1));
        let write = OAuthWriteBack {
            key_id: vendor_key_id.as_ref().to_string(),
            expected_version: loaded.version,
            access_token: parsed.access_token,
            rotated_refresh_token: parsed.rotated_refresh_token,
            previous_refresh_token_enc: None,
            access_expires_at: expires_at,
            token_endpoint: oauth.token_endpoint.clone(),
            scope: parsed.scope,
            quota_project_id: oauth.quota_project_id.clone(),
        };

        match self.writer.write(write).await {
            Ok(_) => {
                metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "ok").increment(1);
                self.reload_token(vendor_key_id, true).await
            }
            Err(OAuthWriteBackError::CasLost { .. }) => {
                metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "cas_lost")
                    .increment(1);
                self.reload_token(vendor_key_id, true).await
            }
            Err(err) => Err(format!("oauth write-back failed: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn should_refresh_uses_skew_window() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        let expires = time::OffsetDateTime::from_unix_timestamp(1_030).unwrap();
        assert!(should_refresh(now, expires, 60));
        assert!(!should_refresh(now, expires, 10));
    }

    #[test]
    fn lock_key_is_per_vendor_key() {
        assert_eq!(lock_key("vk-1"), "lock:oauth:refresh:vk-1");
    }

    #[test]
    fn token_response_preserves_scope_when_scope_is_absent() {
        let body = br#"{"access_token":"new-access","expires_in":3600,"token_type":"bearer"}"#;
        let parsed = parse_token_response(body, &["old-scope".to_string()]).unwrap();
        assert_eq!(parsed.access_token.expose_secret(), "new-access");
        assert!(parsed.rotated_refresh_token.is_none());
        assert_eq!(parsed.scope, vec!["old-scope".to_string()]);
    }
}
