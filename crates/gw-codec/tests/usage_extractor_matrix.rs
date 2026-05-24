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
    assert!(matches!(r.provenance.thinking, ProvenanceTag::UpstreamFinal));
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
