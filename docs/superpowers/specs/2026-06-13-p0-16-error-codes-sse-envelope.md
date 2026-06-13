# P0 #16 — SSE error envelope GW_ code + dedicated user-not-found code

Date: 2026-06-13
Source of truth: Codex analysis (GitLab CNxus-Rust #16).
Scope: Rust-side error contract only. No Go edits. No commit/push/MR.

## Problem

1. `user_not_found` reuses `GW_AUTH_INVALID_APIKEY`, making a vanished user row
   indistinguishable from a bad API key (`crates/gw-auth/src/merge.rs`,
   `crates/gw-proxy/src/filters/request.rs`).
2. `error_passthrough` envelopes expose no stable `GW_` gateway code. OpenAI sets
   `error.code = null`; Anthropic/Gemini/Kiro have no gateway code at all. SSE
   `event: error` frames therefore carry nothing for clients/ops to branch on.

## Part A — dedicated user-not-found code

- Add `GW_AUTH_USER_NOT_FOUND` to `crates/gw-core/src/error/codes.rs` (HTTP 401,
  distinct from invalid/missing API key).
- `merge.rs::user_not_found()` → use the new code; rewrite the stale comment.
- `request.rs` missing-user load path (~line 380) → use the new code.
- Status stays **401** (work item is about code reuse, not status). Bad/missing
  API keys remain on `GW_AUTH_INVALID_APIKEY`.

## Part B — stable gateway code in passthrough envelopes

In `crates/gw-codec/src/error_passthrough.rs`, compute a stable `GW_` code and
add it to every protocol envelope **without** breaking vendor shapes:

- OpenAI: `error.code` = `GW_...` (was `null`).
- Anthropic: add `error.code` = `GW_...`, keep `error.type`.
- Gemini: keep numeric `error.code` (= upstream_status), add `error.gateway_code`.
- Kiro: keep `code` = vendor type, add `gateway_code`.

Derivation (`gateway_code`):
1. Preserve an upstream-supplied gateway code: scan `code`, `error.code`,
   `gateway_code`, `error.gateway_code` for a string starting with `GW_`.
2. Else map from `upstream_status`:
   - 401 → `GW_AUTH_INVALID_APIKEY`
   - 429 → `GW_UPSTREAM_OVERLOADED`
   - 5xx → `GW_UPSTREAM_5XX`
   - other 4xx → `GW_UPSTREAM_4XX`
   - else fall back to `ErrorClass`: AuthDenied→APIKEY, RateLimitedUpstream→OVERLOADED,
     UpstreamTransient→5XX, otherwise `GW_GATEWAY_INTERNAL`.

Message allow/deny masking is unchanged: it masks only `message`, never the code,
type, status, or numeric Gemini code.

## Tests (TDD, written first)

`crates/gw-auth/tests/merge.rs`:
- missing user → `Err` with code `GW_AUTH_USER_NOT_FOUND`, status 401.
- sub-account whose parent row is absent → same.

`crates/gw-codec/tests/error_passthrough.rs`:
- Anthropic SSE: parse `data:` JSON, assert `error.code` starts with `GW_`.
- OpenAI non-SSE 401: `error.code == "GW_AUTH_INVALID_APIKEY"` (not null).
- Gemini: numeric `error.code` preserved + `error.gateway_code` starts with `GW_`.
- Kiro: `code` = vendor type preserved + `gateway_code` starts with `GW_`.
- denied message masked but gateway code still present.
- upstream-supplied `GW_` code preserved.

## Verification

```
cargo fmt --all -- --check
cargo test -p gw-auth --test merge
cargo test -p gw-codec --test error_passthrough
```
