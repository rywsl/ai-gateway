//! Gemini `ProtocolCodec` impl. Streaming uses the
//! `streamGenerateContent?alt=sse` SSE format: each chunk is `data: {json}\n\n`
//! and the stream ends with the final chunk's `finishReason`.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::wire::GeminiRequest;

pub struct Gemini;

impl ProtocolCodec for Gemini {
    #[inline]
    fn detect_request(path: &str, _h: &Headers, _body_head: &[u8]) -> Option<Self> {
        // Gemini paths look like:
        // `/v1beta/models/<model>:generateContent`
        // `/v1beta/models/<model>:streamGenerateContent`
        // `/v1/models/<model>:generateContent`
        if (path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/"))
            && (path.contains(":generateContent") || path.contains(":streamGenerateContent"))
        {
            Some(Gemini)
        } else {
            None
        }
    }

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let req: GeminiRequest<'a> = serde_json::from_slice(body)
            .map_err(|e| bad_request_err(format!("gemini parse: {e}")))?;
        Ok(req.into())
    }

    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let req = GeminiRequest::try_from(ir)?;
        let bytes =
            serde_json::to_vec(&req).map_err(|_| internal_err("gemini serialize failed"))?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<Option<StreamEventIR<'a>>, ErrorIR<'static>> {
        let s = std::str::from_utf8(chunk).map_err(|_| internal_err("invalid utf8 in chunk"))?;
        for line in s.split('\n') {
            let line = line.trim_end_matches('\r');
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            let val: serde_json::Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // finishReason on the candidate marks stream end.
            let candidate = val.get("candidates").and_then(|c| c.get(0));
            if let Some(reason) = candidate
                .and_then(|c| c.get("finishReason"))
                .and_then(|r| r.as_str())
            {
                let finish = match reason {
                    "STOP" => FinishReasonIR::Stop,
                    "MAX_TOKENS" => FinishReasonIR::Length,
                    "SAFETY" => FinishReasonIR::ContentFilter,
                    _ => FinishReasonIR::Other,
                };
                // Some final chunks also carry text; prefer to emit Delta
                // first only if non-empty. For simplicity, surface End.
                return Ok(Some(StreamEventIR::End {
                    finish_reason: finish,
                }));
            }
            let txt = candidate
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.get(0))
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| Cow::Owned(s.to_owned()));
            if txt.is_some() {
                return Ok(Some(StreamEventIR::Delta {
                    text: txt,
                    tool_call: None,
                    thinking: None,
                }));
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
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{ "text": text.as_deref().unwrap_or("") }],
                        },
                    }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::End { finish_reason } => {
                let reason = match finish_reason {
                    FinishReasonIR::Stop => "STOP",
                    FinishReasonIR::Length => "MAX_TOKENS",
                    FinishReasonIR::ContentFilter => "SAFETY",
                    FinishReasonIR::ToolCalls => "STOP",
                    FinishReasonIR::Other => "OTHER",
                };
                let payload = serde_json::json!({
                    "candidates": [{ "finishReason": reason }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize end"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            _ => {}
        }
        Ok(())
    }

    fn parse_response<'a>(&self, _body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        // TODO(v2): non-streaming 2xx response handling.
        unimplemented!("v1: gemini non-stream response not implemented; 2xx flows through stream")
    }

    fn render_response(
        &self,
        _ir: &ResponseIR<'_>,
        _buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        unimplemented!("v1: gemini non-stream response not implemented")
    }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        // Gemini error shape: {"error":{"code":N,"message":"...","status":"..."}}
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
                "code": err.status,
                "message": err.message,
                "status": err.code,
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
