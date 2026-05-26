//! Hand-written `From` / `TryFrom` impls bridging OpenAI wire structs and
//! `gw_core::ir::*`. See `gw-codec-macros/src/lib.rs` for why these are not
//! derived for v1.

use super::wire::{
    OpenAiAnnotation, OpenAiAudioConfig, OpenAiAudioResponse, OpenAiChoice,
    OpenAiCompletionTokensDetails, OpenAiContentPart, OpenAiFileRef, OpenAiFunctionCall,
    OpenAiImageUrl, OpenAiInputAudio, OpenAiJsonSchema, OpenAiMessage, OpenAiPromptTokensDetails,
    OpenAiRequest, OpenAiResponse, OpenAiResponseFormat, OpenAiTool, OpenAiToolCall,
    OpenAiToolFunction, OpenAiUrlCitation, OpenAiUsage, OpenAiUserContent,
};
use crate::common::{content_to_text, internal_err};
use gw_core::error::ErrorIR;
use gw_core::ir::{
    AnnotationIR, AudioConfig, ContentIR, ContentPart, FinishReasonIR, ImageSource, MediaSource,
    MessageIR, MetadataIR, ReasoningConfig, RequestIR, ResponseFormatIR, ResponseIR, ToolCallIR,
    ToolChoiceIR, ToolIR, UsageIR,
};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;

impl<'a> From<OpenAiRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: OpenAiRequest<'a>) -> Self {
        let messages = src
            .messages
            .into_iter()
            .map(openai_msg_to_ir)
            .collect::<Vec<_>>();
        let extensions = if src.extra.is_empty() {
            None
        } else {
            let mut out: std::collections::HashMap<Cow<'a, str>, Box<RawValue>> =
                std::collections::HashMap::with_capacity(src.extra.len());
            for (k, v) in src.extra {
                out.insert(k, v);
            }
            Some(out)
        };
        let tools = src.tools.map(|ts| {
            ts.into_iter()
                .filter_map(|t| {
                    let func = t.function?;
                    let params = func
                        .parameters
                        .map(|p| Cow::Owned(p) as Cow<'a, RawValue>)
                        .unwrap_or_else(|| {
                            Cow::Owned(
                                RawValue::from_string("{}".to_owned()).expect("'{}' is valid JSON"),
                            )
                        });
                    Some(ToolIR {
                        name: func.name,
                        description: func.description,
                        parameters_jsonschema: params,
                    })
                })
                .collect()
        });
        let tool_choice = src.tool_choice.as_deref().and_then(parse_tool_choice);
        let response_format = src.response_format.map(openai_response_format_to_ir);
        let audio_config = if src.audio.is_some() || src.modalities.is_some() {
            let (voice, format) = match src.audio {
                Some(ac) => (Some(ac.voice), Some(ac.format)),
                None => (None, None),
            };
            Some(AudioConfig {
                voice,
                format,
                modalities: src.modalities,
            })
        } else {
            None
        };
        let metadata = MetadataIR {
            openai_logit_bias: src.logit_bias.map(|m| {
                m.into_iter()
                    .map(|(k, v)| (Cow::Owned(k), v))
                    .collect::<std::collections::HashMap<Cow<'a, str>, f32>>()
            }),
            ..MetadataIR::default()
        };
        let reasoning_config = src.reasoning_effort.map(|effort| ReasoningConfig {
            effort: Some(effort),
            budget_tokens: None,
        });
        RequestIR {
            model: src.model,
            messages,
            stream: src.stream,
            max_tokens: src.max_tokens,
            temperature: src.temperature,
            top_p: src.top_p,
            stop: src.stop,
            tools,
            tool_choice,
            response_format,
            metadata,
            audio_config,
            extensions,
            reasoning_config,
            service_tier: src.service_tier,
            web_search: src.web_search_options,
            prediction: src.prediction,
            ..RequestIR::default()
        }
    }
}

impl<'a> TryFrom<&RequestIR<'a>> for OpenAiRequest<'a> {
    type Error = ErrorIR<'static>;

    #[inline]
    fn try_from(ir: &RequestIR<'a>) -> Result<Self, Self::Error> {
        let messages = ir.messages.iter().map(ir_msg_to_openai).collect();
        let logit_bias = ir.metadata.openai_logit_bias.as_ref().map(|m| {
            m.iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect::<std::collections::HashMap<String, f32>>()
        });
        let extra = ir
            .extensions
            .as_ref()
            .map(|ext| {
                ext.iter()
                    .map(|(k, v)| (Cow::Owned(k.to_string()), v.clone()))
                    .collect::<std::collections::HashMap<Cow<'a, str>, Box<RawValue>>>()
            })
            .unwrap_or_default();
        let tools = ir.tools.as_ref().map(|ts| {
            ts.iter()
                .map(|t| OpenAiTool {
                    ty: Cow::Borrowed("function"),
                    function: Some(OpenAiToolFunction {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: Some(
                            RawValue::from_string(t.parameters_jsonschema.get().to_owned())
                                .expect("ToolIR.parameters_jsonschema must hold valid JSON"),
                        ),
                    }),
                    extra: HashMap::new(),
                })
                .collect()
        });
        let tool_choice = ir.tool_choice.as_ref().map(render_tool_choice);
        let response_format = ir
            .response_format
            .as_ref()
            .map(ir_response_format_to_openai);
        let reasoning_effort = ir
            .reasoning_config
            .as_ref()
            .and_then(|rc| rc.effort.clone());
        let (modalities, audio) = match &ir.audio_config {
            Some(ac) => {
                let mods = ac.modalities.clone().or_else(|| {
                    if ac.voice.is_some() || ac.format.is_some() {
                        Some(vec![Cow::Borrowed("text"), Cow::Borrowed("audio")])
                    } else {
                        None
                    }
                });
                let wire_audio = match (ac.voice.as_ref(), ac.format.as_ref()) {
                    (Some(v), Some(f)) => Some(OpenAiAudioConfig {
                        voice: v.clone(),
                        format: f.clone(),
                    }),
                    _ => None,
                };
                (mods, wire_audio)
            }
            None => (None, None),
        };
        Ok(OpenAiRequest {
            model: ir.model.clone(),
            messages,
            stream: ir.stream,
            max_tokens: ir.max_tokens,
            temperature: ir.temperature,
            top_p: ir.top_p,
            stop: ir.stop.clone(),
            logit_bias,
            tools,
            tool_choice,
            response_format,
            reasoning_effort,
            service_tier: ir.service_tier.clone(),
            modalities,
            audio,
            web_search_options: ir.web_search.clone(),
            prediction: ir.prediction.clone(),
            extra,
        })
    }
}

