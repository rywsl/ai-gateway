//! OpenAI Responses `ProtocolCodec` impl. The non-stream response is a
//! single JSON object; streaming uses event-typed SSE (`response.created`,
//! `response.output_item.added`, `response.reasoning_summary_text.delta`,
//! `response.function_call_arguments.delta`, `response.completed`, …).
//!
//! T4-B scope started with the response side (`render_response` /
//! `parse_response` / `parse_stream_chunk` / `render_stream_chunk` /
//! `new_stream_state`). W4 also wires request parse/render so async task
//! submission can persist replayable full IR for `/v1/responses background:true`.

use std::borrow::Cow;
use std::collections::HashMap;

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{
    ContentIR, ContentPart, FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR,
    UsageIR,
};
use serde_json::value::RawValue;
use serde_json::Value;

use crate::common::{
    approx_tokens, bad_request_err, classify, content_to_text, internal_err, map_code,
};
use crate::trait_def::{Headers, ProtocolCodec};

use super::state::OpenAiResponsesStreamState;
use super::wire_request::OpenAiResponsesRequest;
use super::wire_response::OpenAiResponsesResponse;
use super::wire_stream_chunk::{OpenAiResponsesStreamChunk, OpenAiResponsesStreamEventType};

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

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let req: OpenAiResponsesRequest = serde_json::from_slice(body)
            .map_err(|e| bad_request_err(format!("openai responses parse_request: {e}")))?;
        Ok(responses_request_to_ir(req))
    }

    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let req = responses_request_from_ir(ir)?;
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
            let chunk: OpenAiResponsesStreamChunk = match serde_json::from_slice(payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "openai responses stream: malformed data payload");
                    continue;
                }
            };
            dispatch_event(st, &chunk, &mut events);
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
                    FinishReasonIR::Refusal => "completed",
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
            StreamEventIR::RefusalDelta { text } => {
                // OpenAI Responses has a native streaming refusal shape and the
                // parser already lifts it into `RefusalDelta`; render it back
                // instead of dropping the model's refusal (audit P0 #13 §5).
                let payload = serde_json::json!({
                    "type": "response.refusal.delta",
                    "delta": text,
                });
                write_sse(buf, &payload)?;
            }
            StreamEventIR::CitationsDelta { .. }
            | StreamEventIR::ServerToolStart { .. }
            | StreamEventIR::ServerToolDelta { .. } => {}
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
                | MessageIR::Developer { content }
                | MessageIR::User { content }
                | MessageIR::Tool { content, .. }
                | MessageIR::Assistant { content, .. } => approx_tokens(content),
            })
            .sum()
    }
}

const EXT_INPUT: &str = "openai_responses_input";
const EXT_INSTRUCTIONS: &str = "openai_responses_instructions";
const EXT_PREVIOUS_RESPONSE_ID: &str = "openai_responses_previous_response_id";

fn responses_request_to_ir<'a>(req: OpenAiResponsesRequest) -> RequestIR<'a> {
    let mut messages = Vec::new();
    let instructions = req.instructions;
    if let Some(instructions) = instructions.as_ref() {
        messages.push(MessageIR::System {
            content: ContentIR::Text(Cow::Owned(instructions.clone())),
        });
    }
    messages.extend(messages_from_responses_input(&req.input));

    let mut extensions = value_map_to_raw_map(req.extras);
    extensions.insert(EXT_INPUT.into(), value_to_raw(req.input));
    if let Some(instructions) = instructions {
        extensions.insert(
            EXT_INSTRUCTIONS.into(),
            value_to_raw(Value::String(instructions)),
        );
    }
    if let Some(previous_response_id) = req.previous_response_id {
        extensions.insert(
            EXT_PREVIOUS_RESPONSE_ID.into(),
            value_to_raw(Value::String(previous_response_id)),
        );
    }

    RequestIR {
        model: Cow::Owned(req.model),
        messages,
        stream: req.stream,
        extensions: Some(extensions),
        ..RequestIR::default()
    }
}

fn messages_from_responses_input<'a>(input: &Value) -> Vec<MessageIR<'a>> {
    match input {
        Value::String(text) => vec![MessageIR::User {
            content: ContentIR::Text(Cow::Owned(text.clone())),
        }],
        Value::Array(items) => items
            .iter()
            .filter_map(message_from_responses_item)
            .collect(),
        _ => Vec::new(),
    }
}

