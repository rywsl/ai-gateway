use async_trait::async_trait;
use gw_adapter::ctx::UrlAccessor;
use gw_adapter::{
    AdapterCtx, HeadersAccessor, HeadersMut, NoopAdapter, OAuthRuntime, OAuthTokenResult,
    UpstreamAdapter, UrlMut,
};
use gw_core::entity::{KeyStatus, OAuthCredentials, VendorKey};
use gw_core::ids::{KeyId, ModelName, RequestId, TraceId, UserId, VendorId};
use gw_core::protocol::ProtocolKind;
use gw_health::store::KeyHealthStore;
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct InMemoryHeaders(HashMap<String, String>);

impl HeadersAccessor for InMemoryHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }

    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_ascii_lowercase(), value.to_owned());
    }

    fn remove(&mut self, name: &str) {
        self.0.remove(&name.to_ascii_lowercase());
    }
}

struct InMemoryUrl;

impl UrlAccessor for InMemoryUrl {
    fn scheme(&self) -> &str {
        "https"
    }

    fn host(&self) -> &str {
        "api.openai.com"
    }

    fn path(&self) -> &str {
        "/v1/chat/completions"
    }

    fn set_scheme(&mut self, _: &str) {}
    fn set_host(&mut self, _: &str) {}
    fn set_path(&mut self, _: &str) {}
}

fn vendor_key(api_key: &str, oauth: Option<OAuthCredentials>) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from("vk-baseline"),
        vendor_id: VendorId::from("openai"),
        api_key: SecretString::from(api_key.to_owned()),
        accessible_models: Vec::new(),
        model_mapping: HashMap::new(),
        accessible_endpoints: vec![ProtocolKind::OpenAi],
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
        oauth,
        identity_snapshot: None,
        quota_state: Default::default(),
    })
}

fn oauth_credentials(access_token: &str) -> OAuthCredentials {
    OAuthCredentials {
        access_token: SecretString::from(access_token.to_owned()),
        refresh_token: SecretString::from("refresh-token".to_owned()),
        access_expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
        token_endpoint: "https://oauth.example/token".to_owned(),
        scope: Vec::new(),
        quota_project_id: None,
    }
}

fn make_headers() -> InMemoryHeaders {
    InMemoryHeaders(HashMap::from([
        ("accept".to_owned(), "application/json".to_owned()),
        (
            "authorization".to_owned(),
            "Bearer client-test-key".to_owned(),
        ),
        ("content-type".to_owned(), "application/json".to_owned()),
        ("host".to_owned(), "api.openai.com".to_owned()),
        ("x-api-key".to_owned(), "should-be-stripped".to_owned()),
        ("x-api-key-id".to_owned(), "should-be-stripped".to_owned()),
    ]))
}

fn render(headers: &InMemoryHeaders) -> String {
    let mut out = String::from("POST /v1/chat/completions\n");
    let mut entries: Vec<_> = headers
        .0
        .iter()
        .map(|(k, v)| {
            let v = if k == "traceparent" {
                let mut parts: Vec<&str> = v.split('-').collect();
                if parts.len() == 4 {
                    parts[2] = "SPANID";
                    parts.join("-")
                } else {
                    v.to_owned()
                }
            } else {
                v.to_owned()
            };
            (k.as_str(), v)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in entries {
        out.push_str(&format!("{k}: {v}\n"));
    }
    out
}

async fn run_noop(
    headers: &mut InMemoryHeaders,
    key: Arc<VendorKey>,
    auth_template: &str,
    oauth_runtime: Option<&Arc<dyn OAuthRuntime>>,
) -> Result<String, gw_adapter::AdapterError> {
    let health = Arc::new(KeyHealthStore::default());
    let trace_id = TraceId::from("0000000000000000aaaaaaaaaaaaaaaa");
    let request_id = RequestId::from("req-baseline-0001");
    let mut url = InMemoryUrl;
    let mut ctx = AdapterCtx {
        vendor_id: VendorId::from("openai"),
        vendor_key_id: KeyId::from("vk-baseline"),
        vendor_key: key,
        client_protocol: ProtocolKind::OpenAi,
        upstream_protocol: ProtocolKind::OpenAi,
        model: ModelName::from("gpt-4"),
        user_id: UserId::from("u-1"),
        api_format: ProtocolKind::OpenAi,
        health_store: &health,
        oauth_runtime,
        headers_mut: HeadersMut { inner: headers },
        url_mut: UrlMut { inner: &mut url },
        auth_template,
        upstream_host: Some("api.openai.com"),
        content_len: 42,
        body_sha256_hex: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            .to_owned(),
        trace_id: trace_id.as_ref(),
        request_id: request_id.as_ref(),
    };

    let adapter = NoopAdapter;
    adapter.rewrite_auth(&mut ctx).await?;
    Ok(render(headers))
}

#[tokio::test]
async fn direct_noop_rewrite_matches_legacy_proxy_snapshot() {
    let mut headers = make_headers();

    let rendered = run_noop(
        &mut headers,
        vendor_key("sk-vendor-secret", None),
        "Bearer {api_key}",
        None,
    )
    .await
    .expect("rewrite_auth");

    assert_eq!(
        rendered,
        "\
POST /v1/chat/completions
accept: application/json
authorization: Bearer sk-vendor-secret
content-length: 42
content-type: application/json
host: api.openai.com
traceparent: 00-0000000000000000aaaaaaaaaaaaaaaa-SPANID-01
"
    );
}

#[tokio::test]
async fn oauth_noop_rewrite_requires_runtime() {
    let mut headers = make_headers();

    let err = run_noop(
        &mut headers,
        vendor_key(
            "static-api-key",
            Some(oauth_credentials("oauth-access-token")),
        ),
        "Authorization: Bearer {api_key}",
        None,
    )
    .await
    .expect_err("oauth key without runtime must fail");

    assert!(err.to_string().contains("oauth runtime unavailable"));
}

struct TestOAuthRuntime {
    calls: AtomicUsize,
}

#[async_trait]
impl OAuthRuntime for TestOAuthRuntime {
    async fn access_token_for(
        &self,
        _vendor_key_id: &KeyId,
        _request_id: &str,
    ) -> Result<OAuthTokenResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(OAuthTokenResult {
            access_token: SecretString::from("runtime-access".to_string()),
            refreshed_vendor_key: None,
            version: 7,
        })
    }
}

#[tokio::test]
async fn noop_uses_runtime_oauth_token_before_injecting_bearer() {
    let mut headers = make_headers();
    let runtime = Arc::new(TestOAuthRuntime {
        calls: AtomicUsize::new(0),
    });
    let runtime_dyn: Arc<dyn OAuthRuntime> = runtime.clone();

    let rendered = run_noop(
        &mut headers,
        vendor_key(
            "static-api-key",
            Some(oauth_credentials("stale-oauth-access-token")),
        ),
        "Authorization: Bearer {api_key}",
        Some(&runtime_dyn),
    )
    .await
    .expect("rewrite_auth");

    assert!(rendered.contains("authorization: Bearer runtime-access\n"));
    assert!(!rendered.contains("x-api-key:"));
    assert!(!rendered.contains("x-api-key-id:"));
    assert!(!rendered.contains("static-api-key"));
    assert!(!rendered.contains("stale-oauth-access-token"));
    assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
}