#[inline]
fn openai_msg_to_ir(m: OpenAiMessage<'_>) -> MessageIR<'_> {
    match m {
        OpenAiMessage::System { content } => MessageIR::System {
            content: ContentIR::Text(content),
        },
        OpenAiMessage::Developer { content } => MessageIR::Developer {
            content: ContentIR::Text(content),
        },
        OpenAiMessage::User { content } => MessageIR::User {
            content: openai_user_content_to_ir(content),
        },
        OpenAiMessage::Assistant {
            content,
            tool_calls,
            refusal: _,
            annotations: _,
            audio: _,
        } => MessageIR::Assistant {
            content: ContentIR::Text(content.unwrap_or(Cow::Borrowed(""))),
            tool_calls: tool_calls.map(|tcs| {
                tcs.into_iter()
                    .map(|tc| {
                        let args_owned = tc.function.arguments.into_owned();
                        let raw = match RawValue::from_string(args_owned.clone()) {
                            Ok(rv) => rv,
                            Err(_) => {
                                tracing::warn!(
                                    "openai tool_call.arguments not valid JSON; passing through as string literal"
                                );
                                let escaped = serde_json::to_string(&args_owned)
                                    .expect("serde_json::to_string of &String is infallible");
                                RawValue::from_string(escaped).expect(
                                    "serialized JSON string literal must be valid JSON",
                                )
                            }
                        };
                        ToolCallIR {
                            id: tc.id,
                            name: tc.function.name,
                            arguments_json: Cow::Owned(raw),
                        }
                    })
                    .collect()
            }),
        },
        OpenAiMessage::Tool {
            tool_call_id,
            content,
        } => MessageIR::Tool {
            tool_call_id,
            content: ContentIR::Text(content),
        },
    }
}

#[inline]
fn openai_user_content_to_ir(c: OpenAiUserContent<'_>) -> ContentIR<'_> {
    match c {
        OpenAiUserContent::Text(s) => ContentIR::Text(s),
        OpenAiUserContent::Parts(parts) => {
            let ir_parts = parts
                .into_iter()
                .map(|p| match p {
                    OpenAiContentPart::Text { text } => ContentPart::Text(text),
                    OpenAiContentPart::ImageUrl { image_url } => ContentPart::Image {
                        source: ImageSource::Url(image_url.url),
                    },
                    OpenAiContentPart::InputAudio { input_audio } => ContentPart::Audio {
                        source: MediaSource::B64 {
                            media_type: Cow::Owned(format!("audio/{}", input_audio.format)),
                            data: input_audio.data,
                        },
                    },
                    OpenAiContentPart::File { file } => ContentPart::File {
                        source: if let Some(data) = file.file_data {
                            MediaSource::B64 {
                                media_type: file
                                    .mime_type
                                    .unwrap_or(Cow::Borrowed("application/octet-stream")),
                                data,
                            }
                        } else if let Some(id) = file.file_id {
                            MediaSource::Url(id)
                        } else {
                            MediaSource::B64 {
                                media_type: Cow::Borrowed("application/octet-stream"),
                                data: Cow::Borrowed(""),
                            }
                        },
                    },
                })
                .collect();
            ContentIR::Parts(ir_parts)
        }
    }
}

#[inline]
fn ir_msg_to_openai<'a>(m: &MessageIR<'a>) -> OpenAiMessage<'a> {
    match m {
        MessageIR::System { content } => OpenAiMessage::System {
            content: content_to_text(content),
        },
        MessageIR::Developer { content } => OpenAiMessage::Developer {
            content: content_to_text(content),
        },
        MessageIR::User { content } => OpenAiMessage::User {
            content: ir_content_to_openai_user(content),
        },
        MessageIR::Assistant {
            content,
            tool_calls,
        } => OpenAiMessage::Assistant {
            content: Some(content_to_text(content)),
            tool_calls: tool_calls.as_ref().map(|tcs| {
                tcs.iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: tc.name.clone(),
                            arguments: Cow::Owned(tc.arguments_json.get().to_owned()),
                        },
                    })
                    .collect()
            }),
            refusal: None,
            annotations: None,
            audio: None,
        },
        MessageIR::Tool {
            tool_call_id,
            content,
        } => OpenAiMessage::Tool {
            tool_call_id: tool_call_id.clone(),
            content: content_to_text(content),
        },
    }
}

#[inline]
fn ir_content_to_openai_user<'a>(c: &ContentIR<'a>) -> OpenAiUserContent<'a> {
    match c {
        ContentIR::Text(t) => OpenAiUserContent::Text(t.clone()),
        ContentIR::Parts(parts) => OpenAiUserContent::Parts(
            parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text(t) => OpenAiContentPart::Text { text: t.clone() },
                    ContentPart::Image {
                        source: ImageSource::Url(url),
                    } => OpenAiContentPart::ImageUrl {
                        image_url: OpenAiImageUrl {
                            url: url.clone(),
                            detail: None,
                        },
                    },
                    ContentPart::Image {
                        source: ImageSource::B64 { media_type, data },
                    } => OpenAiContentPart::ImageUrl {
                        image_url: OpenAiImageUrl {
                            url: Cow::Owned(format!("data:{};base64,{}", media_type, data)),
                            detail: None,
                        },
                    },
                    ContentPart::Audio {
                        source: MediaSource::B64 { media_type, data },
                    } => OpenAiContentPart::InputAudio {
                        input_audio: OpenAiInputAudio {
                            data: data.clone(),
                            format: Cow::Owned(
                                media_type
                                    .strip_prefix("audio/")
                                    .unwrap_or(media_type.as_ref())
                                    .to_owned(),
                            ),
                        },
                    },
                    ContentPart::Audio {
                        source: MediaSource::Url(url),
                    } => OpenAiContentPart::InputAudio {
                        input_audio: OpenAiInputAudio {
                            data: url.clone(),
                            format: Cow::Borrowed("wav"),
                        },
                    },
                    ContentPart::File {
                        source: MediaSource::B64 { media_type, data },
                    } => OpenAiContentPart::File {
                        file: OpenAiFileRef {
                            file_data: Some(data.clone()),
                            file_id: None,
                            mime_type: Some(media_type.clone()),
                        },
                    },
                    ContentPart::File {
                        source: MediaSource::Url(id),
                    } => OpenAiContentPart::File {
                        file: OpenAiFileRef {
                            file_data: None,
                            file_id: Some(id.clone()),
                            mime_type: None,
                        },
                    },
                })
                .collect(),
        ),
    }
}