fn message_from_responses_item<'a>(item: &Value) -> Option<MessageIR<'a>> {
    let item = item.as_object()?;
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "input_text" | "text" => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            Some(MessageIR::User {
                content: ContentIR::Text(Cow::Owned(text.to_owned())),
            })
        }
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = item
                .get("content")
                .map(content_from_responses_content)
                .unwrap_or_else(|| ContentIR::Text(Cow::Borrowed("")));
            Some(match role {
                "system" => MessageIR::System { content },
                "developer" => MessageIR::Developer { content },
                "assistant" => MessageIR::Assistant {
                    content,
                    tool_calls: None,
                },
                _ => MessageIR::User { content },
            })
        }
        _ => None,
    }
}

fn content_from_responses_content<'a>(value: &Value) -> ContentIR<'a> {
    match value {
        Value::String(text) => ContentIR::Text(Cow::Owned(text.clone())),
        Value::Array(parts) => {
            let mut out = smallvec::SmallVec::new();
            for part in parts {
                let typ = part.get("type").and_then(Value::as_str).unwrap_or_default();
                if matches!(typ, "input_text" | "output_text" | "text") {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        out.push(ContentPart::Text(Cow::Owned(text.to_owned())));
                    }
                }
            }
            ContentIR::Parts(out)
        }
        _ => ContentIR::Text(Cow::Borrowed("")),
    }
}

fn responses_request_from_ir(
    ir: &RequestIR<'_>,
) -> Result<OpenAiResponsesRequest, ErrorIR<'static>> {
    let mut extras = raw_map_to_value_map(ir.extensions.as_ref())?;
    let preserved_input = extras.remove(EXT_INPUT);
    let input = preserved_input
        .clone()
        .unwrap_or_else(|| input_from_ir_messages(&ir.messages));
    let preserved_instructions = extras
        .remove(EXT_INSTRUCTIONS)
        .and_then(|v| v.as_str().map(str::to_owned));
    let previous_response_id = extras
        .remove(EXT_PREVIOUS_RESPONSE_ID)
        .and_then(|v| v.as_str().map(str::to_owned));
    let instructions = if preserved_input.is_some() {
        preserved_instructions
    } else {
        first_system_instruction(&ir.messages)
    };

    Ok(OpenAiResponsesRequest {
        model: ir.model.to_string(),
        input,
        stream: ir.stream,
        instructions,
        previous_response_id,
        extras,
    })
}

fn input_from_ir_messages(messages: &[MessageIR<'_>]) -> Value {
    let mut input = Vec::new();
    for message in messages {
        match message {
            MessageIR::System { .. } | MessageIR::Developer { .. } => {}
            MessageIR::User { content } => {
                input.push(serde_json::json!({
                    "type": "input_text",
                    "text": content_to_text(content),
                }));
            }
            MessageIR::Assistant { content, .. } => {
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": content_to_text(content),
                }));
            }
            MessageIR::Tool { content, .. } => {
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "tool",
                    "content": content_to_text(content),
                }));
            }
        }
    }
    Value::Array(input)
}

fn first_system_instruction(messages: &[MessageIR<'_>]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        MessageIR::System { content } | MessageIR::Developer { content } => {
            Some(content_to_text(content).into_owned())
        }
        _ => None,
    })
}

fn value_map_to_raw_map(
    map: serde_json::Map<String, Value>,
) -> HashMap<Cow<'static, str>, Box<RawValue>> {
    map.into_iter()
        .map(|(key, value)| (Cow::Owned(key), value_to_raw(value)))
        .collect()
}

