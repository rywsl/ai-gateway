# E2E Coverage Known Gaps Resolution

## TL;DR

> **Quick Summary**: Wire three existing-but-disconnected capabilities — ReDoS regex budget guard, LatencyFirst p50 debug endpoint, CacheAffinity message hash — to close the last known e2e coverage gaps and remove all xfail markers.
> 
> **Deliverables**:
> - Gap #1: `model_whitelist_regex` field on `ApiKeyEntry` + `MergedPermissions`, wired to `check_with_budget()` in `request_filter`
> - Gap #2: `/healthz/strategies` token-gated endpoint exposing per-(key, format) p50 TTFT
> - Gap #3: SHA-256 body hash populating `PickCtx.message_hash` for CacheAffinity
> - All xfail e2e markers removed, tests passing
> - Doc updates per Sync rule
> 
> **Estimated Effort**: Medium
> **Parallel Execution**: YES — 3 waves
> **Critical Path**: Wave 1 (types + shared) → Wave 2 (3 gaps parallel) → Wave 3 (e2e + docs + soak)

---

## Context

### Original Request
Resolve all actionable gaps from `docs/issues/2026-05-25-e2e-coverage-known-gaps.md`. Three gaps require code changes (#1 ReDoS, #2 LatencyFirst, #3 CacheAffinity). #4 is already resolved, #5 needs no action.

### Interview Summary
**Key Discussions**:
- User wants all three gaps resolved in one plan
- TDD approach (red-green-refactor) + e2e xfail removal
- Gap #1 regex source: entity store JSONB payload (no DDL migration)
- Gap #2 endpoint: `/healthz/strategies` following existing healthz pattern
- Gap #3 hash input: entire `cached_body` raw bytes, no JSON normalization
- Separate atomic commits per gap

**Research Findings**:
- `RedosTracker` + `check_with_budget()` fully implemented but never called from request path
- `LatencyTracker` exists in `Wired` but not in `HealthState` — needs threading through
- `PickCtx.message_hash` exists but hardcoded to `None` in `upstream.rs:228`
- No DDL migration needed — all entities use JSONB `payload` column
- Hash computed before `forced_params` mutation (line 224 vs line 519) — correct by construction

### Metis Review
**Identified Gaps** (addressed):
- `None` vs `Some("")` regex semantics: `None` = skip regex check, `Some(pattern)` = compile + budget check
- Cold start for latency endpoint: return empty array, not 404
- Empty `cached_body`: `message_hash` stays `None` (preserves existing degraded behavior)
- SHA-256 → u64 truncation: acceptable for cache affinity (hint, not identity)

---

## Work Objectives

### Core Objective
Wire three existing-but-disconnected capabilities to close the last e2e coverage gaps, making all xfail tests pass.

### Concrete Deliverables
- `crates/gw-core/src/entity.rs`: `model_whitelist_regex: Option<String>` on `ApiKeyEntry`
- `crates/gw-core/src/permissions.rs`: `model_whitelist_regex: Option<String>` on `MergedPermissions`
- `crates/gw-proxy/src/filters/request.rs`: `check_with_budget()` call in model permission check
- `crates/gw-gateway/src/health.rs`: `/healthz/strategies` endpoint + `LatencyTracker` in `HealthState`
- `crates/gw-proxy/src/filters/upstream.rs`: SHA-256 hash of `cached_body` → `message_hash`
- `tests/e2e/client/scenarios/test_permissions.py`: xfail markers removed, tests passing
- `tests/e2e/client/scenarios/test_routing_strategies.py`: xfail markers removed, tests passing
- `docs/workflow.md`: Updated with ReDoS budget check location
- `docs/architecture.md`: Updated with deliberate behaviors (auto-disable semantics)

### Definition of Done
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `bash scripts/check-invariants.sh` returns 0
- [ ] All xfail markers removed from e2e tests
- [ ] `make e2e-soak-smoke` passes (zero drift, zero leaks)

### Must Have
- ReDoS budget guard wired into request path with fail-closed auto-disable
- `/healthz/strategies` endpoint token-gated, returning per-(key, format) p50 TTFT JSON
- `message_hash` populated from `cached_body` SHA-256 for CacheAffinity
- All xfail e2e tests passing
- Doc updates per Sync rule in same commits

### Must NOT Have (Guardrails)
- Do NOT refactor `RedosTracker` — only wire it
- Do NOT extend `/healthz/strategies` beyond p50 TTFT data
- Do NOT normalize JSON before hashing — use raw `cached_body` bytes
- Do NOT change `cache_affinity.rs` strategy logic
- Do NOT add `TODO(v2)` / `unimplemented!()` / `todo!()` markers (`no_v1_markers_in_prod` lint is error-mode)
- Do NOT add cost-related logic (`no_cost_micro` lint)
- Do NOT refactor `MergedPermissions` — only add field
- Do NOT add pagination or cardinality limits to `/healthz/strategies` (bounded by design: evict_stale)
- Do NOT touch Go-side code or contracts

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES
- **Automated tests**: TDD (red-green-refactor)
- **Framework**: `cargo test` (Rust unit/integration) + `pytest` (e2e)
- **Each task**: RED (failing test) → GREEN (minimal impl) → REFACTOR

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **API/Backend**: Use Bash (curl) — Send requests, assert status + response fields
- **CLI**: Use Bash — Run `cargo test`, `cargo clippy`, `scripts/check-invariants.sh`
- **Metrics**: Use Bash (curl localhost:9090/metrics) — grep for specific counters

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Foundation — types + shared changes):
├── Task 1: Add model_whitelist_regex to ApiKeyEntry + MergedPermissions types [quick]
├── Task 2: Add LatencyTracker to HealthState struct [quick]
└── Task 3: Add sha2 dependency + hash utility function [quick]