fn openai_response_format_to_ir(src: OpenAiResponseFormat<'_>) -> ResponseFormatIR {
    match src {
        OpenAiResponseFormat::Text => ResponseFormatIR::Text,
        OpenAiResponseFormat::JsonObject => ResponseFormatIR::JsonObject,
        OpenAiResponseFormat::JsonSchema { json_schema } => {
            let raw = render_openai_json_schema(&json_schema);
            ResponseFormatIR::JsonSchema(raw)
        }
    }
}

fn ir_response_format_to_openai(src: &ResponseFormatIR) -> OpenAiResponseFormat<'static> {
    match src {
        ResponseFormatIR::Text => OpenAiResponseFormat::Text,
        ResponseFormatIR::JsonObject => OpenAiResponseFormat::JsonObject,
        ResponseFormatIR::JsonSchema(raw) => OpenAiResponseFormat::JsonSchema {
            json_schema: parse_openai_json_schema(raw),
        },
    }
}

fn render_openai_json_schema(src: &OpenAiJsonSchema<'_>) -> Box<RawValue> {
    let mut map = serde_json::Map::new();
    if let Some(name) = &src.name {
        map.insert(
            "name".to_owned(),
            serde_json::Value::String(name.as_ref().to_owned()),
        );
    }
    if let Some(description) = &src.description {
        map.insert(
            "description".to_owned(),
            serde_json::Value::String(description.as_ref().to_owned()),
        );
    }
    map.insert("schema".to_owned(), raw_to_value(src.schema.as_ref()));
    if let Some(strict) = src.strict {
        map.insert("strict".to_owned(), serde_json::Value::Bool(strict));
    }
    value_to_raw(serde_json::Value::Object(map))
}

fn parse_openai_json_schema(raw: &RawValue) -> OpenAiJsonSchema<'static> {
    let value = raw_to_value(raw);
    let obj = value.as_object();
    let name = obj
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| Cow::Owned(s.to_owned()));
    let description = obj
        .and_then(|m| m.get("description"))
        .and_then(|v| v.as_str())
        .map(|s| Cow::Owned(s.to_owned()));
    let schema = obj
        .and_then(|m| m.get("schema"))
        .cloned()
        .unwrap_or_else(|| value.clone());
    let strict = obj.and_then(|m| m.get("strict")).and_then(|v| v.as_bool());
    OpenAiJsonSchema {
        name,
        description,
        schema: value_to_raw(schema),
        strict,
    }
}

fn value_to_raw(v: serde_json::Value) -> Box<RawValue> {
    let s = serde_json::to_string(&v).expect("serde_json::Value serializes");
    RawValue::from_string(s).expect("serialized JSON is valid RawValue")
}

fn raw_to_value(rv: &RawValue) -> serde_json::Value {
    serde_json::from_str(rv.get()).unwrap_or(serde_json::Value::Null)
}

/// Parse OpenAI's `tool_choice` JSON shape into `ToolChoiceIR`. Returns `None`
/// on unrecognised payloads (lossless behaviour: the original `extra`-free
/// request path simply omits a non-trivially-mappable choice).
fn parse_tool_choice(raw: &RawValue) -> Option<ToolChoiceIR<'static>> {
    let raw = raw.get();
    // First try a bare string ("auto" / "none" / "required").
    if let Ok(s) = serde_json::from_str::<String>(raw) {
        return match s.as_str() {
            "auto" => Some(ToolChoiceIR::Auto),
            "none" => Some(ToolChoiceIR::None),
            "required" => Some(ToolChoiceIR::Required),
            _ => None,
        };
    }
    // Object form: {"type":"function","function":{"name":"X"}}
    #[derive(serde::Deserialize)]
    struct FnChoice {
        function: FnName,
    }
    #[derive(serde::Deserialize)]
    struct FnName {
        name: String,
    }
    if let Ok(obj) = serde_json::from_str::<FnChoice>(raw) {
        return Some(ToolChoiceIR::Function {
            name: Cow::Owned(obj.function.name),
        });
    }
    None
}

/// Render `ToolChoiceIR` back to OpenAI's `tool_choice` JSON shape.
fn render_tool_choice(tc: &ToolChoiceIR<'_>) -> Box<RawValue> {
    let s = match tc {
        ToolChoiceIR::Auto => "\"auto\"".to_owned(),
        ToolChoiceIR::None => "\"none\"".to_owned(),
        ToolChoiceIR::Required => "\"required\"".to_owned(),
        ToolChoiceIR::Function { name } => {
            // serialize the function name through serde_json to be JSON-safe.
            let name_lit = serde_json::to_string(name.as_ref())
                .expect("serde_json::to_string of &str is infallible");
            format!("{{\"type\":\"function\",\"function\":{{\"name\":{name_lit}}}}}")
        }
    };
    RawValue::from_string(s).expect("rendered tool_choice is valid JSON")
}

// ── Response direction ───────────────────────────────────────────────────

