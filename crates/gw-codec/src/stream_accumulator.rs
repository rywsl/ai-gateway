//! Accumulate streamed responses into a single non-stream JSON.
//!
//! Used by the `AccumulateUpstreamStream` policy in `stream_policy.rs`: when
//! the client requested non-stream but the upstream only supports SSE, the
//! gateway buffers chunks here and returns the assembled response.
//!
//! v1 covers OpenAI Chat (delta-style) and Anthropic Messages (event-style).
//! Gemini accumulator is not provided; if/when needed, it follows the OpenAI
//! pattern (parts arrive as deltas in `candidates[0].content.parts[0]`).

use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum AccumulateError {
    #[error("parse: {0}")]
    Parse(String),
}

/// Accumulate OpenAI chat-completion stream chunks into a single non-stream
/// `chat.completion` object.
pub fn accumulate_openai<'a, I>(chunks: I) -> Result<Value, AccumulateError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut role = None::<String>;
    let mut content = String::new();
    let mut finish_reason = None::<String>;
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut model = None::<String>;
    let mut id = None::<String>;
    for raw in chunks {
        let v: Value = serde_json::from_slice(raw)
            .map_err(|e| AccumulateError::Parse(e.to_string()))?;
        if let Some(s) = v.get("model").and_then(|v| v.as_str()) {
            model = Some(s.into());
        }
        if let Some(s) = v.get("id").and_then(|v| v.as_str()) {
            id = Some(s.into());
        }
        if let Some(arr) = v.get("choices").and_then(|v| v.as_array()) {
            for ch in arr {
                if let Some(d) = ch.get("delta") {
                    if let Some(r) = d.get("role").and_then(|v| v.as_str()) {
                        role = Some(r.into());
                    }
                    if let Some(c) = d.get("content").and_then(|v| v.as_str()) {
                        content.push_str(c);
                    }
                    if let Some(tc) = d.get("tool_calls").and_then(|v| v.as_array()) {
                        for t in tc {
                            tool_calls.push(t.clone());
                        }
                    }
                }
                if let Some(f) = ch.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(f.into());
                }
            }
        }
    }
    Ok(json!({
        "id": id.unwrap_or_default(),
        "object": "chat.completion",
        "model": model.unwrap_or_default(),
        "choices": [{
            "index": 0,
            "message": {
                "role": role.unwrap_or_else(|| "assistant".into()),
                "content": content,
                "tool_calls": tool_calls,
            },
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".into()),
        }],
    }))
}

/// Accumulate Anthropic message-stream events into a single non-stream
/// `message` object.
pub fn accumulate_anthropic<'a, I>(events: I) -> Result<Value, AccumulateError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut id = None::<String>;
    let mut role = None::<String>;
    let mut model = None::<String>;
    let mut blocks: Vec<Value> = Vec::new();
    let mut stop_reason = None::<String>;
    for raw in events {
        let v: Value = serde_json::from_slice(raw)
            .map_err(|e| AccumulateError::Parse(e.to_string()))?;
        let typ = v.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match typ {
            "message_start" => {
                if let Some(m) = v.get("message") {
                    if let Some(s) = m.get("id").and_then(|v| v.as_str()) {
                        id = Some(s.into());
                    }
                    if let Some(s) = m.get("role").and_then(|v| v.as_str()) {
                        role = Some(s.into());
                    }
                    if let Some(s) = m.get("model").and_then(|v| v.as_str()) {
                        model = Some(s.into());
                    }
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block = v
                    .get("content_block")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"text","text":""}));
                while blocks.len() <= idx {
                    blocks.push(json!({"type":"text","text":""}));
                }
                blocks[idx] = block;
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if idx >= blocks.len() {
                    continue;
                }
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if dtype == "text_delta" {
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        if let Value::Object(ref mut m) = blocks[idx] {
                            let cur = m
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            m.insert("text".into(), Value::String(cur + t));
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(d) = v.get("delta") {
                    if let Some(s) = d.get("stop_reason").and_then(|v| v.as_str()) {
                        stop_reason = Some(s.into());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(json!({
        "id": id.unwrap_or_default(),
        "type": "message",
        "role": role.unwrap_or_else(|| "assistant".into()),
        "model": model.unwrap_or_default(),
        "content": blocks,
        "stop_reason": stop_reason.unwrap_or_default(),
    }))
}
