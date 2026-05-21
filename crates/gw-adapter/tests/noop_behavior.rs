//! NoopAdapter must perform exactly the legacy x-api-key injection:
//! drop any existing `authorization`, set `x-api-key` to the vendor key's
//! secret material. Nothing else.

use gw_adapter::ctx::UrlAccessor;
use gw_adapter::{AdapterCtx, HeadersAccessor, HeadersMut, NoopAdapter, UpstreamAdapter, UrlMut};
use gw_core::entity::{KeyStatus, VendorKey};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::protocol::ProtocolKind;
use gw_health::store::KeyHealthStore;
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;

struct InMemoryHeaders(HashMap<String, String>);

impl HeadersAccessor for InMemoryHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }
    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_owned(), value.to_owned());
    }
    fn remove(&mut self, name: &str) {
        self.0.remove(name);
    }
}

struct InMemoryUrl;

impl UrlAccessor for InMemoryUrl {
    fn scheme(&self) -> &str {
        "https"
    }
    fn host(&self) -> &str {
        "example.com"
    }
    fn path(&self) -> &str {
        "/"
    }
    fn set_scheme(&mut self, _: &str) {}
    fn set_host(&mut self, _: &str) {}
    fn set_path(&mut self, _: &str) {}
}

fn dummy_vendor_key() -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from("test-key".to_string()),
        vendor_id: VendorId::from("test-vendor".to_string()),
        api_key: SecretString::from("sk-test-123".to_string()),
        accessible_models: vec![],
        model_mapping: HashMap::new(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0,
        weight: 0,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
    })
}

#[tokio::test]
async fn noop_injects_x_api_key_and_removes_authorization() {
    let health = Arc::new(KeyHealthStore::default());
    let mut hdrs = InMemoryHeaders({
        let mut m = HashMap::new();
        m.insert("authorization".into(), "Bearer stale".into());
        m
    });
    let mut url = InMemoryUrl;

    let mut ctx = AdapterCtx {
        vendor_id: VendorId::from("test-vendor".to_string()),
        vendor_key_id: KeyId::from("test-key".to_string()),
        vendor_key: dummy_vendor_key(),
        client_protocol: ProtocolKind::OpenAi,
        upstream_protocol: ProtocolKind::OpenAi,
        model: ModelName::from("gpt-4".to_string()),
        user_id: UserId::from("u-1".to_string()),
        api_format: ProtocolKind::OpenAi,
        health_store: &health,
        headers_mut: HeadersMut { inner: &mut hdrs },
        url_mut: UrlMut { inner: &mut url },
        trace_id: "trace-1",
        request_id: "req-1",
    };

    let n = NoopAdapter;
    n.rewrite_auth(&mut ctx).await.expect("rewrite_auth");

    assert_eq!(
        hdrs.0.get("x-api-key").map(|s| s.as_str()),
        Some("sk-test-123")
    );
    assert!(!hdrs.0.contains_key("authorization"));
}
