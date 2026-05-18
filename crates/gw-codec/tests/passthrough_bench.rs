use gw_codec::{openai::OpenAi, passthrough::same_protocol_passthrough};
use gw_core::ir::Mutations;
use std::borrow::Cow;

#[test]
fn empty_mutation_returns_borrowed() {
    let codec = OpenAi;
    let body =
        br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"# as &[u8];
    let out = same_protocol_passthrough(&codec, body, &Mutations::NONE).unwrap();
    assert!(matches!(out, Cow::Borrowed(_)));
}

#[test]
fn non_empty_mutation_returns_owned() {
    let codec = OpenAi;
    let body =
        br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"# as &[u8];
    let mut m = Mutations::NONE;
    m.set_max_tokens();
    let out = same_protocol_passthrough(&codec, body, &m).unwrap();
    assert!(matches!(out, Cow::Owned(_)));
    // The re-rendered body must still parse back into the same shape.
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["model"], "gpt-4");
}