impl<'a> TryFrom<&ResponseIR<'a>> for OpenAiResponse<'a> {
    type Error = ErrorIR<'static>;

    fn try_from(ir: &ResponseIR<'a>) -> Result<Self, Self::Error> {
        // Walk messages in reverse to find the last Assistant — multi-assistant
        // responses are vanishingly rare in practice (multi-turn comes back as
        // a single Assistant message at the IR layer) but if one appears, last
        // wins per OpenAI semantics.
        let last_assistant = ir
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, MessageIR::Assistant { .. }));

        // Extract content + tool_calls from the assistant message, or fall back
        // to empty content + IR's `tool_use_blocks` if no assistant present.
        let (content_text, ir_tool_calls): (Cow<'a, str>, Option<Vec<&ToolCallIR<'a>>>) =
            match last_assistant {
                Some(MessageIR::Assistant {
                    content: ContentIR::Text(content),
                    tool_calls,
                }) => (
                    content.clone(),
                    tool_calls.as_ref().map(|v| v.iter().collect::<Vec<_>>()),
                ),
                Some(MessageIR::Assistant { .. }) => {
                    return Err(internal_err(
                        "expected ContentIR::Text, got non-text content",
                    ));
                }
                Some(
                    MessageIR::System { .. } | MessageIR::User { .. } | MessageIR::Tool { .. },
                ) => {
                    return Err(internal_err(
                        "expected Assistant, got non-assistant message",
                    ))
                }
                Some(MessageIR::Developer { .. }) => {
                    return Err(internal_err(
                        "expected Assistant, got non-assistant message",
                    ))
                }
                None if ir.messages.is_empty() => (Cow::Borrowed(""), None),
                None => {
                    return Err(internal_err(
                        "expected Assistant, got non-assistant message",
                    ))
                }
            };

        // If the message-level tool_calls is empty but the IR carries
        // tool_use_blocks (Anthropic-shape source), synthesise OpenAI tool_calls
        // from them.
        let wire_tool_calls: Option<Vec<OpenAiToolCall<'a>>> = if let Some(tcs) = ir_tool_calls {
            Some(
                tcs.into_iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: tc.name.clone(),
                            arguments: Cow::Owned(tc.arguments_json.get().to_owned()),
                        },
                    })
                    .collect(),
            )
        } else {
            ir.tool_use_blocks.as_ref().map(|blocks| {
                blocks
                    .iter()
                    .map(|b| OpenAiToolCall {
                        id: b.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: b.name.clone(),
                            arguments: Cow::Owned(b.input.get().to_owned()),
                        },
                    })
                    .collect()
            })
        };

        if matches!(ir.finish_reason, Some(FinishReasonIR::ToolCalls)) && wire_tool_calls.is_none()
        {
            return Err(internal_err("expected tool_calls, got none"));
        }

        let wire_audio_response: Option<OpenAiAudioResponse<'a>> = ir
            .metadata
            .extensions
            .as_ref()
            .and_then(|ext| ext.get("openai_audio"))
            .and_then(|v| {
                let id = v.get("id")?.as_str()?;
                let data = v.get("data")?.as_str()?;
                let expires_at = v.get("expires_at")?.as_u64()?;
                let transcript = v.get("transcript")?.as_str()?;
                Some(OpenAiAudioResponse {
                    id: Cow::Owned(id.to_owned()),
                    data: Cow::Owned(data.to_owned()),
                    expires_at,
                    transcript: Cow::Owned(transcript.to_owned()),
                })
            });

        let message = OpenAiMessage::Assistant {
            content: if content_text.is_empty() && wire_tool_calls.is_some() {
                None
            } else {
                Some(content_text)
            },
            tool_calls: wire_tool_calls,
            refusal: ir.refusal.clone(),
            annotations: ir.annotations.as_ref().map(|anns| {
                anns.iter()
                    .map(|a| {
                        let url_citation =
                            match (a.url.as_ref(), a.title.as_ref(), a.start_index, a.end_index) {
                                (Some(url), Some(title), Some(si), Some(ei)) => {
                                    Some(OpenAiUrlCitation {
                                        start_index: si,
                                        end_index: ei,
                                        title: title.clone(),
                                        url: url.clone(),
                                    })
                                }
                                _ => None,
                            };
                        OpenAiAnnotation {
                            ty: a.kind.clone(),
                            url_citation,
                        }
                    })
                    .collect()
            }),
            audio: wire_audio_response,
        };

        let finish_reason = ir.finish_reason.as_ref().map(|fr| match fr {
            FinishReasonIR::Stop => Cow::Borrowed("stop"),
            FinishReasonIR::Length => Cow::Borrowed("length"),
            FinishReasonIR::ToolCalls => Cow::Borrowed("tool_calls"),
            FinishReasonIR::ContentFilter => Cow::Borrowed("content_filter"),
            FinishReasonIR::Refusal => Cow::Borrowed("stop"),
            // cross_render::map_finish_reason degrades Other → Stop before
            // render_response is reached; defensively map any survivor.
            FinishReasonIR::Other(_) => Cow::Borrowed("stop"),
        });

        let choice = OpenAiChoice {
            index: 0,
            message,
            finish_reason,
        };

        let usage = ir.usage.as_ref().map(|u| OpenAiUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
            prompt_tokens_details: if u.cached_tokens.is_some() || u.audio_tokens_in.is_some() {
                Some(OpenAiPromptTokensDetails {
                    cached_tokens: u.cached_tokens.unwrap_or(0),
                    audio_tokens: u.audio_tokens_in,
                })
            } else {
                None
            },
            completion_tokens_details: if u.reasoning_tokens.is_some()
                || u.audio_tokens_out.is_some()
                || u.accepted_prediction_tokens.is_some()
                || u.rejected_prediction_tokens.is_some()
            {
                Some(OpenAiCompletionTokensDetails {
                    reasoning_tokens: u.reasoning_tokens.unwrap_or(0),
                    audio_tokens: u.audio_tokens_out,
                    accepted_prediction_tokens: u.accepted_prediction_tokens,
                    rejected_prediction_tokens: u.rejected_prediction_tokens,
                })
            } else {
                None
            },
        });

        Ok(OpenAiResponse {
            id: ir.id.clone(),
            object: Cow::Borrowed("chat.completion"),
            created: 0,
            model: ir.model.clone(),
            choices: vec![choice],
            usage,
            service_tier: ir.service_tier.clone(),
            extra: std::collections::HashMap::new(),
        })
    }
}

