//! Codex request-body adapter.
//!
//! Codex identity headers remain owned by `gw-core::impersonate::openai_codex`;
//! this adapter only adds semantic metadata to typed OpenAI requests.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use gw_codec::wire::WireAnyRequest;
use serde_json::value::RawValue;
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct CodexAdapter;

const CODEX_METADATA_FIELD: &str = "codex_gateway_metadata";

#[async_trait]
impl UpstreamAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        crate::noop::NoopAdapter.rewrite_auth(ctx).await
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        crate::noop::NoopAdapter.rewrite_transport(ctx)
    }

    fn request_patch_enabled(&self) -> bool {
        true
    }

    fn patch_request(
        &self,
        ctx: &AdapterCtx<'_>,
        wire: &mut WireAnyRequest<'_>,
    ) -> Result<(), AdapterError> {
        let WireAnyRequest::OpenAi(req) = wire else {
            return Ok(());
        };
        let metadata = serde_json::json!({
            "adapter": "codex",
            "request_id": ctx.request_id,
            "trace_id": ctx.trace_id,
            "vendor_key_id": ctx.vendor_key_id.as_ref(),
            "model": ctx.model.as_ref(),
            "stream": req.stream,
        });
        let raw = RawValue::from_string(metadata.to_string())
            .map_err(|e| AdapterError::PatchRequest(format!("codex metadata json: {e}")))?;
        req.extra
            .insert(Cow::Owned(CODEX_METADATA_FIELD.to_owned()), raw);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeadersAccessor, HeadersMut, UrlAccessor, UrlMut};
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use gw_health::store::KeyHealthStore;
    use secrecy::SecretString;
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
            "api.openai.com"
        }
        fn path(&self) -> &str {
            "/v1/chat/completions"
        }
        fn set_scheme(&mut self, _scheme: &str) {}
        fn set_host(&mut self, _host: &str) {}
        fn set_path(&mut self, _path: &str) {}
    }

    fn vendor_key() -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from("vk-codex"),
            vendor_id: VendorId::from("codex"),
            api_key: SecretString::from("sk-codex".to_owned()),
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
            oauth: None,
            identity_snapshot: None,
            quota_state: Default::default(),
        })
    }

    fn ctx<'a>(
        health: &'a Arc<KeyHealthStore>,
        headers: &'a mut Headers,
        url: &'a mut Url,
    ) -> AdapterCtx<'a> {
        AdapterCtx {
            vendor_id: VendorId::from("codex"),
            vendor_key_id: KeyId::from("vk-codex"),
            vendor_key: vendor_key(),
            client_protocol: ProtocolKind::OpenAi,
            upstream_protocol: ProtocolKind::OpenAi,
            model: ModelName::from("gpt-5-codex"),
            user_id: UserId::from("u-1"),
            api_format: ProtocolKind::OpenAi,
            health_store: health,
            oauth_runtime: None,
            headers_mut: HeadersMut { inner: headers },
            url_mut: UrlMut { inner: url },
            auth_template: "Bearer {api_key}",
            upstream_host: Some("api.openai.com"),
            content_len: 0,
            body_sha256_hex: String::new(),
            trace_id: "0000000000000000aaaaaaaaaaaaaaaa",
            request_id: "req-codex",
        }
    }

    #[test]
    fn codex_patch_adds_metadata_and_preserves_openai_fields() {
        let health = Arc::new(KeyHealthStore::default());
        let mut headers = Headers(HashMap::new());
        let mut url = Url;
        let ctx = ctx(&health, &mut headers, &mut url);
        let body = br#"{"model":"gpt-5-codex","messages":[{"role":"user","content":"hi"}],"stream":true,"tools":[{"type":"function","function":{"name":"lookup","parameters":{}}}],"tool_choice":"auto"}"#;
        let parsed: gw_codec::openai::wire::OpenAiRequest<'_> =
            serde_json::from_slice(body).unwrap();
        let mut wire = WireAnyRequest::OpenAi(Box::new(parsed));

        CodexAdapter.patch_request(&ctx, &mut wire).unwrap();

        let WireAnyRequest::OpenAi(req) = wire else {
            panic!("expected openai request");
        };
        assert_eq!(req.model, "gpt-5-codex");
        assert_eq!(req.messages.len(), 1);
        assert!(req.stream);
        assert!(req.tools.is_some());
        assert!(req.tool_choice.is_some());
        let metadata = req.extra.get(CODEX_METADATA_FIELD).unwrap();
        let json: serde_json::Value = serde_json::from_str(metadata.get()).unwrap();
        assert_eq!(json["adapter"], "codex");
        assert_eq!(json["request_id"], "req-codex");
        assert_eq!(json["vendor_key_id"], "vk-codex");
    }
}
