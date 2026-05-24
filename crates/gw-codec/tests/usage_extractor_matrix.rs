use gw_codec::usage_extractor::{ExtractedUsage, ProvenanceTag, UsageExtractor};

#[test]
fn extractor_trait_object_safe() {
    fn _accept(_: &dyn UsageExtractor) {}
    let _ = _accept;
}

#[test]
fn provenance_default_is_unknown() {
    let tag = ProvenanceTag::default();
    assert!(matches!(tag, ProvenanceTag::Unknown));
}

#[test]
fn extracted_usage_default_is_zero_unknown_nonterminal() {
    let e = ExtractedUsage::default();
    assert_eq!(e.values.input_tokens, 0);
    assert_eq!(e.values.output_tokens, 0);
    assert!(matches!(e.provenance.input, ProvenanceTag::Unknown));
    assert!(!e.is_terminal);
}

use gw_codec::openai::usage_extractor::OpenAiUsageExtractor;

#[test]
fn openai_chat_extracts_six_dims() {
    let body = br#"{
        "id": "x", "object": "chat.completion", "created": 0, "model": "gpt-4o",
        "choices": [],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": { "cached_tokens": 30 },
            "completion_tokens_details": { "reasoning_tokens": 20 }
        }
    }"#;
    let e = OpenAiUsageExtractor::chat();
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 100);
    assert_eq!(r.values.output_tokens, 50);
    assert_eq!(r.values.cache_read_input_tokens, Some(30));
    assert_eq!(r.values.thinking_tokens, Some(20));
    assert!(r.is_terminal);
    assert!(matches!(r.provenance.input, ProvenanceTag::UpstreamFinal));
    assert!(matches!(
        r.provenance.thinking,
        ProvenanceTag::UpstreamFinal
    ));
}

#[test]
fn openai_responses_extracts_six_dims() {
    let body = br#"{
        "id": "resp_1", "object": "response", "model": "o1",
        "usage": {
            "input_tokens": 200, "output_tokens": 80,
            "input_tokens_details": { "cached_tokens": 40 },
            "output_tokens_details": { "reasoning_tokens": 60 }
        }
    }"#;
    let e = OpenAiUsageExtractor::responses();
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 200);
    assert_eq!(r.values.output_tokens, 80);
    assert_eq!(r.values.cache_read_input_tokens, Some(40));
    assert_eq!(r.values.thinking_tokens, Some(60));
    assert!(r.is_terminal);
}

#[test]
fn openai_missing_usage_returns_unknown_provenance() {
    let body = br#"{"id":"x","choices":[]}"#;
    let e = OpenAiUsageExtractor::chat();
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 0);
    assert!(matches!(r.provenance.input, ProvenanceTag::Unknown));
    assert!(!r.is_terminal);
}

use gw_codec::anthropic::usage_extractor::AnthropicUsageExtractor;

#[test]
fn anthropic_extracts_cache_creation_and_read() {
    let body = br#"{
        "id":"msg_1","type":"message","role":"assistant","content":[],
        "model":"claude-3-5-sonnet","stop_reason":"end_turn","stop_sequence":null,
        "usage":{
            "input_tokens":50, "output_tokens":25,
            "cache_creation_input_tokens":1000,
            "cache_read_input_tokens":500
        }
    }"#;
    let e = AnthropicUsageExtractor;
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 50);
    assert_eq!(r.values.output_tokens, 25);
    assert_eq!(r.values.cache_creation_input_tokens, Some(1000));
    assert_eq!(r.values.cache_read_input_tokens, Some(500));
    assert!(r.is_terminal);
    assert!(matches!(
        r.provenance.cache_creation,
        ProvenanceTag::UpstreamFinal
    ));
}

#[test]
fn anthropic_message_delta_with_usage_is_terminal() {
    let frame = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n";
    let e = AnthropicUsageExtractor;
    let r = e
        .extract_chunk(frame)
        .expect("usage event yields ExtractedUsage");
    assert_eq!(r.values.output_tokens, 42);
    assert!(r.is_terminal);
}

#[test]
fn anthropic_message_start_with_usage_is_not_terminal() {
    let frame = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":50,\"output_tokens\":0,\"cache_creation_input_tokens\":1000,\"cache_read_input_tokens\":500}}}\n";
    let e = AnthropicUsageExtractor;
    let r = e
        .extract_chunk(frame)
        .expect("message_start carries initial usage");
    assert_eq!(r.values.input_tokens, 50);
    assert_eq!(r.values.cache_creation_input_tokens, Some(1000));
    assert!(!r.is_terminal);
}

use gw_codec::gemini::usage_extractor::GeminiUsageExtractor;

#[test]
fn gemini_extracts_multimodal_sum() {
    let body = br#"{
        "candidates":[],
        "usageMetadata":{
            "promptTokenCount":1000,
            "candidatesTokenCount":200,
            "cachedContentTokenCount":300,
            "thoughtsTokenCount":50,
            "promptTokensDetails":[
                {"modality":"TEXT","tokenCount":700},
                {"modality":"IMAGE","tokenCount":200},
                {"modality":"VIDEO","tokenCount":80},
                {"modality":"AUDIO","tokenCount":20}
            ]
        }
    }"#;
    let e = GeminiUsageExtractor;
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 1000);
    assert_eq!(r.values.output_tokens, 200);
    assert_eq!(r.values.cache_read_input_tokens, Some(300));
    assert_eq!(r.values.thinking_tokens, Some(50));
    assert_eq!(r.values.multimodal_input_tokens, Some(300)); // 200 + 80 + 20
    assert!(r.is_terminal);
    assert!(matches!(
        r.provenance.multimodal_input,
        ProvenanceTag::UpstreamFinal
    ));
}

#[test]
fn gemini_pure_text_yields_none_multimodal() {
    let body = br#"{
        "usageMetadata":{
            "promptTokenCount":100,"candidatesTokenCount":50,
            "promptTokensDetails":[{"modality":"TEXT","tokenCount":100}]
        }
    }"#;
    let e = GeminiUsageExtractor;
    let r = e.extract_response(body);
    assert!(r.values.multimodal_input_tokens.is_none());
    assert!(matches!(
        r.provenance.multimodal_input,
        ProvenanceTag::Unknown
    ));
}

#[test]
fn gemini_missing_usage_metadata_returns_default() {
    let body = br#"{"candidates":[]}"#;
    let e = GeminiUsageExtractor;
    let r = e.extract_response(body);
    assert_eq!(r.values.input_tokens, 0);
    assert!(!r.is_terminal);
}