impl<'a> From<OpenAiResponse<'a>> for ResponseIR<'a> {
    fn from(w: OpenAiResponse<'a>) -> Self {
        let first = w.choices.into_iter().next();
        let (messages, finish_reason, wire_refusal, wire_annotations, wire_audio) = match first {
            Some(choice) => {
                let fr = choice.finish_reason.as_deref().map(|s| match s {
                    "stop" => FinishReasonIR::Stop,
                    "length" => FinishReasonIR::Length,
                    "tool_calls" => FinishReasonIR::ToolCalls,
                    "content_filter" => FinishReasonIR::ContentFilter,
                    other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
                });
                let (refusal, annotations, audio) = match &choice.message {
                    OpenAiMessage::Assistant {
                        refusal,
                        annotations,
                        audio,
                        ..
                    } => (refusal.clone(), annotations.clone(), audio.clone()),
                    _ => (None, None, None),
                };
                let msg = openai_msg_to_ir(choice.message);
                (vec![msg], fr, refusal, annotations, audio)
            }
            None => (Vec::new(), None, None, None, None),
        };

        let usage = w.usage.map(|u| UsageIR {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_tokens: u.prompt_tokens_details.map(|d| d.cached_tokens),
            audio_tokens_in: u.prompt_tokens_details.and_then(|d| d.audio_tokens),
            reasoning_tokens: u.completion_tokens_details.map(|d| d.reasoning_tokens),
            audio_tokens_out: u.completion_tokens_details.and_then(|d| d.audio_tokens),
            accepted_prediction_tokens: u
                .completion_tokens_details
                .and_then(|d| d.accepted_prediction_tokens),
            rejected_prediction_tokens: u
                .completion_tokens_details
                .and_then(|d| d.rejected_prediction_tokens),
            ..Default::default()
        });

        let annotations_ir = wire_annotations.map(|anns| {
            anns.into_iter()
                .map(|a| {
                    let (url, title, start_index, end_index) = match a.url_citation {
                        Some(uc) => (
                            Some(uc.url),
                            Some(uc.title),
                            Some(uc.start_index),
                            Some(uc.end_index),
                        ),
                        None => (None, None, None, None),
                    };
                    AnnotationIR {
                        kind: a.ty,
                        url,
                        title,
                        start_index,
                        end_index,
                    }
                })
                .collect()
        });

        let metadata_extensions = wire_audio.map(|aud| {
            let mut map = serde_json::Map::new();
            map.insert(
                "openai_audio".to_owned(),
                serde_json::json!({
                    "id": aud.id.as_ref(),
                    "data": aud.data.as_ref(),
                    "expires_at": aud.expires_at,
                    "transcript": aud.transcript.as_ref(),
                }),
            );
            map
        });

        ResponseIR {
            id: w.id,
            model: w.model,
            messages,
            finish_reason,
            usage,
            metadata: MetadataIR {
                extensions: metadata_extensions,
                ..MetadataIR::default()
            },
            thinking: None,
            reasoning: None,
            tool_use_blocks: None,
            refusal: wire_refusal,
            annotations: annotations_ir,
            service_tier: w.service_tier,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tool_choice_tests {
    //! Round-trip coverage for `parse_tool_choice` / `render_tool_choice`.
    //!
    //! T4-A introduced these as the single hand-rolled bit of the request-side
    //! `From<OpenAiRequest> -> RequestIR` mapping (everything else is mechanical
    //! Cow/RawValue plumbing). The helpers accept both the string-shorthand and
    //! the `{"type":"function","function":{"name":"..."}}` object form; these
    //! tests guard against silent regressions in either branch.
    use super::*;
    use serde_json::value::to_raw_value;

    fn raw(v: serde_json::Value) -> Box<RawValue> {
        to_raw_value(&v).expect("test JSON is valid")
    }

    #[test]
    fn tool_choice_auto_round_trip() {
        let wire_in = raw(serde_json::json!("auto"));
        let ir = parse_tool_choice(&wire_in).expect("parses Auto");
        assert!(matches!(ir, ToolChoiceIR::Auto));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"auto\"");
    }

    #[test]
    fn tool_choice_none_round_trip() {
        let wire_in = raw(serde_json::json!("none"));
        let ir = parse_tool_choice(&wire_in).expect("parses None");
        assert!(matches!(ir, ToolChoiceIR::None));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"none\"");
    }

    #[test]
    fn tool_choice_required_round_trip() {
        let wire_in = raw(serde_json::json!("required"));
        let ir = parse_tool_choice(&wire_in).expect("parses Required");
        assert!(matches!(ir, ToolChoiceIR::Required));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"required\"");
    }

    #[test]
    fn tool_choice_function_object_round_trip() {
        let wire_in = raw(serde_json::json!({
            "type": "function",
            "function": { "name": "get_weather" }
        }));
        let ir = parse_tool_choice(&wire_in).expect("parses Function");
        assert_tool_choice_function(&ir).unwrap();
        let wire_out = render_tool_choice(&ir);
        let s = wire_out.get();
        assert!(s.contains("\"type\":\"function\""), "rendered: {s}");
        assert!(s.contains("\"name\":\"get_weather\""), "rendered: {s}");
    }

    #[test]
    fn test_openai_tool_choice_ir_invariant_function_returns_err() {
        let err = assert_tool_choice_function(&ToolChoiceIR::Auto).unwrap_err();

        assert_eq!(err.status, 500);
    }

    fn assert_tool_choice_function(tc: &ToolChoiceIR<'_>) -> Result<(), ErrorIR<'static>> {
        match tc {
            ToolChoiceIR::Function { name } => {
                assert_eq!(name.as_ref(), "get_weather");
                Ok(())
            }
            ToolChoiceIR::Auto => Err(internal_err("expected ToolChoiceIR::Function, got Auto")),
            ToolChoiceIR::None => Err(internal_err("expected ToolChoiceIR::Function, got None")),
            ToolChoiceIR::Required => Err(internal_err(
                "expected ToolChoiceIR::Function, got Required",
            )),
        }
    }
}

