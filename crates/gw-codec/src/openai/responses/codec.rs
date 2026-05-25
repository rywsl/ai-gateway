//! OpenAI Responses `ProtocolCodec` impl. The non-stream response is a
//! single JSON object; streaming uses event-typed SSE (`response.created`,
//! `response.output_item.added`, `response.reasoning_summary_text.delta`,
//! `response.function_call_arguments.delta`, `response.completed`, …).
//!
//! T4-B scope is the response side (`render_response` / `parse_response` /
//! `parse_stream_chunk` / `render_stream_chunk` / `new_stream_state`). The
//! request side: `parse_request` returns `bad_request_err` (Responses wire
//! parsing requires a zero-copy `OpenAiResponsesRequest` variant); `render_request`
//! falls back to the Chat wire form so callers can still serialise an IR.

use std::borrow::Cow;

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR, UsageIR};

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::openai::wire::OpenAiRequest;
use crate::trait_def::{Headers, ProtocolCodec};

use super::state::OpenAiResponsesStreamState;
use super::wire_response::OpenAiResponsesResponse;

pub struct OpenAiResponsesCodec;

impl ProtocolCodec for OpenAiResponsesCodec {
    #[inline]
    fn detect_request(path: &str, _h: &Headers, _body_head: &[u8]) -> Option<Self> {
        if path.starts_with("/v1/responses") {
            Some(OpenAiResponsesCodec)
        } else {
            None
        }
    }

