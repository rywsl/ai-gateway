use gw_codec::openai::responses::{
    OpenAiResponsesRequest, OpenAiResponsesResponse, OpenAiResponsesStreamChunk,
};

#[test]
fn deserialize_minimal_request() {
    let s = r#"{"model":"gpt-5","input":[{"type":"input_text","text":"hi"}],"stream":true}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert_eq!(r.model, "gpt-5");
    assert!(r.stream);
    assert_eq!(r.input.len(), 1);
}

#[test]
fn deserialize_request_preserves_extras() {
    let s = r#"{"model":"gpt-5","input":[],"temperature":0.7,"top_p":0.9}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert!(r.extras.contains_key("temperature"));
    assert!(r.extras.contains_key("top_p"));
}

#[test]
fn deserialize_response_object() {
    let s = r#"{"id":"resp_1","object":"response","model":"gpt-5","output":[]}"#;
    let r: OpenAiResponsesResponse = serde_json::from_str(s).unwrap();
    assert_eq!(r.id, "resp_1");
    assert_eq!(r.model, "gpt-5");
    assert!(r.output.is_empty());
}

#[test]
fn deserialize_stream_chunk_with_type() {
    let s = r#"{"type":"response.output_text.delta","delta":"He"}"#;
    let c: OpenAiResponsesStreamChunk = serde_json::from_str(s).unwrap();
    assert_eq!(c.r#type, "response.output_text.delta");
    assert!(c.fields.contains_key("delta"));
}

#[test]
fn serialize_request_round_trip() {
    let s = r#"{"model":"gpt-5","input":[{"type":"input_text","text":"hi"}],"stream":false}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    let back = serde_json::to_string(&r).unwrap();
    let r2: OpenAiResponsesRequest = serde_json::from_str(&back).unwrap();
    assert_eq!(r2.model, "gpt-5");
}

#[test]
fn deserialize_optional_fields() {
    let s = r#"{"model":"gpt-5","input":[],"instructions":"be helpful","previous_response_id":"resp_0"}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert_eq!(r.instructions.as_deref(), Some("be helpful"));
    assert_eq!(r.previous_response_id.as_deref(), Some("resp_0"));
}
