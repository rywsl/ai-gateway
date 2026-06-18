//! Gemini `ProtocolCodec` impl. Streaming uses the
//! `streamGenerateContent?alt=sse` SSE format: each chunk is a
//! `data: <json>\r\n\r\n` frame carrying a partial `GeminiResponse` (with
//! `candidates[]`, optional `usageMetadata`, optional `finishReason`). T4-D
//! makes parsing stateful so function-call deltas can be threaded into the
//! IR's `ToolCallStart` / `ToolCallDelta` events; pure text frames still
//! decompose 1:1.

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{MessageIR, RequestIR, ResponseIR, StreamEventIR, UsageIR};
use gw_core::observability::{observe, ErrorKind, ErrorObservation, Stage};
use std::borrow::Cow;

use crate::common::{approx_tokens, bad_request_err, classify, internal_err, map_code};
use crate::trait_def::{Headers, ProtocolCodec};

use super::state::{GeminiFunctionCallBuilder, GeminiStreamState};
use super::via::{map_finish_reason_in, map_finish_reason_out};
use super::wire::{GeminiPart, GeminiRequest, GeminiResponse};

pub struct Gemini;

impl ProtocolCodec for Gemini {
    #[inline]
    fn detect_request(path: &str, _h: &Headers, _body_head: &[u8]) -> Option<Self> {
        // Gemini paths look like:
        // `/v1beta/models/<model>:generateContent`
        // `/v1beta/models/<model>:streamGenerateContent`
        // `/v1/models/<model>:generateContent`
        if path == "/v1beta/files"
            || path.starts_with("/v1beta/files/")
            || ((path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/"))
                && (path.contains(":generateContent") || path.contains(":streamGenerateContent")))
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
        let bytes = merge_passthrough_overrides(ir, bytes)?;
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
        Box::new(GeminiStreamState::default())
    }

    fn parse_stream_chunk<'a>(
        &self,
        state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>> {
        let st = state
            .as_any_mut()
            .downcast_mut::<GeminiStreamState>()
            .ok_or_else(stream_state_mismatch)?;

        let mut events: Vec<StreamEventIR<'a>> = Vec::new();

        // The trait contract pre-frames chunks; Gemini's SSE format is
        // `data: <json>\r?\n\r?\n` repeated, so walk each line and feed
        // each `data:` payload to the JSON parser independently.
        for line in chunk.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(payload) = line.strip_prefix(b"data:") else {
                continue;
            };
            let payload = trim_ascii_start(payload);
            if payload.is_empty() {
                continue;
            }
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
            StreamEventIR::ToolCallStart { name, .. } => {
                // Gemini's stream emits the functionCall name + (typically
                // complete) args in a single frame; the IR splits this into
                // ToolCallStart (carrying name) and ToolCallDelta (carrying
                // the args JSON). On render we emit a single Gemini frame
                // with the name and an empty args object; the ToolCallDelta
                // that follows produces a second frame with the full args.
                let payload = serde_json::json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{ "functionCall": { "name": name, "args": {} } }],
                        },
                    }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize tool_call start"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::ToolCallDelta {
                arguments_chunk, ..
            } => {
                let args: serde_json::Value = serde_json::from_str(arguments_chunk.as_ref())
                    .unwrap_or(serde_json::Value::Null);
                let payload = serde_json::json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{ "functionCall": { "name": "", "args": args } }],
                        },
                    }]
                });
                buf.extend_from_slice(b"data: ");
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|_| internal_err("serialize tool_call delta"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::Usage(u) => {
                let payload = serde_json::json!({
                    "candidates": [],
                    "usageMetadata": {
                        "promptTokenCount": u.input_tokens,
                        "candidatesTokenCount": u.output_tokens,
                        "cachedContentTokenCount": u.cached_tokens,
                    }
                });
                buf.extend_from_slice(b"data: ");
                let bytes =
                    serde_json::to_vec(&payload).map_err(|_| internal_err("serialize usage"))?;
                buf.extend_from_slice(&bytes);
                buf.extend_from_slice(b"\n\n");
            }
            StreamEventIR::End { finish_reason } => {
                let reason = map_finish_reason_out(finish_reason);
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
                observe(ErrorObservation {
                    code: "gemini_render_drop_start",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"Start"),
                });
            }
            StreamEventIR::ThinkingDelta { .. } => {
                observe(ErrorObservation {
                    code: "gemini_render_drop_thinking_delta",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"ThinkingDelta"),
                });
            }
            StreamEventIR::RefusalDelta { .. } => {
                // Gemini's wire has no streaming refusal shape, so the drop is
                // correct — but make it observable rather than silent (audit
                // P0 #13 §5), matching the Start / ThinkingDelta markers above.
                observe(ErrorObservation {
                    code: "gemini_render_drop_refusal_delta",
                    kind: ErrorKind::CodecRender,
                    stage: Stage::Response,
                    status: None,
                    trace_id: None,
                    extra: Some(&"RefusalDelta"),
                });
            }
            StreamEventIR::CitationsDelta { .. }
            | StreamEventIR::ServerToolStart { .. }
            | StreamEventIR::ServerToolDelta { .. } => {}
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        let wire: GeminiResponse<'a> = serde_json::from_slice(body).map_err(|e| ErrorIR {
            // ADR-B1-1: protocol decode failure on a 2xx upstream body is an
            // upstream-side defect from the gateway's perspective — surface as
            // 502 with the dedicated GW_UPSTREAM_PROTOCOL_DECODE_ERROR code.
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("gemini parse_response: {e}")),
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
        let wire: GeminiResponse<'_> = ir.try_into()?;
        let mut writer = buf.writer();
        serde_json::to_writer(&mut writer, &wire).map_err(|e| ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 500,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_CODEC_RENDER_RESPONSE),
            message: Cow::Owned(format!("gemini render: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        Ok(())
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
        serde_json::from_slice(&bytes).map_err(|_| internal_err("gemini serialize failed"))?;
    let serde_json::Value::Object(map) = &mut value else {
        return Err(internal_err("gemini serialize failed"));
    };
    for (key, raw) in overrides {
        let value =
            serde_json::from_str(raw.get()).map_err(|_| internal_err("gemini serialize failed"))?;
        map.insert(key.clone(), value);
    }
    serde_json::to_vec(&value).map_err(|_| internal_err("gemini serialize failed"))
}

// ─── streaming dispatch ───────────────────────────────────────────────────

fn dispatch_payload<'a>(
    st: &mut GeminiStreamState,
    payload: &'a [u8],
    events: &mut Vec<StreamEventIR<'a>>,
) {
    let wire: GeminiResponse<'a> = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => {
            // CONTRACT: do not short-circuit the stream on a single
            // malformed payload. Route through observe() and continue.
            observe(ErrorObservation {
                code: "gemini_stream_malformed_data",
                kind: ErrorKind::CodecParse,
                stage: Stage::Response,
                status: None,
                trace_id: None,
                extra: Some(&e),
            });
            return;
        }
    };
    st.seen_response = true;

    for candidate in wire.candidates {
        if let Some(content) = candidate.content {
            for part in content.parts {
                match part {
                    GeminiPart::Text { text, thought, .. } => {
                        if thought == Some(true) {
                            if !text.is_empty() {
                                events.push(StreamEventIR::Delta {
                                    text: None,
                                    tool_call: None,
                                    thinking: Some(text),
                                });
                            }
                        } else if !text.is_empty() {
                            events.push(StreamEventIR::Delta {
                                text: Some(text),
                                tool_call: None,
                                thinking: None,
                            });
                        }
                    }
                    GeminiPart::FunctionCall { function_call } => {
                        // Match by function name across chunks — Gemini
                        // doesn't ship an id. If we've already seen this
                        // call, append the args delta to the existing
                        // builder; otherwise start a new one.
                        let name_str = function_call.name.as_ref();
                        let existing_idx = st
                            .pending_function_calls
                            .iter()
                            .position(|b| b.name == name_str);

                        // T-W3-08: try_push_function_call enforces the
                        // per-stream cap and emits
                        // gw_codec_overflow_total{kind="too_many_function_calls"}.
                        // On overflow we skip this call entirely (no
                        // ToolCallStart / ToolCallDelta) — emitting half a
                        // call would corrupt the IR.
                        let builder_idx = match existing_idx {
                            Some(i) => i,
                            None => {
                                let next = st.pending_function_calls.len() as u32;
                                match st.try_push_function_call(GeminiFunctionCallBuilder {
                                    name: name_str.to_owned(),
                                    args_json: String::new(),
                                    emitted_start: false,
                                    ir_index: next,
                                }) {
                                    Ok(i) => i,
                                    Err(_) => continue,
                                }
                            }
                        };

                        let builder = &mut st.pending_function_calls[builder_idx];
                        if !builder.emitted_start {
                            builder.emitted_start = true;
                            events.push(StreamEventIR::ToolCallStart {
                                index: builder.ir_index,
                                id: Cow::Borrowed(""),
                                name: Cow::Owned(builder.name.clone()),
                            });
                        }
                        let ir_index = builder.ir_index;
                        // Serialize the args fragment to a JSON string and
                        // emit as a ToolCallDelta. Gemini usually delivers
                        // args in one shot, but the IR contract is
                        // fragment-friendly so we re-emit each chunk's
                        // payload as it arrives.
                        let frag = serde_json::to_string(&function_call.args)
                            .unwrap_or_else(|_| "null".to_owned());
                        if !frag.is_empty() && frag != "null" && frag != "{}" {
                            // T-W3-08: try_extend_args enforces the
                            // per-builder args-buffer cap. On overflow we
                            // drop the delta and skip the ToolCallDelta —
                            // the partial buffer that already shipped
                            // stays valid; later finalisation may still
                            // surface what we have.
                            if st.try_extend_args(builder_idx, &frag).is_ok() {
                                events.push(StreamEventIR::ToolCallDelta {
                                    index: ir_index,
                                    arguments_chunk: Cow::Owned(frag),
                                });
                            }
                        }
                    }
                    GeminiPart::FunctionResponse { .. }
                    | GeminiPart::InlineData { .. }
                    | GeminiPart::FileData { .. }
                    | GeminiPart::ExecutableCode { .. }
                    | GeminiPart::CodeExecutionResult { .. } => {}
                }
            }
        }
        if let Some(reason) = candidate.finish_reason.as_deref() {
            events.push(StreamEventIR::End {
                finish_reason: map_finish_reason_in(reason),
            });
        }
    }

    if let Some(u) = wire.usage_metadata {
        events.push(StreamEventIR::Usage(UsageIR {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cached_tokens: u.cached_content_token_count,
            reasoning_tokens: None,
            thoughts_tokens: u.thoughts_token_count,
            ..Default::default()
        }));
    }
}