    fn parse_request<'a>(&self, _body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        // T4-B scope is the response side (render/parse + stream chunks).
        // Request-side parsing for the Responses shape is wired through
        // `responses_to_chat` (in `super::conversion`) at the proxy layer in
        // T6: the proxy detects `/v1/responses`, lifts to chat shape, and
        // calls the OpenAI Chat codec. Exposing native Responses request
        // parsing here would require a `Cow`-borrowing variant of
        // `OpenAiResponsesRequest` (currently owned `String` everywhere),
        // which is out of scope until T6 surfaces a concrete consumer.
        Err(bad_request_err(
            "openai responses parse_request not wired".to_owned(),
        ))
    }

    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        // v1 path: render the IR back as OpenAI Chat JSON. Native Responses
        // request rendering is wired through `conversion::chat_to_responses`
        // when a request was originally Responses-shaped; the cross-render
        // request path is owned by the proxy layer, not this codec method.
        // For now we delegate request rendering to the Chat wire form so the
        // gateway can use this codec end-to-end against an OpenAI-Chat-style
        // upstream until per-shape request rendering lands.
        let req = OpenAiRequest::try_from(ir)?;
        let bytes = serde_json::to_vec(&req)
            .map_err(|_| internal_err("openai responses serialize failed"))?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
        Box::new(OpenAiResponsesStreamState::default())
    }

    fn parse_stream_chunk<'a>(
        &self,
        state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>> {
        let st = state
            .as_any_mut()
            .downcast_mut::<OpenAiResponsesStreamState>()
            .ok_or_else(stream_state_mismatch)?;

        let mut events: Vec<StreamEventIR<'a>> = Vec::new();

        for line in chunk.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            // Skip the `event: <name>` header line (the type is also carried
            // inside the JSON payload's `type` field, so we parse from there).
            if line.starts_with(b"event:") {
                continue;
            }
            let Some(payload) = line.strip_prefix(b"data:") else {
                continue;
            };
            let payload = trim_ascii_start(payload);
            if payload.is_empty() {
                continue;
            }
            // Some servers emit `data: [DONE]` even on the Responses stream;
            // treat it as a no-op (the canonical terminator is
            // `response.completed`).
            if payload == b"[DONE]" {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_slice(payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "openai responses stream: malformed data payload");
                    continue;
                }
            };
            let typ = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
            dispatch_event(st, typ, &json, &mut events);
        }

        Ok(events)
    }

    fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        // Render a minimal but spec-shaped Responses event for each IR event.
        // The full Responses streaming taxonomy (output_item.added/done,
        // reasoning_summary_text.added/delta, function_call_arguments.delta,
        // …) is large; v1 renders the load-bearing subset used by the
        // cross-protocol re-render path. Unrepresentable events drop with a
        // warn rather than panic — matching the OpenAI Chat codec's policy.
        match ir {
            StreamEventIR::Delta { text, .. } => {
                if let Some(t) = text.as_deref().filter(|s| !s.is_empty()) {
                    let payload = serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": t,
                    });
                    write_sse(buf, &payload)?;
                }
            }
            StreamEventIR::ThinkingDelta { text, .. } => {
                let payload = serde_json::json!({
                    "type": "response.reasoning_summary_text.delta",
                    "delta": text,
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::ToolCallStart { id, name, .. } => {
                let payload = serde_json::json!({
                    "type": "response.output_item.added",
                    "item": {
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": "",
                    }
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::ToolCallDelta {
                arguments_chunk, ..
            } => {
                let payload = serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "delta": arguments_chunk,
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::Usage(u) => {
                let payload = serde_json::json!({
                    "type": "response.usage",
                    "usage": {
                        "input_tokens": u.input_tokens,
                        "output_tokens": u.output_tokens,
                        "total_tokens": u.input_tokens.saturating_add(u.output_tokens),
                    }
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::End { finish_reason } => {
                let status = match finish_reason {
                    FinishReasonIR::Stop | FinishReasonIR::ToolCalls => "completed",
                    FinishReasonIR::Length | FinishReasonIR::ContentFilter => "incomplete",
                    FinishReasonIR::Other(_) => "completed",
                };
                let payload = serde_json::json!({
                    "type": "response.completed",
                    "response": { "status": status },
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::Error(err) => {
                let mut tmp = BytesMut::new();
                self.render_error(err, &mut tmp);
                buf.extend_from_slice(b"data: ");
                buf.extend_from_slice(&tmp);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Start { .. } => {
                tracing::warn!(
                    "openai responses: StreamEventIR::Start dropped (no direct equivalent)"
                );
            }
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        let wire: OpenAiResponsesResponse = serde_json::from_slice(body).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("openai responses parse_response: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        Ok(wire.into())
    }

    fn render_response(
        &self,
        ir: &ResponseIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let wire: OpenAiResponsesResponse = ir.try_into()?;
        let mut writer = buf.writer();
        serde_json::to_writer(&mut writer, &wire).map_err(|e| ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 500,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_CODEC_RENDER_RESPONSE),
            message: Cow::Owned(format!("openai responses render: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        Ok(())
    }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        // OpenAI error shape matches Chat: {"error":{"message":...}}.
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

// ─── streaming dispatch helpers ────────────────────────────────────────────

fn dispatch_event(
    st: &mut OpenAiResponsesStreamState,
    typ: &str,
    json: &serde_json::Value,
    events: &mut Vec<StreamEventIR<'_>>,
) {
    match typ {
        "response.created" => {
            if let Some(id) = json
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|v| v.as_str())
            {
                st.seen_response_id = Some(id.to_owned());
            }
        }
        "response.output_item.added" => {
            let Some(item) = json.get("item") else {
                return;
            };
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let item_id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            match item_type {
                "reasoning" => {
                    st.pending_reasoning.entry(item_id).or_default();
                }
                "function_call" => {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let idx = st.next_tool_call_index;
                    st.next_tool_call_index = st.next_tool_call_index.saturating_add(1);
                    let stable_id = if call_id.is_empty() {
                        item_id.clone()
                    } else {
                        call_id.clone()
                    };
                    let builder = st.pending_function_calls.entry(item_id).or_default();
                    builder.index = idx;
                    builder.call_id = call_id;
                    builder.name = name.clone();
                    if !builder.start_emitted {
                        builder.start_emitted = true;
                        events.push(StreamEventIR::ToolCallStart {
                            index: idx,
                            id: Cow::Owned(stable_id),
                            name: Cow::Owned(name),
                        });
                    }
                }
                _ => {
                    // `message` items announce themselves here too, but the
                    // text actually arrives via `response.output_text.delta`.
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let Some(item_id) = json.get("item_id").and_then(|v| v.as_str()) else {
                return;
            };
            let delta = json.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            let builder = st.pending_reasoning.entry(item_id.to_owned()).or_default();
            builder.summary.push_str(delta);
        }
        "response.output_text.delta" => {
            let delta = json.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if !delta.is_empty() {
                events.push(StreamEventIR::Delta {
                    text: Some(Cow::Owned(delta.to_owned())),
                    tool_call: None,
                    thinking: None,
                });
            }
        }
        "response.function_call_arguments.delta" => {
            let Some(item_id) = json.get("item_id").and_then(|v| v.as_str()) else {
                return;
            };
            let delta = json.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if delta.is_empty() {
                return;
            }
            if let Some(builder) = st.pending_function_calls.get_mut(item_id) {
                builder.arguments.push_str(delta);
                events.push(StreamEventIR::ToolCallDelta {
                    index: builder.index,
                    arguments_chunk: Cow::Owned(delta.to_owned()),
                });
            }
        }
        "response.output_item.done" => {
            let Some(item) = json.get("item") else {
                return;
            };
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if item_type == "reasoning" {
                if let Some(builder) = st.pending_reasoning.get_mut(item_id) {
                    if !builder.items_emitted {
                        builder.items_emitted = true;
                        events.push(StreamEventIR::ThinkingDelta {
                            text: Cow::Owned(std::mem::take(&mut builder.summary)),
                            signature_chunk: None,
                        });
                    }
                }
            }
        }
        "response.completed" => {
            let status = json
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            let reason = json
                .get("response")
                .and_then(|r| r.get("incomplete_details"))
                .and_then(|d| d.get("reason"))
                .and_then(|v| v.as_str());
            let saw_function_call = !st.pending_function_calls.is_empty();
            let fr = match status {
                "completed" => {
                    if saw_function_call {
                        FinishReasonIR::ToolCalls
                    } else {
                        FinishReasonIR::Stop
                    }
                }
                "incomplete" => match reason {
                    Some("max_output_tokens") => FinishReasonIR::Length,
                    Some("content_filter") => FinishReasonIR::ContentFilter,
                    Some(other) => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
                    None => FinishReasonIR::Length,
                },
                other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
            };
            // Optional usage embedded in `response`.
            if let Some(u) = json.get("response").and_then(|r| r.get("usage")) {
                let input_tokens = u
                    .get("input_tokens")
                    .or_else(|| u.get("prompt_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output_tokens = u
                    .get("output_tokens")
                    .or_else(|| u.get("completion_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let cached_tokens = u
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                let reasoning_tokens = u
                    .get("output_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                events.push(StreamEventIR::Usage(UsageIR {
                    input_tokens,
                    output_tokens,
                    cached_tokens,
                    reasoning_tokens,
                }));
            }
            events.push(StreamEventIR::End { finish_reason: fr });
        }
        "response.failed" => {
            let msg = json
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("openai responses stream: response.failed")
                .to_owned();
            events.push(StreamEventIR::Error(ErrorIR {
                category: ErrorCategory::UpstreamError,
                status: 502,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
                message: Cow::Owned(msg),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            }));
        }
        _ => {
            // Unknown event type: ignore silently so future SSE additions don't
            // break parsing.
        }
    }
}

// ─── small local helpers ──────────────────────────────────────────────────

fn write_sse(buf: &mut BytesMut, payload: &serde_json::Value) -> Result<(), ErrorIR<'static>> {
    buf.extend_from_slice(b"data: ");
    let bytes = serde_json::to_vec(payload)
        .map_err(|_| internal_err("openai responses serialize stream chunk"))?;
    buf.extend_from_slice(&bytes);
    buf.extend_from_slice(b"\n\n");
    Ok(())
}

#[inline]
fn stream_state_mismatch() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal,
        status: 500,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_CODEC_PARSE_STREAM_CHUNK),
        message: Cow::Borrowed("expected OpenAiResponsesStreamState"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
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

#[inline]
fn trim_ascii_start(mut s: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

#[allow(dead_code)]
const _ASSERT_CODE: &str = codes::GW_UPSTREAM_4XX;

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use gw_core::ir::{
        ContentIR, FinishReasonIR, MessageIR, ReasoningBlockIR, ResponseIR, StreamEventIR,
        StreamState, ToolCallIR, UsageIR,
    };
    use serde_json::value::RawValue;
    use std::borrow::Cow;

    #[test]
    fn render_response_with_reasoning_round_trip() {
        let ir = ResponseIR {
            id: Cow::Borrowed("resp_abc"),
            model: Cow::Borrowed("o1"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("answer")),
                tool_calls: None,
            }],
            finish_reason: Some(FinishReasonIR::Stop),
            usage: Some(UsageIR {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: None,
                reasoning_tokens: Some(20),
            }),
            reasoning: Some(ReasoningBlockIR {
                summary: Cow::Borrowed("step 1; step 2"),
            }),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAiResponsesCodec.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = OpenAiResponsesCodec.parse_response(&buf).unwrap();
        assert_eq!(parsed.id, "resp_abc");
        assert_eq!(parsed.model, "o1");
        let reasoning = parsed.reasoning.as_ref().expect("reasoning round-tripped");
        assert_eq!(reasoning.summary.as_ref(), "step 1; step 2");
        let usage = parsed.usage.as_ref().expect("usage round-tripped");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.reasoning_tokens, Some(20));
        assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));
        assert_responses_assistant_text(&parsed, "answer").unwrap();
    }

    #[test]
    fn render_response_with_function_call_status_is_completed_with_tool_calls_reason() {
        let args = RawValue::from_string(r#"{"city":"SF"}"#.to_owned()).unwrap();
        let ir = ResponseIR {
            id: Cow::Borrowed("resp_tc"),
            model: Cow::Borrowed("gpt-4o"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("")),
                tool_calls: Some(vec![ToolCallIR {
                    id: Cow::Borrowed("call_42"),
                    name: Cow::Borrowed("get_weather"),
                    arguments_json: Cow::Owned(args),
                }]),
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAiResponsesCodec.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = OpenAiResponsesCodec.parse_response(&buf).unwrap();
        // finish_reason maps back to ToolCalls because the output carried a
        // FunctionCall item (status was "completed" on the wire).
        assert!(matches!(
            parsed.finish_reason,
            Some(FinishReasonIR::ToolCalls)
        ));
        assert_responses_assistant_tool_call(&parsed).unwrap();
    }

    #[test]
    fn parse_response_returns_protocol_decode_error_on_garbage() {
        let err = OpenAiResponsesCodec
            .parse_response(b"not json")
            .unwrap_err();
        assert_eq!(err.status, 502);
        assert_eq!(
            err.code.as_ref(),
            gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR
        );
    }

    #[test]
    fn parse_stream_chunk_accumulates_reasoning_summary() {
        let codec = OpenAiResponsesCodec;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        // Open the reasoning item.
        let c1 = br#"event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"reasoning","id":"r_1","summary":[]}}

"#;
        let e1 = codec.parse_stream_chunk(state.as_mut(), c1).unwrap();
        assert!(
            e1.is_empty(),
            "no events on output_item.added for reasoning"
        );

        // Two summary deltas.
        let c2 = br#"data: {"type":"response.reasoning_summary_text.delta","item_id":"r_1","delta":"step 1; "}

"#;
        let e2 = codec.parse_stream_chunk(state.as_mut(), c2).unwrap();
        assert!(e2.is_empty(), "no events emitted yet — buffer until done");

        let c3 = br#"data: {"type":"response.reasoning_summary_text.delta","item_id":"r_1","delta":"step 2"}

"#;
        let e3 = codec.parse_stream_chunk(state.as_mut(), c3).unwrap();
        assert!(e3.is_empty());

        // Close the reasoning item → expect a ThinkingDelta.
        let c4 =
            br#"data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"r_1"}}

"#;
        let e4 = codec.parse_stream_chunk(state.as_mut(), c4).unwrap();
        let thinking = e4
            .iter()
            .find_map(|e| match e {
                StreamEventIR::ThinkingDelta { text, .. } => Some(text.clone().into_owned()),
                _ => None,
            })
            .expect("ThinkingDelta emitted");
        assert_eq!(thinking, "step 1; step 2");

        // response.completed → End.
        let c5 = br#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4,"output_tokens":1,"total_tokens":5}}}

"#;
        let e5 = codec.parse_stream_chunk(state.as_mut(), c5).unwrap();
        assert!(matches!(
            e5.last().unwrap(),
            StreamEventIR::End {
                finish_reason: FinishReasonIR::Stop,
            }
        ));
        let usage = e5
            .iter()
            .find_map(|e| match e {
                StreamEventIR::Usage(u) => Some(*u),
                _ => None,
            })
            .expect("Usage emitted");
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 1);
    }

    #[test]
    fn test_openai_responses_ir_invariant_text_content_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Parts(Default::default()),
                tool_calls: None,
            }],
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAiResponsesCodec.render_response(&ir, &mut buf).is_err());
    }

    #[test]
    fn test_openai_responses_ir_invariant_assistant_role_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("not assistant")),
            }],
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAiResponsesCodec.render_response(&ir, &mut buf).is_err());
    }

    #[test]
    fn test_openai_responses_ir_invariant_tool_call_assistant_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("not assistant")),
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAiResponsesCodec.render_response(&ir, &mut buf).is_err());
    }

    fn assert_responses_assistant_text(
        parsed: &ResponseIR<'_>,
        expected: &str,
    ) -> Result<(), ErrorIR<'static>> {
        match parsed.messages.first() {
            Some(MessageIR::Assistant {
                content: ContentIR::Text(t),
                ..
            }) => {
                assert_eq!(t.as_ref(), expected);
                Ok(())
            }
            Some(MessageIR::Assistant { .. }) => Err(internal_err("expected ContentIR::Text")),
            _ => Err(internal_err("expected Assistant")),
        }
    }

    fn assert_responses_assistant_tool_call(
        parsed: &ResponseIR<'_>,
    ) -> Result<(), ErrorIR<'static>> {
        match parsed.messages.first() {
            Some(MessageIR::Assistant { tool_calls, .. }) => {
                let calls = tool_calls.as_ref().expect("tool_calls present");
                assert_eq!(calls[0].id.as_ref(), "call_42");
                assert_eq!(calls[0].name.as_ref(), "get_weather");
                assert!(calls[0].arguments_json.get().contains("SF"));
                Ok(())
            }
            _ => Err(internal_err("expected Assistant")),
        }
    }
}