#[cfg(test)]
mod user_multimodal_tests {
    use super::*;
    use gw_core::ir::{ContentPart, ImageSource, MediaSource};

    #[test]
    fn developer_role_round_trip_stays_developer() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"developer","content":"Follow repo policy"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        match &ir.messages[0] {
            MessageIR::Developer {
                content: ContentIR::Text(t),
            } => assert_eq!(t.as_ref(), "Follow repo policy"),
            other => panic!("expected Developer/Text, got {other:?}"),
        }

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let out = serde_json::to_value(&rendered).unwrap();
        assert_eq!(out["messages"][0]["role"], "developer");
        assert_eq!(out["messages"][0]["content"], "Follow repo policy");
    }

    #[test]
    fn user_string_content_backward_compat() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello world"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        assert_eq!(ir.messages.len(), 1);
        match &ir.messages[0] {
            MessageIR::User {
                content: ContentIR::Text(t),
            } => assert_eq!(t.as_ref(), "hello world"),
            other => panic!("expected User/Text, got {other:?}"),
        }
    }

    #[test]
    fn user_multimodal_image_url_parses() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"user","content":[{"type":"text","text":"What is in this image?"},{"type":"image_url","image_url":{"url":"https://example.com/cat.png","detail":"high"}}]}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        match &ir.messages[0] {
            MessageIR::User {
                content: ContentIR::Parts(parts),
            } => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::Text(t) => assert_eq!(t.as_ref(), "What is in this image?"),
                    other => panic!("expected Text, got {other:?}"),
                }
                match &parts[1] {
                    ContentPart::Image {
                        source: ImageSource::Url(url),
                    } => {
                        assert_eq!(url.as_ref(), "https://example.com/cat.png");
                    }
                    other => panic!("expected Image/Url, got {other:?}"),
                }
            }
            other => panic!("expected User/Parts, got {other:?}"),
        }
    }

    #[test]
    fn user_multimodal_round_trip() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"user","content":[{"type":"text","text":"Describe this"},{"type":"image_url","image_url":{"url":"https://example.com/img.png"}}]}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let out = serde_json::to_value(&rendered).unwrap();
        let content = &out["messages"][0]["content"];
        assert!(content.is_array());
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "Describe this");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(arr[1]["image_url"]["url"], "https://example.com/img.png");
    }

    #[test]
    fn user_input_audio_parses() {
        let src = r#"{"model":"gpt-4o-audio-preview","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"base64data==","format":"wav"}}]}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        match &ir.messages[0] {
            MessageIR::User {
                content: ContentIR::Parts(parts),
            } => match &parts[0] {
                ContentPart::Audio {
                    source: MediaSource::B64 { media_type, data },
                } => {
                    assert_eq!(media_type.as_ref(), "audio/wav");
                    assert_eq!(data.as_ref(), "base64data==");
                }
                other => panic!("expected Audio/B64, got {other:?}"),
            },
            other => panic!("expected User/Parts, got {other:?}"),
        }
    }

    #[test]
    fn user_string_content_render_stays_string() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"just text"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let out = serde_json::to_value(&rendered).unwrap();
        assert_eq!(out["messages"][0]["content"], "just text");
    }
}

#[cfg(test)]
mod web_search_prediction_custom_tool_tests {
    use super::*;
    use gw_core::ir::RequestIR;

    #[test]
    fn web_search_options_round_trip() {
        let src = r#"{
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"search the web"}],
            "stream": false,
            "web_search_options": {"search_context_size": "medium", "user_location": {"type": "approximate", "city": "SF"}}
        }"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.web_search_options.is_some());

        let ir = RequestIR::from(req);
        assert!(ir.web_search.is_some());
        let ws_json: serde_json::Value =
            serde_json::from_str(ir.web_search.as_ref().unwrap().get()).unwrap();
        assert_eq!(ws_json["search_context_size"], "medium");

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert!(rendered.web_search_options.is_some());

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["web_search_options"]["search_context_size"], "medium");
        assert_eq!(json["web_search_options"]["user_location"]["city"], "SF");
    }

    #[test]
    fn web_search_options_absent_when_not_set() {
        let src =
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.web_search_options.is_none());
        let ir = RequestIR::from(req);
        assert!(ir.web_search.is_none());
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json.get("web_search_options").is_none());
    }

    #[test]
    fn prediction_round_trip() {
        let src = r#"{
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"predict"}],
            "stream": false,
            "prediction": {"type": "content", "content": "The quick brown fox"}
        }"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.prediction.is_some());

        let ir = RequestIR::from(req);
        assert!(ir.prediction.is_some());
        let pred_json: serde_json::Value =
            serde_json::from_str(ir.prediction.as_ref().unwrap().get()).unwrap();
        assert_eq!(pred_json["type"], "content");
        assert_eq!(pred_json["content"], "The quick brown fox");

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert!(rendered.prediction.is_some());

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["prediction"]["type"], "content");
        assert_eq!(json["prediction"]["content"], "The quick brown fox");
    }

    #[test]
    fn prediction_absent_when_not_set() {
        let src =
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.prediction.is_none());
        let ir = RequestIR::from(req);
        assert!(ir.prediction.is_none());
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json.get("prediction").is_none());
    }

    #[test]
    fn custom_tool_parses_without_panic() {
        let src = r#"{
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"search"}],
            "stream": false,
            "tools": [
                {"type": "web_search_preview", "search_context_size": "medium"},
                {"type": "function", "function": {"name": "get_weather", "parameters": {"type":"object"}}}
            ]
        }"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let tools = req.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 2);

        assert_eq!(tools[0].ty.as_ref(), "web_search_preview");
        assert!(tools[0].function.is_none());
        assert!(tools[0].extra.contains_key("search_context_size"));

        assert_eq!(tools[1].ty.as_ref(), "function");
        assert!(tools[1].function.is_some());
    }

    #[test]
    fn custom_tool_skipped_in_ir_function_tools_preserved() {
        let src = r#"{
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"search"}],
            "stream": false,
            "tools": [
                {"type": "web_search_preview"},
                {"type": "function", "function": {"name": "get_weather", "parameters": {"type":"object"}}}
            ]
        }"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        let ir_tools = ir.tools.as_ref().unwrap();
        assert_eq!(ir_tools.len(), 1);
        assert_eq!(ir_tools[0].name.as_ref(), "get_weather");
    }

    #[test]
    fn custom_tool_wire_round_trip() {
        let src = r#"{
            "type": "web_search_preview",
            "search_context_size": "medium",
            "user_location": {"type": "approximate"}
        }"#;
        let tool: OpenAiTool<'_> = serde_json::from_str(src).unwrap();
        assert_eq!(tool.ty.as_ref(), "web_search_preview");
        assert!(tool.function.is_none());

        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "web_search_preview");
        assert_eq!(json["search_context_size"], "medium");
        assert!(json.get("function").is_none());
    }
}

