//! OpenAI `ProtocolCodec` impl. Streaming uses OpenAI's SSE format:
//! `data: {json}\n\n` per chunk, terminated by `data: [DONE]\n\n`.

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{FinishReasonIR, MessageIR, RequestIR, ResponseIR, StreamEventIR, UsageIR};
use gw_core::observability::{observe, ErrorKind, ErrorObservation, Stage};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::state::OpenAiStreamState;
use super::wire::{OpenAiRequest, OpenAiResponse, OpenAiStreamPayload};

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
        let bytes = merge_passthrough_overrides(ir, bytes)?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
        Box::new(OpenAiStreamState::default())
    }

    fn parse_stream_chunk<'a>(
        &self,
        state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>> {
        let st = state
            .as_any_mut()
            .downcast_mut::<OpenAiStreamState>()
            .ok_or_else(stream_state_mismatch)?;

        let mut events: Vec<StreamEventIR<'a>> = Vec::new();

        // The trait contract pre-frames chunks on `\n\n`; iterate lines so we
        // handle multi-line `data:` events tolerantly (including any
        // intermixed comment lines).
        for line in chunk.split(|b| *b == b'\n') {
            // Trim trailing CR — SSE typically uses `\r\n`.
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let Some(payload) = line.strip_prefix(b"data:") else {
                continue;
            };
            // Trim leading whitespace (the SSE spec mandates a single space
            // but real producers vary).
            let payload = trim_ascii_start(payload);
            if payload.is_empty() {
                continue;
            }
            if payload == b"[DONE]" {
                // Closing marker. OpenAI typically sends a finish_reason chunk
                // immediately before [DONE], but downstream consumers (and
                // existing tests) rely on the codec surfacing an End regardless
                // — emit it idempotently here.
                events.push(StreamEventIR::End {
                    finish_reason: FinishReasonIR::Stop,
                });
                continue;
            }
            let json: OpenAiStreamPayload<'a> = match serde_json::from_slice(payload) {
                Ok(v) => v,
                Err(e) => {
                    // CONTRACT: do not short-circuit the stream on a single
                    // malformed payload. Route through observe() and continue.
                    observe(ErrorObservation {
                        code: "openai_stream_malformed_data",
                        kind: ErrorKind::CodecParse,
                        stage: Stage::Response,
                        status: None,
                        trace_id: None,
                        extra: Some(&e),
                    });
                    continue;
                }
            };

            if let Some(choice) = json.choices.into_iter().next() {
                if let Some(text) = choice.delta.content.as_deref().filter(|s| !s.is_empty()) {
                    events.push(StreamEventIR::Delta {
                        text: Some(Cow::Owned(text.to_owned())),
                        tool_call: None,
                        thinking: None,
                    });
                }

                if let Some(refusal) = choice.delta.refusal.as_deref().filter(|s| !s.is_empty()) {
                    events.push(StreamEventIR::RefusalDelta {
                        text: Cow::Owned(refusal.to_owned()),
                    });
                }

                if let Some(tcs) = choice.delta.tool_calls {
                    for tc in tcs {
                        let builder = st.pending_tool_calls.entry(tc.index).or_default();
                        if !builder.started_emitted {
                            // First appearance: we need both id and name to
                            // emit ToolCallStart. OpenAI streams these in the
                            // first chunk for the call together with index.
                            if let (Some(id), Some(func)) = (tc.id.as_deref(), tc.function.as_ref())
                            {
                                if let Some(name) = func.name.as_deref() {
                                    builder.id = id.to_owned();
                                    builder.name = name.to_owned();
                                    builder.started_emitted = true;
                                    events.push(StreamEventIR::ToolCallStart {
                                        index: tc.index,
                                        id: Cow::Owned(builder.id.clone()),
                                        name: Cow::Owned(builder.name.clone()),
                                    });
                                }
                            }
                        }
                        if let Some(func) = tc.function {
                            if let Some(args_chunk) =
                                func.arguments.as_deref().filter(|s| !s.is_empty())
                            {
                                builder.arguments.push_str(args_chunk);
                                events.push(StreamEventIR::ToolCallDelta {
                                    index: tc.index,
                                    arguments_chunk: Cow::Owned(args_chunk.to_owned()),
                                });
                            }
                        }
                    }
                }

                if let Some(reason) = choice.finish_reason.as_deref() {
                    let fr = match reason {
                        "stop" => FinishReasonIR::Stop,
                        "length" => FinishReasonIR::Length,
                        "tool_calls" => FinishReasonIR::ToolCalls,
                        "content_filter" => FinishReasonIR::ContentFilter,
                        other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
                    };
                    events.push(StreamEventIR::End { finish_reason: fr });
                }
            }

            if let Some(usage) = json.usage {
                events.push(StreamEventIR::Usage(UsageIR {
                    input_tokens: usage.prompt_tokens,
                    output_tokens: usage.completion_tokens,
                    cached_tokens: usage.prompt_tokens_details.map(|d| d.cached_tokens),
                    audio_tokens_in: usage.prompt_tokens_details.and_then(|d| d.audio_tokens),
                    reasoning_tokens: usage.completion_tokens_details.map(|d| d.reasoning_tokens),
                    audio_tokens_out: usage.completion_tokens_details.and_then(|d| d.audio_tokens),
                    accepted_prediction_tokens: usage
                        .completion_tokens_details
                        .and_then(|d| d.accepted_prediction_tokens),
                    rejected_prediction_tokens: usage
                        .completion_tokens_details
                        .and_then(|d| d.rejected_prediction_tokens),
                    ..Default::default()
                }));
            }
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
                let mut tmp = BytesMut::new();
                self.render_error(err, &mut tmp);
                buf.extend_from_slice(b"data: ");
                buf.extend_from_slice(&tmp);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Start { .. } => {
                observe(ErrorObservation {
                    code: "openai_render_drop_start",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"Start"),
                });
            }
            StreamEventIR::Usage(_) => {
                observe(ErrorObservation {
                    code: "openai_render_drop_usage",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"Usage"),
                });
            }
            StreamEventIR::ThinkingDelta { .. } => {
                observe(ErrorObservation {
                    code: "openai_render_drop_thinking_delta",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"ThinkingDelta"),
                });
            }
            StreamEventIR::ToolCallStart { .. } => {
                observe(ErrorObservation {
                    code: "openai_render_drop_tool_call_start",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"ToolCallStart"),
                });
            }
            StreamEventIR::ToolCallDelta { .. } => {
                observe(ErrorObservation {
                    code: "openai_render_drop_tool_call_delta",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"ToolCallDelta"),
                });
            }
            StreamEventIR::RefusalDelta { text } => {
                let payload = serde_json::json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "refusal": text.as_ref() },
                    }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize refusal delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::CitationsDelta { .. }
            | StreamEventIR::ServerToolStart { .. }
            | StreamEventIR::ServerToolDelta { .. } => {}
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        let wire: OpenAiResponse<'a> = serde_json::from_slice(body).map_err(|e| ErrorIR {
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
        Ok(wire.into())
    }

    fn render_response(
        &self,
        ir: &ResponseIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let wire: OpenAiResponse<'_> = ir.try_into()?;
        let mut writer = buf.writer();
        serde_json::to_writer(&mut writer, &wire).map_err(|e| ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 500,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_CODEC_RENDER_RESPONSE),
            message: Cow::Owned(format!("openai chat render: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        Ok(())
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
                | MessageIR::Developer { content }
                | MessageIR::User { content }
                | MessageIR::Tool { content, .. }
                | MessageIR::Assistant { content, .. } => approx_tokens(content),
            })
            .sum()
    }
}