#[inline]
fn stream_state_mismatch() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal,
        status: 500,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_CODEC_PARSE_STREAM_CHUNK),
        message: Cow::Borrowed("expected GeminiStreamState"),
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
    fn parse_response_extracts_usage_metadata() {
        let body = br#"{
            "candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}],
            "modelVersion":"gemini-1.5-pro",
            "usageMetadata":{
                "promptTokenCount":15,
                "candidatesTokenCount":4,
                "totalTokenCount":19,
                "cachedContentTokenCount":8
            }
        }"#;
        let codec = Gemini;
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.cached_tokens, Some(8));
        assert_eq!(ir.model.as_ref(), "gemini-1.5-pro");
    }

    /// Audit P0 #13 §2: native Gemini top-level `safetySettings` and
    /// `toolConfig` must survive the parse→render round trip (the path taken
    /// whenever a mutation — forced params, model mapping, body rules —
    /// forces a re-render instead of zero-copy passthrough). The wire struct
    /// previously declared neither field, so serde silently dropped them.
    #[test]
    fn render_request_preserves_safety_settings_and_tool_config() {
        let body = br#"{
            "contents":[{"role":"user","parts":[{"text":"hi"}]}],
            "safetySettings":[{"category":"HARM_CATEGORY_HATE_SPEECH","threshold":"BLOCK_NONE"}],
            "toolConfig":{"functionCallingConfig":{"mode":"AUTO"}}
        }"#;
        let codec = Gemini;
        let ir = codec.parse_request(body).expect("parse ok");
        let mut buf = BytesMut::new();
        codec.render_request(&ir, &mut buf).expect("render ok");
        let rendered: serde_json::Value = serde_json::from_slice(&buf).unwrap();

        assert_eq!(
            rendered["safetySettings"][0]["category"],
            serde_json::json!("HARM_CATEGORY_HATE_SPEECH")
        );
        assert_eq!(
            rendered["safetySettings"][0]["threshold"],
            serde_json::json!("BLOCK_NONE")
        );
        assert_eq!(
            rendered["toolConfig"]["functionCallingConfig"]["mode"],
            serde_json::json!("AUTO")
        );
    }

    #[test]
    fn render_request_merges_passthrough_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert("temperature".to_owned(), raw("0.5"));
        overrides.insert("custom_field".to_owned(), raw(r#""value""#));
        let ir = RequestIR {
            model: Cow::Borrowed("gemini-1.5-pro"),
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("hi")),
            }],
            temperature: Some(1.0),
            passthrough_overrides: Some(overrides),
            ..RequestIR::default()
        };

        let mut buf = BytesMut::new();
        Gemini.render_request(&ir, &mut buf).unwrap();
        let rendered: serde_json::Value = serde_json::from_slice(&buf).unwrap();

        assert_eq!(
            rendered["generationConfig"]["temperature"],
            serde_json::json!(1.0)
        );
        assert_eq!(rendered["temperature"], serde_json::json!(0.5));
        assert_eq!(rendered["custom_field"], serde_json::json!("value"));
    }

    #[test]
    fn parse_response_without_cached_field() {
        let body = br#"{"candidates":[],"modelVersion":"m",
            "usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":1,"totalTokenCount":3}}"#;
        let codec = Gemini;
        let usage = codec.parse_response(body).unwrap().usage.unwrap();
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cached_tokens, None);
    }

    #[test]
    fn parse_response_returns_protocol_decode_error_on_garbage() {
        let body = b"not json";
        let codec = Gemini;
        let err = codec.parse_response(body).expect_err("must fail");
        assert_eq!(err.status, 502);
        assert_eq!(
            err.code.as_ref(),
            gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR
        );
    }

    #[test]
    fn parse_response_extracts_operation_id_when_present() {
        let body = br#"{
            "name":"operations/foo-bar-12345",
            "candidates":[]
        }"#;
        let codec = Gemini;
        let ir = codec.parse_response(body).unwrap();
        assert_eq!(ir.id.as_ref(), "operations/foo-bar-12345");
    }

    #[test]
    fn parse_response_normal_chat_id_unchanged() {
        let body = br#"{"candidates":[],"modelVersion":"m",
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}}"#;
        let codec = Gemini;
        let ir = codec.parse_response(body).unwrap();
        assert_eq!(ir.id.as_ref(), "");
    }

    #[test]
    fn render_response_with_function_call_round_trip() {
        let args = RawValue::from_string(r#"{"city":"SF"}"#.to_owned()).expect("valid JSON");
        let ir = ResponseIR {
            id: Cow::Borrowed(""),
            model: Cow::Borrowed("gemini-1.5-pro"),
            messages: vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Borrowed("")),
                tool_calls: Some(vec![ToolCallIR {
                    id: Cow::Borrowed(""),
                    name: Cow::Borrowed("get_weather"),
                    arguments_json: Cow::Owned(args),
                }]),
            }],
            finish_reason: Some(FinishReasonIR::ToolCalls),
            usage: Some(UsageIR {
                input_tokens: 7,
                output_tokens: 2,
                cached_tokens: None,
                reasoning_tokens: None,
                ..Default::default()
            }),
            ..ResponseIR::default()
        };

        let mut buf = BytesMut::new();
        Gemini.render_response(&ir, &mut buf).unwrap();
        let parsed: ResponseIR = Gemini.parse_response(&buf).unwrap();

        // finish_reason mapping: ToolCalls renders as "STOP" → reads back as Stop
        assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));

        // Assistant message preserves the tool_call entry.
        let tool_calls = gemini_assistant_tool_calls(&parsed).unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name.as_ref(), "get_weather");
        assert!(tool_calls[0].arguments_json.get().contains("SF"));
    }

    #[test]
    fn parse_response_safety_finish_reason_maps_to_content_filter() {
        let body = br#"{"candidates":[{"content":{"parts":[]},"finishReason":"SAFETY"}],
            "usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":0}}"#;
        let codec = Gemini;
        let ir = codec.parse_response(body).unwrap();
        assert!(matches!(
            ir.finish_reason,
            Some(FinishReasonIR::ContentFilter)
        ));
    }

    /// Audit P0 #13 §5: Gemini has no streaming refusal wire shape, so a
    /// `RefusalDelta` is dropped — but gracefully and without emitting bytes
    /// (the drop is now also surfaced via `observe()` for parity with the
    /// Start / ThinkingDelta markers). Locks the no-wire-output behaviour.
    #[test]
    fn render_stream_chunk_refusal_delta_emits_nothing() {
        let event = StreamEventIR::RefusalDelta {
            text: Cow::Borrowed("I won't do that"),
        };
        let mut buf = BytesMut::new();
        Gemini.render_stream_chunk(&event, &mut buf).unwrap();
        assert!(
            buf.is_empty(),
            "gemini has no refusal wire shape; nothing must be emitted"
        );
    }

    #[test]
    fn parse_stream_chunk_emits_function_call_events() {
        let codec = Gemini;
        let mut state: Box<dyn StreamState> = codec.new_stream_state();

        let chunk = br#"data: {"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"get_weather","args":{"city":"SF"}}}]},"finishReason":"STOP"}]}

"#;
        let events = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();

        // Expected sequence: ToolCallStart, ToolCallDelta, End.
        assert!(
            events.len() >= 3,
            "expected ToolCallStart + ToolCallDelta + End, got {events:?}"
        );
        assert_gemini_tool_call_start_first(&events).unwrap();
        assert_gemini_tool_call_delta_second(&events).unwrap();
        assert_gemini_end_last(&events).unwrap();
    }

    #[test]
    fn test_gemini_ir_invariant_assistant_role_returns_err() {
        let ir = ResponseIR {
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("not assistant")),
            }],
            ..ResponseIR::default()
        };
        let mut buf = BytesMut::new();

        assert!(Gemini.render_response(&ir, &mut buf).is_err());
    }

    #[test]
    fn test_gemini_ir_invariant_tool_call_start_first_returns_err() {
        let events = vec![StreamEventIR::Delta {
            text: Some(Cow::Borrowed("hi")),
            tool_call: None,
            thinking: None,
        }];

        assert!(assert_gemini_tool_call_start_first(&events).is_err());
    }

    #[test]
    fn test_gemini_ir_invariant_tool_call_delta_second_returns_err() {
        let events = vec![
            StreamEventIR::ToolCallStart {
                index: 0,
                id: Cow::Borrowed(""),
                name: Cow::Borrowed("get_weather"),
            },
            StreamEventIR::End {
                finish_reason: FinishReasonIR::Stop,
            },
        ];

        assert!(assert_gemini_tool_call_delta_second(&events).is_err());
    }

    #[test]
    fn test_gemini_ir_invariant_end_last_returns_err() {
        let events = vec![
            StreamEventIR::ToolCallStart {
                index: 0,
                id: Cow::Borrowed(""),
                name: Cow::Borrowed("get_weather"),
            },
            StreamEventIR::ToolCallDelta {
                index: 0,
                arguments_chunk: Cow::Borrowed(r#"{"city":"SF"}"#),
            },
        ];

        assert!(assert_gemini_end_last(&events).is_err());
    }

    fn gemini_assistant_tool_calls<'parsed, 'ir>(
        parsed: &'parsed ResponseIR<'ir>,
    ) -> Result<&'parsed Vec<ToolCallIR<'ir>>, ErrorIR<'static>> {
        match parsed.messages.first() {
            Some(MessageIR::Assistant { tool_calls, .. }) => tool_calls
                .as_ref()
                .ok_or_else(|| internal_err("expected Assistant")),
            _ => Err(internal_err("expected Assistant")),
        }
    }

    fn assert_gemini_tool_call_start_first(
        events: &[StreamEventIR<'_>],
    ) -> Result<(), ErrorIR<'static>> {
        match events.first() {
            Some(StreamEventIR::ToolCallStart { name, .. }) => {
                assert_eq!(name.as_ref(), "get_weather");
                Ok(())
            }
            _ => Err(internal_err(
                "expected ToolCallStart first, got other event",
            )),
        }
    }

    fn assert_gemini_tool_call_delta_second(
        events: &[StreamEventIR<'_>],
    ) -> Result<(), ErrorIR<'static>> {
        match events.get(1) {
            Some(StreamEventIR::ToolCallDelta {
                arguments_chunk, ..
            }) => {
                assert!(
                    arguments_chunk.contains("SF"),
                    "args chunk should carry payload: {arguments_chunk}"
                );
                Ok(())
            }
            _ => Err(internal_err(
                "expected ToolCallDelta second, got other event",
            )),
        }
    }

    fn assert_gemini_end_last(events: &[StreamEventIR<'_>]) -> Result<(), ErrorIR<'static>> {
        match events.last() {
            Some(StreamEventIR::End { .. }) => Ok(()),
            _ => Err(internal_err("expected End last, got other event")),
        }
    }
}