Wave 2 (Core implementation — 3 gaps in parallel):
├── Task 4: Wire ReDoS budget guard in request_filter (depends: 1) [deep]
├── Task 5: Implement /healthz/strategies endpoint (depends: 2) [unspecified-high]
└── Task 6: Wire message_hash in upstream.rs (depends: 3) [quick]

Wave 3 (Integration + validation):
├── Task 7: Update e2e tests — remove xfail markers (depends: 4, 5, 6) [unspecified-high]
├── Task 8: Update docs per Sync rule (depends: 4, 5, 6) [writing]
└── Task 9: Run soak smoke + invariant checks (depends: 7, 8) [quick]

Wave FINAL (4 parallel reviews, then user okay):
├── F1. Plan compliance audit (oracle)
├── F2. Code quality review (unspecified-high)
├── F3. Real manual QA (unspecified-high)
└── F4. Scope fidelity check (deep)
-> Present results -> Get explicit user okay

Critical Path: Task 1 → Task 4 → Task 7 → Task 9 → F1-F4 → user okay
Parallel Speedup: ~50% faster than sequential
Max Concurrent: 3 (Waves 1 & 2)
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | — | 4 | 1 |
| 2 | — | 5 | 1 |
| 3 | — | 6 | 1 |
| 4 | 1 | 7, 8 | 2 |
| 5 | 2 | 7, 8 | 2 |
| 6 | 3 | 7, 8 | 2 |
| 7 | 4, 5, 6 | 9 | 3 |
| 8 | 4, 5, 6 | 9 | 3 |
| 9 | 7, 8 | F1-F4 | 3 |

### Agent Dispatch Summary

- **Wave 1**: **3** — T1 → `quick`, T2 → `quick`, T3 → `quick`
- **Wave 2**: **3** — T4 → `deep`, T5 → `unspecified-high`, T6 → `quick`
- **Wave 3**: **3** — T7 → `unspecified-high`, T8 → `writing`, T9 → `quick`
- **FINAL**: **4** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [ ] 1. Add `model_whitelist_regex` to `ApiKeyEntry` + `model_whitelist_regex` to `MergedPermissions`

  **What to do**:
  - RED: Write unit test in `crates/gw-core/src/entity.rs` (or separate test module) that deserializes a JSON payload with `model_whitelist_regex: "(gpt-4|claude).*"` into `ApiKeyEntry` and asserts the field is `Some("(gpt-4|claude).*")`
  - RED: Write unit test that deserializes JSON without `model_whitelist_regex` and asserts field is `None`
  - GREEN: Add `#[serde(default)] pub model_whitelist_regex: Option<String>` to `ApiKeyEntry` struct at `crates/gw-core/src/entity.rs:92`
  - RED: Write test that merge correctly copies `model_whitelist_regex` from `ApiKeyEntry` into `MergedPermissions` (regex is api-key-tier only, not other tiers)
  - GREEN: Add `pub model_whitelist_regex: Option<String>` to `MergedPermissions` at `crates/gw-core/src/permissions.rs:45`. During merge, copy `api_key_entry.model_whitelist_regex` directly into `merged.model_whitelist_regex`. Do NOT compile to `ModelPermission` here — that happens in `gw-auth`/`gw-proxy` (Task 4) to avoid a `gw-core → gw-auth` dependency cycle. Note: `MergedPermissions` is NOT serde-derived — it's constructed programmatically during merge.
  - REFACTOR: Ensure serde defaults, derive macros, and imports are clean

  **Must NOT do**:
  - Do NOT add DDL migration — field comes from JSONB payload
  - Do NOT refactor existing `allowed_models` logic
  - Do NOT add any `ModelPermission` evaluation logic here (that's Task 4)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Type additions with serde derive — straightforward struct field additions
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None relevant

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3)
  - **Blocks**: Task 4
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-core/src/entity.rs:92` — `ApiKeyEntry` struct definition. Follow existing field pattern (all fields from JSONB payload via serde)
  - `crates/gw-core/src/permissions.rs:45` — `MergedPermissions` struct. Add new field following existing field pattern
  - `crates/gw-auth/src/model_permissions.rs` — `ModelPermission::new(pattern, effect) -> Result<Self, RegexError>` — reference only; compilation happens in Task 4 (`gw-proxy`), NOT in `gw-core`

  **API/Type References**:
  - `crates/gw-auth/src/model_permissions.rs` — `ModelPermission` type, `PermissionEffect::Allow` enum variant
  - `crates/gw-core/src/entity.rs` — Existing `allowed_models: Vec<ModelName>` for pattern reference

  **Test References**:
  - Look for existing entity deserialization tests in `crates/gw-core/` for test structure patterns

  **Acceptance Criteria**:
  - [ ] `ApiKeyEntry` has `model_whitelist_regex: Option<String>` field with `#[serde(default)]`
  - [ ] `MergedPermissions` has `model_whitelist_regex: Option<String>` field (no serde needed — constructed during merge)
  - [ ] Deserialization with regex present → `Some(pattern)`
  - [ ] Deserialization without regex → `None`
  - [ ] `cargo test -p gw-core` passes

  **QA Scenarios**:

  ```
  Scenario: ApiKeyEntry deserializes with regex field
    Tool: Bash (cargo test)
    Preconditions: Unit test exists
    Steps:
      1. cargo test -p gw-core -- model_whitelist_regex --nocapture
      2. Assert test output contains "test result: ok"
    Expected Result: All new tests pass
    Failure Indicators: "FAILED" in output, compilation errors
    Evidence: .omo/evidence/task-1-entity-deser.txt

  Scenario: ApiKeyEntry deserializes without regex field (backward compat)
    Tool: Bash (cargo test)
    Preconditions: Unit test with JSON missing the field exists
    Steps:
      1. cargo test -p gw-core -- model_whitelist_regex_absent --nocapture
      2. Assert field is None
    Expected Result: Backward-compatible deserialization works
    Failure Indicators: Serde error, panic
    Evidence: .omo/evidence/task-1-entity-compat.txt
  ```

  **Commit**: YES (groups with Commit C)
  - Message: `feat(auth): wire ReDoS budget guard for model_whitelist_regex`
  - Files: `crates/gw-core/src/entity.rs`, `crates/gw-core/src/permissions.rs`
  - Pre-commit: `cargo test -p gw-core`

