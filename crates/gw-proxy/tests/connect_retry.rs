//! T3-B (G10) — smoke test for the `fail_to_connect` retry hook.
//!
//! The hook itself can only run inside Pingora's proxy machinery (it takes a
//! live `Session` and `HttpPeer`). A pingora-driven end-to-end test that
//! forces a TCP-level connect failure on the first key and a recovered
//! connect on the second key is tracked under the docker black-box harness
//! (`feature/e2e-blackbox`).
//!
//! This file is a placeholder so the test layout matches the plan and so
//! later integration coverage can drop in next to the classifier checks
//! already covered by the `gw-routing::candidates_blacklist` test
//! (cross-key filtering) and the in-crate unit checks on the classifier.

#[ignore = "covered by gw-routing::candidates_blacklist (filter) and \
            the docker black-box harness on feature/e2e-blackbox (live retry)"]
#[test]
fn placeholder_e2e_retry() {
    // Intentionally empty.
}
