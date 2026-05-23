//! G8 — cross-protocol stream chunk re-render via IR round-trip.
//!
//! Upstream emits one protocol's SSE frame; the gateway parses it into
//! `StreamEventIR` and re-renders through the *client* codec. This exercises
//! the exact codec pair `response_body_filter` drives in the cross-protocol
//! branch.
use bytes::BytesMut;
use gw_codec::{anthropic::Anthropic, gemini::Gemini, openai::OpenAi, ProtocolCodec};

#[test]
fn anthropic_chunk_rendered_as_openai_for_client() {
    let upstream_chunk = br#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}

"#;
    let upstream_codec = Anthropic;
    let client_codec = OpenAi;
    let events = upstream_codec.parse_stream_chunk(upstream_chunk).unwrap();
    assert!(!events.is_empty());
    let mut buf = BytesMut::new();
    for ev in &events {
        client_codec.render_stream_chunk(ev, &mut buf).unwrap();
    }
    let rendered = std::str::from_utf8(&buf).unwrap();
    assert!(
        rendered.contains("\"delta\":{\"content\":\"hello\"}"),
        "openai delta shape must carry the text: {rendered}"
    );
    assert!(rendered.starts_with("data: "));
}

#[test]
fn gemini_combined_chunk_renders_all_events_to_openai_client() {
    // G9's SmallVec yields [Delta, End]; G8 must render BOTH into the
    // client buffer (regression guard against single-shot overwrite).
    let upstream = Gemini;
    let client = OpenAi;
    let upstream_chunk =
        br#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#;
    let events = upstream.parse_stream_chunk(upstream_chunk).unwrap();
    assert!(
        events.len() >= 2,
        "Gemini combined chunk must decompose into >=2 events"
    );

    let mut buf = BytesMut::new();
    for ev in &events {
        client.render_stream_chunk(ev, &mut buf).unwrap();
    }
    let rendered = std::str::from_utf8(&buf).unwrap();
    // Delta survives as an openai content delta.
    assert!(
        rendered.contains("\"content\":\"hi\""),
        "text delta must survive: {rendered}"
    );
    // The openai codec renders `StreamEventIR::End` as the terminal
    // `data: [DONE]` frame (no inline finish_reason on the wire in v1).
    assert!(
        rendered.contains("data: [DONE]"),
        "finish event must survive as the openai [DONE] frame: {rendered}"
    );
}

#[test]
fn same_protocol_openai_round_trips_unchanged() {
    // Sanity guard for the same-protocol branch: openai -> openai re-render
    // reproduces a well-formed delta frame.
    let codec = OpenAi;
    let upstream_chunk = br#"data: {"choices":[{"index":0,"delta":{"content":"x"}}]}"#;
    let events = codec.parse_stream_chunk(upstream_chunk).unwrap();
    assert_eq!(events.len(), 1);
    let mut buf = BytesMut::new();
    for ev in &events {
        codec.render_stream_chunk(ev, &mut buf).unwrap();
    }
    let rendered = std::str::from_utf8(&buf).unwrap();
    assert!(rendered.contains("\"delta\":{\"content\":\"x\"}"));
}
