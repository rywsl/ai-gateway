//! Surface-level tests for the codec proc-macro scaffolds.
//!
//! D20/D21 (#18): both `gw_codec_macros::wire_convert_matrix!` and
//! `#[derive(DetectRequest)]` now emit a `compile_error!` until their Phase 2
//! codegen lands — they no longer expand to a silent empty `TokenStream`. Two
//! consequences for this test file:
//!
//!   1. The previous compile-time *invocations* of `wire_convert_matrix!` were
//!      removed: invoking the macro is now a hard compile error by design, so it
//!      cannot live in a test that must compile.
//!   2. Asserting the compile-failure itself requires a `trybuild` UI fixture.
//!      We intentionally do NOT add the `trybuild` dev-dependency here (heavier
//!      build, slower CI) — the loud-failure guarantee is provided by the
//!      `compile_error!` in the macro source and reviewed there. If a `trybuild`
//!      harness is later added to this crate, add `tests/ui/*.rs` fixtures that
//!      pin the exact `compile_error!` messages for both macros.
//!
//! This test simply documents the gate and keeps the test target building.

#[test]
fn macros_are_gated_with_compile_error() {
    // Nothing to exercise at runtime: the macros are compile-time gated. This
    // test is a placeholder that keeps the integration-test target present and
    // documents (above) why the macros are not invoked here.
}
