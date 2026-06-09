pub mod claude_cli;
pub mod gemini_cli;
pub mod openai_cli;
pub mod openai_compact;
pub mod traits;

pub use traits::{passthrough_bytes, CliHints, CliUsageDelta, CliUsageExtractor, CliUsageFull};

use gw_core::protocol::ApiShape;
use std::borrow::Cow;

pub const FORMAT_CLAUDE_CLI: &str = "claude:cli";
pub const FORMAT_OPENAI_CLI: &str = "openai:cli";
pub const FORMAT_GEMINI_CLI: &str = "gemini:cli";
pub const FORMAT_OPENAI_COMPACT: &str = "openai:compact";

pub trait CliCodec: Send + Sync + 'static {
    fn shape(&self) -> ApiShape;

    fn passthrough_request<'a>(&self, body: &'a [u8]) -> Cow<'a, [u8]> {
        Cow::Borrowed(body)
    }

    fn passthrough_response_chunk<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        Cow::Borrowed(chunk)
    }

    fn extract_hints(&self, path: &str, body: Option<&[u8]>) -> CliHints;

    fn usage_extractor(&self) -> &dyn CliUsageExtractor;

    fn ttft_event_types(&self) -> &'static [&'static str];
}

pub fn detect_cli(path: &str, headers: &crate::trait_def::Headers) -> Option<ApiShape> {
    if path.starts_with("/v1/responses/compact") {
        return Some(ApiShape::OpenAiCompact);
    }

    if path.starts_with("/v1/messages") {
        let has_bearer = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("authorization")
                && v.len() >= 7
                && v[..7].eq_ignore_ascii_case("bearer ")
        });
        let has_api_key = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("x-api-key"));

        if has_bearer && !has_api_key {
            return Some(ApiShape::ClaudeCli);
        }
        return None;
    }

    if is_gemini_path(path) {
        let is_cli = headers.iter().any(|(k, v)| {
            (k.eq_ignore_ascii_case("x-app") && v.to_lowercase().contains("cli"))
                || (k.eq_ignore_ascii_case("user-agent") && {
                    let lower = v.to_lowercase();
                    lower.contains("gemini-cli") || lower.contains("geminicli")
                })
        });
        if is_cli {
            return Some(ApiShape::GeminiCli);
        }
        return None;
    }

    None
}

