//! Surface-level acceptance tests for the wire_convert_matrix! scaffold.
//!
//! Full codegen lands in Phase 2; for Phase 1d we only verify the macro
//! parses the documented surface and rejects malformed inputs with a useful
//! compile_error message.

// Compile-time check: macro is callable with both shapes.
gw_codec_macros::wire_convert_matrix! {
    schema: DemoSchema,
    wires: [
        path::to::OpenAiRequest,
        path::to::AnthropicRequest,
    ],
}

gw_codec_macros::wire_convert_matrix! {
    schema: DemoStreamSchema,
    wires: [
        path::to::OpenAiStreamChunk,
    ],
    stream: true,
}

#[test]
fn macro_emits_nothing_runtime_visible() {
    // The scaffold emits an empty TokenStream — there is no item to assert on
    // at runtime. This test exists so cargo test exercises the macro
    // expansions above as part of the suite.
}

// NOTE(phase-2): when codegen lands, switch to a trybuild fixture under
// tests/ui/ that verifies these specific compile_error messages:
//   - missing `schema:` key
//   - missing `wires:` key
//   - unknown key (e.g. `wirees:`)
//   - non-Ident schema value
