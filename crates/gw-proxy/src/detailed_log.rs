//! Detailed per-request log record, written by the `logging` filter alongside
//! the (cheaper) `UsageRecord`. v1 captures the structured fields the spec
//! calls out; redaction is delegated to a user-supplied [`Redactor`] before
//! the record reaches the sink.

use gw_core::ids::{ApiKeyId, ModelName, RequestId, TraceId, UserId};
use gw_core::traits::Redactor;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DetailedLogRecord {
    pub ts_ms: u64,
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub user_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub model: Option<ModelName>,
    pub client_protocol: Option<String>,
    pub upstream_protocol: Option<String>,
    pub http_status: u16,
    pub error_code: Option<String>,
    pub is_stream: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    /// Truncated, redacted preview of the request body.
    pub request_body_preview: Option<String>,
    /// Truncated, redacted preview of the response body.
    pub response_body_preview: Option<String>,
    /// `"direct"`, or the node_id when tunnel was used. `None` when
    /// `upstream_peer` short-circuited before the decision. Spec §6.
    pub tunnel_resolution: Option<String>,
}

/// Maximum preview size kept inline in the log record. Larger bodies are
/// truncated; the truncated tail is replaced with `…(<N> bytes truncated)`.
pub const PREVIEW_MAX_BYTES: usize = 4 * 1024;

/// Build a redacted preview from `body`, capped at [`PREVIEW_MAX_BYTES`].
/// Returns `None` if `body` is empty. Non-UTF8 bytes are replaced with the
/// Unicode replacement character via `from_utf8_lossy`.
pub fn redacted_preview(body: &[u8], redactor: &dyn Redactor) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let truncated = body.len() > PREVIEW_MAX_BYTES;
    let head = &body[..body.len().min(PREVIEW_MAX_BYTES)];
    let mut s = String::from_utf8_lossy(head).into_owned();
    redactor.redact(&mut s);
    if truncated {
        s.push_str(&format!(
            "…({} bytes truncated)",
            body.len() - PREVIEW_MAX_BYTES
        ));
    }
    Some(s)
}