fn raw_map_to_value_map(
    map: Option<&HashMap<Cow<'_, str>, Box<RawValue>>>,
) -> Result<serde_json::Map<String, Value>, ErrorIR<'static>> {
    let mut out = serde_json::Map::new();
    let Some(map) = map else {
        return Ok(out);
    };
    for (key, raw) in map {
        let value = serde_json::from_str(raw.get())
            .map_err(|_| internal_err("openai responses serialize failed"))?;
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

fn value_to_raw(value: Value) -> Box<RawValue> {
    let serialized =
        serde_json::to_string(&value).expect("serializing serde_json::Value is infallible");
    RawValue::from_string(serialized).expect("serialized JSON value must be a valid RawValue")
}

// ─── streaming dispatch helpers ────────────────────────────────────────────

fn dispatch_event(
    st: &mut OpenAiResponsesStreamState,
    chunk: &OpenAiResponsesStreamChunk,
    events: &mut Vec<StreamEventIR<'_>>,
) {
    match &chunk.r#type {
        OpenAiResponsesStreamEventType::ResponseCreated => {
            if let Some(id) = chunk
                .fields
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|v| v.as_str())
            {
                st.seen_response_id = Some(id.to_owned());
            }
        }
        OpenAiResponsesStreamEventType::ResponseOutputItemAdded => {
            let Some(item) = chunk.fields.get("item") else {
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
                    let request_id = item_id.clone();
                    st.pending_reasoning.entry(item_id).or_default();
                    if !request_id.is_empty() {
                        events.push(StreamEventIR::Start {
                            request_id: Cow::Owned(request_id),
                        });
                    }
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
                "web_search_call"
                | "file_search_call"
                | "computer_call"
                | "computer_call_output"
                | "code_interpreter_call"
                | "image_generation_call"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "mcp_call"
                | "mcp_list_tools"
                | "shell_call"
                | "shell_call_output"
                | "apply_patch_call"
                | "apply_patch_call_output"
                | "tool_search_call"
                | "tool_search_output"
                | "compaction" => {
                    push_server_tool_start(st, item_type, item, events);
                }
                _ => {
                    // `message` items announce themselves here too, but the
                    // text actually arrives via `response.output_text.delta`.
                }
            }
        }
        OpenAiResponsesStreamEventType::ResponseReasoningSummaryTextDelta
        | OpenAiResponsesStreamEventType::ResponseReasoningTextDelta => {
            let delta = chunk
                .fields
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !delta.is_empty() {
                if let Some(item_id) = chunk.fields.get("item_id").and_then(|v| v.as_str()) {
                    let builder = st.pending_reasoning.entry(item_id.to_owned()).or_default();
                    builder.summary.push_str(delta);
                }
                events.push(StreamEventIR::ThinkingDelta {
                    text: Cow::Owned(delta.to_owned()),
                    signature_chunk: None,
                });
            }
        }
        OpenAiResponsesStreamEventType::ResponseOutputTextDelta
        | OpenAiResponsesStreamEventType::ResponseTextDelta => {
            let delta = chunk
                .fields
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !delta.is_empty() {
                events.push(StreamEventIR::Delta {
                    text: Some(Cow::Owned(delta.to_owned())),
                    tool_call: None,
                    thinking: None,
                });
            }
        }
        OpenAiResponsesStreamEventType::ResponseRefusalDelta => {
            let delta = chunk
                .fields
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !delta.is_empty() {
                events.push(StreamEventIR::RefusalDelta {
                    text: Cow::Owned(delta.to_owned()),
                });
            }
        }
        OpenAiResponsesStreamEventType::ResponseFunctionCallArgumentsDelta => {
            let Some(item_id) = chunk.fields.get("item_id").and_then(|v| v.as_str()) else {
                return;
            };
            let delta = chunk
                .fields
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
        OpenAiResponsesStreamEventType::ResponseOutputItemDone => {
            let Some(item) = chunk.fields.get("item") else {
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
        OpenAiResponsesStreamEventType::ResponseCompleted => {
            let status = chunk
                .fields
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            let reason = chunk
                .fields
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
            if let Some(u) = chunk.fields.get("response").and_then(|r| r.get("usage")) {
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
                    ..Default::default()
                }));
            }
            events.push(StreamEventIR::End { finish_reason: fr });
        }
        OpenAiResponsesStreamEventType::ResponseFailed
        | OpenAiResponsesStreamEventType::ResponseError => {
            let msg = chunk
                .fields
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .or_else(|| chunk.fields.get("error").and_then(|e| e.get("message")))
                .and_then(|v| v.as_str())
                .unwrap_or("openai responses stream: error")
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
        OpenAiResponsesStreamEventType::ResponseFileSearchCallSearching
        | OpenAiResponsesStreamEventType::ResponseWebSearchCallSearching
        | OpenAiResponsesStreamEventType::ResponseCodeInterpreterCallInterpreting
        | OpenAiResponsesStreamEventType::ResponseImageGenerationCallGenerating
        | OpenAiResponsesStreamEventType::ResponseImageGenerationCallPartialImage
        | OpenAiResponsesStreamEventType::ResponseMcpCallExecuting
        | OpenAiResponsesStreamEventType::ResponseCustomToolCallInputDelta
        | OpenAiResponsesStreamEventType::ResponseCodeInterpreterCallCodeDelta => {
            push_server_tool_delta(chunk, events);
        }
        OpenAiResponsesStreamEventType::Unknown(typ) => {
            tracing::debug!(
                event_type = typ.as_str(),
                "openai responses stream event skipped"
            );
        }
        _ => {
            tracing::debug!(
                event_type = chunk.r#type.as_str(),
                "openai responses stream event skipped"
            );
        }
    }
}