#[cfg(test)]
mod reasoning_service_tier_tests {
    use super::*;
    use gw_core::ir::ReasoningConfig;

    #[test]
    fn reasoning_effort_request_round_trip() {
        let src = r#"{"model":"o3","messages":[{"role":"user","content":"hi"}],"stream":false,"reasoning_effort":"low"}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert_eq!(req.reasoning_effort.as_deref(), Some("low"));

        let ir = RequestIR::from(req);
        let rc = ir.reasoning_config.as_ref().expect("reasoning_config set");
        assert_eq!(rc.effort.as_deref(), Some("low"));
        assert!(rc.budget_tokens.is_none());

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert_eq!(rendered.reasoning_effort.as_deref(), Some("low"));
        assert!(rendered.service_tier.is_none());

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["reasoning_effort"], "low");
        assert!(json.get("service_tier").is_none());
    }

    #[test]
    fn service_tier_request_round_trip() {
        let src = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false,"service_tier":"flex"}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert_eq!(req.service_tier.as_deref(), Some("flex"));

        let ir = RequestIR::from(req);
        assert_eq!(ir.service_tier.as_deref(), Some("flex"));

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert_eq!(rendered.service_tier.as_deref(), Some("flex"));

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["service_tier"], "flex");
    }

    #[test]
    fn service_tier_response_round_trip() {
        let src = r#"{"id":"chatcmpl-1","object":"chat.completion","created":0,"model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"service_tier":"default"}"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        assert_eq!(resp.service_tier.as_deref(), Some("default"));

        let ir = ResponseIR::from(resp);
        assert_eq!(ir.service_tier.as_deref(), Some("default"));

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        assert_eq!(rendered.service_tier.as_deref(), Some("default"));

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["service_tier"], "default");
    }

    #[test]
    fn absent_reasoning_effort_and_service_tier_omitted_from_json() {
        let src =
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.reasoning_effort.is_none());
        assert!(req.service_tier.is_none());

        let ir = RequestIR::from(req);
        assert!(ir.reasoning_config.is_none());
        assert!(ir.service_tier.is_none());

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("service_tier").is_none());
    }

    #[test]
    fn reasoning_effort_with_service_tier_combined() {
        let src = r#"{"model":"o3","messages":[{"role":"user","content":"think"}],"stream":false,"reasoning_effort":"high","service_tier":"auto"}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        let ir = RequestIR::from(req);
        assert_eq!(
            ir.reasoning_config
                .as_ref()
                .and_then(|r| r.effort.as_deref()),
            Some("high")
        );
        assert_eq!(ir.service_tier.as_deref(), Some("auto"));

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert_eq!(rendered.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(rendered.service_tier.as_deref(), Some("auto"));
    }

    #[test]
    fn ir_with_budget_tokens_renders_only_effort() {
        let ir = RequestIR {
            model: Cow::Borrowed("o3"),
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("hi")),
            }],
            reasoning_config: Some(ReasoningConfig {
                effort: Some(Cow::Borrowed("medium")),
                budget_tokens: Some(1024),
            }),
            ..RequestIR::default()
        };
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert_eq!(rendered.reasoning_effort.as_deref(), Some("medium"));
        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["reasoning_effort"], "medium");
    }
}

#[cfg(test)]
mod audio_tests {
    use super::*;
    use gw_core::ir::{AudioConfig, ContentIR, MessageIR, RequestIR};

    #[test]
    fn audio_config_request_round_trip() {
        let src = r#"{
            "model": "gpt-4o-audio-preview",
            "modalities": ["text", "audio"],
            "audio": { "voice": "alloy", "format": "wav" },
            "messages": [{"role":"user","content":"Tell me a joke"}],
            "stream": false
        }"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert_eq!(req.modalities.as_ref().unwrap().len(), 2);
        assert_eq!(req.audio.as_ref().unwrap().voice.as_ref(), "alloy");
        assert_eq!(req.audio.as_ref().unwrap().format.as_ref(), "wav");

