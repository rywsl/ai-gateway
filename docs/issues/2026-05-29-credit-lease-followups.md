# Credit Lease Follow-ups

Recorded 2026-05-29 after the credit-lease feature merge (15 commits, `credit-lease-impl` → `main`).

These are known gaps, deferred items, and pre-existing issues surfaced (but not introduced) during implementation. Each is independently actionable; none blocks the credit-lease feature itself from shipping. Severity reflects estimated user/operator impact, not engineering effort.

Related spec: [`docs/spec/06-billing-ledger.md`](../spec/06-billing-ledger.md)
Related contract: [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](../go-contracts/contract-change/2026-05-29-credit-lease.md)

---

## 1. Inflight crash recovery loses lease attribution

**Status**: open

**Severity**: medium — affects accounting correctness on protected-mode crash recovery.

**What**: `crates/gw-ledger/src/inflight_reconciler.rs:122` constructs a `UsageRecord` with `credits_consumed: None` and `coef_table_version: None` on the crash-fallback path. For requests that originally went through step 8.5 (`lease_guard`-acquired), this means Go skips the `outstanding[user][pod]` decrement entirely — Go thinks the credits are still in flight even though Rust crashed mid-request.

**Why it matters**: Each crash-recovered protected-mode request that goes uncredited shrinks the user's effective lease pool by exactly the request's reserved credits, until Go's TTL GC reclaims the orphan outstanding entry (typically `lease_ttl_ms + outstanding_grace_ms` ≈ 5 min). During that window the user may see spurious `GW_QUOTA_LEASE_INSUFFICIENT`.

**Fix sketch**: persist `credits_consumed` (= the reserved amount at acquire time) and `coef_table_version` into the inflight hash at `upstream_request_filter` time, then have the reconciler emit them on the fallback UsageRecord. Spec §13 ("Partial usage persistence") covers a related but different concern; the fix here is simpler — reserve-time values are known and stable.

**Tests**: none yet. The existing `tests/integration/credit_lease_integration.rs::e2e_protected_user_request_uses_lease_path` is `#[ignore]` and would naturally cover the crash path once wired.

---

## 2. `gw_credit_lease_active_users` / `gw_credit_coef_table_version_active` gauges have no sampler

**Status**: open

**Severity**: medium — operational blind spot.

**What**: `crates/gw-gateway/src/metrics_catalog.rs` registers descriptions for both gauges (Task 11) but no background task ever sets their values. Both stay at 0 in `/metrics`. Code-level TODO comments mark the gap.

**Why it matters**: dashboards and alerts that key off "are any users in protected mode" or "which coef_table_version is live" silently report nothing. Operators relying on either gauge for the §13 staged rollout will not see the signal they need.

**Fix sketch**: a small `ManagedLoop` task in `gw-ledger` that, every ~10s, sets:
- `gw_credit_lease_active_users` = count of `LEASE_POOL` entries with `mode == Protected`
- `gw_credit_coef_table_version_active` = `current_coef_snapshot().version`

`gw-ledger` already has the `ManagedLoop` infrastructure (`InflightReaper`, `DeductGc`) — follow that pattern.

---

## 3. Integration tests flaky in parallel mode

**Status**: open, pre-existing pattern in `gw-ledger`

**Severity**: medium — CI noise risk.

**What**: `crates/gw-ledger/tests/credit_lease_integration.rs` cases `complete_coef_push_swaps_active_snapshot` and `coefs_for_six_dim_actual_usage` share global `COEF_CACHE` / `COEF_STAGING` with the inline unit tests in `coef.rs` and `credit_estimator.rs`. The inline unit tests use a private `TEST_COEF_LOCK` mutex; integration tests cannot access `pub(crate)` items, so they coordinate through unique-high-version numbers (≥ 1_000_000) instead.

Pass rate is stable in `cargo test -- --test-threads=1` and stable when running just the integration target. Occasional flap is possible when integration + unit tests interleave at high parallelism.

**Why it matters**: any future test in `gw-ledger` that touches `COEF_CACHE` will hit the same pattern. Without a uniform discipline, flake will reappear.

**Fix sketch options**:
1. Promote `TEST_COEF_LOCK` to a `pub fn for_test()` accessor in `coef.rs` so integration tests can lock the same mutex.
2. Move all integration tests that touch globals into a `serial_test` crate group.
3. Refactor `COEF_CACHE` / `COEF_STAGING` to be parameterized by an opaque `CoefCacheHandle` for tests (production keeps the global).

Option 1 is the cheapest; option 3 is the cleanest long-term but invasive.

---

## 4. e2e fixture for credit lease not wired

**Status**: open, scoped out of implementation plan

**Severity**: medium — spec §16 verification target unmet.