fn push_server_tool_start(
    st: &mut OpenAiResponsesStreamState,
    kind: &str,
    item: &serde_json::Value,
    events: &mut Vec<StreamEventIR<'_>>,
) {
    let index = st.next_tool_call_index;
    st.next_tool_call_index = st.next_tool_call_index.saturating_add(1);
    let id = item
        .get("id")
        .or_else(|| item.get("call_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or(kind);
    events.push(StreamEventIR::ServerToolStart {
        index,
        kind: Cow::Owned(kind.to_owned()),
        id: Cow::Owned(id.to_owned()),
        name: Cow::Owned(name.to_owned()),
    });
}

fn push_server_tool_delta(chunk: &OpenAiResponsesStreamChunk, events: &mut Vec<StreamEventIR<'_>>) {
    let data = chunk
        .fields
        .get("delta")
        .or_else(|| chunk.fields.get("partial_image_b64"))
        .or_else(|| chunk.fields.get("code"))
        .or_else(|| chunk.fields.get("input"))
        .map(|v| match v.as_str() {
            Some(s) => s.to_owned(),
            None => v.to_string(),
        })
        .unwrap_or_default();
    if data.is_empty() {
        return;
    }
    let index = chunk
        .fields
        .get("output_index")
        .or_else(|| chunk.fields.get("item_index"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    events.push(StreamEventIR::ServerToolDelta {
        index,
        data: Cow::Owned(data),
    });
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
                ..Default::default()
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

        let c1 = br#"event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"reasoning","id":"r_1","summary":[]}}

"#;
        let e1 = codec.parse_stream_chunk(state.as_mut(), c1).unwrap();
        assert!(matches!(
            e1.first(),
            Some(StreamEventIR::Start { request_id }) if request_id == "r_1"
        ));

        let c2 = br#"data: {"type":"response.reasoning_summary_text.delta","item_id":"r_1","delta":"step 1; "}

"#;
        let e2 = codec.parse_stream_chunk(state.as_mut(), c2).unwrap();
        assert!(matches!(
            e2.first(),
            Some(StreamEventIR::ThinkingDelta { text, .. }) if text == "step 1; "
        ));

        let c3 = br#"data: {"type":"response.reasoning_summary_text.delta","item_id":"r_1","delta":"step 2"}

"#;
        let e3 = codec.parse_stream_chunk(state.as_mut(), c3).unwrap();
        assert!(matches!(
            e3.first(),
            Some(StreamEventIR::ThinkingDelta { text, .. }) if text == "step 2"
        ));

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
                StreamEventIR::Usage(u) => Some(u.clone()),
                _ => None,
            })
            .expect("Usage emitted");
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 1);
    }

    #[test]
    fn parse_stream_chunk_refusal_delta() {
        let codec = OpenAiResponsesCodec;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        let chunk = br#"event: response.refusal.delta
data: {"type":"response.refusal.delta","delta":"I can't help with that"}

"#;
        let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();

        let refusal = events
            .iter()
            .find_map(|e| match e {
                StreamEventIR::RefusalDelta { text } => Some(text.as_ref()),
                _ => None,
            })
            .expect("RefusalDelta emitted");
        assert_eq!(refusal, "I can't help with that");
    }

    /// Audit P0 #13 §5: a `RefusalDelta` must render back to the native
    /// `response.refusal.delta` SSE event, not be silently dropped. OpenAI
    /// Responses both parses and emits this shape, so a refusal streamed
    /// through the IR must survive round-trip rendering.
    #[test]
    fn render_stream_chunk_refusal_delta_emits_native_event() {
        let codec = OpenAiResponsesCodec;
        let event = StreamEventIR::RefusalDelta {
            text: Cow::Borrowed("I can't help with that"),
        };
        let mut buf = BytesMut::new();
        codec.render_stream_chunk(&event, &mut buf).unwrap();
        let out = String::from_utf8(buf.to_vec()).unwrap();
        assert!(
            out.contains("response.refusal.delta"),
            "refusal must render as the native event: {out}"
        );
        assert!(
            out.contains("I can't help with that"),
            "refusal text must be preserved: {out}"
        );
    }

    #[test]
    fn parse_stream_chunk_reasoning_text_delta_emits_thinking_delta() {
        let codec = OpenAiResponsesCodec;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        let chunk = br#"data: {"type":"response.reasoning_text.delta","item_id":"rs_1","delta":"considering evidence"}

"#;
        let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();

        let thinking = events
            .iter()
            .find_map(|e| match e {
                StreamEventIR::ThinkingDelta { text, .. } => Some(text.as_ref()),
                _ => None,
            })
            .expect("ThinkingDelta emitted");
        assert_eq!(thinking, "considering evidence");
    }

    #[test]
    fn parse_stream_chunk_unknown_event_skips_without_error() {
        let codec = OpenAiResponsesCodec;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        let chunk = br#"event: response.future.delta
data: {"type":"response.future.delta","delta":"new thing"}

"#;
        let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();

        assert!(events.is_empty());
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
