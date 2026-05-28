//! Fingerprint-verification integration test, gated behind the
//! `fingerprint-verify` Cargo feature.
//!
//! Drives a request through the same `ClientPool` the production loopback
//! uses, with `ImpersonateProfile::ClaudeCodeChrome131`, against a JA3/JA4
//! echo service. The echo service is expected to return a JSON body whose
//! `"ja4"` (or `"ja4_r"`) field reports the client's fingerprint.
//!
//! How to run
//! ----------
//! ```
//! export GW_FINGERPRINT_ECHO_URL=https://your-echo-host/  # required
//! cargo test -p gw-egress-impersonate \
//!     --features fingerprint-verify \
//!     --test fingerprint_verify \
//!     -- --nocapture
//! ```
//!
//! What it asserts
//! ---------------
//! 1. The pool builds a `Client` for the Claude Code profile without error.
//! 2. The echo service receives a TLS handshake the request reaches HTTP-200.
//! 3. The echo response carries a `"ja4"` string field (the actual hash value
//!    is logged but not pinned — the wreq emulation pool tracks upstream
//!    Chrome major versions and we don't want a hash bump to fail CI). For
//!    hash-pinning, snapshot the value with `insta` in a follow-up and
//!    update on every wreq bump.
//!
//! Why feature-gated
//! -----------------
//! The echo service is external infrastructure. Default workspace tests must
//! not depend on it; only nightly CI (`.github/workflows/header-drift.yml`
//! or similar) opts in via `--features fingerprint-verify` once the secret
//! `GW_FINGERPRINT_ECHO_URL` is configured.

#![cfg(feature = "fingerprint-verify")]

use std::env;
use std::time::Duration;

use gw_core::impersonate::ImpersonateProfile;
use gw_egress_impersonate::pool::{ClientPool, PoolKey};

#[tokio::test]
async fn claude_code_emits_chrome_class_ja4() {
    let echo = env::var("GW_FINGERPRINT_ECHO_URL")
        .expect("set GW_FINGERPRINT_ECHO_URL to run fingerprint-verify");

    let pool = ClientPool::new(2);
    let key = PoolKey {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
        proxy: None,
    };
    let client = pool
        .get_or_create(key)
        .await
        .expect("ClientPool::get_or_create failed");

    let resp = client
        .get(&echo)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .expect("echo request failed");

    let status = resp.status();
    assert!(status.is_success(), "echo returned {status}");

    let body: serde_json::Value = resp.json().await.expect("echo body was not JSON");

    let ja4 = body
        .get("ja4")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("ja4_r").and_then(|v| v.as_str()))
        .unwrap_or_else(|| panic!("echo response missing ja4 / ja4_r field: {body}"));

    eprintln!("[fingerprint-verify] ClaudeCodeChrome131 ja4 = {ja4}");

    assert!(
        ja4.starts_with("t13d"),
        "expected TLS 1.3 client (ja4 t13d* prefix), got {ja4}",
    );
}
