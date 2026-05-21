use gw_codec::stream_accumulator::{accumulate_anthropic, accumulate_openai};

#[test]
fn openai_chat_chunks_concat_into_message() {
    let chunks = [
        r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ];
    let final_json = accumulate_openai(chunks.iter().map(|s| s.as_bytes())).unwrap();
    assert_eq!(final_json["choices"][0]["message"]["role"], "assistant");
    assert_eq!(final_json["choices"][0]["message"]["content"], "Hello");
    assert_eq!(final_json["choices"][0]["finish_reason"], "stop");
    assert_eq!(final_json["object"], "chat.completion");
}

#[test]
fn anthropic_event_stream_concat_into_message() {
    let events = [
        r#"{"type":"message_start","message":{"id":"m1","role":"assistant","model":"claude-3","content":[]}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
    ];
    let final_json = accumulate_anthropic(events.iter().map(|s| s.as_bytes())).unwrap();
    assert_eq!(final_json["id"], "m1");
    assert_eq!(final_json["role"], "assistant");
    assert_eq!(final_json["model"], "claude-3");
    assert_eq!(final_json["content"][0]["text"], "Hello");
    assert_eq!(final_json["stop_reason"], "end_turn");
    assert_eq!(final_json["type"], "message");
}

#[test]
fn openai_malformed_chunk_returns_error() {
    let chunks = ["{not json"];
    let r = accumulate_openai(chunks.iter().map(|s| s.as_bytes()));
    assert!(r.is_err());
}

#[test]
fn anthropic_malformed_event_returns_error() {
    let events = ["not-json"];
    let r = accumulate_anthropic(events.iter().map(|s| s.as_bytes()));
    assert!(r.is_err());
}

#[test]
fn openai_tool_calls_collected() {
    let chunks = [
        r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"t1","function":{"name":"foo"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let final_json = accumulate_openai(chunks.iter().map(|s| s.as_bytes())).unwrap();
    let tcs = final_json["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert!(!tcs.is_empty());
    assert_eq!(tcs[0]["id"], "t1");
}

#[test]
fn anthropic_multiple_text_blocks() {
    let events = [
        r#"{"type":"message_start","message":{"id":"m1","role":"assistant","model":"claude-3","content":[]}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"second"}}"#,
    ];
    let final_json = accumulate_anthropic(events.iter().map(|s| s.as_bytes())).unwrap();
    assert_eq!(final_json["content"][0]["text"], "first");
    assert_eq!(final_json["content"][1]["text"], "second");
}

#[test]
fn empty_streams_return_default_shape() {
    let openai: Vec<&[u8]> = vec![];
    let r = accumulate_openai(openai).unwrap();
    assert_eq!(r["object"], "chat.completion");

    let anthropic: Vec<&[u8]> = vec![];
    let r = accumulate_anthropic(anthropic).unwrap();
    assert_eq!(r["type"], "message");
}
