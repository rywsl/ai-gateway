//! T3-A / G9 — `parse_stream_chunk` returns `Vec<StreamEventIR>`.
//!
//! S-codec T2 widened the return from `SmallVec<[_; 1]>` to `Vec<_>` so the
//! stateful codecs can emit 0..N events per chunk. The "no spill" invariant
//! from the SmallVec days is gone; the remaining invariant is:
//!
//! Gemini's "combined final event" carrying both `text` and `finishReason`
//! decomposes into a `[Delta, End]` pair — no silent text loss, no
//! synthesized End on the framer side.

use gw_codec::anthropic::Anthropic;
use gw_codec::gemini::Gemini;
use gw_codec::openai::OpenAi;
use gw_codec::ProtocolCodec;
use gw_core::ir::StreamEventIR;

#[test]
fn gemini_combined_final_event_splits_into_two() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"bye\"}]},\"finishReason\":\"STOP\"}]}\n\n";
    let evs = codec
        .parse_stream_chunk(&mut gw_codec::trait_def::EmptyStreamState, chunk)
        .unwrap();
    assert_eq!(evs.len(), 2, "expected Delta + End from combined event");
    match &evs[0] {
        StreamEventIR::Delta { text, .. } => {
            assert_eq!(text.as_deref().unwrap(), "bye");
        }
        other => panic!("expected first event Delta, got {other:?}"),
    }
    match &evs[1] {
        StreamEventIR::End { .. } => {}
        other => panic!("expected second event End, got {other:?}"),
    }
}

#[test]
fn openai_single_delta_does_not_spill() {
    let codec = OpenAi;
    let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::Delta { .. }));
}

#[test]
fn anthropic_single_delta_does_not_spill() {
    let codec = Anthropic;
    let chunk = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
    let evs = codec
        .parse_stream_chunk(&mut gw_codec::trait_def::EmptyStreamState, chunk)
        .unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::Delta { .. }));
}

#[test]
fn gemini_text_only_returns_single_delta() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]}}]}\n\n";
    let evs = codec
        .parse_stream_chunk(&mut gw_codec::trait_def::EmptyStreamState, chunk)
        .unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::Delta { .. }));
}

#[test]
fn gemini_finish_only_returns_single_end() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";
    let evs = codec
        .parse_stream_chunk(&mut gw_codec::trait_def::EmptyStreamState, chunk)
        .unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::End { .. }));
}