#[inline]
fn merge_passthrough_overrides(
    ir: &RequestIR<'_>,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, ErrorIR<'static>> {
    let Some(overrides) = ir.passthrough_overrides.as_ref() else {
        return Ok(bytes);
    };
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| internal_err("openai serialize failed"))?;
    let serde_json::Value::Object(map) = &mut value else {
        return Err(internal_err("openai serialize failed"));
    };
    for (key, raw) in overrides {
        let value =
            serde_json::from_str(raw.get()).map_err(|_| internal_err("openai serialize failed"))?;
        map.insert(key.clone(), value);
    }
    serde_json::to_vec(&value).map_err(|_| internal_err("openai serialize failed"))
}

#[inline]
fn stream_state_mismatch() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal,
        status: 500,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_CODEC_PARSE_STREAM_CHUNK),
        message: Cow::Borrowed("expected OpenAiStreamState"),
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

/// `<[u8]>::trim_ascii_start` is stabilised in 1.80; this repo pins 1.86 so
/// using it directly is fine, but inlining a tiny helper keeps this file
/// free of nightly-feature ambiguity should the toolchain ever drift back.
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
        ContentIR, FinishReasonIR, MessageIR, ResponseIR, StreamEventIR, StreamState, ToolCallIR,
        UsageIR,
    };
    use serde_json::value::RawValue;
    use std::borrow::Cow;
    use std::collections::HashMap;

    fn raw(json: &str) -> Box<RawValue> {
        RawValue::from_string(json.to_owned()).expect("valid raw JSON")
    }

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
    fn render_request_merges_passthrough_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert("temperature".to_owned(), raw("0.5"));
        overrides.insert("custom_field".to_owned(), raw(r#""value""#));
        let ir = RequestIR {
            model: Cow::Borrowed("gpt-4o"),
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("hi")),
            }],
            temperature: Some(1.0),
            passthrough_overrides: Some(overrides),
            ..RequestIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAi.render_request(&ir, &mut buf).unwrap();
        let rendered: serde_json::Value = serde_json::from_slice(&buf).unwrap();

        assert_eq!(rendered["temperature"], serde_json::json!(0.5));
        assert_eq!(rendered["custom_field"], serde_json::json!("value"));
    }

    #[test]
    fn render_request_without_overrides_matches_existing_shape() {
        let ir = RequestIR {
            model: Cow::Borrowed("gpt-4o"),
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("hi")),
            }],
            ..RequestIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAi.render_request(&ir, &mut buf).unwrap();

        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false}"#
        );
    }

    #[test]
    fn request_ir_json_storage_restores_for_render() {
        let body =
            br#"{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hi"}]}"#;
        let parsed = OpenAi.parse_request(body).expect("parse request");
        let stored = serde_json::to_value(&parsed).expect("serialize request ir");
        let restored: RequestIR<'static> =
            serde_json::from_value(stored).expect("restore request ir");

        let mut rendered = BytesMut::new();
        OpenAi
            .render_request(&restored, &mut rendered)
            .expect("render restored request");
        let rendered_json: serde_json::Value =
            serde_json::from_slice(&rendered).expect("rendered request json");

        assert_eq!(rendered_json["model"], "gpt-4.1-mini");
        assert_eq!(rendered_json["messages"][0]["content"], "hi");
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
    fn response_ir_json_storage_restores_for_render() {
        let body = br#"{
            "id":"chatcmpl-task",
            "object":"chat.completion",
            "created":1700000000,
            "model":"gpt-4.1-mini",
            "choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}
        }"#;
        let parsed = OpenAi.parse_response(body).expect("parse response");
        let stored = serde_json::to_value(&parsed).expect("serialize response ir");
        let restored: ResponseIR<'static> =
            serde_json::from_value(stored).expect("restore response ir");

        let mut rendered = BytesMut::new();
        OpenAi
            .render_response(&restored, &mut rendered)
            .expect("render restored response");
        let reparsed = OpenAi
            .parse_response(&rendered)
            .expect("reparse rendered response");

        assert_eq!(reparsed.id, "chatcmpl-task");
        assert_openai_assistant_text(&reparsed, "done").unwrap();
        let usage = reparsed.usage.expect("usage");
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 1);
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

    #[test]
    fn render_response_text_only_round_trip() {
        let ir = ResponseIR {
            id: Cow::Borrowed("chatcmpl-abc"),
            model: Cow::Borrowed("gpt-4o"),
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
                ..Default::default()
            }),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAi.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = OpenAi.parse_response(&buf).unwrap();
        assert_eq!(parsed.id, "chatcmpl-abc");
        assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));
        let usage = parsed.usage.as_ref().expect("usage round-tripped");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 3);

        assert_openai_assistant_text(&parsed, "Hello, world!").unwrap();
    }

    #[test]
    fn render_response_with_tool_calls_round_trip() {
        let args_raw = RawValue::from_string(r#"{"city":"SF"}"#.to_owned()).expect("valid JSON");
        let ir = ResponseIR {
            id: Cow::Borrowed("chatcmpl-tool"),
            model: Cow::Borrowed("gpt-4o"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("")),
                tool_calls: Some(vec![ToolCallIR {
                    id: Cow::Borrowed("call_1"),
                    name: Cow::Borrowed("get_weather"),
                    arguments_json: Cow::Owned(args_raw),
                }]),
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAi.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = OpenAi.parse_response(&buf).unwrap();
        let calls = openai_assistant_tool_calls(&parsed).unwrap();
        assert_eq!(calls[0].name, "get_weather");
        assert!(calls[0].arguments_json.get().contains("SF"));
        assert!(matches!(
            parsed.finish_reason,
            Some(FinishReasonIR::ToolCalls)
        ));
    }

    #[test]
    fn parse_stream_chunk_accumulates_tool_call_arguments() {
        let codec = OpenAi;
        let mut state: Box<dyn StreamState> = Box::new(OpenAiStreamState::default());

        let c1 = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\"}}]}}]}\n\n";
        let events1 = codec.parse_stream_chunk(state.as_mut(), c1).unwrap();
        assert!(matches!(
            events1[0],
            StreamEventIR::ToolCallStart { index: 0, .. }
        ));

        let c2 = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n";
        let events2 = codec.parse_stream_chunk(state.as_mut(), c2).unwrap();
        let delta_count = events2
            .iter()
            .filter(|e| matches!(e, StreamEventIR::ToolCallDelta { .. }))
            .count();
        assert_eq!(delta_count, 1);

        let c3 = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"SF\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let events3 = codec.parse_stream_chunk(state.as_mut(), c3).unwrap();
        let final_delta = events3
            .iter()
            .filter(|e| matches!(e, StreamEventIR::ToolCallDelta { .. }))
            .count();
        assert!(final_delta >= 1);
        assert!(matches!(events3.last().unwrap(), StreamEventIR::End { .. }));

        // After all chunks the builder's accumulated arguments should be the
        // concatenated JSON the upstream sent.
        let st = state
            .as_any_mut()
            .downcast_mut::<OpenAiStreamState>()
            .unwrap();
        let acc = &st.pending_tool_calls.get(&0).unwrap().arguments;
        assert_eq!(acc, r#"{"city":"SF"}"#);
    }

    #[test]
    fn test_openai_chat_ir_invariant_text_content_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Parts(Default::default()),
                tool_calls: None,
            }],
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAi.render_response(&ir, &mut buf).is_err());
    }

    #[test]
    fn test_openai_chat_ir_invariant_assistant_role_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("not assistant")),
            }],
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAi.render_response(&ir, &mut buf).is_err());
    }

    #[test]
    fn test_openai_chat_ir_invariant_tool_calls_present_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("")),
                tool_calls: None,
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(OpenAi.render_response(&ir, &mut buf).is_err());
    }

    fn assert_openai_assistant_text(
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

    fn openai_assistant_tool_calls(
        parsed: &ResponseIR<'_>,
    ) -> Result<Vec<ToolCallIR<'static>>, ErrorIR<'static>> {
        match parsed.messages.first() {
            Some(MessageIR::Assistant {
                tool_calls: Some(c),
                ..
            }) => Ok(c
                .iter()
                .map(|tc| ToolCallIR {
                    id: Cow::Owned(tc.id.to_string()),
                    name: Cow::Owned(tc.name.to_string()),
                    arguments_json: Cow::Owned(
                        RawValue::from_string(tc.arguments_json.get().to_owned()).unwrap(),
                    ),
                })
                .collect()),
            Some(MessageIR::Assistant { .. }) => Err(internal_err("expected tool_calls")),
            _ => Err(internal_err("expected tool_calls")),
        }
    }

    #[test]
    fn parse_response_extracts_refusal() {
        let body = br#"{
            "id":"chatcmpl-ref",
            "object":"chat.completion",
            "created":0,
            "model":"gpt-4o",
            "choices":[{"index":0,"message":{"role":"assistant","content":null,"refusal":"I cannot help with that."},"finish_reason":"stop"}]
        }"#;
        let ir = OpenAi.parse_response(body).expect("parse ok");
        assert_eq!(ir.refusal.as_deref(), Some("I cannot help with that."));
    }

    #[test]
    fn parse_response_extracts_annotations() {
        let body = br#"{
            "id":"chatcmpl-ann",
            "object":"chat.completion",
            "created":0,
            "model":"gpt-4o",
            "choices":[{"index":0,"message":{"role":"assistant","content":"See source.","annotations":[{"type":"url_citation","url_citation":{"start_index":4,"end_index":10,"title":"Source Title","url":"https://example.com"}}]},"finish_reason":"stop"}]
        }"#;
        let ir = OpenAi.parse_response(body).expect("parse ok");
        let anns = ir.annotations.as_ref().expect("annotations present");
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].kind.as_ref(), "url_citation");
        assert_eq!(anns[0].url.as_deref(), Some("https://example.com"));
        assert_eq!(anns[0].title.as_deref(), Some("Source Title"));
        assert_eq!(anns[0].start_index, Some(4));
        assert_eq!(anns[0].end_index, Some(10));
    }

    #[test]
    fn refusal_annotations_round_trip() {
        let ir = ResponseIR {
            id: Cow::Borrowed("chatcmpl-rt"),
            model: Cow::Borrowed("gpt-4o"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("See source.")),
                tool_calls: None,
            }],
            finish_reason: Some(FinishReasonIR::Stop),
            refusal: Some(Cow::Borrowed("I cannot help.")),
            annotations: Some(vec![gw_core::ir::AnnotationIR {
                kind: Cow::Borrowed("url_citation"),
                url: Some(Cow::Borrowed("https://example.com")),
                title: Some(Cow::Borrowed("Example")),
                start_index: Some(4),
                end_index: Some(10),
            }]),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        OpenAi.render_response(&ir, &mut buf).unwrap();
        let parsed = OpenAi.parse_response(&buf).unwrap();
        assert_eq!(parsed.refusal.as_deref(), Some("I cannot help."));
        let anns = parsed
            .annotations
            .as_ref()
            .expect("annotations round-tripped");
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn parse_stream_chunk_refusal_delta() {
        let codec = OpenAi;
        let mut state: Box<dyn StreamState> = Box::new(OpenAiStreamState::default());

        let chunk = b"data: {\"choices\":[{\"delta\":{\"refusal\":\"I cannot\"}}]}\n\n";
        let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEventIR::RefusalDelta { text } => assert_eq!(text.as_ref(), "I cannot"),
            other => panic!("expected RefusalDelta, got {other:?}"),
        }
    }

    #[test]
    fn render_stream_chunk_refusal_delta() {
        let event = StreamEventIR::RefusalDelta {
            text: Cow::Borrowed("I cannot help"),
        };
        let mut buf = BytesMut::new();
        OpenAi.render_stream_chunk(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf.to_vec()).unwrap();
        assert!(output.starts_with("data: "));
        assert!(output.contains("\"refusal\""));
        assert!(output.contains("I cannot help"));
    }
}
