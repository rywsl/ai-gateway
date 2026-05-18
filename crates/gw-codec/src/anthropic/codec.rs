//! Anthropic `ProtocolCodec` impl. Streaming uses Anthropic's SSE format:
//! each event is an `event: <kind>\ndata: {json}\n\n` pair. v1 understands
//! `content_block_delta` and `message_stop`.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::wire::AnthropicRequest;

pub struct Anthropic;

impl ProtocolCodec for Anthropic {
    #[inline]
    fn detect_request(path: &str, _h: &Headers, _body_head: &[u8]) -> Option<Self> {
        // Anthropic's Messages API path.
        if path.starts_with("/v1/messages") {
            Some(Anthropic)
        } else {
            None
        }
    }

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let req: AnthropicRequest<'a> = serde_json::from_slice(body)
            .map_err(|e| bad_request_err(format!("anthropic parse: {e}")))?;
        Ok(req.into())
    }

    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let req = AnthropicRequest::try_from(ir)?;
        let bytes =
            serde_json::to_vec(&req).map_err(|_| internal_err("anthropic serialize failed"))?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<Option<StreamEventIR<'a>>, ErrorIR<'static>> {
        let s = std::str::from_utf8(chunk).map_err(|_| internal_err("invalid utf8 in chunk"))?;
        let mut event_name: Option<&str> = None;
        let mut data_line: Option<&str> = None;
        for line in s.split('\n') {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("event:") {
                event_name = Some(rest.trim());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_line = Some(rest.trim());
            }
        }
        match (event_name, data_line) {
            (Some("message_stop"), _) => Ok(Some(StreamEventIR::End {
                finish_reason: FinishReasonIR::Stop,
            })),
            (Some("content_block_delta"), Some(payload)) if !payload.is_empty() => {
                let val: serde_json::Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => {
                        // CONTRACT: malformed payloads must not short-circuit
                        // the stream — surface a warn and return Ok(None) so
                        // the framer continues to the next chunk.
                        tracing::warn!("malformed SSE data on anthropic");
                        return Ok(None);
                    }
                };
                let txt = val
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
                    .map(|s| Cow::Owned(s.to_owned()));
                Ok(Some(StreamEventIR::Delta {
                    text: txt,
                    tool_call: None,
                    thinking: None,
                }))
            }
            _ => Ok(None),
        }
    }

    fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        match ir {
            StreamEventIR::Delta { text, .. } => {
                let payload = serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": text.as_deref().unwrap_or("") }
                });
                buf.extend_from_slice(b"event: content_block_delta\ndata: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::End { .. } => {
                buf.extend_from_slice(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
            }
            StreamEventIR::Error(err) => {
                // Anthropic SSE error shape: `event: error\ndata: {json}\n\n`.
                let mut tmp = BytesMut::new();
                self.render_error(err, &mut tmp);
                buf.extend_from_slice(b"event: error\ndata: ");
                buf.extend_from_slice(&tmp);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Start { .. } => {
                tracing::warn!(
                    "anthropic: StreamEventIR::Start dropped (no wire equivalent in v1)"
                );
            }
            StreamEventIR::Usage(_) => {
                tracing::warn!(
                    "anthropic: StreamEventIR::Usage dropped (no wire equivalent in v1)"
                );
            }
        }
        Ok(())
    }

    fn parse_response<'a>(&self, _body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        // TODO(v2): non-streaming 2xx response handling.
        unimplemented!(
            "v1: anthropic non-stream response not implemented; 2xx flows through stream"
        )
    }

    fn render_response(
        &self,
        _ir: &ResponseIR<'_>,
        _buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        unimplemented!("v1: anthropic non-stream response not implemented")
    }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        // Anthropic error shape: {"type":"error","error":{"type":"...","message":"..."}}
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
            "type": "error",
            "error": {
                "type": err.code,
                "message": err.message,
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
