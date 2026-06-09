use gw_codec::openai::responses::conversion::{
    chat_to_responses, responses_to_anthropic, responses_to_chat, responses_to_gemini,
};
use gw_codec::openai::responses::OpenAiResponsesRequest;
use serde_json::{json, Value};

#[test]
fn responses_to_chat_lifts_input_to_messages() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hi"}],
        "stream": true
    }))
    .unwrap();
    let chat: Value = responses_to_chat(&req).unwrap();
    assert_eq!(chat["model"], "gpt-5");
    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][0]["content"], "hi");
    assert_eq!(chat["stream"], true);
}

#[test]
fn responses_to_chat_accepts_string_input() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": "hi"
    }))
    .unwrap();
    let chat: Value = responses_to_chat(&req).unwrap();
    assert_eq!(chat["messages"][0]["role"], "user");
    assert_eq!(chat["messages"][0]["content"], "hi");
}

#[test]
fn responses_to_chat_with_instructions_prepends_system() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hi"}],
        "instructions": "be concise"
    }))
    .unwrap();
    let chat: Value = responses_to_chat(&req).unwrap();
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][0]["content"], "be concise");
    assert_eq!(chat["messages"][1]["role"], "user");
}

#[test]
fn responses_to_anthropic_minimal() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hello"}]
    }))
    .unwrap();
    let an: Value = responses_to_anthropic(&req).unwrap();
    assert_eq!(an["messages"][0]["role"], "user");
    assert_eq!(an["messages"][0]["content"], "hello");
    assert!(an["max_tokens"].is_number());
}

#[test]
fn responses_to_anthropic_lifts_instructions_to_system() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hi"}],
        "instructions": "tone: serious"
    }))
    .unwrap();
    let an: Value = responses_to_anthropic(&req).unwrap();
    assert_eq!(an["system"], "tone: serious");
}

#[test]
fn responses_to_gemini_minimal() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hello"}]
    }))
    .unwrap();
    let g: Value = responses_to_gemini(&req).unwrap();
    assert_eq!(g["contents"][0]["role"], "user");
    assert_eq!(g["contents"][0]["parts"][0]["text"], "hello");
}

#[test]
fn responses_to_gemini_with_instructions_uses_system_instruction() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hi"}],
        "instructions": "tone: friendly"
    }))
    .unwrap();
    let g: Value = responses_to_gemini(&req).unwrap();
    assert_eq!(g["systemInstruction"]["parts"][0]["text"], "tone: friendly");
}

#[test]
fn chat_to_responses_round_trip_preserves_text() {
    let chat = json!({
        "model": "gpt-5",
        "messages": [{"role":"user","content":"hi"}]
    });
    let r = chat_to_responses(&chat).unwrap();
    assert_eq!(r.model, "gpt-5");
    assert_eq!(r.input[0]["type"], "input_text");
    assert_eq!(r.input[0]["text"], "hi");
}

#[test]
fn chat_to_responses_lifts_system_to_instructions() {
    let chat = json!({
        "model": "gpt-5",
        "messages": [
            {"role":"system","content":"be helpful"},
            {"role":"user","content":"hi"}
        ]
    });
    let r = chat_to_responses(&chat).unwrap();
    assert_eq!(r.instructions.as_deref(), Some("be helpful"));
    assert_eq!(r.input.as_array().unwrap().len(), 1);
    assert_eq!(r.input[0]["text"], "hi");
}
