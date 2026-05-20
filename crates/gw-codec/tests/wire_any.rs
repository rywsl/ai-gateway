//! WireAny scaffold — verify the Request variants exist and `kind()` matches.
//! WireAnyResponse / WireAnyStreamChunk are empty enums in Phase 1d (Phase 2
//! adds real variants when wire-level response/stream types exist); we
//! typecheck their surface only.

use gw_codec::wire::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
use gw_core::protocol::ProtocolKind;

#[test]
fn request_variant_tag_matches_protocol() {
    let openai_body = br#"{"model":"gpt-4","messages":[]}"#;
    let parsed: gw_codec::openai::wire::OpenAiRequest<'_> =
        serde_json::from_slice(openai_body).expect("parse");
    let wire = WireAnyRequest::OpenAi(parsed);
    assert_eq!(wire.kind(), ProtocolKind::OpenAi);
}

#[test]
fn response_and_stream_chunk_surface_typechecks() {
    // WireAnyResponse and WireAnyStreamChunk are uninhabited in Phase 1d.
    // Phase 2 (codec S2/S4) replaces _Phantom with OpenAi/Anthropic/Gemini variants.
    // This is a compile-only check that the surface (kind()) exists.
    #[allow(dead_code)]
    fn _r(r: &WireAnyResponse<'_>) -> ProtocolKind {
        r.kind()
    }
    #[allow(dead_code)]
    fn _s(s: &WireAnyStreamChunk<'_>) -> ProtocolKind {
        s.kind()
    }
}
