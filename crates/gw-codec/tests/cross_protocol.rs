use gw_codec::{anthropic::Anthropic, gemini::Gemini, openai::OpenAi, ProtocolCodec};

#[test]
fn openai_to_anthropic_via_ir() {
    let openai = OpenAi;
    let anthropic = Anthropic;
    let body = include_bytes!("fixtures/openai_basic.json");
    let ir = openai.parse_request(body).unwrap();
    let mut buf = bytes::BytesMut::new();
    anthropic.render_request(&ir, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(v["model"], "gpt-4");
    // OpenAI's System message folds into Anthropic's top-level `system`.
    assert_eq!(v["system"], "You are helpful");
    assert!(v.get("messages").and_then(|m| m.as_array()).is_some());
    // Only the User turn remains in `messages`.
    assert_eq!(v["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn openai_to_gemini_via_ir() {
    let openai = OpenAi;
    let gemini = Gemini;
    let body = include_bytes!("fixtures/openai_basic.json");
    let ir = openai.parse_request(body).unwrap();
    let mut buf = bytes::BytesMut::new();
    gemini.render_request(&ir, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert!(v.get("contents").and_then(|c| c.as_array()).is_some());
    assert!(v.get("systemInstruction").is_some());
    assert_eq!(v["generationConfig"]["maxOutputTokens"], 100);
}

#[test]
fn anthropic_to_openai_via_ir() {
    let openai = OpenAi;
    let anthropic = Anthropic;
    let body = include_bytes!("fixtures/anthropic_basic.json");
    let ir = anthropic.parse_request(body).unwrap();
    let mut buf = bytes::BytesMut::new();
    openai.render_request(&ir, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(v["model"], "claude-3-5-sonnet-20241022");
    // OpenAI surfaces the system prompt as a System role message.
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
}
