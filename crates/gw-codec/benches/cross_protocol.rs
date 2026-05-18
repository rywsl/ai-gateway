//! Cross-protocol IR render microbenchmark (AC-2.3-2).
//!
//! Measures the cost of parsing a request body via one codec into the
//! gateway IR and re-rendering it via a different codec. This is the path
//! exercised whenever a client speaks OpenAI but the routed vendor key is
//! configured for Anthropic / Gemini and `protocol_conversion = true`.
//!
//! Spec target (record gap as TODO if missed):
//!   P50 ≤ 800 µs / P99 ≤ 2 ms.
//!
//! Run with: `cargo bench -p gw-codec --bench cross_protocol`.

use bytes::BytesMut;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gw_codec::{anthropic::Anthropic, gemini::Gemini, openai::OpenAi, ProtocolCodec};

const OPENAI_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/openai_basic.json");
const ANTHROPIC_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/anthropic_basic.json");

fn bench_cross_protocol(c: &mut Criterion) {
    let openai = OpenAi;
    let anthropic = Anthropic;
    let gemini = Gemini;

    c.bench_function("openai_to_anthropic_via_ir", |b| {
        b.iter(|| {
            let ir = openai
                .parse_request(black_box(OPENAI_FIXTURE))
                .expect("parse openai");
            let mut buf = BytesMut::with_capacity(2048);
            anthropic
                .render_request(&ir, &mut buf)
                .expect("render anthropic");
            black_box(buf);
        });
    });

    c.bench_function("openai_to_gemini_via_ir", |b| {
        b.iter(|| {
            let ir = openai
                .parse_request(black_box(OPENAI_FIXTURE))
                .expect("parse openai");
            let mut buf = BytesMut::with_capacity(2048);
            gemini.render_request(&ir, &mut buf).expect("render gemini");
            black_box(buf);
        });
    });

    c.bench_function("anthropic_to_openai_via_ir", |b| {
        b.iter(|| {
            let ir = anthropic
                .parse_request(black_box(ANTHROPIC_FIXTURE))
                .expect("parse anthropic");
            let mut buf = BytesMut::with_capacity(2048);
            openai.render_request(&ir, &mut buf).expect("render openai");
            black_box(buf);
        });
    });
}

criterion_group!(benches, bench_cross_protocol);
criterion_main!(benches);