- [ ] 2. Add `LatencyTracker` to `HealthState` struct

  **What to do**:
  - RED: Write test that `HealthState` can be constructed with a `LatencyTracker` field
  - GREEN: Add `pub latency_tracker: Arc<LatencyTracker>` to `HealthState` at `crates/gw-gateway/src/health.rs:50`
  - GREEN: Update `HealthState` construction in `crates/gw-gateway/src/lib.rs` (in the `Wired` struct bootstrap) to pass the existing `Arc<LatencyTracker>` into `HealthState`
  - REFACTOR: Ensure imports are clean

  **Must NOT do**:
  - Do NOT add the endpoint handler (that's Task 5)
  - Do NOT modify `LatencyTracker` internals
  - Do NOT add any new public API to `LatencyTracker`

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single field addition to struct + wiring in constructor
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3)
  - **Blocks**: Task 5
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/health.rs:50` — `HealthState` struct. Existing fields: `key_health`, `resilience`, `tunnel_enabled`, `tunnel_stack`. Follow this pattern.
  - `crates/gw-gateway/src/lib.rs:60-84` — `Wired` struct with `latency_tracker: Arc<LatencyTracker>`. This is the source to thread into `HealthState`.

  **API/Type References**:
  - `crates/gw-routing/src/latency.rs` (or wherever `LatencyTracker` is defined) — `by_key_format: DashMap<(KeyId, ApiFormat), Arc<KeyLatencyState>>`

  **Acceptance Criteria**:
  - [ ] `HealthState` has `latency_tracker: Arc<LatencyTracker>` field
  - [ ] `HealthState` construction in `lib.rs` passes the tracker
  - [ ] `cargo test -p gw-gateway` passes
  - [ ] `cargo build -p gw-gateway` succeeds

  **QA Scenarios**:

  ```
  Scenario: Gateway compiles with LatencyTracker in HealthState
    Tool: Bash
    Preconditions: Code changes applied
    Steps:
      1. cargo build -p gw-gateway
      2. Assert exit code 0
    Expected Result: Clean compilation
    Failure Indicators: Type mismatch errors, missing import
    Evidence: .omo/evidence/task-2-build.txt

  Scenario: Existing health tests still pass
    Tool: Bash
    Preconditions: HealthState field added
    Steps:
      1. cargo test -p gw-gateway --nocapture
      2. Assert no regressions
    Expected Result: All existing tests pass
    Failure Indicators: Constructor argument mismatch in tests
    Evidence: .omo/evidence/task-2-test.txt
  ```

  **Commit**: YES (groups with Commit B)
  - Message: `feat(admin): add /healthz/strategies endpoint for latency p50 TTFT`
  - Files: `crates/gw-gateway/src/health.rs`, `crates/gw-gateway/src/lib.rs`
  - Pre-commit: `cargo test -p gw-gateway`

- [ ] 3. Add `sha2` dependency + body hash utility

  **What to do**:
  - Add `sha2 = "0.10"` to `crates/gw-proxy/Cargo.toml` dependencies
  - RED: Write unit test: given a known byte slice, `body_hash_u64(b"test")` returns a deterministic `u64`
  - RED: Write unit test: `body_hash_u64(b"")` — decide behavior (should return `None` to preserve degraded path when body is empty)
  - GREEN: Implement `fn body_hash_u64(body: &[u8]) -> Option<u64>` in `crates/gw-proxy/src/filters/upstream.rs` (or a small util module). If body is empty, return `None`. Otherwise: `let digest = Sha256::digest(body); let hash = u64::from_le_bytes(digest[..8].try_into().unwrap()); Some(hash)`
  - REFACTOR: Ensure function is `#[inline]` and well-documented

  **Must NOT do**:
  - Do NOT normalize or parse the body JSON — hash raw bytes
  - Do NOT wire the hash into `PickCtx` yet (that's Task 6)
  - Do NOT add the dependency to workspace root — only `gw-proxy`

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: One dependency + one small pure function
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2)
  - **Blocks**: Task 6
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/Cargo.toml` — Existing dependency format
  - `crates/gw-proxy/src/filters/upstream.rs:228` — Where `message_hash: None` is hardcoded, context for the util function location

  **External References**:
  - `sha2` crate: `use sha2::{Sha256, Digest}; Sha256::digest(data)` returns `[u8; 32]`

  **Acceptance Criteria**:
  - [ ] `sha2` in `crates/gw-proxy/Cargo.toml`
  - [ ] `body_hash_u64` function exists and is tested
  - [ ] Empty input → `None`
  - [ ] Non-empty input → deterministic `Some(u64)`
  - [ ] `cargo test -p gw-proxy` passes

  **QA Scenarios**:

  ```
  Scenario: Deterministic hash for known input
    Tool: Bash (cargo test)
    Preconditions: Hash function implemented
    Steps:
      1. cargo test -p gw-proxy -- body_hash_u64 --nocapture
      2. Assert test with known input produces expected u64
    Expected Result: Hash is deterministic across runs
    Failure Indicators: Different hash values, panic
    Evidence: .omo/evidence/task-3-hash-deterministic.txt

  Scenario: Empty body returns None
    Tool: Bash (cargo test)
    Preconditions: Hash function implemented
    Steps:
      1. cargo test -p gw-proxy -- body_hash_empty --nocapture
      2. Assert returns None
    Expected Result: None for empty body
    Failure Indicators: Returns Some(0) or panics
    Evidence: .omo/evidence/task-3-hash-empty.txt
  ```

  **Commit**: YES (groups with Commit A)
  - Message: `feat(routing): wire CacheAffinity message_hash from cached_body SHA-256`
  - Files: `crates/gw-proxy/Cargo.toml`, `crates/gw-proxy/src/filters/upstream.rs`
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 4. Wire ReDoS budget guard in `request_filter` model permission check

  **What to do**:
  - RED: Write integration test in `crates/gw-proxy/` that constructs a request with a model name, `MergedPermissions` containing `model_whitelist_regex: Some("gpt-4.*".into())`, compiles it to `ModelPermission` via `ModelPermission::new()`, and asserts the model is allowed via `check_with_budget()`
  - RED: Write test with non-matching model name → denied (403)
  - RED: Write test where `check_with_budget()` returns `None` (auto-disabled) → denied (fail-closed)
  - GREEN: In `crates/gw-proxy/src/filters/request.rs`, at the model permission check site (~line 629):
    1. After the existing `allowed_models.contains()` check
    2. If `merged_permissions.model_whitelist_regex` is `Some(pattern)`, compile it via `ModelPermission::new(pattern, Allow)` into a `Vec<ModelPermission>`, then call `check_with_budget(model, &perms, api_key_id, &redos_tracker)`
    3. If result is `None` → return error with appropriate error code from `gw-core/src/error/codes.rs` (model denied, fail-closed)
    4. If result is `Some(false)` → return error with model denied code
    5. If result is `Some(true)` → continue (model allowed by regex)
    6. If `model_whitelist_regex` is `None` → skip regex check entirely (existing `allowed_models` logic only)
    7. Cache the compiled `ModelPermission` if needed for performance (or compile per-request — regex compilation is cheap for simple patterns within the 100ms budget)
  - GREEN: Ensure `RedosTracker` is accessible in `request_filter` — it should already be in `ReqCtx` or available via `Wired`. Check how other filter-phase resources are accessed.
  - REFACTOR: Clean up, ensure error codes match existing patterns

  **Must NOT do**:
  - Do NOT refactor `RedosTracker` internals
  - Do NOT change `check_with_budget()` signature
  - Do NOT modify existing `allowed_models.contains()` logic — regex check is additive
  - Do NOT add cost-related logic

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Touches request hot path, requires understanding filter chain context, error code selection, and `RedosTracker` threading. Risk of breaking existing auth flow.
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5, 6)
  - **Blocks**: Tasks 7, 8
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/request.rs:629` — Current model permission check site. This is where the new `check_with_budget()` call goes.
  - `crates/gw-proxy/src/filters/request.rs` — How other `Wired` resources are accessed in the filter (e.g., `self.wired.X`)

  **API/Type References**:
  - `crates/gw-auth/src/model_permissions.rs` — `check_with_budget(model: &str, perms: &[ModelPermission], api_key_id: &str, tracker: &RedosTracker) -> Option<bool>`. `None` = disabled (fail-closed), `Some(bool)` = result
  - `crates/gw-auth/src/model_permissions.rs` — `RedosTracker::is_disabled(ak: &str) -> bool`, `RedosTracker::record_timeout(ak: &str)`
  - `crates/gw-core/src/error/codes.rs` — Error codes for model denied. Find the appropriate code for "model not in whitelist"
  - `crates/gw-core/src/permissions.rs:45` — `MergedPermissions.model_whitelist_regex: Option<String>` (raw regex string added in Task 1; compile to `ModelPermission` here in Task 4)

  **Test References**:
  - Look at existing permission-related tests in `crates/gw-proxy/` or `crates/gw-auth/` for test patterns

  **Acceptance Criteria**:
  - [ ] `model_whitelist_regex: Some(pattern)` + matching model → request proceeds
  - [ ] `model_whitelist_regex: Some(pattern)` + non-matching model → 403 with correct error code
  - [ ] `check_with_budget()` returns `None` (auto-disabled) → 403 (fail-closed deny)
  - [ ] `model_whitelist_regex: None` → regex check skipped entirely, only `allowed_models` matters
  - [ ] `cargo test -p gw-proxy` passes
  - [ ] `cargo test --workspace` passes (no regressions)

  **QA Scenarios**:

  ```
  Scenario: Regex allows matching model
    Tool: Bash (cargo test)
    Preconditions: Test with model "gpt-4-turbo" and regex "gpt-4.*"
    Steps:
      1. cargo test -p gw-proxy -- redos_budget_allows_matching --nocapture
      2. Assert request proceeds (no error returned)
    Expected Result: Model check passes
    Failure Indicators: Unexpected 403, wrong error code
    Evidence: .omo/evidence/task-4-regex-allow.txt

  Scenario: Regex denies non-matching model
    Tool: Bash (cargo test)
    Preconditions: Test with model "claude-3" and regex "gpt-4.*"
    Steps:
      1. cargo test -p gw-proxy -- redos_budget_denies_nonmatch --nocapture
      2. Assert 403 returned with model denied error code
    Expected Result: Model denied with correct error code
    Failure Indicators: Request proceeds, wrong error code
    Evidence: .omo/evidence/task-4-regex-deny.txt

  Scenario: Auto-disabled tracker returns fail-closed deny
    Tool: Bash (cargo test)
    Preconditions: Test with RedosTracker pre-disabled for the api key
    Steps:
      1. cargo test -p gw-proxy -- redos_auto_disabled_deny --nocapture
      2. Assert 403 returned (fail-closed)
    Expected Result: Fail-closed deny when tracker disabled
    Failure Indicators: Request proceeds despite disabled tracker
    Evidence: .omo/evidence/task-4-auto-disable.txt
  ```

  **Commit**: YES (groups with Commit C)
  - Message: `feat(auth): wire ReDoS budget guard for model_whitelist_regex`
  - Files: `crates/gw-proxy/src/filters/request.rs`, test files
  - Pre-commit: `cargo test --workspace`

