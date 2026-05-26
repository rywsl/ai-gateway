//! WireAnyRequest — verify the Request variants exist and `kind()` matches.

use gw_codec::wire::WireAnyRequest;
use gw_core::protocol::ProtocolKind;

#[test]
fn request_variant_tag_matches_protocol() {
    let openai_body = br#"{"model":"gpt-4","messages":[]}"#;
    let parsed: gw_codec::openai::wire::OpenAiRequest<'_> =
        serde_json::from_slice(openai_body).expect("parse");
    let wire = WireAnyRequest::OpenAi(Box::new(parsed));
    assert_eq!(wire.kind(), ProtocolKind::OpenAi);
}