fn is_gemini_path(path: &str) -> bool {
    path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_format_id_registration() {
        assert_eq!(FORMAT_CLAUDE_CLI, "claude:cli");
        assert_eq!(FORMAT_OPENAI_CLI, "openai:cli");
        assert_eq!(FORMAT_GEMINI_CLI, "gemini:cli");
        assert_eq!(FORMAT_OPENAI_COMPACT, "openai:compact");

        assert_eq!(ApiShape::ClaudeCli.format_id(), FORMAT_CLAUDE_CLI);
        assert_eq!(ApiShape::OpenAiCli.format_id(), FORMAT_OPENAI_CLI);
        assert_eq!(ApiShape::GeminiCli.format_id(), FORMAT_GEMINI_CLI);
        assert_eq!(ApiShape::OpenAiCompact.format_id(), FORMAT_OPENAI_COMPACT);
        assert_eq!(ApiShape::KiroCli.format_id(), "kiro:cli");
        assert_eq!(ApiShape::KiroEventStream.format_id(), "kiro:eventstream");
    }

    #[test]
    fn test_api_shape_is_cli() {
        assert!(ApiShape::ClaudeCli.is_cli());
        assert!(ApiShape::OpenAiCli.is_cli());
        assert!(ApiShape::OpenAiCompact.is_cli());
        assert!(ApiShape::GeminiCli.is_cli());
        assert!(ApiShape::KiroCli.is_cli());

        assert!(!ApiShape::OpenAiChat.is_cli());
        assert!(!ApiShape::OpenAiResponses.is_cli());
        assert!(!ApiShape::AnthropicMessages.is_cli());
        assert!(!ApiShape::GeminiGenerate.is_cli());
        assert!(!ApiShape::KiroEventStream.is_cli());
    }

    #[test]
    fn test_api_shape_protocol_kind() {
        use gw_core::protocol::ProtocolKind;

        assert_eq!(ApiShape::ClaudeCli.protocol_kind(), ProtocolKind::Anthropic);
        assert_eq!(ApiShape::OpenAiCli.protocol_kind(), ProtocolKind::OpenAi);
        assert_eq!(
            ApiShape::OpenAiCompact.protocol_kind(),
            ProtocolKind::OpenAi
        );
        assert_eq!(ApiShape::GeminiCli.protocol_kind(), ProtocolKind::Gemini);
        assert_eq!(ApiShape::KiroCli.protocol_kind(), ProtocolKind::Kiro);
        assert_eq!(
            ApiShape::KiroEventStream.protocol_kind(),
            ProtocolKind::Kiro
        );
    }

    #[test]
    fn test_cli_codec_trait_surface() {
        fn assert_cli_codec<T: CliCodec>(_: &T) {}

        struct DummyExtractor;
        impl CliUsageExtractor for DummyExtractor {
            fn extract_stream_usage(&self, _: &str, _: &[u8]) -> Option<CliUsageDelta> {
                None
            }
            fn extract_response_usage(&self, _: &[u8]) -> Option<CliUsageFull> {
                None
            }
        }

        struct DummyCliCodec;
        impl CliCodec for DummyCliCodec {
            fn shape(&self) -> ApiShape {
                ApiShape::ClaudeCli
            }
            fn extract_hints(&self, _path: &str, body: Option<&[u8]>) -> CliHints {
                body.and_then(|b| serde_json::from_slice(b).ok())
                    .unwrap_or_default()
            }
            fn usage_extractor(&self) -> &dyn CliUsageExtractor {
                &DummyExtractor
            }
            fn ttft_event_types(&self) -> &'static [&'static str] {
                &["content_block_delta"]
            }
        }

        let codec = DummyCliCodec;
        assert_cli_codec(&codec);
        assert_eq!(codec.shape(), ApiShape::ClaudeCli);
        assert_eq!(codec.ttft_event_types(), &["content_block_delta"]);

        let hints = codec.extract_hints(
            "/v1/messages",
            Some(br#"{"model":"claude-sonnet-4-20250514","stream":true}"#),
        );
        assert_eq!(hints.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(hints.stream, Some(true));
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn test_detect_claude_cli() {
        let headers = h(&[("authorization", "Bearer sk-ant-oauthXXXX")]);
        assert_eq!(
            detect_cli("/v1/messages", &headers),
            Some(ApiShape::ClaudeCli)
        );
    }

    #[test]
    fn test_detect_anthropic_standard() {
        let headers = h(&[
            ("authorization", "Bearer sk-ant-oauthXXXX"),
            ("x-api-key", "sk-ant-XXXX"),
        ]);
        assert_eq!(detect_cli("/v1/messages", &headers), None);
    }

    #[test]
    fn test_detect_anthropic_apikey_only() {
        let headers = h(&[("x-api-key", "sk-ant-XXXX")]);
        assert_eq!(detect_cli("/v1/messages", &headers), None);
    }

    #[test]
    fn test_detect_openai_cli() {
        let headers = h(&[("authorization", "Bearer sk-XXXX")]);
        assert_eq!(detect_cli("/v1/responses", &headers), None);
    }

    #[test]
    fn test_detect_openai_compact() {
        let headers = h(&[("authorization", "Bearer sk-XXXX")]);
        assert_eq!(
            detect_cli("/v1/responses/compact", &headers),
            Some(ApiShape::OpenAiCompact)
        );
    }

    #[test]
    fn test_detect_gemini_cli_xapp() {
        let headers = h(&[("x-app", "cli")]);
        assert_eq!(
            detect_cli(
                "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
                &headers
            ),
            Some(ApiShape::GeminiCli)
        );
    }

    #[test]
    fn test_detect_gemini_cli_ua() {
        let headers = h(&[("user-agent", "gemini-cli/0.1.0 (linux; x64)")]);
        assert_eq!(
            detect_cli("/v1beta/models/gemini-2.5-pro:generateContent", &headers),
            Some(ApiShape::GeminiCli)
        );
    }

    #[test]
    fn test_detect_gemini_standard() {
        let headers = h(&[("user-agent", "python-requests/2.28")]);
        assert_eq!(
            detect_cli("/v1beta/models/gemini-2.5-pro:generateContent", &headers),
            None
        );
    }

    #[test]
    fn test_detect_unrelated_path() {
        let headers = h(&[("authorization", "Bearer sk-XXXX")]);
        assert_eq!(detect_cli("/v1/chat/completions", &headers), None);
    }

    #[test]
    fn test_cli_hints_default() {
        let hints = CliHints::default();
        assert!(hints.model.is_none());
        assert!(hints.stream.is_none());
    }

    #[test]
    fn test_cli_hints_deserialize() {
        let body = br#"{"model":"o3","stream":false}"#;
        let hints: CliHints = serde_json::from_slice(body).unwrap();
        assert_eq!(hints.model.as_deref(), Some("o3"));
        assert_eq!(hints.stream, Some(false));
    }

    #[test]
    fn test_cli_hints_partial() {
        let body = br#"{"model":"claude-sonnet-4-20250514"}"#;
        let hints: CliHints = serde_json::from_slice(body).unwrap();
        assert_eq!(hints.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert!(hints.stream.is_none());
    }

    #[test]
    fn test_claude_cli_passthrough() {
        let codec = crate::cli::claude_cli::ClaudeCliCodec;
        let body = br#"{"model":"claude-sonnet-4-20250514","stream":true,"anthropic_beta":["fine-grained-tool-streaming-2025-05-14"],"thinking":{"type":"enabled","budget_tokens":1024}}"#;

        let hints = codec.extract_hints("/v1/messages", Some(body));

        assert_eq!(codec.shape(), ApiShape::ClaudeCli);
        assert_eq!(hints.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(hints.stream, Some(true));
        assert_eq!(body.as_slice(), br#"{"model":"claude-sonnet-4-20250514","stream":true,"anthropic_beta":["fine-grained-tool-streaming-2025-05-14"],"thinking":{"type":"enabled","budget_tokens":1024}}"#);
    }

    #[test]
    fn test_openai_cli_passthrough() {
        let codec = crate::cli::openai_cli::OpenAiCliCodec;
        let body = br#"{"model":"o3","stream":true,"tools":[{"type":"computer_use_preview"}],"reasoning":{"effort":"high"}}"#;

        let hints = codec.extract_hints("/v1/responses", Some(body));

        assert_eq!(codec.shape(), ApiShape::OpenAiCli);
        assert_eq!(hints.model.as_deref(), Some("o3"));
        assert_eq!(hints.stream, Some(true));
        assert_eq!(body.as_slice(), br#"{"model":"o3","stream":true,"tools":[{"type":"computer_use_preview"}],"reasoning":{"effort":"high"}}"#);
    }

    #[test]
    fn test_openai_cli_responses_api() {
        let codec = crate::cli::openai_cli::OpenAiCliCodec;
        let body = br#"{"model":"gpt-4.1","input":"summarize","metadata":{"cli":"codex"}}"#;

        let hints = codec.extract_hints("/v1/responses", Some(body));

        assert_eq!(
            detect_cli("/v1/responses", &h(&[("authorization", "Bearer sk-test")])),
            None
        );
        assert_eq!(hints.model.as_deref(), Some("gpt-4.1"));
        assert_eq!(hints.stream, None);
        assert_eq!(
            body.as_slice(),
            br#"{"model":"gpt-4.1","input":"summarize","metadata":{"cli":"codex"}}"#
        );
    }

    #[test]
    fn test_cli_preserves_headers() {
        let headers = h(&[
            ("authorization", "Bearer cli-oauth-token"),
            ("anthropic-beta", "fine-grained-tool-streaming-2025-05-14"),
            ("user-agent", "Claude-Code/1.0"),
        ]);
        let before = headers.clone();

        assert_eq!(
            detect_cli("/v1/messages", &headers),
            Some(ApiShape::ClaudeCli)
        );
        let codec = crate::cli::claude_cli::ClaudeCliCodec;
        let _ = codec.extract_hints(
            "/v1/messages",
            Some(br#"{"model":"claude-sonnet-4-20250514"}"#),
        );

        assert_eq!(headers, before);
    }

    #[test]
    fn test_claude_cli_usage_extraction() {
        let codec = crate::cli::claude_cli::ClaudeCliCodec;
        let extractor = codec.usage_extractor();

        let start = extractor
            .extract_stream_usage(
                "message_start",
                br#"{"type":"message_start","message":{"usage":{"input_tokens":150,"cache_read_input_tokens":50,"cache_creation_input_tokens":7}}}"#,
            )
            .unwrap();
        assert_eq!(start.input_tokens, Some(150));
        assert_eq!(start.output_tokens, None);
        assert_eq!(start.cache_read_input_tokens, Some(50));
        assert_eq!(start.cache_creation_input_tokens, Some(7));

        let delta = extractor
            .extract_stream_usage(
                "message_delta",
                br#"{"type":"message_delta","usage":{"output_tokens":342}}"#,
            )
            .unwrap();
        assert_eq!(delta.input_tokens, None);
        assert_eq!(delta.output_tokens, Some(342));
    }

    #[test]
    fn test_openai_cli_usage_extraction() {
        let codec = crate::cli::openai_cli::OpenAiCliCodec;
        let extractor = codec.usage_extractor();

        let full = extractor
            .extract_response_usage(
                br#"{"id":"resp_1","status":"completed","usage":{"input_tokens":100,"output_tokens":200,"input_tokens_details":{"cached_tokens":30}}}"#,
            )
            .unwrap();
        assert_eq!(full.input_tokens, 100);
        assert_eq!(full.output_tokens, 200);
        assert_eq!(full.cache_read_input_tokens, 30);
        assert_eq!(full.cache_creation_input_tokens, 0);

        let delta = extractor
            .extract_stream_usage(
                "response.completed",
                br#"{"type":"response.completed","response":{"usage":{"input_tokens":101,"output_tokens":201,"input_tokens_details":{"cached_tokens":31}}}}"#,
            )
            .unwrap();
        assert_eq!(delta.input_tokens, Some(101));
        assert_eq!(delta.output_tokens, Some(201));
        assert_eq!(delta.cache_read_input_tokens, Some(31));
        assert_eq!(delta.cache_creation_input_tokens, None);
    }
}