- [ ] 5. Implement `/healthz/strategies` endpoint

  **What to do**:
  - RED: Write test that `GET /healthz/strategies` with valid token returns 200 + JSON array of `{key_id, format, p50_ttft_ms}` objects
  - RED: Write test that request without token returns 401
  - RED: Write test that with no samples (cold start) returns 200 + empty array `[]`
  - GREEN: In `crates/gw-gateway/src/health.rs`:
    1. Add route match for `path.starts_with("/healthz/strategies")` at line ~132-135 (following existing pattern)
    2. Call `verify_healthz_request()` for token gating
    3. Iterate `health_state.latency_tracker.by_key_format` DashMap
    4. For each `(key_id, format) → KeyLatencyState`, call `.p50_ttft_ms()` (only include entries with `>= MIN_SAMPLES_FOR_P50`)
    5. Build JSON manually (following existing `write_json_response()` pattern — no serde_json)
    6. Format: `[{"key_id":"...","format":"...","p50_ttft_ms":12.5}, ...]`
    7. Empty DashMap or no qualifying entries → `[]`
  - REFACTOR: Ensure response content-type is `application/json`

  **Must NOT do**:
  - Do NOT expose anything beyond p50 TTFT per (key, format)
  - Do NOT add pagination — bounded by design (evict_stale with TTL)
  - Do NOT modify `LatencyTracker` internals
  - Do NOT add serde_json as dependency to the health server module
  - Do NOT expose raw `KeyLatencyState` internals beyond p50

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Requires understanding admin endpoint pattern, manual JSON building, DashMap iteration, and token gating
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 6)
  - **Blocks**: Tasks 7, 8
  - **Blocked By**: Task 2

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/health.rs:132-135` — Route matching pattern: `path.starts_with("/healthz/X") || path == "/healthz/Y"`. Follow this exactly.
  - `crates/gw-gateway/src/health.rs:143` — `verify_healthz_request()` for token gating. Reuse for the new endpoint.
  - `crates/gw-gateway/src/health.rs:265` — `build_healthz_body()` dispatch pattern. Add a new arm here.
  - `crates/gw-gateway/src/health.rs` — `write_json_response()` — use this for the response. Manual JSON string building, no serde.

  **API/Type References**:
  - `LatencyTracker.by_key_format: DashMap<(KeyId, ApiFormat), Arc<KeyLatencyState>>` — iterate this
  - `KeyLatencyState.p50_ttft_ms() -> Option<f64>` (or similar — verify exact signature)
  - `MIN_SAMPLES_FOR_P50 = 5` — only include entries meeting minimum sample threshold

  **Test References**:
  - Look at existing health endpoint tests in `crates/gw-gateway/` for test patterns and token setup

  **Acceptance Criteria**:
  - [ ] `GET /healthz/strategies` with valid token → 200 + JSON array
  - [ ] Without token → 401
  - [ ] Cold start (no samples) → 200 + `[]`
  - [ ] JSON format: `[{"key_id":"...","format":"...","p50_ttft_ms":N.N}]`
  - [ ] `cargo test -p gw-gateway` passes

  **QA Scenarios**:

  ```
  Scenario: Endpoint returns p50 data with valid token
    Tool: Bash (curl)
    Preconditions: Gateway running with at least one key having latency data
    Steps:
      1. curl -s -H "Authorization: Bearer $HEALTHZ_TOKEN" http://localhost:8080/healthz/strategies
      2. Assert HTTP 200
      3. Assert response body is JSON array
      4. Assert each element has "key_id", "format", "p50_ttft_ms" fields
    Expected Result: 200 with valid JSON array containing p50 data
    Failure Indicators: 404, 500, malformed JSON, missing fields
    Evidence: .omo/evidence/task-5-strategies-ok.txt

  Scenario: Endpoint rejects unauthenticated request
    Tool: Bash (curl)
    Preconditions: Gateway running
    Steps:
      1. curl -s -w "%{http_code}" http://localhost:8080/healthz/strategies
      2. Assert HTTP 401
    Expected Result: 401 Unauthorized
    Failure Indicators: 200 (data leak), 404
    Evidence: .omo/evidence/task-5-strategies-noauth.txt

  Scenario: Cold start returns empty array
    Tool: Bash (curl)
    Preconditions: Gateway freshly started, no requests processed yet
    Steps:
      1. curl -s -H "Authorization: Bearer $HEALTHZ_TOKEN" http://localhost:8080/healthz/strategies
      2. Assert HTTP 200
      3. Assert response body is "[]"
    Expected Result: Empty array, not 404 or error
    Failure Indicators: 404, non-empty response, error
    Evidence: .omo/evidence/task-5-strategies-cold.txt
  ```

  **Commit**: YES (groups with Commit B)
  - Message: `feat(admin): add /healthz/strategies endpoint for latency p50 TTFT`
  - Files: `crates/gw-gateway/src/health.rs`
  - Pre-commit: `cargo test -p gw-gateway`

- [ ] 6. Wire `message_hash` in `upstream.rs` PickCtx construction

  **What to do**:
  - RED: Write test that when `cached_body` is `Some(Bytes::from("test body"))`, `PickCtx.message_hash` is `Some(expected_u64)` (use the same deterministic hash from Task 3)
  - RED: Write test that when `cached_body` is `None`, `PickCtx.message_hash` is `None`
  - RED: Write test that when `cached_body` is `Some(Bytes::new())` (empty), `message_hash` is `None`
  - GREEN: In `crates/gw-proxy/src/filters/upstream.rs:228`, replace `message_hash: None` with:
    ```rust
    message_hash: ctx.cached_body.as_ref()
        .filter(|b| !b.is_empty())
        .and_then(|b| body_hash_u64(b)),
    ```
  - REFACTOR: Import `body_hash_u64` from wherever Task 3 placed it

  **Must NOT do**:
  - Do NOT modify `cache_affinity.rs` logic
  - Do NOT add JSON parsing or normalization
  - Do NOT change `PickCtx` struct definition
  - Do NOT hash `forced_params` mutations (hash is pre-mutation by construction)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single-line change at a known location, using function from Task 3
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5)
  - **Blocks**: Tasks 7, 8
  - **Blocked By**: Task 3

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/upstream.rs:224-232` — PickCtx construction site. Line 228 has `message_hash: None`. This is the exact change site.
  - Task 3 — `body_hash_u64()` function location

  **API/Type References**:
  - `crates/gw-routing/src/strategy/mod.rs:28` — `PickCtx.message_hash: Option<u64>`
  - `ctx.cached_body: Option<Bytes>` — Set in `request_filter` (line 416), available at PickCtx construction

  **Acceptance Criteria**:
  - [ ] Non-empty body → `message_hash` is `Some(deterministic_u64)`
  - [ ] `None` body → `message_hash` is `None`
  - [ ] Empty body → `message_hash` is `None`
  - [ ] `cargo test -p gw-proxy` passes

  **QA Scenarios**:

  ```
  Scenario: Non-empty body produces deterministic hash
    Tool: Bash (cargo test)
    Preconditions: body_hash_u64 from Task 3 available
    Steps:
      1. cargo test -p gw-proxy -- message_hash_nonempty --nocapture
      2. Assert message_hash matches expected u64 for known body
    Expected Result: Deterministic Some(u64)
    Failure Indicators: None when should be Some, wrong value
    Evidence: .omo/evidence/task-6-hash-nonempty.txt

  Scenario: None/empty body produces None hash
    Tool: Bash (cargo test)
    Preconditions: PickCtx construction test with None/empty body
    Steps:
      1. cargo test -p gw-proxy -- message_hash_empty --nocapture
      2. Assert message_hash is None
    Expected Result: None (preserves degraded CacheAffinity behavior)
    Failure Indicators: Some(0) or panic
    Evidence: .omo/evidence/task-6-hash-empty.txt
  ```

  **Commit**: YES (groups with Commit A)
  - Message: `feat(routing): wire CacheAffinity message_hash from cached_body SHA-256`
  - Files: `crates/gw-proxy/src/filters/upstream.rs`
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 7. Update e2e tests — remove xfail markers and fix assertions

  **What to do**:
  - In `tests/e2e/client/scenarios/test_permissions.py`:
    1. Line 312: Remove xfail marker from `test_redos_pathological_regex_killed_within_budget`
    2. Line 433: Remove xfail marker from `test_redos_auto_disable_after_threshold`
    3. Verify test assertions match implemented behavior (fail-closed deny on auto-disable, correct error code)
  - In `tests/e2e/client/scenarios/test_routing_strategies.py`:
    1. Line 328: Remove xfail marker from `test_latency_first_picks_lowest_p50`
    2. Update test to query `/healthz/strategies` to verify p50 values
  - For CacheAffinity: Find and update any degraded assertion that checks for `message_hash` behavior — should now assert affinity (same body → same key)

  **Must NOT do**:
  - Do NOT modify test infrastructure or fixtures beyond what's needed
  - Do NOT add new test files — modify existing ones

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Requires understanding e2e test framework, xfail semantics, and verifying behavior matches
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 8)
  - **Blocks**: Task 9
  - **Blocked By**: Tasks 4, 5, 6

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_permissions.py:312` — xfail for `test_redos_pathological_regex_killed_within_budget`
  - `tests/e2e/client/scenarios/test_permissions.py:433` — xfail for `test_redos_auto_disable_after_threshold`
  - `tests/e2e/client/scenarios/test_routing_strategies.py:328` — xfail for `test_latency_first_picks_lowest_p50`

  **Acceptance Criteria**:
  - [ ] All xfail markers removed
  - [ ] All previously-xfailed tests pass
  - [ ] No regression in other e2e tests
  - [ ] `pytest tests/e2e/client/scenarios/test_permissions.py -v` all pass
  - [ ] `pytest tests/e2e/client/scenarios/test_routing_strategies.py -v` all pass

  **QA Scenarios**:

  ```
  Scenario: Previously-xfailed ReDoS tests now pass
    Tool: Bash (pytest)
    Preconditions: Gateway running with full implementation
    Steps:
      1. pytest tests/e2e/client/scenarios/test_permissions.py::test_redos_pathological_regex_killed_within_budget -v
      2. pytest tests/e2e/client/scenarios/test_permissions.py::test_redos_auto_disable_after_threshold -v
      3. Assert both PASSED (not xfail, not skip)
    Expected Result: Both tests PASSED
    Failure Indicators: XFAIL, FAILED, ERROR
    Evidence: .omo/evidence/task-7-redos-e2e.txt

  Scenario: Previously-xfailed LatencyFirst test now passes
    Tool: Bash (pytest)
    Preconditions: Gateway running with /healthz/strategies endpoint
    Steps:
      1. pytest tests/e2e/client/scenarios/test_routing_strategies.py::test_latency_first_picks_lowest_p50 -v
      2. Assert PASSED
    Expected Result: Test PASSED
    Failure Indicators: XFAIL, FAILED
    Evidence: .omo/evidence/task-7-latency-e2e.txt
  ```

  **Commit**: YES (groups with Commit D)
  - Message: `test(e2e): remove xfail markers for resolved coverage gaps`
  - Files: `tests/e2e/client/scenarios/test_permissions.py`, `tests/e2e/client/scenarios/test_routing_strategies.py`
  - Pre-commit: `pytest tests/e2e/client/scenarios/ -v`

- [ ] 8. Update docs per Sync rule

  **What to do**:
  - Update `docs/workflow.md` §request_filter:
    1. Document ReDoS budget-guarded regex check after `allowed_models.contains()`
    2. Document fail-closed behavior when auto-disabled
  - Update `docs/architecture.md`:
    1. §Deliberate behaviours: Add ReDoS auto-disable → fail-closed (deny) entry
    2. §Capability surface: Mention `message_hash` populated from request body SHA-256 for CacheAffinity
    3. §Observability/admin: Document `/healthz/strategies` endpoint
  - Update `docs/issues/2026-05-25-e2e-coverage-known-gaps.md`: Mark gaps #1, #2, #3 as resolved

  **Must NOT do**:
  - Do NOT expand docs beyond what Sync rule requires
  - Do NOT rewrite existing doc sections — only add/update

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: Documentation-only task
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Task 7)
  - **Blocks**: Task 9
  - **Blocked By**: Tasks 4, 5, 6

  **References**:

  **Pattern References**:
  - `docs/workflow.md` — §request_filter section
  - `docs/architecture.md` — §Deliberate behaviours, §Capability surface
  - `CLAUDE.md` — Sync rule table

  **Acceptance Criteria**:
  - [ ] `docs/workflow.md` updated with ReDoS budget check
  - [ ] `docs/architecture.md` updated with auto-disable behavior + `/healthz/strategies` + `message_hash`
  - [ ] `docs/issues/2026-05-25-e2e-coverage-known-gaps.md` gaps marked resolved

  **QA Scenarios**:

  ```
  Scenario: Sync rule compliance verified
    Tool: Bash (grep)
    Preconditions: All code changes complete
    Steps:
      1. grep -l "check_with_budget\|ReDoS\|redos" docs/workflow.md
      2. grep -l "auto-disable\|fail-closed" docs/architecture.md
      3. grep -l "/healthz/strategies" docs/architecture.md
      4. grep -l "message_hash\|CacheAffinity" docs/architecture.md
    Expected Result: All greps find matches
    Failure Indicators: Empty grep output
    Evidence: .omo/evidence/task-8-sync-rule.txt

  Scenario: Known gaps doc updated
    Tool: Bash (grep)
    Preconditions: docs/issues file exists
    Steps:
      1. grep -i "resolved\|closed\|fixed" docs/issues/2026-05-25-e2e-coverage-known-gaps.md
      2. Assert gaps #1, #2, #3 marked resolved
    Expected Result: All 3 gaps resolved
    Failure Indicators: Gaps still open
    Evidence: .omo/evidence/task-8-gaps-resolved.txt
  ```

  **Commit**: YES (groups with Commit D)
  - Message: `test(e2e): remove xfail markers for resolved coverage gaps`
  - Files: `docs/workflow.md`, `docs/architecture.md`, `docs/issues/2026-05-25-e2e-coverage-known-gaps.md`
  - Pre-commit: `bash scripts/check-invariants.sh`

- [ ] 9. Run soak smoke + invariant checks — final validation

  **What to do**:
  - Run `bash scripts/check-invariants.sh` → must exit 0
  - Run `cargo test --workspace` → all pass
  - Run `cargo clippy --workspace --all-targets -- -D warnings` → clean
  - Run `cargo fmt --all -- --check` → clean
  - Run `make e2e-soak-smoke` → zero drift, zero leaks
  - If any failure: report for responsible task to fix

  **Must NOT do**:
  - Do NOT fix issues here — report them
  - Do NOT skip any check

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Running commands and checking output
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential (after Tasks 7, 8)
  - **Blocks**: F1-F4
  - **Blocked By**: Tasks 7, 8

  **References**:

  **Pattern References**:
  - `CLAUDE.md` — Common commands section

  **Acceptance Criteria**:
  - [ ] `bash scripts/check-invariants.sh` → exit 0
  - [ ] `cargo test --workspace` → all pass
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` → clean
  - [ ] `cargo fmt --all -- --check` → clean
  - [ ] `make e2e-soak-smoke` → zero drift, zero leaks

  **QA Scenarios**:

  ```
  Scenario: Full validation suite passes
    Tool: Bash
    Preconditions: All tasks complete
    Steps:
      1. bash scripts/check-invariants.sh
      2. cargo test --workspace
      3. cargo clippy --workspace --all-targets -- -D warnings
      4. cargo fmt --all -- --check
      5. make e2e-soak-smoke
      6. Assert all exit 0
    Expected Result: All checks pass
    Failure Indicators: Non-zero exit, test failures, clippy warnings, soak drift
    Evidence: .omo/evidence/task-9-validation.txt

  Scenario: No v1/v2 markers introduced
    Tool: Bash
    Preconditions: All code committed
    Steps:
      1. bash scripts/check-invariants.sh 2>&1 | grep -i "v1_markers"
      2. Assert no violations
    Expected Result: no_v1_markers_in_prod passes
    Failure Indicators: Marker found
    Evidence: .omo/evidence/task-9-no-markers.txt
  ```

  **Commit**: NO (validation only)

