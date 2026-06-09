//! Cross-shape conversion: OpenAI Responses ⇄ Chat / Anthropic / Gemini.
//!
//! v1 ships hand-written `via` functions for the 4 most common transitions.
//! Once codec S1 matrix-codegen lands, these are migrated to derive-generated
//! variants. For now: text-only paths are correct; multimodal / tool_use is
//! left unimplemented.

use super::OpenAiResponsesRequest;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("unsupported part type: {0}")]
    Unsupported(String),
    #[error("missing field: {0}")]
    Missing(&'static str),
}

pub fn responses_to_chat(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut messages = Vec::new();
    if let Some(instr) = &req.instructions {
        messages.push(json!({"role": "system", "content": instr}));
    }
    match &req.input {
        Value::String(s) => messages.push(json!({"role": "user", "content": s})),
        Value::Array(items) => {
            for p in items {
                let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match typ {
                    "input_text" | "text" => {
                        let text = p.get("text").cloned().unwrap_or(Value::Null);
                        messages.push(json!({"role": "user", "content": text}));
                    }
                    other => return Err(ConvertError::Unsupported(other.into())),
                }
            }
        }
        _ => return Err(ConvertError::Unsupported("input".into())),
    }
    let mut out = json!({
        "model": req.model,
        "messages": messages,
        "stream": req.stream,
    });
    if let Value::Object(ref mut m) = out {
        for (k, v) in &req.extras {
            m.insert(k.clone(), v.clone());
        }
    }
    metrics::counter!(
        "gw_codec_convert_total",
        "direction" => "request",
        "source" => "openai_responses",
        "target" => "openai_chat",
        "status" => "ok",
    )
    .increment(1);
    Ok(out)
}

pub fn chat_to_responses(chat: &Value) -> Result<OpenAiResponsesRequest, ConvertError> {
    let model = chat
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(ConvertError::Missing("model"))?
        .to_string();
    let messages = chat
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or(ConvertError::Missing("messages"))?;
    let mut input: Vec<Value> = Vec::new();
    let mut instructions: Option<String> = None;
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        if role == "system" {
            if let Value::String(s) = &content {
                instructions = Some(s.clone());
                continue;
            }
        }
        match content {
            Value::String(s) => input.push(json!({"type":"input_text","text":s})),
            Value::Array(arr) => {
                for p in arr {
                    input.push(p);
                }
            }
            _ => return Err(ConvertError::Unsupported("non-text content".into())),
        }
    }
    metrics::counter!(
        "gw_codec_convert_total",
        "direction" => "request",
        "source" => "openai_chat",
        "target" => "openai_responses",
        "status" => "ok",
    )
    .increment(1);
    Ok(OpenAiResponsesRequest {
        model,
        input: Value::Array(input),
        stream: chat
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        instructions,
        previous_response_id: None,
        extras: serde_json::Map::new(),
    })
}

pub fn responses_to_anthropic(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut messages = Vec::new();
    match &req.input {
        Value::String(s) => messages.push(json!({"role": "user", "content": s})),
        Value::Array(items) => {
            for p in items {
                let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match typ {
                    "input_text" | "text" => {
                        let text = p.get("text").cloned().unwrap_or(Value::Null);
                        messages.push(json!({"role": "user", "content": text}));
                    }
                    other => return Err(ConvertError::Unsupported(other.into())),
                }
            }
        }
        _ => return Err(ConvertError::Unsupported("input".into())),
    }
    let mut out = json!({
        "model": req.model,
        "messages": messages,
        "stream": req.stream,
        "max_tokens": 4096,
    });
    if let Some(s) = &req.instructions {
        if let Value::Object(ref mut m) = out {
            m.insert("system".into(), Value::String(s.clone()));
        }
    }
    metrics::counter!(
        "gw_codec_convert_total",
        "direction" => "request",
        "source" => "openai_responses",
        "target" => "anthropic_messages",
        "status" => "ok",
    )
    .increment(1);
    Ok(out)
}

pub fn responses_to_gemini(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut contents = Vec::new();
    match &req.input {
        Value::String(s) => contents.push(json!({
            "role": "user",
            "parts": [{"text": s}]
        })),
        Value::Array(items) => {
            for p in items {
                let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match typ {
                    "input_text" | "text" => {
                        let text = p.get("text").cloned().unwrap_or(Value::Null);
                        contents.push(json!({
                            "role": "user",
                            "parts": [{"text": text}]
                        }));
                    }
                    other => return Err(ConvertError::Unsupported(other.into())),
                }
            }
        }
        _ => return Err(ConvertError::Unsupported("input".into())),
    }
    let mut out = json!({
        "contents": contents,
    });
    if let Some(s) = &req.instructions {
        if let Value::Object(ref mut m) = out {
            m.insert(
                "systemInstruction".into(),
                json!({"role":"system","parts":[{"text":s}]}),
            );
        }
    }
    metrics::counter!(
        "gw_codec_convert_total",
        "direction" => "request",
        "source" => "openai_responses",
        "target" => "gemini_generate",
        "status" => "ok",
    )
    .increment(1);
    Ok(out)
}
