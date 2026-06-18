//! `NoopAdapter` — the default `UpstreamAdapter` impl.
//!
//! Equivalent to the legacy direct-mode header rewrite in
//! `gw-proxy::filters::upstream`.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use secrecy::ExposeSecret;

pub struct NoopAdapter;

#[async_trait]
impl UpstreamAdapter for NoopAdapter {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.remove("x-api-key");
        ctx.headers_mut.remove("x-api-key-id");

        let auth_secret = if ctx.vendor_key.oauth.is_some() {
            let runtime = ctx.oauth_runtime.ok_or_else(|| {
                AdapterError::Oauth("oauth runtime unavailable for oauth vendor key".to_string())
            })?;
            let token = runtime
                .access_token_for(&ctx.vendor_key.key_id, ctx.request_id)
                .await
                .map_err(AdapterError::Oauth)?;
            if let Some(refreshed) = token.refreshed_vendor_key {
                ctx.vendor_key = refreshed;
            }
            token.access_token.expose_secret().to_owned()
        } else {
            ctx.vendor_key.api_key.expose_secret().to_owned()
        };

        let (header_name, value_template) = match ctx.auth_template.split_once(':') {
            Some((name, rest)) if is_header_token(name.trim()) => (name.trim(), rest.trim_start()),
            Some(_) => {
                metrics::counter!("gw_upstream_auth_header_invalid_total").increment(1);
                ("authorization", ctx.auth_template)
            }
            None => ("authorization", ctx.auth_template),
        };
        let auth_value = value_template.replace("{api_key}", &auth_secret);
        ctx.headers_mut.set(header_name, &auth_value);
        if let Some(host) = ctx.upstream_host {
            ctx.headers_mut.set("host", host);
        }
        // D24 (#18): derive the traceparent span id from an explicit OS CSPRNG
        // (getrandom-backed `OsRng`) rather than the implicit thread RNG, so the
        // randomness source is auditable.
        let span_id = random_span_id();
        let traceparent = format!("00-{}-{}-01", ctx.trace_id, span_id);
        ctx.headers_mut.set("traceparent", &traceparent);
        ctx.headers_mut
            .set("content-length", &ctx.content_len.to_string());
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

fn is_header_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Generate a W3C-traceparent span id (16 lowercase hex chars / 8 bytes) from an
/// explicit OS CSPRNG. D24 (#18): the source is `rand::rngs::OsRng`
/// (getrandom-backed) so the randomness is auditable rather than relying on the
/// implicit thread-local RNG of `rand::random`.
fn random_span_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::random_span_id;

    #[test]
    fn span_id_is_16_lowercase_hex() {
        let id = random_span_id();
        assert_eq!(id.len(), 16, "8-byte span id encodes to 16 hex chars");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "span id must be lowercase hex: {id}"
        );
    }

    #[test]
    fn span_id_is_not_constant() {
        // Two draws from the CSPRNG should differ with overwhelming probability.
        let a = random_span_id();
        let b = random_span_id();
        assert_ne!(a, b, "CSPRNG span ids must not repeat across calls");
    }
}
