//! Centralized error-to-wire translation.
//!
//! Renders an [`ErrorIR`] through whichever client codec is on the request
//! (defaulting to OpenAI if codec detection failed) and writes the response
//! header + body back to the Pingora `Session`.
//!
//! Headers always set:
//! - `Content-Type: application/json`
//! - `X-Request-Id: <err.request_id>` when non-empty
//! - `Retry-After: <secs>` when `err.retry_after_secs` is `Some`.

use bytes::BytesMut;
use gw_codec::AnyCodec;
use gw_core::error::ErrorIR;
use pingora::http::ResponseHeader;
use pingora_proxy::Session;

/// Render `err` through `codec`, then write a single-shot response to
/// `session`. Errors from the underlying writes are logged but not propagated:
/// once we're in the error path there's no useful recovery from a write
/// failure beyond letting Pingora close the connection.
pub async fn send_error(session: &mut Session, codec: &AnyCodec, err: &ErrorIR<'_>) {
    let mut buf = BytesMut::with_capacity(256);
    codec.render_error(err, &mut buf);

    let mut resp = match ResponseHeader::build(err.status, None) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "send_error: failed to build response header");
            return;
        }
    };
    let _ = resp.insert_header("Content-Type", "application/json");
    if !err.request_id.is_empty() {
        let _ = resp.insert_header("X-Request-Id", err.request_id);
    }
    if let Some(s) = err.retry_after_secs {
        let _ = resp.insert_header("Retry-After", s.to_string());
    }

    if let Err(e) = session.write_response_header(Box::new(resp), false).await {
        tracing::warn!(error = %e, "send_error: write_response_header failed");
        return;
    }
    if let Err(e) = session.write_response_body(Some(buf.freeze()), true).await {
        tracing::warn!(error = %e, "send_error: write_response_body failed");
    }
}
