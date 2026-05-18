//! Codec same-protocol passthrough microbenchmark (AC-2.3-1).
//!
//! Measures the hot path of `same_protocol_passthrough` with both
//! `Mutations::NONE` (the borrow-only fast path that returns
//! `Cow::Borrowed(input)`) and a non-empty mutation (forcing a parse +
//! re-render). Inputs are sourced from the in-tree test fixtures so we don't
//! duplicate sample bodies; for a slightly bigger payload we also feed a
//! synthetic ~6 KB OpenAI request constructed at bench-init time.
//!
//! Spec target (record gap as TODO if missed):
//!   P50 ≤ 200 µs / P99 ≤ 800 µs at ~8 KB.
//!
//! Run with: `cargo bench -p gw-codec --bench passthrough`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gw_codec::{openai::OpenAi, passthrough::same_protocol_passthrough};
use gw_core::ir::Mutations;

const SMALL_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/openai_basic.json");

/// Build a synthetic ~6 KB OpenAI request body. Constructed via `serde_json`
/// at bench-init time (i.e. outside the timed loop) so the steady-state
/// measurement reflects only `same_protocol_passthrough`.
fn synth_large_openai_body() -> Vec<u8> {
    let mut messages = Vec::new();
    messages.push(serde_json::json!({
        "role": "system",
        "content": "You are a helpful assistant that answers concisely.",
    }));
    // ~50 user turns of moderate length lands us near 6-8 KB after serde.
    for i in 0..50 {
        messages.push(serde_json::json!({
            "role": "user",
            "content": format!(
                "Turn {i}: please summarise the following paragraph in three bullet \
                 points and identify any factual inaccuracies if present."
            ),
        }));
    }
    let req = serde_json::json!({
        "model": "gpt-4",
        "messages": messages,
        "max_tokens": 1024,
        "temperature": 0.7,
    });
    serde_json::to_vec(&req).expect("serialize synth body")
}

fn bench_passthrough(c: &mut Criterion) {
    let codec = OpenAi;
    let large = synth_large_openai_body();

    c.bench_function("openai_same_proto_no_mutation_small", |b| {
        b.iter(|| {
            let out = same_protocol_passthrough(
                &codec,
                black_box(SMALL_FIXTURE),
                black_box(&Mutations::NONE),
            )
            .expect("passthrough");
            black_box(out);
        });
    });

    c.bench_function("openai_same_proto_no_mutation_6kb", |b| {
        b.iter(|| {
            let out = same_protocol_passthrough(
                &codec,
                black_box(&large),
                black_box(&Mutations::NONE),
            )
            .expect("passthrough");
            black_box(out);
        });
    });

    c.bench_function("openai_same_proto_with_max_tokens_mutation_6kb", |b| {
        // Setting `set_max_tokens` flips the mutation bitset, forcing the
        // slow path (parse + re-render). This is the upper bound for the
        // same-protocol path.
        let mut m = Mutations::NONE;
        m.set_max_tokens();
        b.iter(|| {
            let out = same_protocol_passthrough(&codec, black_box(&large), black_box(&m))
                .expect("passthrough");
            black_box(out);
        });
    });
}

criterion_group!(benches, bench_passthrough);
criterion_main!(benches);