        let ir = RequestIR::from(req);
        let ac = ir.audio_config.as_ref().unwrap();
        assert_eq!(ac.voice.as_deref(), Some("alloy"));
        assert_eq!(ac.format.as_deref(), Some("wav"));
        assert_eq!(ac.modalities.as_ref().unwrap().len(), 2);

        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert_eq!(rendered.modalities.as_ref().unwrap().len(), 2);
        assert_eq!(rendered.audio.as_ref().unwrap().voice.as_ref(), "alloy");
        assert_eq!(rendered.audio.as_ref().unwrap().format.as_ref(), "wav");

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["modalities"], serde_json::json!(["text", "audio"]));
        assert_eq!(json["audio"]["voice"], "alloy");
        assert_eq!(json["audio"]["format"], "wav");
    }

    #[test]
    fn audio_config_absent_when_not_set() {
        let src =
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let req: OpenAiRequest<'_> = serde_json::from_str(src).unwrap();
        assert!(req.modalities.is_none());
        assert!(req.audio.is_none());
        let ir = RequestIR::from(req);
        assert!(ir.audio_config.is_none());
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        assert!(rendered.modalities.is_none());
        assert!(rendered.audio.is_none());
        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json.get("modalities").is_none());
        assert!(json.get("audio").is_none());
    }

    #[test]
    fn ir_audio_config_renders_modalities_when_missing() {
        let ir = RequestIR {
            model: Cow::Borrowed("gpt-4o-audio-preview"),
            messages: vec![MessageIR::User {
                content: ContentIR::Text(Cow::Borrowed("hi")),
            }],
            audio_config: Some(AudioConfig {
                voice: Some(Cow::Borrowed("nova")),
                format: Some(Cow::Borrowed("mp3")),
                modalities: None,
            }),
            ..RequestIR::default()
        };
        let rendered = OpenAiRequest::try_from(&ir).unwrap();
        let mods = rendered.modalities.as_ref().unwrap();
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].as_ref(), "text");
        assert_eq!(mods[1].as_ref(), "audio");
        assert_eq!(rendered.audio.as_ref().unwrap().voice.as_ref(), "nova");
    }

    #[test]
    fn response_audio_parse_and_round_trip() {
        let src = r#"{
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o-audio-preview",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "audio": {
                        "id": "audio_abc123",
                        "data": "base64encodedaudiodata==",
                        "expires_at": 1700003600,
                        "transcript": "Here is a joke for you."
                    }
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 50, "total_tokens": 60 }
        }"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        let ir = ResponseIR::from(resp);

        let ext = ir.metadata.extensions.as_ref().unwrap();
        let aud = ext.get("openai_audio").unwrap();
        assert_eq!(aud["id"], "audio_abc123");
        assert_eq!(aud["data"], "base64encodedaudiodata==");
        assert_eq!(aud["expires_at"], 1700003600);
        assert_eq!(aud["transcript"], "Here is a joke for you.");

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        let msg = &rendered.choices[0].message;
        match msg {
            OpenAiMessage::Assistant { audio, .. } => {
                let a = audio.as_ref().unwrap();
                assert_eq!(a.id.as_ref(), "audio_abc123");
                assert_eq!(a.data.as_ref(), "base64encodedaudiodata==");
                assert_eq!(a.expires_at, 1700003600);
                assert_eq!(a.transcript.as_ref(), "Here is a joke for you.");
            }
            _ => panic!("expected Assistant message"),
        }
    }

    #[test]
    fn response_without_audio_omits_field() {
        let src = r#"{
            "id": "chatcmpl-def456",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Hello!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        }"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        let ir = ResponseIR::from(resp);
        assert!(ir.metadata.extensions.is_none());

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json["choices"][0]["message"].get("audio").is_none());
    }
}

#[cfg(test)]
mod usage_detail_tests {
    use super::*;

    #[test]
    fn audio_tokens_round_trip() {
        let src = r#"{
            "id":"chatcmpl-audio1","object":"chat.completion","created":0,"model":"gpt-4o-audio",
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":20,"completion_tokens":15,"total_tokens":35,
                "prompt_tokens_details":{"cached_tokens":5,"audio_tokens":10},
                "completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":7}
            }
        }"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        let u = resp.usage.as_ref().unwrap();
        assert_eq!(u.prompt_tokens_details.unwrap().audio_tokens, Some(10));
        assert_eq!(u.completion_tokens_details.unwrap().audio_tokens, Some(7));

        let ir = ResponseIR::from(resp);
        let usage = ir.usage.as_ref().unwrap();
        assert_eq!(usage.audio_tokens_in, Some(10));
        assert_eq!(usage.audio_tokens_out, Some(7));
        assert_eq!(usage.cached_tokens, Some(5));
        assert_eq!(usage.reasoning_tokens, Some(3));

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        let ru = rendered.usage.as_ref().unwrap();
        assert_eq!(ru.prompt_tokens_details.unwrap().audio_tokens, Some(10));
        assert_eq!(ru.prompt_tokens_details.unwrap().cached_tokens, 5);
        assert_eq!(ru.completion_tokens_details.unwrap().audio_tokens, Some(7));
        assert_eq!(ru.completion_tokens_details.unwrap().reasoning_tokens, 3);
    }

    #[test]
    fn prediction_tokens_round_trip() {
        let src = r#"{
            "id":"chatcmpl-pred1","object":"chat.completion","created":0,"model":"gpt-4o",
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":10,"completion_tokens":8,"total_tokens":18,
                "completion_tokens_details":{
                    "reasoning_tokens":0,
                    "accepted_prediction_tokens":5,
                    "rejected_prediction_tokens":2
                }
            }
        }"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        let ir = ResponseIR::from(resp);
        let usage = ir.usage.as_ref().unwrap();
        assert_eq!(usage.accepted_prediction_tokens, Some(5));
        assert_eq!(usage.rejected_prediction_tokens, Some(2));

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        let ru = rendered.usage.as_ref().unwrap();
        let cd = ru.completion_tokens_details.unwrap();
        assert_eq!(cd.accepted_prediction_tokens, Some(5));
        assert_eq!(cd.rejected_prediction_tokens, Some(2));
    }

    #[test]
    fn absent_detail_fields_stay_none() {
        let src = r#"{
            "id":"chatcmpl-plain","object":"chat.completion","created":0,"model":"gpt-4o",
            "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}
        }"#;
        let resp: OpenAiResponse<'_> = serde_json::from_str(src).unwrap();
        assert!(resp.usage.as_ref().unwrap().prompt_tokens_details.is_none());
        assert!(resp
            .usage
            .as_ref()
            .unwrap()
            .completion_tokens_details
            .is_none());

        let ir = ResponseIR::from(resp);
        let usage = ir.usage.as_ref().unwrap();
        assert!(usage.audio_tokens_in.is_none());
        assert!(usage.audio_tokens_out.is_none());
        assert!(usage.accepted_prediction_tokens.is_none());
        assert!(usage.rejected_prediction_tokens.is_none());

        let rendered = OpenAiResponse::try_from(&ir).unwrap();
        assert!(rendered
            .usage
            .as_ref()
            .unwrap()
            .prompt_tokens_details
            .is_none());
        assert!(rendered
            .usage
            .as_ref()
            .unwrap()
            .completion_tokens_details
            .is_none());

        let json = serde_json::to_value(&rendered).unwrap();
        assert!(json["usage"].get("prompt_tokens_details").is_none());
        assert!(json["usage"].get("completion_tokens_details").is_none());
    }
}
