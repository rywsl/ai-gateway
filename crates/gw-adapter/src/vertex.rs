//! Vertex AI outbound adapter.
//!
//! Runtime access tokens are supplied by the W6 `OAuthRuntime`; this adapter
//! does not call Google, PgSQL, or Redis directly.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct VertexAdapter;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VertexJwtClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[async_trait]
impl UpstreamAdapter for VertexAdapter {
    fn name(&self) -> &'static str {
        "vertex"
    }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.remove("x-api-key");
        ctx.headers_mut.remove("x-api-key-id");

        let runtime = ctx.oauth_runtime.ok_or_else(|| {
            AdapterError::Oauth("vertex requires W6 oauth runtime access token".to_owned())
        })?;
        let token = runtime
            .access_token_for(&ctx.vendor_key.key_id, ctx.request_id)
            .await
            .map_err(AdapterError::Oauth)?;
        if let Some(refreshed) = token.refreshed_vendor_key {
            ctx.vendor_key = refreshed;
        }
        ctx.headers_mut.set(
            "authorization",
            &format!("Bearer {}", token.access_token.expose_secret()),
        );
        if let Some(host) = ctx.upstream_host {
            ctx.headers_mut.set("host", host);
        }
        ctx.headers_mut
            .set("content-length", &ctx.content_len.to_string());
        if let Some(project) = ctx
            .vendor_key
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.quota_project_id.as_deref())
        {
            ctx.headers_mut.set("x-goog-user-project", project);
        }
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeadersAccessor, HeadersMut, OAuthRuntime, OAuthTokenResult, UrlAccessor, UrlMut};
    use async_trait::async_trait;
    use gw_core::entity::{KeyStatus, OAuthCredentials, VendorKey};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use gw_health::store::KeyHealthStore;
    use secrecy::SecretString;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
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
            "us-central1-aiplatform.googleapis.com"
        }
        fn path(&self) -> &str {
            "/v1/projects/p/locations/us-central1/publishers/google/models/m:streamGenerateContent"
        }
        fn set_scheme(&mut self, _scheme: &str) {}
        fn set_host(&mut self, _host: &str) {}
        fn set_path(&mut self, _path: &str) {}
    }

    struct Runtime {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl OAuthRuntime for Runtime {
        async fn access_token_for(
            &self,
            _vendor_key_id: &KeyId,
            _request_id: &str,
        ) -> Result<OAuthTokenResult, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(OAuthTokenResult {
                access_token: SecretString::from("runtime-vertex-access".to_owned()),
                refreshed_vendor_key: None,
                version: 2,
            })
        }
    }

    fn vendor_key() -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from("vk-vertex"),
            vendor_id: VendorId::from("google_vertex"),
            api_key: SecretString::from("unused-static".to_owned()),
            accessible_models: Vec::new(),
            model_mapping: HashMap::new(),
            accessible_endpoints: vec![ProtocolKind::Gemini],
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
            oauth: Some(OAuthCredentials {
                access_token: SecretString::from("stale".to_owned()),
                refresh_token: SecretString::from("refresh".to_owned()),
                access_expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
                token_endpoint: "https://oauth2.googleapis.com/token".to_owned(),
                scope: vec!["https://www.googleapis.com/auth/cloud-platform".to_owned()],
                quota_project_id: Some("billing-project".to_owned()),
            }),
            identity_snapshot: None,
            quota_state: Default::default(),
        })
    }

    #[test]
    fn vertex_jwt_claims_service_account_contract_serializes() {
        let claims = VertexJwtClaims {
            iss: "svc@example.iam.gserviceaccount.com".to_owned(),
            sub: "svc@example.iam.gserviceaccount.com".to_owned(),
            aud: "https://oauth2.googleapis.com/token".to_owned(),
            iat: 1_700_000_000,
            exp: 1_700_003_600,
            scope: Some("https://www.googleapis.com/auth/cloud-platform".to_owned()),
        };
        let json = serde_json::to_value(&claims).unwrap();
        assert_eq!(json["iss"], "svc@example.iam.gserviceaccount.com");
        assert_eq!(json["aud"], "https://oauth2.googleapis.com/token");
        assert_eq!(
            json["scope"],
            "https://www.googleapis.com/auth/cloud-platform"
        );
    }

    #[tokio::test]
    async fn vertex_rewrite_auth_consumes_runtime_token_and_quota_project() {
        let health = Arc::new(KeyHealthStore::default());
        let runtime = Arc::new(Runtime {
            calls: AtomicUsize::new(0),
        });
        let runtime_dyn: Arc<dyn OAuthRuntime> = runtime.clone();
        let key = vendor_key();
        let mut headers = Headers(HashMap::from([
            ("authorization".to_owned(), "Bearer inbound".to_owned()),
            ("x-api-key".to_owned(), "inbound".to_owned()),
        ]));
        let mut url = Url;
        let mut ctx = AdapterCtx {
            vendor_id: VendorId::from("google_vertex"),
            vendor_key_id: KeyId::from("vk-vertex"),
            vendor_key: key,
            client_protocol: ProtocolKind::Gemini,
            upstream_protocol: ProtocolKind::Gemini,
            model: ModelName::from("gemini-2.5-pro"),
            user_id: UserId::from("u-1"),
            api_format: ProtocolKind::Gemini,
            health_store: &health,
            oauth_runtime: Some(&runtime_dyn),
            headers_mut: HeadersMut {
                inner: &mut headers,
            },
            url_mut: UrlMut { inner: &mut url },
            auth_template: "",
            upstream_host: Some("us-central1-aiplatform.googleapis.com"),
            http_method: "POST",
            content_len: 31,
            body_sha256_hex: "hash".to_owned(),
            trace_id: "0000000000000000aaaaaaaaaaaaaaaa",
            request_id: "req-vertex",
        };

        VertexAdapter.rewrite_auth(&mut ctx).await.unwrap();

        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            ctx.headers_mut.get("authorization"),
            Some("Bearer runtime-vertex-access")
        );
        assert_eq!(
            ctx.headers_mut.get("x-goog-user-project"),
            Some("billing-project")
        );
        assert!(ctx.headers_mut.get("x-api-key").is_none());
    }

    /// Contract lock (audit P0 #13 §3): Vertex AI (`aiplatform.googleapis.com`)
    /// authenticates with an OAuth Bearer token **only**. `x-goog-api-key` is
    /// the Gemini *Developer* API (`generativelanguage.googleapis.com`) header
    /// and is served generically by `NoopAdapter` + an `auth_template` of
    /// `"x-goog-api-key: {api_key}"`. The Vertex adapter must therefore never
    /// emit `x-goog-api-key`; doing so would send a static key to a surface
    /// that rejects it. This test pins the OAuth-only behaviour.
    #[tokio::test]
    async fn vertex_rewrite_auth_is_oauth_only_no_goog_api_key() {
        let health = Arc::new(KeyHealthStore::default());
        let runtime = Arc::new(Runtime {
            calls: AtomicUsize::new(0),
        });
        let runtime_dyn: Arc<dyn OAuthRuntime> = runtime.clone();
        let key = vendor_key();
        let mut headers = Headers(HashMap::from([(
            "x-api-key".to_owned(),
            "inbound".to_owned(),
        )]));
        let mut url = Url;
        let mut ctx = AdapterCtx {
            vendor_id: VendorId::from("google_vertex"),
            vendor_key_id: KeyId::from("vk-vertex"),
            vendor_key: key,
            client_protocol: ProtocolKind::Gemini,
            upstream_protocol: ProtocolKind::Gemini,
            model: ModelName::from("gemini-2.5-pro"),
            user_id: UserId::from("u-1"),
            api_format: ProtocolKind::Gemini,
            health_store: &health,
            oauth_runtime: Some(&runtime_dyn),
            headers_mut: HeadersMut {
                inner: &mut headers,
            },
            url_mut: UrlMut { inner: &mut url },
            auth_template: "",
            upstream_host: Some("us-central1-aiplatform.googleapis.com"),
            http_method: "POST",
            content_len: 31,
            body_sha256_hex: "hash".to_owned(),
            trace_id: "0000000000000000aaaaaaaaaaaaaaaa",
            request_id: "req-vertex",
        };

        VertexAdapter.rewrite_auth(&mut ctx).await.unwrap();

        // OAuth Bearer is the sole credential; no static API-key header leaks.
        assert!(ctx
            .headers_mut
            .get("authorization")
            .is_some_and(|v| v.starts_with("Bearer ")));
        assert!(ctx.headers_mut.get("x-goog-api-key").is_none());
        assert!(ctx.headers_mut.get("x-api-key").is_none());
    }
}