---

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, curl endpoint, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check` + `cargo test --workspace`. Review all changed files for: `as any` equivalent (`unsafe` blocks, `unwrap()` in non-test), empty catches, `println!` in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.
  Output: `Build [PASS/FAIL] | Clippy [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Start gateway from clean state. Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-gap integration (all three features working together). Test edge cases: empty body, missing regex field, cold latency tracker. Save to `.omo/evidence/final-qa/`.
  Output: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (`git diff`). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance: no RedosTracker refactor, no strategy logic changes, no JSON normalization, no TODO markers. Detect cross-task contamination.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

- **Commit A** (Gap #3 — smallest, lowest risk): `feat(routing): wire CacheAffinity message_hash from cached_body SHA-256`
  - Files: `upstream.rs`, `Cargo.toml` (sha2 dep), unit test file
  - Pre-commit: `cargo test -p gw-proxy`, `bash scripts/check-invariants.sh`

- **Commit B** (Gap #2 — self-contained endpoint): `feat(admin): add /healthz/strategies endpoint for latency p50 TTFT`
  - Files: `health.rs`, `lib.rs` (HealthState), unit test file
  - Pre-commit: `cargo test -p gw-gateway`, `bash scripts/check-invariants.sh`

- **Commit C** (Gap #1 — largest, highest risk): `feat(auth): wire ReDoS budget guard for model_whitelist_regex`
  - Files: `entity.rs`, `permissions.rs`, `request.rs`, unit test files
  - Pre-commit: `cargo test --workspace`, `bash scripts/check-invariants.sh`

- **Commit D** (e2e + docs): `test(e2e): remove xfail markers for resolved coverage gaps`
  - Files: `test_permissions.py`, `test_routing_strategies.py`, `docs/workflow.md`, `docs/architecture.md`, `docs/issues/2026-05-25-e2e-coverage-known-gaps.md`
  - Pre-commit: `make e2e-soak-smoke`

---

## Success Criteria

### Verification Commands
```bash
cargo test --workspace                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings     # Expected: clean
cargo fmt --all -- --check                                # Expected: clean
bash scripts/check-invariants.sh                          # Expected: exit 0
make e2e-soak-smoke                                       # Expected: zero drift, zero leaks
```

### Final Checklist
- [ ] All "Must Have" present and verified
- [ ] All "Must NOT Have" absent (grep-verified)
- [ ] All unit tests pass
- [ ] All xfail markers removed
- [ ] Soak smoke clean
- [ ] Docs updated in same commits per Sync rule
