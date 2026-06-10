//! Kiro outbound adapter.
//!
//! Implements AWS SigV4-style header injection for the registered Kiro
//! EventStream wire shape. CLI marker detection remains outside this adapter
//! until real CLI traffic samples exist.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::format_description::FormatItem;
use time::macros::format_description;

type HmacSha256 = Hmac<Sha256>;

static AWS_DATE_FORMAT: &[FormatItem<'_>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");
static AWS_SCOPE_DATE_FORMAT: &[FormatItem<'_>] = format_description!("[year][month][day]");

#[derive(Clone, Debug)]
pub struct KiroAdapter;

#[derive(Clone)]
struct KiroSigV4Config {
    access_key_id: String,
    secret_access_key: SecretString,
    session_token: Option<SecretString>,
    region: String,
    service: String,
}

impl std::fmt::Debug for KiroSigV4Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroSigV4Config")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("region", &self.region)
            .field("service", &self.service)
            .finish()
    }
}

#[derive(Deserialize)]
struct KiroSigV4ConfigWire {
    access_key_id: String,
    secret_access_key: SecretString,
    #[serde(default)]
    session_token: Option<SecretString>,
    region: String,
    #[serde(default = "default_service")]
    service: String,
}

fn default_service() -> String {
    "execute-api".to_owned()
}

#[async_trait]
impl UpstreamAdapter for KiroAdapter {
    fn name(&self) -> &'static str {
        "kiro"
    }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let cfg = parse_sigv4_config(&ctx.vendor_key.api_key)?;
        let now = time::OffsetDateTime::now_utc();
        let amz_date = now
            .format(AWS_DATE_FORMAT)
            .map_err(|e| AdapterError::PatchRequest(format!("format x-amz-date: {e}")))?;
        let scope_date = now
            .format(AWS_SCOPE_DATE_FORMAT)
            .map_err(|e| AdapterError::PatchRequest(format!("format credential date: {e}")))?;

        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.remove("x-api-key");
        ctx.headers_mut.remove("x-api-key-id");
        ctx.headers_mut.remove("x-amz-security-token");

        let host = ctx.upstream_host.ok_or(AdapterError::MissingConfig(
            "kiro upstream host required for SigV4",
        ))?;
        ctx.headers_mut.set("host", host);
        ctx.headers_mut
            .set("content-length", &ctx.content_len.to_string());
        ctx.headers_mut
            .set("x-amz-content-sha256", &ctx.body_sha256_hex);
        ctx.headers_mut.set("x-amz-date", &amz_date);
        if let Some(token) = cfg.session_token.as_ref() {
            ctx.headers_mut
                .set("x-amz-security-token", token.expose_secret());
        }

        let authorization = build_authorization(ctx, &cfg, &scope_date, &amz_date, host)?;
        ctx.headers_mut.set("authorization", &authorization);
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

fn parse_sigv4_config(secret: &SecretString) -> Result<KiroSigV4Config, AdapterError> {
    let wire: KiroSigV4ConfigWire = serde_json::from_str(secret.expose_secret()).map_err(|e| {
        AdapterError::MissingConfig(match e.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Data => {
                "kiro api_key must be SigV4 credential JSON"
            }
            _ => "kiro api_key credential JSON unreadable",
        })
    })?;
    Ok(KiroSigV4Config {
        access_key_id: wire.access_key_id,
        secret_access_key: wire.secret_access_key,
        session_token: wire.session_token,
        region: wire.region,
        service: wire.service,
    })
}

fn build_authorization(
    ctx: &AdapterCtx<'_>,
    cfg: &KiroSigV4Config,
    scope_date: &str,
    amz_date: &str,
    host: &str,
) -> Result<String, AdapterError> {
    let credential_scope = format!("{scope_date}/{}/{}/aws4_request", cfg.region, cfg.service);
    let signed_headers = if cfg.session_token.is_some() {
        "host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
    } else {
        "host;x-amz-content-sha256;x-amz-date"
    };
    let mut canonical_headers = format!(
        "host:{host}\nx-amz-content-sha256:{}\nx-amz-date:{amz_date}",
        ctx.body_sha256_hex
    );
    if let Some(token) = cfg.session_token.as_ref() {
        canonical_headers.push_str("\nx-amz-security-token:");
        canonical_headers.push_str(token.expose_secret());
    }
    let canonical_request = format!(
        "POST\n{}\n\n{}\n\n{}\n{}",
        ctx.url_mut.path(),
        canonical_headers,
        signed_headers,
        ctx.body_sha256_hex
    );
    let canonical_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_hash}");
    let signing_key = sigv4_signing_key(
        cfg.secret_access_key.expose_secret().as_bytes(),
        scope_date,
        &cfg.region,
        &cfg.service,
    )?;
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        cfg.access_key_id, credential_scope, signed_headers, signature
    ))
}

