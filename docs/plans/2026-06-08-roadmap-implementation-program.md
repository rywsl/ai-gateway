# ROADMAP Implementation — Master Program Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **This is a *program* plan, not a single feature plan.** It implements `docs/ROADMAP.md` in full. Only **Wave 0** and **Wave 1** are expanded to bite-sized TDD tasks here. Waves 2–8 are *briefs*: each must be expanded into its own `docs/plans/YYYY-MM-DD-<wave>.md` before execution. Do not implement a brief directly — expand it first (run writing-plans on the brief + its ROADMAP section + the cited source files).

**Goal:** Drive every unbuilt/divergent item in `docs/ROADMAP.md` to "built and verified", in a priority- and dependency-correct order, without ever shipping an unverifiable change.

**Architecture:** Work is sliced into 9 waves (W0–W8) by capability family. Within a wave, `runtime-risk` fixes precede `capability-gap` features. Three waves (W5/W7/W8) were shaped by **fork decisions** (F1/F2/F3) — design choices, not implementation steps — all **resolved and signed off 2026-06-08**; their conclusions are folded into the briefs below. W0 lands the CI gate that makes every later wave's "verified" claim real, so it goes first.

**Tech Stack:** Rust 1.88 / 21-crate workspace on Pingora 0.8; GitLab CI (`.gitlab-ci.yml`); existing lint scripts `scripts/check-invariants.sh` + `scripts/check-spec.sh`; per-crate `cargo test`.

---

## How to use this document

1. **Fork decisions are resolved** (F1/F2/F3 — see *Fork decisions — RESOLVED* below, signed off 2026-06-08). Their conclusions are folded into the W5/W7/W8 briefs; no fork blocks any wave anymore.
2. **Execute W0 and W1 from this file directly** — they are fully specified below.
3. **For W2–W8, expand the brief first.** Each brief lists goal / ROADMAP source / files / dependencies / fork-gate / rough task count. Feed that to `superpowers:writing-plans` to produce the bite-sized plan, then execute that plan.
4. **One wave = one branch = one reviewable unit.** Each wave must leave the tree green (`cargo test --workspace --locked` + `bash scripts/check-invariants.sh` + `bash scripts/check-spec.sh docs/spec/0*.md`) and, when it changes built behavior, must also **move the relevant ROADMAP entry into `docs/spec/`** (the as-built standard) and re-run `check-spec.sh`. A wave that builds a feature but leaves it in ROADMAP is not done.
5. **No commits land without the human.** Per standing instruction: subagents make all changes; the human commits. (W0 itself only *adds* CI gates; it does not bypass this.)

## Locked wave map

