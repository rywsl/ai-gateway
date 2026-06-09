use crate::encrypt::{encrypt_with_active_kek, EncryptedKey, KekCache};
use crate::redis_client::RedisCtx;
use fred::interfaces::{KeysInterface, StreamsInterface};
use secrecy::SecretString;
use sqlx::PgPool;
use std::fmt::Display;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub struct OAuthWriteBack {
    pub key_id: String,
    pub expected_version: u64,
    pub access_token: SecretString,
    pub rotated_refresh_token: Option<SecretString>,
    pub previous_refresh_token_enc: Option<EncryptedKey>,
    pub access_expires_at: OffsetDateTime,
    pub token_endpoint: String,
    pub scope: Vec<String>,
    pub quota_project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthWriteBackResult {
    pub key_id: String,
    pub new_version: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthWriteBackError {
    #[error("oauth write CAS lost for key {key_id} at expected version {expected_version}")]
    CasLost {
        key_id: String,
        expected_version: u64,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub fn update_sql() -> &'static str {
    "UPDATE vendor_keys \
     SET payload = jsonb_set(payload, '{oauth}', $3::jsonb, true), \
         version = version + 1, \
         updated_at = NOW() \
     WHERE id = $1 AND version = $2 AND deleted_at IS NULL \
     RETURNING version"
}

pub fn entity_event_fields(key_id: &str, new_version: u64, ts_ms: i64) -> Vec<(String, String)> {
    vec![
        ("ns".to_string(), "vendor_key".to_string()),
        ("id".to_string(), key_id.to_string()),
        ("op".to_string(), "upsert".to_string()),
        ("v".to_string(), new_version.to_string()),
        ("ts_ms".to_string(), ts_ms.to_string()),
        ("source".to_string(), "rust_oauth_refresh".to_string()),
    ]
}

pub fn build_oauth_payload(
    access_token_enc: EncryptedKey,
    refresh_token_enc: EncryptedKey,
    access_expires_at: OffsetDateTime,
    token_endpoint: String,
    scope: Vec<String>,
    quota_project_id: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "access_token_enc": access_token_enc,
        "refresh_token_enc": refresh_token_enc,
        "access_expires_at": access_expires_at,
        "token_endpoint": token_endpoint,
        "scope": scope,
        "quota_project_id": quota_project_id,
    })
}

fn require_redis_side_effects<DelErr, XaddErr>(
    key_id: &str,
    del_result: Result<(), DelErr>,
    xadd_result: Result<String, XaddErr>,
) -> Result<(), OAuthWriteBackError>
where
    DelErr: Display,
    XaddErr: Display,
{
    del_result.map_err(|e| {
        OAuthWriteBackError::Other(anyhow::anyhow!(
            "oauth write-back Redis DEL failed for vendor key {key_id}: {e}"
        ))
    })?;
    xadd_result.map_err(|e| {
        OAuthWriteBackError::Other(anyhow::anyhow!(
            "oauth write-back events:entity XADD failed for vendor key {key_id}: {e}"
        ))
    })?;
    Ok(())
}

#[derive(Clone)]
pub struct PgOAuthRefreshWriter {
    pg: PgPool,
    redis: RedisCtx,
    kek: KekCache,
}

impl PgOAuthRefreshWriter {
    pub fn new(pg: PgPool, redis: RedisCtx, kek: KekCache) -> Self {
        Self { pg, redis, kek }
    }

    pub async fn write(
        &self,
        input: OAuthWriteBack,
    ) -> Result<OAuthWriteBackResult, OAuthWriteBackError> {
        let access_token_enc = encrypt_with_active_kek(&input.access_token, &self.kek)?;
        let refresh_token_enc = match input.rotated_refresh_token.as_ref() {
            Some(token) => encrypt_with_active_kek(token, &self.kek)?,
            None => match input.previous_refresh_token_enc {
                Some(enc) => enc,
                None => {
                    self.load_existing_refresh_token_enc(&input.key_id, input.expected_version)
                        .await?
                }
            },
        };
        let payload = build_oauth_payload(
            access_token_enc,
            refresh_token_enc,
            input.access_expires_at,
            input.token_endpoint,
            input.scope,
            input.quota_project_id,
        );

        let row: Option<(i64,)> = sqlx::query_as(update_sql())
            .bind(&input.key_id)
            .bind(input.expected_version as i64)
            .bind(payload)
            .fetch_optional(&self.pg)
            .await
            .map_err(anyhow::Error::from)?;

        let Some((new_version_i64,)) = row else {
            metrics::counter!("gw_oauth_runtime_writeback_total", "result" => "cas_lost")
                .increment(1);
            return Err(OAuthWriteBackError::CasLost {
                key_id: input.key_id,
                expected_version: input.expected_version,
            });
        };
        let new_version = new_version_i64 as u64;

        let client = self.redis.pool.next();
        let redis_key = format!("entity:vendor_key:{}", input.key_id);
        let del_result: Result<(), _> = client.del(redis_key).await;
        let xadd_result: Result<String, _> = client
            .xadd(
                "events:entity",
                false,
                None::<()>,
                fred::types::XID::Auto,
                entity_event_fields(&input.key_id, new_version, now_ms()),
            )
            .await;
        require_redis_side_effects(&input.key_id, del_result, xadd_result)?;

        metrics::counter!("gw_oauth_runtime_writeback_total", "result" => "ok").increment(1);

        Ok(OAuthWriteBackResult {
            key_id: input.key_id,
            new_version,
        })
    }

    async fn load_existing_refresh_token_enc(
        &self,
        key_id: &str,
        expected_version: u64,
    ) -> Result<EncryptedKey, OAuthWriteBackError> {
        let value: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload #> '{oauth,refresh_token_enc}' \
             FROM vendor_keys \
             WHERE id = $1 AND version = $2 AND deleted_at IS NULL",
        )
        .bind(key_id)
        .bind(expected_version as i64)
        .fetch_optional(&self.pg)
        .await
        .map_err(anyhow::Error::from)?;
        let Some(value) = value else {
            return Err(OAuthWriteBackError::CasLost {
                key_id: key_id.to_string(),
                expected_version,
            });
        };
        serde_json::from_value(value)
            .map_err(anyhow::Error::from)
            .map_err(Into::into)
    }
}

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_sql_uses_version_cas_and_returns_new_version() {
        let sql = update_sql();
        assert!(sql.contains("jsonb_set(payload, '{oauth}'"));
        assert!(sql.contains("version = version + 1"));
        assert!(sql.contains("WHERE id = $1 AND version = $2 AND deleted_at IS NULL"));
        assert!(sql.contains("RETURNING version"));
    }

    #[test]
    fn entity_event_fields_are_vendor_key_upsert() {
        let fields = entity_event_fields("vk-1", 12, 1_770_000_000_000);
        assert!(fields.contains(&("ns".to_string(), "vendor_key".to_string())));
        assert!(fields.contains(&("id".to_string(), "vk-1".to_string())));
        assert!(fields.contains(&("op".to_string(), "upsert".to_string())));
        assert!(fields.contains(&("v".to_string(), "12".to_string())));
        assert!(fields.contains(&("source".to_string(), "rust_oauth_refresh".to_string())));
    }

    #[test]
    fn oauth_payload_preserves_refresh_token_when_provider_omits_rotation() {
        let previous = EncryptedKey {
            ciphertext: vec![1],
            nonce: vec![2],
            auth_tag: vec![3],
            kek_kid: "old".to_string(),
        };
        let payload = build_oauth_payload(
            EncryptedKey {
                ciphertext: vec![4],
                nonce: vec![5],
                auth_tag: vec![6],
                kek_kid: "new".to_string(),
            },
            previous.clone(),
            time::OffsetDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
            "https://oauth.example/token".to_string(),
            vec!["scope-a".to_string()],
            Some("quota-project".to_string()),
        );
        assert_eq!(payload["refresh_token_enc"]["kek_kid"], "old");
        assert_eq!(payload["access_token_enc"]["kek_kid"], "new");
        assert_eq!(payload["token_endpoint"], "https://oauth.example/token");
        assert_eq!(payload["quota_project_id"], "quota-project");
    }

    #[test]
    fn redis_side_effects_fail_when_cache_delete_fails() {
        let result = require_redis_side_effects(
            "vk-1",
            Err("del failed"),
            Ok::<String, &str>("stream-id".to_string()),
        );

        assert!(matches!(result, Err(OAuthWriteBackError::Other(_))));
    }

    #[test]
    fn redis_side_effects_fail_when_entity_event_publish_fails() {
        let result = require_redis_side_effects("vk-1", Ok::<(), &str>(()), Err("xadd failed"));

        assert!(matches!(result, Err(OAuthWriteBackError::Other(_))));
    }
}
