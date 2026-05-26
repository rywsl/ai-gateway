use gw_codec::cli::{CliCodec, CliUsageFull};
use gw_core::protocol::ApiShape;

#[test]
fn test_gemini_cli_passthrough() {
    let codec = gw_codec::cli::gemini_cli::GeminiCliCodec;
    let body = br#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}],"generationConfig":{"temperature":0.2}}"#;

    assert_eq!(codec.shape(), ApiShape::GeminiCli);
    assert_eq!(codec.passthrough_request(body).as_ref(), body);

    let hints = codec.extract_hints(
        "/v1beta/models/gemini-2.5-pro:generateContent?key=redacted",
        Some(body),
    );
    assert_eq!(hints.model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(hints.stream, Some(false));
}

#[test]
fn test_openai_compact_passthrough() {
    let codec = gw_codec::cli::openai_compact::OpenAiCompactCodec;
    let body = br#"{"model":"gpt-5.1-codex","input":"summarize","compact":true,"metadata":{"cli":"codex"}}"#;

    assert_eq!(codec.shape(), ApiShape::OpenAiCompact);
    assert_eq!(codec.passthrough_request(body).as_ref(), body);

    let hints = codec.extract_hints("/v1/responses/compact", Some(body));
    assert_eq!(hints.model.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(hints.stream, Some(false));
}

#[test]
fn test_gemini_cli_streaming() {
    let codec = gw_codec::cli::gemini_cli::GeminiCliCodec;
    let frame = br#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}

"#;

    assert_eq!(codec.passthrough_response_chunk(frame).as_ref(), frame);
    assert_eq!(codec.ttft_event_types(), &["data"]);

    let hints = codec.extract_hints(
        "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
        None,
    );
    assert_eq!(hints.model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(hints.stream, Some(true));
}

#[test]
fn test_gemini_cli_usage_extraction() {
    let codec = gw_codec::cli::gemini_cli::GeminiCliCodec;
    let body = br#"{
        "candidates":[],
        "usageMetadata":{
            "promptTokenCount":120,
            "candidatesTokenCount":280,
            "thoughtsTokenCount":50,
            "cachedContentTokenCount":20
        }
    }"#;

    assert_eq!(
        codec.usage_extractor().extract_response_usage(body),
        Some(CliUsageFull {
            input_tokens: 120,
            output_tokens: 330,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 0,
        })
    );

    let delta = codec
        .usage_extractor()
        .extract_stream_usage("data", body)
        .expect("Gemini usageMetadata yields stream delta");
    assert_eq!(delta.input_tokens, Some(120));
    assert_eq!(delta.output_tokens, Some(330));
    assert_eq!(delta.cache_read_input_tokens, Some(20));
}

#[test]
fn test_openai_compact_usage_extraction() {
    let codec = gw_codec::cli::openai_compact::OpenAiCompactCodec;
    let body = br#"{
        "type":"response.completed",
        "response":{
            "usage":{
                "input_tokens":100,
                "output_tokens":200,
                "input_tokens_details":{"cached_tokens":30}
            }
        }
    }"#;

    assert_eq!(
        codec.usage_extractor().extract_response_usage(body),
        Some(CliUsageFull {
            input_tokens: 100,
            output_tokens: 200,
            cache_read_input_tokens: 30,
            cache_creation_input_tokens: 0,
        })
    );
}
