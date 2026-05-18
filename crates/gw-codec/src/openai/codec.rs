//! OpenAI `ProtocolCodec` impl. Streaming uses OpenAI's SSE format:
//! `data: {json}\n\n` per chunk, terminated by `data: [DONE]\n\n`.

use bytes::BytesMut;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use std::borrow::Cow;

use crate::common::{
    approx_tokens, bad_request_err, classify, internal_err, map_code,
};
use crate::trait_def::{Headers, ProtocolCodec};

use super::wire::OpenAiRequest;

pub struct OpenAi;

impl ProtocolCodec for OpenAi {
    #[inline]
    fn detect_request(path: &str, _h: &Headers, _body_head: &[u8]) -> Option<Self> {
        if path.starts_with("/v1/chat/completions") {
            Some(OpenAi)
        } else {
            None
        }
    }

    #[inline]
    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let req: OpenAiRequest<'a> =
            serde_json::from_slice(body).map_err(|e| bad_request_err(format!("openai parse: {e}")))?;
        Ok(req.into())
    }

    #[inline]
    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let req = OpenAiRequest::try_from(ir)?;
        let bytes = serde_json::to_vec(&req).map_err(|_| internal_err("openai serialize failed"))?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<Option<StreamEventIR<'a>>, ErrorIR<'static>> {
        let s = std::str::from_utf8(chunk).map_err(|_| internal_err("invalid utf8 in chunk"))?;
        for line in s.split('\n') {
            let line = line.trim_start();
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim_start();
            if payload.trim() == "[DONE]" {
                return Ok(Some(StreamEventIR::End {
                    finish_reason: FinishReasonIR::Stop,
                }));
            }
            if payload.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(payload) {
                Ok(val) => {
                    let txt = val
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                        .map(|s| Cow::Owned(s.to_owned()));
                    return Ok(Some(StreamEventIR::Delta {
                        text: txt,
                        tool_call: None,
                        thinking: None,
                    }));
                }
                Err(_) => {
                    // CONTRACT: a malformed `data:` payload should NOT
                    // short-circuit the entire stream — surface a warn and
                    // return Ok(None) so the framer can continue with the
                    // next chunk.
                    tracing::warn!("malformed SSE data on openai");
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        match ir {
            StreamEventIR::Delta { text, .. } => {
                let payload = serde_json::json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": text.as_deref().unwrap_or("") },
                    }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::End { .. } => {
                buf.extend_from_slice(b"data: [DONE]\n\n");
            }
            StreamEventIR::Error(err) => {
                // Surface the gateway error on the wire as a final `data:`
                // frame so the client observes it even mid-stream.
                let mut tmp = BytesMut::new();
                self.render_error(err, &mut tmp);
                buf.extend_from_slice(b"data: ");
                buf.extend_from_slice(&tmp);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Start { .. } => {
                tracing::warn!("openai: StreamEventIR::Start dropped (no wire equivalent in v1)");
            }
            StreamEventIR::Usage(_) => {
                tracing::warn!("openai: StreamEventIR::Usage dropped (no wire equivalent in v1)");
            }
        }
        Ok(())
    }

    fn parse_response<'a>(&self, _body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        // TODO(v2): non-streaming 2xx response handling. v1 routes 2xx
        // through `parse_stream_chunk`; `parse_error` covers error bodies.
        unimplemented!("v1: openai non-stream response not implemented; 2xx flows through stream")
    }

    fn render_response(
        &self,
        _ir: &ResponseIR<'_>,
        _buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        unimplemented!("v1: openai non-stream response not implemented")
    }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        // OpenAI error shape: {"error": {"message": ..., "type": ..., "code": ...}}
        let msg = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_owned())
            })
            .unwrap_or_default();
        ErrorIR {
            category: classify(status),
            status,
            upstream_status: Some(status),
            code: Cow::Borrowed(map_code(status)),
            message: Cow::Owned(msg),
            retryable: matches!(status, 500..=599 | 429),
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        }
    }

    fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut) {
        let body = serde_json::json!({
            "error": {
                "type": category_str(err.category),
                "message": err.message,
                "code": err.code,
            }
        });
        if let Ok(bytes) = serde_json::to_vec(&body) {
            buf.extend_from_slice(&bytes);
        }
    }

    fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32 {
        ir.messages
            .iter()
            .map(|m| match m {
                MessageIR::System { content }
                | MessageIR::User { content }
                | MessageIR::Tool { content, .. }
                | MessageIR::Assistant { content, .. } => approx_tokens(content),
            })
            .sum()
    }
}

#[inline]
fn category_str(c: ErrorCategory) -> &'static str {
    match c {
        ErrorCategory::ClientBadRequest => "client_bad_request",
        ErrorCategory::ClientAuth => "client_auth",
        ErrorCategory::ClientQuota => "client_quota",
        ErrorCategory::UpstreamError => "upstream_error",
        ErrorCategory::UpstreamTimeout => "upstream_timeout",
        ErrorCategory::UpstreamOverloaded => "upstream_overloaded",
        ErrorCategory::GatewayInternal => "gateway_internal",
        ErrorCategory::Cancelled => "cancelled",
    }
}

// Reference unused imports to keep the helper available if future code paths
// (e.g. response rendering) need them.
#[allow(dead_code)]
const _ASSERT_CODE: &str = codes::GW_UPSTREAM_4XX;