fn sigv4_signing_key(
    secret: &[u8],
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, AdapterError> {
    let k_secret = [b"AWS4".as_slice(), secret].concat();
    let k_date = hmac_sha256(&k_secret, date.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, AdapterError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| AdapterError::PatchRequest(format!("hmac init: {e}")))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeadersAccessor, HeadersMut, UrlAccessor, UrlMut};
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use gw_health::store::KeyHealthStore;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct Headers(HashMap<String, String>);
    impl HeadersAccessor for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            self.0.get(&name.to_ascii_lowercase()).map(String::as_str)
        }
        fn set(&mut self, name: &str, value: &str) {
            self.0.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        fn remove(&mut self, name: &str) {
            self.0.remove(&name.to_ascii_lowercase());
        }
    }

    struct Url;
    impl UrlAccessor for Url {
        fn scheme(&self) -> &str {
            "https"
        }
        fn host(&self) -> &str {
            "kiro.example.com"
        }
        fn path(&self) -> &str {
            "/kiro/invoke"
        }
        fn set_scheme(&mut self, _scheme: &str) {}
        fn set_host(&mut self, _host: &str) {}
        fn set_path(&mut self, _path: &str) {}
    }

    fn vendor_key() -> Arc<VendorKey> {
        vendor_key_with_secret(
            r#"{"access_key_id":"AKIA_TEST","secret_access_key":"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY","region":"us-east-1","service":"bedrock"}"#,
        )
    }

    fn vendor_key_with_secret(secret: &str) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from("vk-kiro"),
            vendor_id: VendorId::from("kiro"),
            api_key: SecretString::from(secret.to_owned()),
            accessible_models: Vec::new(),
            model_mapping: HashMap::new(),
            accessible_endpoints: vec![ProtocolKind::Kiro],
            protocol_conversion: false,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: Vec::new(),
            forced_capabilities: Vec::new(),
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
            quota_state: Default::default(),
        })
    }

    #[tokio::test]
    async fn kiro_rewrite_auth_injects_sigv4_headers_and_final_body_hash() {
        let health = Arc::new(KeyHealthStore::default());
        let key = vendor_key();
        let mut headers = Headers(HashMap::from([
            ("authorization".to_owned(), "Bearer inbound".to_owned()),
            ("x-api-key".to_owned(), "inbound".to_owned()),
        ]));
        let mut url = Url;
        let mut ctx = AdapterCtx {
            vendor_id: VendorId::from("kiro"),
            vendor_key_id: KeyId::from("vk-kiro"),
            vendor_key: key,
            client_protocol: ProtocolKind::Kiro,
            upstream_protocol: ProtocolKind::Kiro,
            model: ModelName::from("kiro-model"),
            user_id: UserId::from("u-1"),
            api_format: ProtocolKind::Kiro,
            health_store: &health,
            oauth_runtime: None,
            headers_mut: HeadersMut {
                inner: &mut headers,
            },
            url_mut: UrlMut { inner: &mut url },
            auth_template: "",
            upstream_host: Some("kiro.example.com"),
            content_len: 17,
            body_sha256_hex: "8f434346648f6b96df89dda901c5176d09833210c989a459b957023e1ffde663"
                .to_owned(),
            trace_id: "0000000000000000aaaaaaaaaaaaaaaa",
            request_id: "req-1",
        };

        KiroAdapter.rewrite_auth(&mut ctx).await.unwrap();

        assert!(ctx.headers_mut.get("x-api-key").is_none());
        assert_eq!(
            ctx.headers_mut.get("x-amz-content-sha256"),
            Some("8f434346648f6b96df89dda901c5176d09833210c989a459b957023e1ffde663")
        );
        assert!(ctx.headers_mut.get("x-amz-date").is_some());
        let auth = ctx.headers_mut.get("authorization").unwrap();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIA_TEST/"));
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
        assert!(auth.contains("Signature="));
        assert!(
            !format!("{:?}", parse_sigv4_config(&ctx.vendor_key.api_key).unwrap())
                .contains("EXAMPLEKEY")
        );
    }

    #[tokio::test]
    async fn kiro_rewrite_auth_signs_session_token_header() {
        let health = Arc::new(KeyHealthStore::default());
        let key = vendor_key_with_secret(
            r#"{"access_key_id":"AKIA_TEST","secret_access_key":"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY","session_token":"session-token-123","region":"us-east-1","service":"bedrock"}"#,
        );
        let mut headers = Headers(HashMap::new());
        let mut url = Url;
        let mut ctx = AdapterCtx {
            vendor_id: VendorId::from("kiro"),
            vendor_key_id: KeyId::from("vk-kiro"),
            vendor_key: key,
            client_protocol: ProtocolKind::Kiro,
            upstream_protocol: ProtocolKind::Kiro,
            model: ModelName::from("kiro-model"),
            user_id: UserId::from("u-1"),
            api_format: ProtocolKind::Kiro,
            health_store: &health,
            oauth_runtime: None,
            headers_mut: HeadersMut {
                inner: &mut headers,
            },
            url_mut: UrlMut { inner: &mut url },
            auth_template: "",
            upstream_host: Some("kiro.example.com"),
            content_len: 17,
            body_sha256_hex: "8f434346648f6b96df89dda901c5176d09833210c989a459b957023e1ffde663"
                .to_owned(),
            trace_id: "0000000000000000aaaaaaaaaaaaaaaa",
            request_id: "req-1",
        };

        KiroAdapter.rewrite_auth(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.headers_mut.get("x-amz-security-token"),
            Some("session-token-123")
        );
        let auth = ctx.headers_mut.get("authorization").unwrap();
        assert!(auth
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"));
    }
}
