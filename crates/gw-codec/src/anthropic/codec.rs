//! Anthropic `ProtocolCodec` impl. Streaming uses Anthropic's event-typed
//! SSE: each frame is an `event: <type>\ndata: <json>\n\n` pair carrying
//! one of `message_start`, `content_block_start`, `content_block_delta`,
//! `content_block_stop`, `message_delta`, `message_stop`, `ping`, or
//! `error`. The codec accumulates per-block builders inside
//! `AnthropicStreamState` and emits IR events at the natural flush points
//! (block stop / message_delta).

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{MessageIR, RequestIR, ResponseIR, StreamEventIR, UsageIR};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::state::{AnthropicStreamState, OpenBlock};
use super::via::{map_finish_to_stop_reason, map_stop_reason_to_finish};
use super::wire::{
    AnthropicContent, AnthropicRequest, AnthropicResponse, AnthropicStreamDelta,
    AnthropicStreamEvent,
};

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

    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
        Box::new(AnthropicStreamState::default())
    }

    fn parse_stream_chunk<'a>(
        &self,
        state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>> {
        let st = state
            .as_any_mut()
            .downcast_mut::<AnthropicStreamState>()
            .ok_or_else(stream_state_mismatch)?;

        let mut events: Vec<StreamEventIR<'a>> = Vec::new();

        // The trait contract pre-frames chunks on blank-line boundaries. Walk
        // each line collecting `event:` (informational; the payload's `type`
        // field is authoritative) and `data:` so a single SSE event reaches
        // the JSON parser as a single payload.
        let mut pending_data: Option<&[u8]> = None;
        for line in chunk.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                if let Some(payload) = pending_data.take() {
                    dispatch_payload(st, payload, &mut events);
                }
                continue;
            }
            // Skip `event:` headers — payload's `type` discriminant is the
            // authoritative dispatch key.
            if line.starts_with(b"event:") {
                continue;
            }
            if let Some(rest) = line.strip_prefix(b"data:") {
                let rest = trim_ascii_start(rest);
                pending_data = Some(rest);
            }
        }
        if let Some(payload) = pending_data.take() {
            dispatch_payload(st, payload, &mut events);
        }

        Ok(events)
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
            StreamEventIR::ThinkingDelta {
                text,
                signature_chunk,
            } => {
                let payload = serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "thinking_delta", "thinking": text }
                });
                buf.extend_from_slice(b"event: content_block_delta\ndata: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize thinking delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
                if let Some(sig) = signature_chunk.as_deref().filter(|s| !s.is_empty()) {
                    let sig_payload = serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "signature_delta", "signature": sig }
                    });
                    buf.extend_from_slice(b"event: content_block_delta\ndata: ");
                    let bytes = serde_json::to_vec(&sig_payload)
                        .map_err(|_| internal_err("serialize signature delta"))?;
                    buf.extend_from_slice(&bytes);
                    buf.extend_from_slice(b"\n\n");
                }
            }
            StreamEventIR::ToolCallStart { index, id, name } => {
                let payload = serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {},
                    }
                });
                buf.extend_from_slice(b"event: content_block_start\ndata: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize tool_call start"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::ToolCallDelta {
                index,
                arguments_chunk,
            } => {
                let payload = serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "input_json_delta", "partial_json": arguments_chunk }
                });
                buf.extend_from_slice(b"event: content_block_delta\ndata: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize tool_call delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Usage(u) => {
                let payload = serde_json::json!({
                    "type": "message_delta",
                    "delta": {},
                    "usage": {
                        "input_tokens": u.input_tokens,
                        "output_tokens": u.output_tokens,
                    }
                });
                buf.extend_from_slice(b"event: message_delta\ndata: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize usage"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::End { finish_reason } => {
                // Emit a `message_delta` with the stop_reason, then the
                // canonical `message_stop` terminator.
                let stop_reason = map_finish_to_stop_reason(finish_reason);
                let payload = serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason }
                });
                buf.extend_from_slice(b"event: message_delta\ndata: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize message_delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
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
                    "anthropic: StreamEventIR::Start dropped (no wire equivalent in this codec)"
                );
            }
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        let wire: AnthropicResponse<'a> = serde_json::from_slice(body).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("anthropic parse_response: {e}")),
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
        let wire: AnthropicResponse<'_> = ir.try_into()?;
        let mut writer = buf.writer();
        serde_json::to_writer(&mut writer, &wire).map_err(|e| ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 500,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_CODEC_RENDER_RESPONSE),
            message: Cow::Owned(format!("anthropic render: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        Ok(())
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

// ─── streaming dispatch ───────────────────────────────────────────────────

fn dispatch_payload<'a>(
    st: &mut AnthropicStreamState,
    payload: &'a [u8],
    events: &mut Vec<StreamEventIR<'a>>,
) {
    if payload.is_empty() {
        return;
    }
    let event: AnthropicStreamEvent<'a> = match serde_json::from_slice(payload) {
        Ok(e) => e,
        Err(e) => {
            // CONTRACT: malformed payloads must not short-circuit the stream.
            tracing::warn!(error = %e, "anthropic stream: malformed data payload");
            return;
        }
    };
    match event {
        AnthropicStreamEvent::MessageStart { message } => {
            st.seen_message_id = Some(message.id.into_owned());
            // Anthropic provides `input_tokens` on message_start so downstream
            // can surface a partial usage early; the final cumulative usage
            // arrives on `message_delta`.
            if let Some(u) = message.usage {
                events.push(StreamEventIR::Usage(UsageIR {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cached_tokens: u.cache_read_input_tokens,
                    reasoning_tokens: None,
                }));
            }
        }
        AnthropicStreamEvent::ContentBlockStart {
            index,
            content_block,
        } => {
            match content_block {
                AnthropicContent::Text { .. } => {
                    st.open_blocks.insert(index, OpenBlock::Text(String::new()));
                }
                AnthropicContent::Thinking {
                    thinking,
                    signature,
                } => {
                    st.open_blocks.insert(
                        index,
                        OpenBlock::Thinking {
                            text: thinking.into_owned(),
                            signature: signature.map(|s| s.into_owned()).unwrap_or_default(),
                        },
                    );
                }
                AnthropicContent::ToolUse { id, name, input } => {
                    let ir_index = st.next_tool_call_index;
                    st.next_tool_call_index = st.next_tool_call_index.saturating_add(1);
                    // Anthropic sends the empty `input: {}` on the block_start
                    // event; subsequent fragments arrive as `input_json_delta`.
                    let starting_input = match &input {
                        serde_json::Value::Object(m) if m.is_empty() => String::new(),
                        serde_json::Value::Null => String::new(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    st.open_blocks.insert(
                        index,
                        OpenBlock::ToolUse {
                            input_json: starting_input,
                            ir_index,
                        },
                    );
                    // Emit ToolCallStart immediately — id/name are available
                    // up-front in Anthropic's stream, unlike OpenAI which
                    // dribbles them out per-chunk.
                    events.push(StreamEventIR::ToolCallStart {
                        index: ir_index,
                        id: Cow::Owned(id.into_owned()),
                        name: Cow::Owned(name.into_owned()),
                    });
                }
                // Image / ToolResult are not assistant-output blocks.
                AnthropicContent::Image { .. } | AnthropicContent::ToolResult { .. } => {}
            }
        }
        AnthropicStreamEvent::ContentBlockDelta { index, delta } => {
            let Some(builder) = st.open_blocks.get_mut(&index) else {
                tracing::warn!(
                    index,
                    "anthropic stream: content_block_delta for unopened block"
                );
                return;
            };
            match (builder, delta) {
                (OpenBlock::Text(buf), AnthropicStreamDelta::TextDelta { text }) => {
                    buf.push_str(text.as_ref());
                    if !text.is_empty() {
                        events.push(StreamEventIR::Delta {
                            text: Some(Cow::Owned(text.into_owned())),
                            tool_call: None,
                            thinking: None,
                        });
                    }
                }
                (
                    OpenBlock::Thinking { text: buf, .. },
                    AnthropicStreamDelta::ThinkingDelta { thinking },
                ) => {
                    buf.push_str(thinking.as_ref());
                }
                (
                    OpenBlock::Thinking { signature, .. },
                    AnthropicStreamDelta::SignatureDelta { signature: sig },
                ) => {
                    signature.push_str(sig.as_ref());
                }
                (
                    OpenBlock::ToolUse {
                        input_json,
                        ir_index,
                        ..
                    },
                    AnthropicStreamDelta::InputJsonDelta { partial_json },
                ) => {
                    let frag = partial_json.into_owned();
                    input_json.push_str(&frag);
                    if !frag.is_empty() {
                        events.push(StreamEventIR::ToolCallDelta {
                            index: *ir_index,
                            arguments_chunk: Cow::Owned(frag),
                        });
                    }
                }
                // Mismatched delta kind for the open block — silently ignore;
                // Anthropic does not promise strict pairing.
                _ => {}
            }
        }
        AnthropicStreamEvent::ContentBlockStop { index } => {
            let Some(builder) = st.open_blocks.remove(&index) else {
                return;
            };
            match builder {
                OpenBlock::Text(_) => {
                    // Text deltas already streamed event-by-event — nothing
                    // to emit at stop.
                }
                OpenBlock::Thinking { text, signature } => {
                    events.push(StreamEventIR::ThinkingDelta {
                        text: Cow::Owned(text),
                        signature_chunk: if signature.is_empty() {
                            None
                        } else {
                            Some(Cow::Owned(signature))
                        },
                    });
                }
                OpenBlock::ToolUse { .. } => {
                    // ToolCallStart was emitted on block_start; per-fragment
                    // ToolCallDelta events have already been emitted; nothing
                    // remains to flush here.
                }
            }
        }
        AnthropicStreamEvent::MessageDelta { delta, usage } => {
            if let Some(u) = usage {
                events.push(StreamEventIR::Usage(UsageIR {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cached_tokens: u.cache_read_input_tokens,
                    reasoning_tokens: None,
                }));
            }
            if let Some(reason) = delta.stop_reason.as_deref() {
                events.push(StreamEventIR::End {
                    finish_reason: map_stop_reason_to_finish(reason),
                });
            }
        }
        AnthropicStreamEvent::MessageStop => {
            // No-op flush — the canonical End event is emitted on
            // message_delta where the stop_reason carries the IR-mappable
            // signal.
        }
        AnthropicStreamEvent::Ping => {
            // Keepalive.
        }
        AnthropicStreamEvent::Error { error } => {
            events.push(StreamEventIR::Error(ErrorIR {
                category: ErrorCategory::UpstreamError,
                status: 502,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
                message: Cow::Owned(error.message.into_owned()),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            }));
        }
    }
}

#[inline]
fn stream_state_mismatch() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal,
        status: 500,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_CODEC_PARSE_STREAM_CHUNK),
        message: Cow::Borrowed("expected AnthropicStreamState"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
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
        ContentIR, FinishReasonIR, MessageIR, ResponseIR, ResponseThinkingIR, StreamEventIR,
        StreamState, ThinkingBlockIR, ToolUseBlockIR, UsageIR,
    };
    use serde_json::value::RawValue;
    use std::borrow::Cow;

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

    #[test]
    fn render_response_text_only_round_trip() {
        let ir = ResponseIR {
            id: Cow::Borrowed("msg_text"),
            model: Cow::Borrowed("claude-3-5-sonnet"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("Hello, world!")),
                tool_calls: None,
            }],
            finish_reason: Some(FinishReasonIR::Stop),
            usage: Some(UsageIR {
                input_tokens: 5,
                output_tokens: 3,
                cached_tokens: None,
                reasoning_tokens: None,
            }),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        Anthropic.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = Anthropic.parse_response(&buf).unwrap();
        assert_eq!(parsed.id, "msg_text");
        assert_eq!(parsed.model, "claude-3-5-sonnet");
        assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));
        let usage = parsed.usage.expect("usage round-tripped");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 3);
        match &parsed.messages[0] {
            MessageIR::Assistant { content, .. } => match content {
                ContentIR::Text(t) => assert_eq!(t.as_ref(), "Hello, world!"),
                _ => panic!("expected ContentIR::Text"),
            },
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn render_response_with_thinking_and_tool_use() {
        let args = RawValue::from_string(r#"{"city":"SF"}"#.to_owned()).expect("valid JSON");
        let ir = ResponseIR {
            id: Cow::Borrowed("msg_tc"),
            model: Cow::Borrowed("claude-3-5-sonnet"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("")),
                tool_calls: None,
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            thinking: Some(ResponseThinkingIR {
                blocks: vec![ThinkingBlockIR {
                    text: Cow::Borrowed("step 1; step 2"),
                    signature: Some(Cow::Borrowed("sig-xyz")),
                }],
            }),
            tool_use_blocks: Some(vec![ToolUseBlockIR {
                id: Cow::Borrowed("tu_1"),
                name: Cow::Borrowed("get_weather"),
                input: Cow::Owned(args),
            }]),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        Anthropic.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = Anthropic.parse_response(&buf).unwrap();

        // thinking text + signature round-trip
        let thinking = parsed.thinking.expect("thinking round-tripped");
        assert_eq!(thinking.blocks.len(), 1);
        assert_eq!(thinking.blocks[0].text.as_ref(), "step 1; step 2");
        assert_eq!(thinking.blocks[0].signature.as_deref(), Some("sig-xyz"));

        // tool_use_blocks carries the function name
        let tubs = parsed
            .tool_use_blocks
            .expect("tool_use_blocks round-tripped");
        assert_eq!(tubs.len(), 1);
        assert_eq!(tubs[0].name.as_ref(), "get_weather");
        assert_eq!(tubs[0].id.as_ref(), "tu_1");
        assert!(tubs[0].input.get().contains("SF"));

        // finish_reason maps back from "tool_use" → ToolCalls
        assert!(matches!(
            parsed.finish_reason,
            Some(FinishReasonIR::ToolCalls)
        ));
    }

    #[test]
    fn parse_stream_chunk_thinking_block_accumulates_signature() {
        let codec = Anthropic;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        let c1 = br#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

"#;
        let e1 = codec.parse_stream_chunk(state.as_mut(), c1).unwrap();
        assert!(
            e1.is_empty(),
            "no events on thinking content_block_start; got {e1:?}"
        );

        let c2 = br#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"step 1"}}

"#;
        let e2 = codec.parse_stream_chunk(state.as_mut(), c2).unwrap();
        assert!(
            e2.is_empty(),
            "no events emitted yet — buffer until block stop; got {e2:?}"
        );

        let c3 = br#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig123"}}

"#;
        let e3 = codec.parse_stream_chunk(state.as_mut(), c3).unwrap();
        assert!(e3.is_empty());

        let c4 = br#"event: content_block_stop
data: {"type":"content_block_stop","index":0}

"#;
        let e4 = codec.parse_stream_chunk(state.as_mut(), c4).unwrap();
        let (text, sig) = e4
            .iter()
            .find_map(|e| match e {
                StreamEventIR::ThinkingDelta {
                    text,
                    signature_chunk,
                } => Some((text.clone().into_owned(), signature_chunk.clone())),
                _ => None,
            })
            .expect("ThinkingDelta emitted at content_block_stop");
        assert_eq!(text, "step 1");
        assert_eq!(sig.as_deref(), Some("sig123"));
    }
}