**What**: `tests/integration/credit_lease/end_to_end_test.rs` and the `#[ignore]`d `e2e_protected_user_request_uses_lease_path` in `credit_lease_integration.rs` are both placeholders. The full e2e flow (seed PG, push `coef_table` + `coef_table_commit` + `protected_lease` over Redis, fire request, assert UsageRecord shape) is documented in the placeholder but not exercised.

**Why it matters**: spec §16 calls for a soak with `--protected-mode` that asserts "at any instant, `usage:records.credits_consumed` cumulative ≤ total dispensed credits + overcommit." Without this, the headline invariant is only proven by reasoning, not by fuzzing.

**Fix sketch**: extend `tests/e2e/client/soak/` with a `--protected-mode` flag that drives Go-side stream pushes from the test harness. Reuse the existing `fake_upstream` fixture; instrumenting outstanding accounting on the Rust side is unnecessary because the soak's drain-time invariant already provides the bound.

---

## 5. Non-LLM paths (`gemini-files`, CLI variants) under protected mode

**Status**: open, design corner

**Severity**: low — fail-closed, but UX is sharp.

**What**: `request_filter` step 8.5 in `crates/gw-proxy/src/filters/request.rs` captures `req_max_tokens` only on LLM-protocol parse paths. The Gemini Files and CLI dispatch branches leave it at `None`. When a protected user hits one of those paths:
1. `req_max_tokens` defaults to `protection_cfg.default_output_ceiling_tokens` (8192).
2. `estimate_request_credits` looks up the model id (e.g., `"gemini-files"`) in the coef table.
3. Lookup fails → `EstimatorError::UnknownModel` → 402 `GW_QUOTA_LEASE_INSUFFICIENT`.

The request is rejected even though Gemini Files uploads have well-defined per-byte pricing on Go's side.

**Why it matters**: a protected user who tries to upload a file gets a confusing 402 with no path to recovery (the credits are fine; the model just isn't in the lease's coef view).

**Fix sketch**: two options.
1. (Preferred) Skip step 8.5 entirely for non-LLM protocol paths — fall through to the legacy `frozen_micro` soft path, which Go's existing usage:records pipeline already handles for file uploads.
2. Have Go publish coef entries for `gemini-files`-class pseudo-models in the same `coef_table` push, with input/output coefs that approximate the by-byte price.

Option 1 is one `if matches!(ctx.protocol, ...) { skip step 8.5 }` check. Option 2 requires Go-side schema thought.

---

## 6. Step 8.5 mode-flip race documented only in code comments

**Status**: open (documentation gap, behavior is correct)

**Severity**: low — design is correct, doc is thin.

**What**: between the initial `LEASE_POOL.get(&uid).map(|s| s.mode() == Protected)` check and `LeaseGuard::try_acquire`, a concurrent Go push of `mode=unprotected` can flip the state. `try_acquire` returns `LeaseError::NoLease`; step 8.5 falls through to step 9-10 (soft defense) and increments `gw_credit_lease_warmup_pending_total`.

This is correct behavior — the request gets served, not rejected — but [`docs/workflow.md`](../workflow.md) §step 8.5 should explicitly call it out so future readers don't mistake it for a bug.

**Fix**: a one-paragraph addition to `docs/workflow.md` describing the race window and why `NoLease` falls through rather than failing.

---

## 7. `tunnel_require_means_hard_fail` invariant (pre-existing, unrelated)

**Status**: pre-existing on `main`; NOT introduced by credit-lease branch

**Severity**: tracked elsewhere — included here only so future implementers stop flagging it as related to credit-lease.

**What**: `bash scripts/check-invariants.sh` fails on `crates/gw-proxy/src/filters/upstream.rs:381` with "require_tunnel without GW_TUNNEL_NO_NODE within 8 lines". Multiple credit-lease implementers independently verified this failure existed on `credit-lease-impl`'s base commit by stashing all credit-lease changes and re-running the script.

**Why it's here**: every Task report during the credit-lease implementation flagged this as "pre-existing, ignored". Future contributors will hit it again until the underlying issue is resolved.

**Fix**: out of scope. Belongs to the tunnel feature.

---

## 8. Staged rollout runbook missing

**Status**: open

**Severity**: medium — operational risk on first protected-mode user.

**What**: spec §13 describes a three-phase rollout (staging → production with `overcommit_ratio=0.1` → ratio ramp down to 0). No corresponding ops runbook exists. Key questions left unanswered:
- How does an operator confirm Go's outstanding ledger is healthy before flipping `enabled = true`?
- What's the rollback path mid-incident? (Likely: flip `enabled = false` and rolling-restart Pods, but this should be written down.)
- Which metrics/alerts should be wired before first protected user?

**Fix sketch**: a short ops runbook in `docs/runbooks/` (create directory if missing) covering pre-flight, ramp, and rollback for the protection layer. Should reference items 2 (gauge samplers) and 4 (e2e soak) as prerequisites.