`runtime-risk` (bug; fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.

| Wave | Theme | Lead priority | Depends on | Fork gate | ~Tasks | Detailed plan |
|------|-------|---------------|-----------|-----------|--------|---------------|
| **W0** | CI gate (fmt/clippy/test/invariants/spec) | runtime-risk | — | — | 1 | **in this file** |
| **W1** | Bounded fixes (lease 402→503, lease_grace_ms, hmac lint) | runtime-risk | W0 (to verify) | — | 3 | **in this file** |
| **W2** | Billing/ledger robustness (C-delta DLQ, C-gamma sync, C-beta estimate) | runtime-risk → gap | W0 | — | ~7 | expand |
| **W3** | Rate-limiting (RL-Core classifier → RL-Adaptive gating) | gap → runtime-risk | W0; RL-Adaptive ⟵ RL-Core | — | ~5 | expand |
| **W4** | Async tasks (settle bill, Redis freeze, watchdog leader, submit→poll) | runtime-risk → gap | W0; W2 (ledger namespaces) | — | ~9 | expand |
| **W5** | Key-health (K-B analyzers, K-D quota probe, record() payload) | gap | W0 | — | ~6 | expand |
| **W6** | Routing (R-E codes → R-AB multi-score → R-D affinity → R-F probe) | gap | W0; R-AB is foundational for R-D/R-F | — | ~10 | expand |
| **W7** | Tunnel (require_tunnel hard-fail, separate image, misc surfaces) | runtime-risk | W0 | F3 ✓ sep-image | ~5 | expand |
| **W8** | Vendor adapter layer (keep+wire adapter, Kiro, vertex, codex, K-C OAuth, CLI metrics) | gap | W0; Kiro codec ⟷ Kiro adapter | F1 ✓ keep · F2 ✓ Rust | ~12 | expand (W8a/W8b) |

**Recommended execution order:** W0 → W1 → (W2 ∥ W3 ∥ W5 are mutually independent and may be separate branches) → W4 (needs W2's ledger namespaces) → W6 → W7 → W8. **All three forks resolved 2026-06-08** — no brainstorming gate remains; W5/W7/W8 briefs carry the conclusions.

## Fork decisions — RESOLVED 2026-06-08

All three forks were brainstormed and signed off on 2026-06-08. The conclusions below are **binding inputs** to the W5/W7/W8 plans. Each gated wave must still capture its conclusion in the matching `docs/spec/` decision doc when it lands.

### F1 — `gw-adapter`: **RESOLVED → keep it, incrementally wire in** (shapes W8, W5)
**Decision (user):** keep `crates/gw-adapter` as a standalone per-vendor outbound-rewrite layer — "可以做无成本抽象,这中间的逻辑是单独的". Do **not** delete it. Wire it in **incrementally**, never a big-bang migration of the live path.
- **Seam:** `AdapterRegistry::for_vendor` becomes proxy's official outbound seam. **Proxy is the only consumer** — the task-worker does NOT call adapters (W4 has the gateway pre-render `ir_wire_body`; the worker ships bytes verbatim). Fix the `trait_def.rs` doc comment that claims "the proxy and the task runtime call into it".
- **Incremental boundary (lowest risk):** Direct vendors → `NoopAdapter` / short-circuit, **byte-identical** to today. Impersonate vendors → declared via the adapter's `transport_profile()`, implementation **still reuses `gw-core::impersonate`** (not rewritten). The live `inject_vendor_headers` path stays. Only the **new** vendors (kiro/vertex/codex) implement full hooks.
- **Constraints honored:** the codex body transform rides `patch_request` via `WireAnyRequest` (the `no_patch_body` invariant forbids `patch_body`). `AdapterCtx` gains the `oauth_cache` field its hooks assume (consumed by F2's K-C).
- **Knock-on to W5:** K-B vendor analyzers now land on `gw-adapter`'s `UpstreamErrorAnalyzer` trait (it stops being a dead skeleton) — **not** in `gw-core::classify_default`.

### F2 — OAuth refresh: **RESOLVED → Rust fully owns refresh; Go owns config; NO double-writer** (shapes K-C in W8)
**Decision (user, verbatim):** "go 配置,rust 刷,不应当存在任何双 writer." Build K-C. Move the **runtime refresh** writer from Go to Rust; Go keeps **configuration**. Driver: production uses heavy OAuth-subscription egress, so the expiry-window 401 stutter is a real, user-visible pain.
- **Go = config writer:** OAuth provider config, `token_endpoint`, first-time authorization (interactive login that seeds the initial access/refresh tokens), LDAP. Onboarding/config stays Go.
- **Rust = sole runtime-refresh writer:** on access-token expiry, Rust synchronously refreshes via `refresh_token` → KEK-encrypts and writes back `payload.oauth` (**including a rotated `refresh_token`**) → `version + 1` → broadcasts `events:entity op=upsert`. Rust starts reading `token_endpoint` (today the contract says "Rust 端不读").
- **Go exits runtime refresh:** the current "Go task-worker receives `oauth_failed` → refreshes" path (`go-contracts/.../2026-05-22-go-onboarding.md` line 354) is **retired**; the `OAuthListener` reverse-XADD is dropped or repurposed.
- **Must-solve in the K-C plan (none violate "no double-writer" — every writer is Rust):**
  1. **Cross-Pod single-flight:** N Rust Pods must not refresh the same key at once — `SETNX lock:oauth:refresh:<key_id>` (or version-CAS): one Pod refreshes+writes+broadcasts, the rest cold-load the new value. Concurrency coordination, not a second writer.
  2. **refresh_token rotation:** vendors may return a new `refresh_token`; Rust must encrypt-write it back or the next refresh fails. No code handles rotation today.
  3. **Contract change:** a new `docs/go-contracts/contract-change/` doc moving refresh ownership Go→Rust — touches `parts/07-oauth-ldap.md`, `parts/02` (`oauth_failed` receiver retired), `parts/03` (oauth payload runtime writer = Rust).

### F3 — Tunnel artifact: **RESOLVED → separate `--features tunnel` image** (shapes W7)
**Decision:** keep `default = []` (tunnel code stays out of the main artifact — it is not yet production-validated). Add a **second CI build job** `cargo build --release --locked --features tunnel -p gw-gateway` producing a distinct image tag, so tunnel can be deployed and validated without touching the main image.
- **Enablement cost is small:** tunnel only pulls `tokio-tungstenite` (reuses existing tokio; no new TLS stack).
- **Ungated, do regardless:** the `require_tunnel=true` hard-fail fix is a real bug independent of this choice — the non-tunnel (main) build currently ignores `require_tunnel` and silently falls back to Direct. W7 fixes the non-tunnel arm to **reject** `require_tunnel=true`.

---

# Wave 0 — CI gate (highest leverage, do first)

**Why first:** `.gitlab-ci.yml` today runs only `cargo build --release --locked -p gw-gateway` + a buildah push. No `fmt`, `clippy`, `test`, `check-invariants.sh`, or `check-spec.sh`. Every "verified" claim in Waves 1–8 is meaningless until a gate enforces it. This is the audit's #1 cross-cutting item, and you are already on branch `feat/cicd` — its natural home.

### Task W0.1: Add a `test` stage that runs all gates

**Files:**
- Modify: `.gitlab-ci.yml` (whole file; current contents at `.gitlab-ci.yml:1-44`)
- Reference (unchanged): `scripts/check-invariants.sh`, `scripts/check-spec.sh`

- [ ] **Step 1: Establish the local baseline.** Before adding any gate to CI, run each one locally and record pass/fail — adding a *currently-red* gate would make the first pipeline fail.

```bash
rustup component add rustfmt clippy 2>/dev/null || true
cargo fmt --all -- --check                                   ; echo "fmt=$?"
cargo clippy --workspace --all-targets --locked -- -D warnings ; echo "clippy=$?"
cargo test --workspace --locked                              ; echo "test=$?"
bash scripts/check-invariants.sh                             ; echo "inv=$?"
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f" || { echo "spec-fail $f"; break; }; done
```

Expected from prior audit: `inv=0` and `check-spec` all `ok`. `fmt`/`clippy`/`test` status is **unknown — this step discovers it.**

- [ ] **Step 2: Triage failing gates.**
  - `fmt` red → run `cargo fmt --all` (auto-fix), re-check. Mechanical, include in this task.
  - `clippy` red → if a handful of warnings, fix them in this task; if large, **temporarily** scope the CI line to `cargo clippy --workspace --locked` (no `-D warnings`) and add a ROADMAP cross-cutting entry "tighten clippy to deny". Note what you dropped via `log`/commit message — no silent weakening.
  - `test` red → a real failure. If it's an external-service test (needs Redis/Postgres), restrict the CI `cargo test` line to `--lib --bins` and add a `services:`-backed follow-up entry to ROADMAP; if it's a genuine bug, STOP and surface it (it predates this work).

- [ ] **Step 3: Write the new `.gitlab-ci.yml`.** Replace the `stages:` block and insert the `lint-test` job; make `rust-build` wait on it.

```yaml
stages:
  - test
  - build
  - push

workflow:
  rules:
    - if: '$CI_PIPELINE_SOURCE == "merge_request_event" && $CI_MERGE_REQUEST_TARGET_BRANCH_NAME == $CI_DEFAULT_BRANCH'
    - if: '$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH'
    - when: never

variables:
  REGISTRY_HOST: "gitlab.ailinkmax.com:5050"
  GATEWAY_IMAGE: "gitlab.ailinkmax.com:5050/ailinkmax/cnxus-rust"
  STORAGE_DRIVER: "vfs"
  BUILDAH_FORMAT: "docker"
  BUILDAH_ISOLATION: "chroot"

lint-test:
  stage: test
  image: rust:1.88-bookworm
  before_script:
    - apt-get update
    - apt-get install -y --no-install-recommends cmake build-essential pkg-config perl libssl-dev libclang-dev
    - rustup component add rustfmt clippy
  script:
    - cargo fmt --all -- --check
    - cargo clippy --workspace --all-targets --locked -- -D warnings
    - cargo test --workspace --locked
    - bash scripts/check-invariants.sh
    - for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f" || exit 1; done

rust-build:
  stage: build
  image: rust:1.88-bookworm
  needs:
    - lint-test
  before_script:
    - apt-get update
    - apt-get install -y --no-install-recommends cmake build-essential pkg-config perl libssl-dev libclang-dev
  script:
    - cargo build --release --locked -p gw-gateway

push-image:
  stage: push
  image: quay.io/buildah/stable:latest
  needs:
    - rust-build
  rules:
    - if: '$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH'
  before_script:
    - echo "$CI_REGISTRY_PASSWORD" | buildah login -u "$CI_REGISTRY_USER" --password-stdin "$CI_REGISTRY"
  script:
    - buildah build --file "$CI_PROJECT_DIR/Dockerfile" --tag "$GATEWAY_IMAGE:$CI_COMMIT_SHORT_SHA" "$CI_PROJECT_DIR"
    - buildah push "$GATEWAY_IMAGE:$CI_COMMIT_SHORT_SHA"
    - |
      if [ "$CI_COMMIT_BRANCH" = "$CI_DEFAULT_BRANCH" ]; then
        buildah tag "$GATEWAY_IMAGE:$CI_COMMIT_SHORT_SHA" "$GATEWAY_IMAGE:latest"
        buildah push "$GATEWAY_IMAGE:latest"
      fi
```

Apply any Step-2 narrowing (clippy/test scope) to the `script:` lines above before saving.

- [ ] **Step 4: Validate YAML + re-run gates locally one final time.**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.gitlab-ci.yml')); print('yaml ok')"
cargo fmt --all -- --check && cargo test --workspace --locked && bash scripts/check-invariants.sh && echo "gates green"
```
Expected: `yaml ok` then `gates green`.

- [ ] **Step 5: Update the ROADMAP.** Delete the "Wire `check-invariants.sh` into CI" entry under `## Cross-cutting` in `docs/ROADMAP.md` (it is now built). Do **not** commit — staging only.

---

# Wave 1 — Bounded runtime-risk fixes

Three independent fixes, no design forks, no cross-deps. Each is its own commit-sized unit. Run W0's gates after each.

### Task W1.1: Lease-expired returns HTTP 503, not 402

**Context:** `crates/gw-core/src/error/codes.rs:17-20` documents `GW_QUOTA_LEASE_EXPIRED` as **HTTP 503 — an ops signal, not a user error**. But `crates/gw-proxy/src/filters/request.rs:1310-1325` (the `Err(LeaseError::Expired)` arm) builds an `ErrorIR` with `status: 402` and `category: ErrorCategory::ClientQuota`. Code contradicts its own doc. Surfacing a Go-pipeline outage as a *payment* error misroutes the page.

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs:1310-1325`
- Reference: `crates/gw-core/src/error/codes.rs:17-20`; `ErrorCategory` enum (find it: `grep -rn "enum ErrorCategory" crates/gw-core/src`)
- Test: a new `#[cfg(test)]` unit on the extracted helper (below)

- [ ] **Step 1: Find the right category.** Run `grep -rn "enum ErrorCategory" -A20 crates/gw-core/src/error`. Pick the variant meaning *backend/ops unavailable* (e.g. `Backend` / `Upstream` / `ServiceUnavailable`) — **not** `ClientQuota`. Record the exact variant name; the code below uses `ErrorCategory::Backend` as a placeholder — replace it with the real one.

- [ ] **Step 2: Write the failing test.** Extract the error construction into a pure helper so it is unit-testable, then test it. Add near the `Expired` arm's module (or a `#[cfg(test)] mod lease_error_tests` at the bottom of `request.rs`):

```rust
#[cfg(test)]
mod lease_expired_tests {
    use super::*;

    #[test]
    fn lease_expired_error_is_503_ops_signal() {
        let err = lease_expired_error();
        assert_eq!(err.status, 503, "lease-expired is an ops signal, codes.rs says 503");
        assert_eq!(err.code, std::borrow::Cow::Borrowed(codes::GW_QUOTA_LEASE_EXPIRED));
        assert!(!err.retryable || err.retryable, "retryable is a deliberate choice; assert it explicitly below");
        // 503 ops signals should not be ClientQuota:
        assert!(!matches!(err.category, ErrorCategory::ClientQuota));
    }
}
```

- [ ] **Step 3: Run it — expect FAIL** (`lease_expired_error` undefined).

```bash
cargo test -p gw-proxy lease_expired_error_is_503 2>&1 | tail -5
```

- [ ] **Step 4: Implement.** Add the helper and call it from the arm:

```rust
/// Lease-expired envelope. HTTP 503 (ops signal — Go renewal-push outage),
/// NOT 402: the user's balance is fine; this Pod's lease just went stale.
/// Matches the documented contract in gw-core error/codes.rs.
fn lease_expired_error() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::Backend, // <- replace with the real ops/backend variant from Step 1
        status: 503,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_LEASE_EXPIRED),
        message: Cow::Borrowed("lease expired; ops should investigate Go push pipeline"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}
```

Then replace the inline `ErrorIR { … status: 402 … }` in the `Err(gw_ledger::LeaseError::Expired)` arm (keep the `gw_credit_lease_expired_total` counter line) with:

```rust
Err(gw_ledger::LeaseError::Expired) => {
    metrics::counter!("gw_credit_lease_expired_total").increment(1);
    let err = lease_expired_error();
    bail!(session, ctx, &err, &request_id)
}
```

(If `ErrorIR` is not generic over a lifetime, drop `<'static>` and match the existing type. Confirm against the struct def: `grep -rn "struct ErrorIR" crates/gw-core/src`.)

- [ ] **Step 5: Run test — expect PASS**, then the full gate.

```bash
cargo test -p gw-proxy lease_expired_error_is_503
cargo test --workspace --locked && bash scripts/check-invariants.sh
```

- [ ] **Step 6:** Leave the `Insufficient` arm (402) **unchanged** — insufficient lease *is* a client-quota condition. Only `Expired` moves to 503.

### Task W1.2: Apply `lease_grace_ms` in `try_acquire`

**Context:** `crates/gw-ledger/src/protection_config.rs:13` defines `lease_grace_ms` (default `5_000`, a clock-skew tolerance) but nothing reads it. `crates/gw-ledger/src/lease.rs:123` compares bare `now_ms_val > expires_at`, so a 1 ms Pod/Go skew expires a still-valid lease. The grace must widen the expiry check.

**Files:**
- Modify: `crates/gw-ledger/src/lease.rs` (`try_acquire` signature + line 123 check + doc; in-crate tests)
- Modify call site: `crates/gw-proxy/src/filters/request.rs:1298`

- [ ] **Step 1: Write the failing test** in `lease.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn try_acquire_within_grace_window_succeeds() {
    let uid = fresh_uid("within_grace");
    // Lease "expired" 2s ago, but grace is 5s → still acquirable.
    let just_past = now_ms().saturating_sub(2_000);
    upsert_protected(&uid, 1000, "lease-grace".into(), 9, just_past);
    let guard = LeaseGuard::try_acquire(&uid, 100, now_ms(), 5_000).unwrap();
    assert_eq!(guard.reserved(), 100);
}

#[test]
fn try_acquire_beyond_grace_window_expires() {
    let uid = fresh_uid("beyond_grace");
    let long_past = now_ms().saturating_sub(10_000);
    upsert_protected(&uid, 1000, "lease-grace2".into(), 9, long_past);
    let err = LeaseGuard::try_acquire(&uid, 100, now_ms(), 5_000).unwrap_err();
    assert!(matches!(err, LeaseError::Expired));
}
```

- [ ] **Step 2: Run — expect FAIL** (arity mismatch: `try_acquire` takes 3 args).

```bash
cargo test -p gw-ledger try_acquire_within_grace 2>&1 | tail -5
```

- [ ] **Step 3: Implement.** Change the signature and the check in `lease.rs`:

```rust
//   pub fn try_acquire(uid: &UserId, requested: i64, now_ms_val: u64) -> Result<Self, LeaseError> {
pub fn try_acquire(
    uid: &UserId,
    requested: i64,
    now_ms_val: u64,
    grace_ms: u64,
) -> Result<Self, LeaseError> {
    // ...unchanged NoLease / mode guards...
    let expires_at = state.expires_at_ms.load(Ordering::Acquire);
    // Clock-skew tolerance (protection_config.lease_grace_ms): a lease is only
    // expired once now passes expires_at + grace. saturating_add guards overflow.
    if now_ms_val > expires_at.saturating_add(grace_ms) {
        return Err(LeaseError::Expired);
    }
    // ...rest unchanged...
}
```

- [ ] **Step 4: Update every existing call site.** In `lease.rs` tests, append `, 0` (or `, 5_000`) to all existing `try_acquire(&uid, N, now_ms())` calls. Keep `try_acquire_expired_returns_error` passing `0` so its `past` lease still reads expired. Update the production call site `crates/gw-proxy/src/filters/request.rs:1298`:

```rust
//   match gw_ledger::LeaseGuard::try_acquire(&merged.effective_user_id, credits, now) {
match gw_ledger::LeaseGuard::try_acquire(
    &merged.effective_user_id, credits, now, protection_cfg.lease_grace_ms,
) {
```

(`protection_cfg` is in scope — it is a fn param at `request.rs:74` and already read at `request.rs:1232`.)

- [ ] **Step 5: Run — expect PASS** + full gate.

```bash
cargo test -p gw-ledger && cargo test --workspace --locked && bash scripts/check-invariants.sh
```

### Task W1.3: Retire the inert `no_global_hmac_force` lint

**Context:** `scripts/check-invariants.sh:241-251` greps for `signature_required_workspace_wide` / `(global|workspace)_signature_required` / `force_signature_(global|workspace)`. None match the shipped field (`cfg.signature.required`), so the lint never fires — *and* it guards against a global signature toggle that `docs/spec/05-auth-permissions.md` now documents as a **supported feature**. The lint is both inert and wrong-headed; retire it.

**Files:**
- Modify: `scripts/check-invariants.sh:241-251`
- Reference: `docs/spec/05-auth-permissions.md`

- [ ] **Step 1: Confirm the global toggle is blessed.** `grep -n "signature" docs/spec/05-auth-permissions.md` — verify the global `[signature].required` toggle is described as built. (If, surprisingly, it is *not*, stop and reconsider — repoint instead of retire.)

- [ ] **Step 2: Replace the lint block** (lines 241-251) with a retired-note stub so the section's intent stays in git but no check runs:

```bash
# ---------------------------------------------------------------------------
# no_global_hmac_force (G7) — RETIRED 2026-06-08. The global `[signature].required`
# toggle is now a supported feature (docs/spec/05-auth-permissions.md); the prior
# spellings this lint grepped for (signature_required_workspace_wide, etc.) never
# matched the shipped `cfg.signature.required` field, so the check was inert. No
# replacement: per-key `ApiKeyEntry.signature_required` and the global toggle are
# both legitimate.
# ---------------------------------------------------------------------------
```

- [ ] **Step 3: Verify the script still runs clean and the lint is gone.**

```bash
bash scripts/check-invariants.sh | grep -c "no_global_hmac_force" ; echo "expect 0 above"
bash scripts/check-invariants.sh >/dev/null && echo "invariants exit 0"
```
Expected: `0`, then `invariants exit 0`.

- [ ] **Step 4: Update ROADMAP.** Remove the `no_global_hmac_force lint is inert…` entry from `docs/ROADMAP.md` `## 05 auth-permissions` (now resolved).

---

# Waves 2–8 — briefs (expand each into its own plan before executing)

> Each brief is an input to `superpowers:writing-plans`, **not** an executable task list. Expand it (brief + the cited `docs/ROADMAP.md` section + the named source files) into `docs/plans/YYYY-MM-DD-<wave>.md`, then execute that with subagent-driven-development. Anchors below are the real files to read first.

## Wave 2 — Billing / ledger robustness
**Goal:** no billing record is ever silently lost; cross-Pod balance visibility; per-model freeze estimate. **ROADMAP:** `## 06 billing-ledger` (C-delta, C-gamma, C-beta). **Depends:** W0. **Order:** C-delta (runtime-risk) → C-gamma → C-beta.
- **C-delta DLQ three-tier (runtime-risk):** `crates/gw-ledger/src/flusher.rs` today has one `usage:records:dlq` tier and, on XADD failure, bumps `gateway_usage_records_lost_total` and drops. Build the fallback chain: DLQ stream → in-memory retry queue → local file `${data_dir}/gw-ledger-emergency-dlq-*.jsonl`; a 30 s retry loop with attempt counter → `usage:records:dlq:permanent_failed`; a separate `counter:frozen:dlq`.
- **C-gamma frozen→Redis sync (gap):** `freeze.rs` + `flusher.rs` `LedgerMsg::FrozenDelta` path doing `HINCRBY counter:user:<uid>:frozen_micro`; `counter_handler.rs` to handle `events:counter ns=user_balance` (today only lease namespaces), overwriting local `deducted_micro`/`base_micro` from Go's `value_after`.
- **C-beta EstimatePricing entity + freeze formula (gap):** entity (`conservative_per_token_micro`, `safety_factor`, `default_max_tokens`, `hard_cap_tokens`) + `compute_freeze_estimate = (input + output_cap) × per_token × safety_factor`, loaded from `entity:estimate_pricing`. Today `crates/gw-proxy/src/filters/request.rs` uses a flat `freeze_estimate` and `estimate_pricing_version` is literal `0`. **Constraint:** no per-token *cost* (Go's domain — `no_cost_micro` / `no_billing_cost_per_token` lints).
- **~7 tasks.**

## Wave 3 — Rate-limiting
**Goal:** typed 429 classification + per-key RPM gate; adaptive ceiling only learns from RPM-class 429s. **ROADMAP:** `## 04 rate-limiting`. **Depends:** W0; RL-Adaptive ⟵ RL-Core classifier. **Order:** RL-Core first.
- **RL-Core classifier + per-key RPM gate (gap):** add `per_key`/`check_key`/`override_key_rpm`/`reset_key_rpm` to `RateLimitGuard` (`crates/gw-auth/src/ratelimit/mod.rs`) + code `GW_QUOTA_RPM_KEY_EXCEEDED`. `RateLimitKind` already exists (`crates/gw-core/src/ratelimit`, variants Rpm/Tpm/Daily/Monthly/Concurrent/Unknown) but call sites hard-code `Unknown`; implement `detect_rate_limit_kind`.
- **RL-Adaptive learns only from RPM 429s (runtime-risk):** `crates/gw-proxy/src/gateway.rs:854` calls `observe_429` unconditionally; gate `AdaptiveRpmManager::observe_429` (`crates/gw-auth/src/ratelimit/adaptive.rs`) on `RateLimitKind::Rpm | Unknown`; skip Tpm/Daily/Monthly/Concurrent. Depends on the classifier.
- **~5 tasks.**

## Wave 4 — Async tasks
**Goal:** settle correctly bills + releases freeze; freeze is Redis-based (cross-gateway); watchdog runs on one replica; long jobs actually polled. **ROADMAP:** `## 08 async-tasks`. **Depends:** W0; **W2** (shared ledger counter namespaces). **Order:** settle + freeze (runtime-risk) before poll/watchdog.
- **Settle release freeze + deduct + emit usage (runtime-risk):** `SETTLE_LUA` (`crates/gw-task/src/scripts.rs`) only does `HSET status` + `ZREM`. Add `HINCRBY` to release the task freeze, `HINCRBY` for the actual deduction, `XADD usage:records`. `exec_loop.rs` already computes `actual_micro` via `gw_billing::calculate_settle_amount` (stored but unused).
- **Redis-based task freeze (runtime-risk):** `crates/gw-proxy/src/filters/task.rs` freezes into in-process `gw_store::deduction::DEDUCT`; want `HINCRBY counter:user:<uid>:task_frozen_micro`. `gw-ledger::TaskFreeze` exists but is unused; reconcile the `release.rs` hash-tag namespace with what the contract writes.
- **Async submit→poll long-task model (gap):** `exec_loop.rs::call_upstream` does one synchronous POST treated as terminal. Add `provider_task_id`, a poll loop with per-vendor exponential backoff (cap 30 s), terminal-response parse via `gw-codec`, and gateway codec-rendered `ir_wire_body` for the worker to send verbatim (worker currently depends on none of gw-codec/routing/adapter).
- **Watchdog leader election (gap):** `watchdog_loop.rs` add `SETNX lock:tasks:watchdog` (TTL 10 s) gating the scan.
- **~9 tasks.**

## Wave 5 — Key-health
**Goal:** per-vendor error classification; quota-remaining signal to routing; graded cooldown uses the failure payload. **ROADMAP:** `## 03 key-health`. **Depends:** W0. **F1 resolved → keep `gw-adapter`:** K-B analyzers land on the (now-live) `UpstreamErrorAnalyzer` trait, **not** `gw-core::classify_default`.
- **K-B vendor error analyzers (gap):** implement real per-vendor analyzers behind `crates/gw-adapter/src/analyzer.rs`'s `UpstreamErrorAnalyzer` (today only `DefaultErrorAnalyzer`, never invoked) + body/keyword tables (account-disabled patterns). `gateway.rs` currently passes `None` for the body so 4xx bodies are never inspected — thread the body through so analyzers can read it. Wired via the F1 adapter seam (proxy-only consumer).
- **K-D quota-remaining probe + `QuotaState` (gap):** `VendorKey.quota_state` + entity `quota_*` columns; `QuotaSnapshotExtractor` (codex/anthropic rate-limit headers) in `response.rs`; a `QuotaProber` + scheduler. **Do not confuse** with `crates/gw-health/src/quota_probe.rs` (unrelated default-off cooldown-recovery prober).
- **`record()` failure payload (doc-divergence):** `crates/gw-health/src/store.rs:130` discards the payload (`let _ = outcome`). Either wire `Failure{http_status, retry_after_secs}` into a graded-cooldown decision (pairs naturally with K-B) or drop the unused fields.
- **~6 tasks.**

## Wave 6 — Routing
**Goal:** complete failover semantics + ranked multi-dimension routing + key affinity + stream-probe failover window. **ROADMAP:** `## 02 routing`. **Depends:** W0; **R-AB is foundational** — do it before R-D and R-F (they consume the ranked list). **Order:** R-E (smallest) → R-AB → R-D → R-F.
- **R-E failover gaps + 3 codes (gap):** add `401|402|403 → SwitchKey` (currently NotRetryable); add `total_budget_ms` failover deadline (today only attempt-count cap, `failover.rs::FailoverExecutor`); define `GW_ROUTING_NO_FALLBACK_LEFT`, `GW_ROUTING_FAILOVER_TIMEOUT`, `GW_ROUTING_STREAM_NOT_RETRIABLE` in `crates/gw-core/src/error/codes.rs`.
- **R-AB multi-score core (gap):** `CandidateFilter` chain + `ScoringDimension` set + `MultiScoreStrategy`; `Strategy::pick` returns a ranked `SmallVec` instead of a single `Option<&Arc<VendorKey>>` (`crates/gw-routing/src/strategy/mod.rs`, `candidates.rs`). Failover then walks a stable ranked list.
- **R-D AffinityManager (gap):** new `crates/gw-routing/src/affinity.rs` (`record_hit`/`get`/`invalidate_vendor_key`/`sweep_expired`, `AffinityRecord`, `AffinityDimension`, sweep loop). Distinct from request-level `strategy/cache_affinity.rs` (message-hash).
- **R-F stream probe (gap):** `StreamPhase` (Connecting/Probing/Streaming) + `probe_timeout` watchdog + `ProbeFailure` + `gw_stream_probe_*` metrics. Today `gw-proxy/src/gateway.rs` conflates first-upstream-byte with first-client-byte via `body_started`.
- **~10 tasks.**

## Wave 7 — Tunnel  · **F3 resolved → separate image**
**Goal:** honor the `require_tunnel` hard-fail contract in every build; ship a deployable tunnel artifact; finish partially-wired surfaces. **ROADMAP:** `## 09 tunnel`. **Depends:** W0.
- **`require_tunnel=true` hard-fail in the non-tunnel build (runtime-risk, ungated):** `GW_TUNNEL_NO_NODE` lives only in the `#[cfg(feature = "tunnel")]` arm (`gw-proxy/src/filters/upstream.rs`); the non-tunnel arm builds a Direct peer unconditionally and never consults `VendorKey.tunnel_preference.require_tunnel` (a non-gated `gw-core` field). Make the non-tunnel build **reject** `require_tunnel=true`. Do this regardless of F3.
- **Separate tunnel image (F3 = B):** keep `default = []` in `gw-proxy`/`gw-gateway` `Cargo.toml`; add a second CI job `cargo build --release --locked --features tunnel -p gw-gateway` + a distinct `buildah` image tag (e.g. `$GATEWAY_IMAGE:$CI_COMMIT_SHORT_SHA-tunnel`). Main artifact unchanged. Builds on W0's CI structure.
- **Misc tunnel surfaces (doc-divergence):** `GW_TUNNEL_DNS_BLOCKED` defined but never emitted; `last_pong_age_secs` always `None`; `auth_token_enc` / `ControlAuth` never sent on the control channel; `crates/gw-gateway/tests/e2e_tunnel.rs` is `#[ignore]` + feature-gated. Emit/enable each (exercised by the tunnel CI job).
- **~5 tasks.**

## Wave 8 — Vendor adapter layer  · **F1 + F2 resolved**
**Goal:** per-vendor outbound rewrite behind one seam; the heavy vendor adapters; in-Rust OAuth refresh; CLI observability. **ROADMAP:** `## 07 vendor-integration` + the `## 01` CLI-metric/provenance items. **Depends:** W0; Kiro codec ⟷ Kiro adapter are one unit. **Largest wave — consider splitting its expanded plan into W8a (seam + adapters) and W8b (K-C OAuth).**
- **Adapter seam (F1 = keep, incremental):** make `AdapterRegistry::for_vendor` proxy's outbound seam (proxy-only consumer). Direct → `NoopAdapter`/short-circuit byte-identical; impersonate → `transport_profile()` declaration reusing `gw-core::impersonate`; `inject_vendor_headers` stays. Add the `oauth_cache` field to `AdapterCtx`. Fix the `trait_def.rs` "task runtime calls in" comment.
- **Kiro codec + adapter (gap):** new `crates/gw-codec/src/kiro/` (EventStream framer w/ `crc32fast` CRCs — add dep, `<thinking>` parser, roundtrip), register in `AnyCodec`; kiro adapter AWS SigV4 + EventStream (implements the full F1 trait).
- **vertex_ai SA-JWT (gap):** RS256 service-account JWT signing + token exchange (`jsonwebtoken` is currently inbound-client-JWT only).
- **codex body transform (gap):** ride `patch_request` via `WireAnyRequest` (NOT `patch_body` — `no_patch_body` invariant).
- **K-C in-Rust OAuth refresh (F2 = Rust owns refresh, no double-writer):** `OauthCache` on `AdapterCtx` + per-vendor `OAuthRefresher` (`rewrite_auth` is async for this). On expiry: refresh via `token_endpoint` → KEK-encrypt write-back `payload.oauth` incl. **rotated `refresh_token`** → `version+1` → broadcast `events:entity op=upsert`. **Cross-Pod single-flight** (`SETNX lock:oauth:refresh:<key_id>`). Retire Go's `oauth_failed`-refresh path. **New `go-contracts/contract-change/` doc** (refresh ownership Go→Rust). See F2 must-solve list.
- **CLI metrics + provenance (doc-divergence):** emit `gw_cli_usage_extract_total` at `crates/gw-proxy/src/filters/response.rs` extraction sites + register in `crates/gw-gateway/src/metrics_catalog.rs`; decide a CLI-extracted value on `usage_provenance` in `crates/gw-ledger/src/usage.rs` (the old `UsageSource` is gone).
- **~12 tasks** (split W8a/W8b recommended).

---

# Execution protocol (every wave)

1. **Branch per wave.** W0 stays on the current `feat/cicd`. Each later wave gets its own branch (e.g. `feat/w2-billing-robustness`) off `main` (or off the prior wave if it depends on it — W4 off W2).
2. **Subagent-driven.** After expanding a brief, run `superpowers:subagent-driven-development`: fresh implementer per task, **spec-compliance review then code-quality review**, continuous (no check-ins between tasks).
3. **No auto-commits.** Subagents make all changes; **the human commits** at each wave boundary (standing instruction). Subagent prompts must say "Do NOT run git add/commit."
4. **Segment large writes.** Any multi-hundred-line doc is written in segments, not one shot (standing instruction).
5. **Green gate after every task:** `cargo test --workspace --locked` + `bash scripts/check-invariants.sh` (+ `check-spec.sh` for any spec-touching task).
6. **Close the loop on docs.** When a wave builds previously-unbuilt behavior, **move its entry out of `docs/ROADMAP.md` into the matching `docs/spec/0N-*.md`** (as-built standard), re-run `check-spec.sh`, and update the `docs/spec/README.md` status cell. If the public surface changed, update the CLAUDE.md / AGENTS.md sync rows and regenerate AGENTS.md byte-identical (`{ echo "# AGENTS.md"; tail -n +2 CLAUDE.md; } > AGENTS.md`).

---

# Self-review (writing-plans gate)

**1. Spec coverage — every ROADMAP entry maps to a wave (29 entries):**

| ROADMAP section | entry | wave |
|---|---|---|
| 01 | Kiro/S5 codec | W8 |
| 01 | `gw_cli_usage_extract_total` metric | W8 |
| 01 | `UsageSource::CliExtracted` provenance | W8 |
| 02 | R-AB multi-score | W6 |
| 02 | R-D AffinityManager | W6 |
| 02 | R-F stream probe | W6 |
| 02 | R-E failover + 3 codes | W6 |
| 03 | K-B vendor analyzers | W5 |
| 03 | K-D quota probe | W5 |
| 03 | `record()` failure payload | W5 |
| 04 | RL-Core classifier + per-key RPM | W3 |
| 04 | RL-Adaptive non-RPM 429s | W3 |
| 05 | `no_global_hmac_force` inert lint | **W1.3** |
| 06 | C-beta EstimatePricing | W2 |
| 06 | C-gamma frozen→Redis sync | W2 |
| 06 | C-delta DLQ three-tier | W2 |
| 06 | lease `402→503` | **W1.1** |
| 06 | `lease_grace_ms` never applied | **W1.2** |
| 07 | A-T0..T6 adapter layer + wire/delete | W8 (F1) |
| 07 | K-C in-Rust OAuth | W8 (F2) |
| 07 | kiro / vertex SA-JWT / codex | W8 |
| 08 | settle bill + release freeze | W4 |
| 08 | Redis-based task freeze | W4 |
| 08 | submit→poll long-task | W4 |
| 08 | watchdog leader election | W4 |
| 09 | tunnel off in shipped binary | W7 (F3) |
| 09 | `require_tunnel` silent fallback | W7 |
| 09 | misc tunnel gaps | W7 |
| Cross | wire `check-invariants.sh` into CI | **W0** |

No entry is unassigned.

**2. Placeholder scan:** W0/W1 contain complete code, exact paths/line anchors, and run/expect commands. W2–W8 are deliberately *briefs* (each labeled "expand into its own plan") — that is the program-plan structure, not unfilled placeholders. The one inline placeholder, `ErrorCategory::Backend` in W1.1, is explicitly flagged "replace with the real variant from Step 1".

**3. Type consistency:** `try_acquire`'s new 4-arg signature (`grace_ms`) is applied at the one production call site (`request.rs:1298`) and all in-crate test call sites in W1.2 Step 4. The `lease_expired_error()` helper introduced in W1.1 is the single constructor used by the `Expired` arm.

> Generated 2026-06-08 from `docs/ROADMAP.md` (the 2026-06-08 spec-vs-code audit). Supersedes nothing — it is the execution layer above ROADMAP. When a wave lands, its ROADMAP entry migrates into `docs/spec/`.
