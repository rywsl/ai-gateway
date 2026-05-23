//! OpenAI `ProtocolCodec` impl. Streaming uses OpenAI's SSE format:
//! `data: {json}\n\n` per chunk, terminated by `data: [DONE]\n\n`.

use bytes::BytesMut;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR, UsageIR};
use smallvec::{smallvec, SmallVec};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
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
        let req: OpenAiRequest<'a> = serde_json::from_slice(body)
            .map_err(|e| bad_request_err(format!("openai parse: {e}")))?;
        Ok(req.into())
    }

    #[inline]
    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let req = OpenAiRequest::try_from(ir)?;
        let bytes =
            serde_json::to_vec(&req).map_err(|_| internal_err("openai serialize failed"))?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<SmallVec<[StreamEventIR<'a>; 1]>, ErrorIR<'static>> {
        let s = std::str::from_utf8(chunk).map_err(|_| internal_err("invalid utf8 in chunk"))?;
        for line in s.split('\n') {
            let line = line.trim_start();
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim_start();
            if payload.trim() == "[DONE]" {
                return Ok(smallvec![StreamEventIR::End {
                    finish_reason: FinishReasonIR::Stop,
                }]);
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
                    return Ok(smallvec![StreamEventIR::Delta {
                        text: txt,
                        tool_call: None,
                        thinking: None,
                    }]);
                }
                Err(_) => {
                    // CONTRACT: a malformed `data:` payload should NOT
                    // short-circuit the entire stream — surface a warn and
                    // return an empty SmallVec so the framer can continue
                    // with the next chunk.
                    tracing::warn!("malformed SSE data on openai");
                    return Ok(SmallVec::new());
                }
            }
        }
        Ok(SmallVec::new())
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
            StreamEventIR::ThinkingDelta { .. } => {
                tracing::warn!(
                    "openai: StreamEventIR::ThinkingDelta dropped (no wire equivalent in v1)"
                );
            }
            StreamEventIR::ToolCallStart { .. } => {
                tracing::warn!(
                    "openai: StreamEventIR::ToolCallStart dropped (no wire equivalent in v1)"
                );
            }
            StreamEventIR::ToolCallDelta { .. } => {
                tracing::warn!(
                    "openai: StreamEventIR::ToolCallDelta dropped (no wire equivalent in v1)"
                );
            }
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        // v1: extract just `id`, `model`, and `usage` from the upstream
        // ChatCompletion body. `choices` is intentionally not parsed — v2
        // cross-protocol non-stream response render (B1-4) will need it but
        // currently short-circuits 503 until then.
        let parsed: OpenAiResponseBody = serde_json::from_slice(body).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("openai parse_response: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;

        let usage = parsed.usage.map(|u| UsageIR {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
            reasoning_tokens: None,
        });

        Ok(ResponseIR {
            id: Cow::Owned(parsed.id),
            model: Cow::Owned(parsed.model),
            messages: Vec::new(),
            finish_reason: None,
            usage,
            metadata: Default::default(),
            thinking: None,
            reasoning: None,
            tool_use_blocks: None,
        })
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

// ── OpenAI ChatCompletion non-stream response wire structs ────────────────
//
// Kept private to this module: v1 only needs `id`, `model`, and `usage`
// (incl. `prompt_tokens_details.cached_tokens`). `choices` is intentionally
// dropped — v2 cross-protocol render (B1-4) will widen this.

#[derive(serde::Deserialize)]
struct OpenAiResponseUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptDetails>,
}

#[derive(serde::Deserialize)]
struct OpenAiPromptDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(serde::Deserialize)]
struct OpenAiResponseBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<OpenAiResponseUsage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_extracts_usage() {
        let body = br#"{
            "id":"chatcmpl-abc",
            "object":"chat.completion",
            "created":1700000000,
            "model":"gpt-4-turbo",
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":12,
                "completion_tokens":3,
                "total_tokens":15,
                "prompt_tokens_details":{"cached_tokens":7}
            }
        }"#;
        let codec = OpenAi;
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cached_tokens, Some(7));
        assert_eq!(ir.model.as_ref(), "gpt-4-turbo");
    }

    #[test]
    fn parse_response_without_cached_tokens_field() {
        let body = br#"{"id":"x","object":"chat.completion","created":0,"model":"m",
            "choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#;
        let codec = OpenAi;
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cached_tokens, None);
    }

    #[test]
    fn parse_response_returns_protocol_decode_error_on_garbage() {
        let body = b"not json";
        let codec = OpenAi;
        let err = codec.parse_response(body).expect_err("must fail");
        assert_eq!(err.status, 502);
        assert_eq!(
            err.code.as_ref(),
            gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR
        );
    }
}
