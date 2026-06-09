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

        let auth_secret = if let Some(oauth) = ctx.vendor_key.oauth.as_ref() {
            let now = time::OffsetDateTime::now_utc();
            if now >= oauth.access_expires_at {
                metrics::counter!(
                    "gw_oauth_token_expired_total",
                    "vendor_key_id" => ctx.vendor_key.key_id.as_ref().to_string(),
                )
                .increment(1);
            } else if now >= oauth.access_expires_at - time::Duration::seconds(60) {
                metrics::counter!(
                    "gw_oauth_token_near_expiry_total",
                    "vendor_key_id" => ctx.vendor_key.key_id.as_ref().to_string(),
                )
                .increment(1);
            }
            oauth.access_token.expose_secret().to_owned()
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
        let span_id = hex::encode(rand::random::<[u8; 8]>());
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
