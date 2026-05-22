//! Gemini `ProtocolCodec` impl. Streaming uses the
//! `streamGenerateContent?alt=sse` SSE format: each chunk is `data: {json}\n\n`
//! and the stream ends with the final chunk's `finishReason`.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR};
use smallvec::{smallvec, SmallVec};
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

    /// ADR-S1: Gemini's JSON body carries no `stream` field; streaming is
    /// signalled exclusively by the `:streamGenerateContent` URL token
    /// (both `/v1/` and `/v1beta/` flavours use the same token).
    #[inline]
    fn path_implies_stream(path: &str) -> bool {
        path.contains(":streamGenerateContent")
    }

    /// ADR-S1b: Gemini wire spec places the model in the URL path, not the
    /// JSON body (`GeminiRequest::model` is `#[serde(skip)]`). Extract the
    /// substring between `/models/` and the verb separator `:`.
    #[inline]
    fn extract_path_model(path: &str) -> Option<&str> {
        let after = path.split_once("/models/")?.1;
        let model = after.split_once(':').map(|(m, _)| m).unwrap_or(after);
        if model.is_empty() {
            None
        } else {
            Some(model)
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
    ) -> Result<SmallVec<[StreamEventIR<'a>; 1]>, ErrorIR<'static>> {
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
                Err(_) => {
                    // CONTRACT: malformed SSE payloads do not short-circuit
                    // the stream — log and continue scanning the chunk.
                    tracing::warn!("malformed SSE data on gemini");
                    continue;
                }
            };
            let candidate = val.get("candidates").and_then(|c| c.get(0));
            let txt = candidate
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.get(0))
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| Cow::Owned(s.to_owned()));
            let finish = candidate
                .and_then(|c| c.get("finishReason"))
                .and_then(|r| r.as_str())
                .map(|reason| match reason {
                    "STOP" => FinishReasonIR::Stop,
                    "MAX_TOKENS" => FinishReasonIR::Length,
                    "SAFETY" => FinishReasonIR::ContentFilter,
                    _ => FinishReasonIR::Other,
                });

            // ADR-G9-1: a single Gemini SSE frame may carry BOTH `text` and
            // `finishReason` (the "combined final event"). Decompose into
            // `[Delta, End]` so downstream consumers (cross-protocol render,
            // usage accounting) see each IR event distinctly. The common
            // single-event cases still fit in the inline `[_; 1]` storage.
            match (txt, finish) {
                (Some(t), Some(fr)) => {
                    let mut out: SmallVec<[StreamEventIR<'a>; 1]> = SmallVec::new();
                    out.push(StreamEventIR::Delta {
                        text: Some(t),
                        tool_call: None,
                        thinking: None,
                    });
                    out.push(StreamEventIR::End { finish_reason: fr });
                    return Ok(out);
                }
                (Some(t), None) => {
                    return Ok(smallvec![StreamEventIR::Delta {
                        text: Some(t),
                        tool_call: None,
                        thinking: None,
                    }]);
                }
                (None, Some(fr)) => {
                    return Ok(smallvec![StreamEventIR::End { finish_reason: fr }]);
                }
                (None, None) => continue,
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
            StreamEventIR::Error(err) => {
                // Gemini has no formal SSE error event shape; surface the
                // gateway error as a `data:` JSON frame.
                let mut tmp = BytesMut::new();
                self.render_error(err, &mut tmp);
                buf.extend_from_slice(b"data: ");
                buf.extend_from_slice(&tmp);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Start { .. } => {
                tracing::warn!("gemini: StreamEventIR::Start dropped (no wire equivalent in v1)");
            }
            StreamEventIR::Usage(_) => {
                tracing::warn!("gemini: StreamEventIR::Usage dropped (no wire equivalent in v1)");
            }
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
