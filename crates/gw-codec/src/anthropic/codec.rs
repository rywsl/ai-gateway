//! Anthropic `ProtocolCodec` impl. Streaming uses Anthropic's SSE format:
//! each event is an `event: <kind>\ndata: {json}\n\n` pair. v1 understands
//! `content_block_delta` and `message_stop`.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use smallvec::{smallvec, SmallVec};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::wire::AnthropicRequest;

pub struct Anthropic;

/// Subset of the Anthropic Messages non-stream response body needed for
/// `parse_response`. Field names match Anthropic's wire shape.
///
/// `cache_creation_input_tokens` is intentionally not deserialised because v1
/// `UsageIR` has no field for it; it will be wired when codec-s4 v2 extends
/// `UsageIR`.
#[derive(serde::Deserialize)]
struct AnthropicResponseUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(serde::Deserialize)]
struct AnthropicResponseBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<AnthropicResponseUsage>,
}

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
    ) -> Result<SmallVec<[StreamEventIR<'a>; 1]>, ErrorIR<'static>> {
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
            (Some("message_stop"), _) => Ok(smallvec![StreamEventIR::End {
                finish_reason: FinishReasonIR::Stop,
            }]),
            (Some("content_block_delta"), Some(payload)) if !payload.is_empty() => {
                let val: serde_json::Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => {
                        // CONTRACT: malformed payloads must not short-circuit
                        // the stream — surface a warn and return an empty
                        // SmallVec so the framer continues to the next chunk.
                        tracing::warn!("malformed SSE data on anthropic");
                        return Ok(SmallVec::new());
                    }
                };
                let txt = val
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
                    .map(|s| Cow::Owned(s.to_owned()));
                Ok(smallvec![StreamEventIR::Delta {
                    text: txt,
                    tool_call: None,
                    thinking: None,
                }])
            }
            _ => Ok(SmallVec::new()),
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
                buf.extend_from_slice(
                    b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                );
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

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        // v1.x: extract usage + model/id from Anthropic Messages non-stream
        // response. `messages` / `finish_reason` are not yet reconstructed
        // because same-protocol passthrough does not need them — the bytes
        // are forwarded verbatim by the proxy layer. Will be revisited when
        // codec-s4 v2 wires cross-protocol non-stream re-render.
        let parsed: AnthropicResponseBody = serde_json::from_slice(body).map_err(|e| ErrorIR {
            category: gw_core::error::ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("anthropic parse_response: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;

        let usage = parsed.usage.map(|u| gw_core::ir::UsageIR {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_tokens: u.cache_read_input_tokens,
        });

        Ok(ResponseIR {
            id: Cow::Owned(parsed.id),
            model: Cow::Owned(parsed.model),
            messages: Vec::new(),
            finish_reason: None,
            usage,
            metadata: Default::default(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trait_def::ProtocolCodec;

    #[test]
    fn parse_response_extracts_usage() {
        let body = br#"{
            "id":"msg_abc",
            "type":"message",
            "role":"assistant",
            "content":[{"type":"text","text":"hi"}],
            "model":"claude-3-5-sonnet",
            "stop_reason":"end_turn",
            "usage":{
                "input_tokens":20,
                "output_tokens":5,
                "cache_read_input_tokens":15,
                "cache_creation_input_tokens":0
            }
        }"#;
        let codec = Anthropic;
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cached_tokens, Some(15));
        assert_eq!(ir.model.as_ref(), "claude-3-5-sonnet");
    }

    #[test]
    fn parse_response_no_cache_fields() {
        let body = br#"{"id":"x","type":"message","role":"assistant","content":[],
            "model":"m","stop_reason":"end_turn",
            "usage":{"input_tokens":3,"output_tokens":1}}"#;
        let codec = Anthropic;
        let usage = codec.parse_response(body).unwrap().usage.unwrap();
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cached_tokens, None);
    }

    #[test]
    fn parse_response_returns_protocol_decode_error_on_garbage() {
        let body = b"not json";
        let codec = Anthropic;
        let err = codec.parse_response(body).expect_err("must fail");
        assert_eq!(err.status, 502);
        assert_eq!(
            err.code.as_ref(),
            gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR
        );
    }
}
