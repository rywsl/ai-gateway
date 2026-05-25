# E2E Test Coverage Gap Closure

## TL;DR

> **Quick Summary**: Add ~30 new e2e scenario tests covering all 8 routing strategies, 5xx/429 failover, ReDoS budget, usage:detailed archive integrity, 5-tier permission merge, cross-protocol non-stream matrix, tunnel mode, response deadline, forced_params, /readyz semantics, and long-tail security/resilience surfaces. Also add soak smoke recommendation to CLAUDE.md.
>
> **Deliverables**:
> - 5 new test files + extensions to 3 existing files in `tests/e2e/client/scenarios/`
> - Conftest helper additions (redis stream decode, lz4/zstd, permission merge seeds)
> - CLAUDE.md update (soak recommendation)
> - docs/architecture.md update if test infrastructure changes
>
> **Estimated Effort**: Large
> **Parallel Execution**: YES - 5 waves
> **Critical Path**: Task 1 (conftest infra) -> Wave 2 (all independent test files) -> Wave 3 (dependent tests) -> Wave 4 (tunnel + doc) -> Final Verification

---

## Context

### Original Request
Close ALL e2e test coverage gaps identified by Metis gap analysis (P0 through P3), covering routing strategies, failover/retry, ReDoS, usage:detailed, permissions, cross-protocol, tunnel, response deadline, forced_params, TPM, /readyz, HMAC, JWT, capability resolver, graceful shutdown, and KEK rotation. Add soak recommendation to CLAUDE.md. Ship as one PR from a feature worktree.

### Interview Summary
**Key Discussions**:
- User wants ALL gaps (P0-P3) in one plan, one PR
- Feature worktree required for isolation
- Soak smoke should be recommended in CLAUDE.md but NOT added to default `make e2e`
- Tests-after approach (no TDD); Agent-Executed QA mandatory
- Tunnel tests need `--features tunnel` Docker image build

**Research Findings**:
- 6 of 8 routing strategies have zero e2e coverage (highest aggregate risk)
- Production classifier wiring was dead before this session's fix; now live
- `usage:detailed` has 5 surfaces with tiered compression but only XLEN-asserted in soak
- `fake-upstream` supports `StatusWithRetryAfter`, `SlowBodyBytePerSecond` but both unused in scenarios
- Permission merge is intersection-based for models/channels/groups, min-positive for RPM/TPM
- Tunnel is feature-gated `cfg(feature = "tunnel")` and boot-time only

### Metis Review
**Identified Gaps** (addressed):
- Corrected scenario count: 12 files, not 10 (added `test_mgmt_token.py`, `test_cross_protocol.py`)
- Cross-protocol stream re-render already covered; non-stream only 1 of 6 directions
- `events:entity` preservation in `reset()` creates cross-test leak risk
- Single fake-upstream port limits per-key connection failure modeling

---

## Work Objectives

### Core Objective
Achieve comprehensive e2e coverage across all gateway capability surfaces, eliminating blind spots in routing, failover, security, observability, and resilience.

### Concrete Deliverables
- `tests/e2e/client/scenarios/test_routing_strategies.py` — 6 strategy tests
- `tests/e2e/client/scenarios/test_failover_retry.py` — 5xx/429 failover + recovery
- `tests/e2e/client/scenarios/test_permissions.py` — 5-tier merge + ReDoS
- `tests/e2e/client/scenarios/test_observability.py` — usage:detailed, forced_params, TPM
- `tests/e2e/client/scenarios/test_resilience.py` — response deadline, /readyz, graceful shutdown
- Extended `test_cross_protocol.py` — non-stream matrix + lossy metric
- Extended `test_auth_failures.py` — HMAC body-tamper, JWT nbf/leeway
- Extended `test_routing_retry.py` — capability positive match
- `tests/e2e/client/conftest.py` — decode_detailed_record(), redis helpers
- CLAUDE.md — soak recommendation paragraph
- Tunnel tests (conditional on `--features tunnel` build)

### Definition of Done
- [ ] `make e2e` passes with all new tests included (0 failures, 0 errors)
- [ ] `make e2e-soak-smoke` still passes
- [ ] `cargo fmt --all -- --check` clean
- [ ] `bash scripts/check-invariants.sh` all hold
- [ ] All new test files follow existing conftest conventions

### Must Have
- Every test uses globally unique entity IDs (no collision with existing tests)
- Every test seeds its own entities (no shared state between tests)
- Every test asserts specific error codes via `_error_code()` or specific metrics via `label_delta()`
- `wait_for_records()` used for any usage stream assertions
- Routing strategy tests must verify actual key distribution, not just success

### Must NOT Have (Guardrails)
- Do NOT modify `make e2e` to include soak tests (user explicit)
- Do NOT modify gateway Rust source code (test-only PR)
- Do NOT add new Python dependencies beyond what's in requirements.txt, EXCEPT `lz4` and `zstandard` which are required for `usage:detailed` decode (Task 1)
- Do NOT create test fixtures that depend on execution order
- Do NOT use `time.sleep()` for timing — use `wait_for_records()` or retry loops
- Do NOT duplicate seed helpers — extend conftest if needed
- Do NOT touch `crates/` Rust source files (except potentially `e2e-fake-upstream` if stub extension needed)

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (pytest + docker-compose harness)
- **Automated tests**: YES (tests-after — we ARE writing the tests)
- **Framework**: pytest with conftest.py seed helpers

### QA Policy
Every task includes agent-executed QA scenarios. Evidence to `.omo/evidence/task-{N}-*.txt`.

- **Test files**: Run `pytest {file} -v` and capture output
- **Metrics**: Use `metrics_snapshot()`/`label_delta()` pattern in assertions
- **Full suite**: `make e2e` at end of each wave

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Foundation — conftest infrastructure):
├── 1. Conftest helpers (decode_detailed, redis decode, merge seed) [quick]
├── 2. Worktree setup + branch creation [quick]
└── 3. CLAUDE.md soak recommendation [writing]

Wave 2 (Core test files — MAX PARALLEL, all independent):
├── 4. test_routing_strategies.py — RoundRobin + WeightedRr [unspecified-high]
├── 5. test_routing_strategies.py — CostFirst + LatencyFirst [unspecified-high]
├── 6. test_routing_strategies.py — LoadBalance + CacheAffinity [unspecified-high]
├── 7. test_failover_retry.py — 5xx failover + 429 cooldown [unspecified-high]
├── 8. test_permissions.py — 5-tier merge + ReDoS [unspecified-high]
├── 9. test_observability.py — usage:detailed 5-surface decode [deep]
└── 10. test_resilience.py — response deadline + /readyz [unspecified-high]

Wave 3 (Extensions to existing files — after Wave 2 validates patterns):
├── 11. test_cross_protocol.py extensions — non-stream matrix + lossy metric [unspecified-high]
├── 12. test_auth_failures.py extensions — HMAC body-tamper + JWT nbf [unspecified-high]
├── 13. test_routing_retry.py extension — capability positive match [quick]
├── 14. test_observability.py additions — forced_params + TPM [unspecified-high]
└── 15. test_resilience.py additions — graceful shutdown [deep]

Wave 4 (Tunnel + long-tail — after core green):
├── 16. Tunnel test file (needs --features tunnel build) [deep]
├── 17. KEK rotation test [deep]
├── 18. test_routing_strategies.py — Sticky TTL edge cases [quick]
└── 19. docs updates if needed [writing]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── F1. Plan compliance audit (oracle)
├── F2. Code quality review (unspecified-high)
├── F3. Real manual QA — run make e2e + make e2e-soak-smoke (unspecified-high)
└── F4. Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | - | 4-10 | 1 |
| 2 | - | all | 1 |
| 3 | - | 19 | 1 |
| 4 | 1,2 | 18 | 2 |
| 5 | 1,2 | - | 2 |
| 6 | 1,2 | - | 2 |
| 7 | 1,2 | - | 2 |
| 8 | 1,2 | - | 2 |
| 9 | 1,2 | 14 | 2 |
| 10 | 1,2 | 15 | 2 |
| 11 | 1,2 | - | 3 |
| 12 | 1,2 | - | 3 |
| 13 | 1,2 | - | 3 |
| 14 | 9 | - | 3 |
| 15 | 10 | - | 3 |
| 16 | 1,2 | - | 4 |
| 17 | 1,2 | - | 4 |
| 18 | 4 | - | 4 |
| 19 | 3 | - | 4 |
| F1-F4 | 1-19 | - | FINAL |

### Agent Dispatch Summary

- **Wave 1**: **3** — 1 `quick`, 2 `quick`, 3 `writing`
- **Wave 2**: **7** — 4-6 `unspecified-high`, 7-8 `unspecified-high`, 9 `deep`, 10 `unspecified-high`
- **Wave 3**: **5** — 11-12 `unspecified-high`, 13 `quick`, 14 `unspecified-high`, 15 `deep`
- **Wave 4**: **4** — 16-17 `deep`, 18 `quick`, 19 `writing`
- **FINAL**: **4** — F1 `oracle`, F2 `unspecified-high`, F3 `unspecified-high`, F4 `deep`

---

## TODOs

- [x] 1. Conftest Infrastructure — decode helpers and permission merge seeds

  **What to do**:
  - Add `decode_detailed_record(raw_entry: dict) -> dict` to conftest.py: reads `encoding` field, decodes `payload` (raw passthrough / lz4 decompress+json / zstd decompress+json), returns parsed dict with `kind`, `encoding`, `decoded_payload`
  - Add `seed_permission_tiers(fake_web, *, global_config, user_group, user, sub_account, api_key)` helper to conftest.py for multi-tier permission merge tests — each tier dict has `allowed_models`, `allowed_channels`, `allowed_groups`, `rpm_limit`, `tpm_limit`, `markup_percent`
  - Add `redis_xrange_decoded(redis_client, stream, count=None, min_id="-", max_id="+")` helper that wraps `XRANGE` and returns list of dicts
  - Import `lz4.frame` and `zstandard` — verify both are in `requirements.txt`; if not, add them

  **Must NOT do**:
  - Do not modify existing seed functions
  - Do not add dependencies not needed for decode (lz4 + zstandard only)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3)
  - **Blocks**: Tasks 4-10 (all Wave 2 tasks need these helpers)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `tests/e2e/client/conftest.py` — all existing seed functions, `wait_for_records()`, `_error_code()` patterns
  - `tests/e2e/client/scenarios/test_non_stream.py:_assert_record_shape()` — how usage records are decoded today

  **API/Type References**:
  - `crates/gw-ledger/src/detailed_archive.rs` — 5 surface kinds: `downstream_request`, `upstream_request`, `upstream_response`, `downstream_response`, `summary`. XADD fields: `ts_ms`, `request_id`, `trace_id`, `kind`, `encoding`, `payload`. Encoding values: `raw+json`, `lz4+json`, `zstd+json`
  - `crates/gw-auth/src/merge.rs:28-169` — 5-tier merge semantics: intersection for models/channels/groups, min-positive for rpm/tpm, product for markup

  **External References**:
  - `lz4` Python package: `lz4.frame.decompress(data)` returns bytes
  - `zstandard` Python package: `zstandard.ZstdDecompressor().decompress(data)` returns bytes

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: decode_detailed_record handles raw+json encoding
    Tool: Bash (python3)
    Preconditions: conftest.py saved with new function
    Steps:
      1. Run: .venv/bin/python3 -c "from conftest import decode_detailed_record; r = decode_detailed_record({'encoding': 'raw+json', 'payload': '{\"key\":\"val\"}'}); assert r['decoded_payload'] == {'key': 'val'}; assert r['encoding'] == 'raw+json'; print('PASS')"
    Expected Result: Prints "PASS"
    Evidence: .omo/evidence/task-1-decode-raw.txt

  Scenario: decode_detailed_record handles lz4+json encoding
    Tool: Bash (python3)
    Preconditions: lz4 installed in .venv
    Steps:
      1. Run: .venv/bin/python3 -c "import lz4.frame, json, base64; from conftest import decode_detailed_record; data = lz4.frame.compress(json.dumps({'k':'v'}).encode()); r = decode_detailed_record({'encoding': 'lz4+json', 'payload': base64.b64encode(data).decode()}); assert r['decoded_payload'] == {'k': 'v'}; print('PASS')"
    Expected Result: Prints "PASS"
    Evidence: .omo/evidence/task-1-decode-lz4.txt
  ```

  **Commit**: YES (C1)
  - Message: `test(e2e): add conftest helpers for detailed-log decode and permission merge seeds`
  - Files: `tests/e2e/client/conftest.py`, `tests/e2e/client/requirements.txt` (if deps added)
  - Pre-commit: `.venv/bin/python3 -c "from conftest import decode_detailed_record, seed_permission_tiers, redis_xrange_decoded"`

- [x] 2. Worktree Setup and Branch Creation

  **What to do**:
  - Create feature branch `feat/e2e-coverage-gaps` from current HEAD
  - Create git worktree at `../ailinkmax-gateway-e2e-gaps` on that branch
  - Verify worktree is functional: `git log --oneline -1` in new worktree
  - All subsequent tasks work in this worktree

  **Must NOT do**:
  - Do not push the branch until all tasks complete
  - Do not modify the main worktree

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: [`git-master`]

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3)
  - **Blocks**: All subsequent tasks (must work in worktree)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - Git worktree docs: `git worktree add <path> -b <branch>`

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Worktree created and functional
    Tool: Bash
    Steps:
      1. Run: cd ../ailinkmax-gateway-e2e-gaps && git branch --show-current
    Expected Result: Output is "feat/e2e-coverage-gaps"
    Evidence: .omo/evidence/task-2-worktree.txt

  Scenario: Worktree has same HEAD as main
    Tool: Bash
    Steps:
      1. Run: diff <(cd ../ailinkmax-gateway && git rev-parse HEAD) <(cd ../ailinkmax-gateway-e2e-gaps && git rev-parse HEAD)
    Expected Result: No diff output (same commit)
    Evidence: .omo/evidence/task-2-head-match.txt
  ```

  **Commit**: NO (infrastructure only)

- [x] 3. CLAUDE.md Soak Recommendation

  **What to do**:
  - Add a paragraph to `CLAUDE.md` in the "Common commands" section, after the soak harness description, recommending that `make e2e-soak-smoke` be run before any PR that touches gateway request handling, codec, routing, or ledger code paths
  - Phrasing should be advisory ("recommended", "strongly encouraged"), NOT mandatory
  - Keep it concise — 3-4 sentences max

  **Must NOT do**:
  - Do not modify Makefile to add soak to default `make e2e`
  - Do not make soak a CI gate
  - Do not add it as a git hook

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2)
  - **Blocks**: Task 19 (docs finalization)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `CLAUDE.md` §Common commands — existing soak harness description with `make e2e-soak` and `make e2e-soak-smoke` commands

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: CLAUDE.md contains soak recommendation
    Tool: Bash (grep)
    Steps:
      1. Run: grep -c "e2e-soak-smoke" CLAUDE.md
    Expected Result: Count >= 3 (existing 2 + new recommendation)
    Evidence: .omo/evidence/task-3-claudemd.txt

  Scenario: Recommendation is advisory, not mandatory
    Tool: Bash (grep)
    Steps:
      1. Run: grep -i "recommended\|strongly encouraged" CLAUDE.md | head -5
    Expected Result: At least one line with advisory language
    Evidence: .omo/evidence/task-3-advisory.txt
  ```

  **Commit**: YES (C2)
  - Message: `docs: add soak smoke recommendation to CLAUDE.md`
  - Files: `CLAUDE.md`
  - Pre-commit: `grep "e2e-soak-smoke" CLAUDE.md`

- [x] 4. Routing Strategies — RoundRobin + WeightedRr

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_routing_strategies.py`
  - `test_round_robin_distributes_across_keys`: Seed 3 vendor keys A/B/C under one model group with `strategy="round_robin"`. Configure happy-path `Static{status=200}` scenario. Issue 6 sequential same-shape OpenAI requests. Assert each key's hit count is exactly 2 via fake-upstream `/admin/hits-total` or per-key route counters.
  - `test_weighted_rr_respects_weights`: Seed 2 keys with `weight=3` and `weight=1` under `strategy="weighted_rr"`. Issue 100 requests. Assert distribution within ±15% of 75:25 ratio. Use `wait_for_records()` to collect `usage:records` and count by `vendor_key_id` field to determine per-key distribution.

  **Must NOT do**:
  - Do not share entity IDs with other test files — use prefix `rr_` for round-robin, `wrr_` for weighted
  - Do not use `time.sleep()` — seed and fire immediately

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5-10)
  - **Blocks**: Task 18 (Sticky edge cases)
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `tests/e2e/client/conftest.py:seed_model_group()` — `strategy` parameter accepts string matching `ModelGroup.strategy` enum: `"round_robin"`, `"weighted_rr"`, etc.
  - `tests/e2e/client/scenarios/test_routing_retry.py:test_sticky_strategy_binds_user_to_same_key_then_expires` — how to seed multi-key groups with specific strategies
  - `crates/gw-routing/src/strategy/round_robin.rs` — AtomicUsize counter, skip unavailable; confirms per-model round-robin

  **API/Type References**:
  - `crates/gw-routing/src/strategy/weighted_rr.rs` — Nginx SWRR algorithm using `VendorKey.weight` field
  - `crates/e2e-fake-web/src/store.rs` — `seed_vendor_key` accepts `weight` in payload JSON

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Round-robin distributes evenly across 3 keys
    Tool: Bash (pytest)
    Preconditions: compose stack running, entities seeded
    Steps:
      1. Run: cd tests/e2e/client && .venv/bin/python3 -m pytest scenarios/test_routing_strategies.py::test_round_robin_distributes_across_keys -v
    Expected Result: PASSED — each key hit exactly 2 times out of 6
    Failure Indicators: AssertionError on hit counts, any key hit 0 times
    Evidence: .omo/evidence/task-4-round-robin.txt

  Scenario: Weighted round-robin respects 3:1 weight ratio
    Tool: Bash (pytest)
    Preconditions: compose stack running
    Steps:
      1. Run: cd tests/e2e/client && .venv/bin/python3 -m pytest scenarios/test_routing_strategies.py::test_weighted_rr_respects_weights -v
    Expected Result: PASSED — distribution within ±15% of 75:25
    Failure Indicators: AssertionError on ratio bounds
    Evidence: .omo/evidence/task-4-weighted-rr.txt
  ```

  **Commit**: YES (C3, grouped with Tasks 5, 6)
  - Message: `test(e2e): add routing strategy tests (round-robin, weighted-rr, cost-first, latency-first, load-balance, cache-affinity)`
  - Files: `tests/e2e/client/scenarios/test_routing_strategies.py`

- [x] 5. Routing Strategies — CostFirst + LatencyFirst

  **What to do**:
  - Add to `test_routing_strategies.py`:
  - `test_cost_first_picks_cheapest_key`: Seed 2 keys under `strategy="cost_first"` with different `priority` values (CostFirst sorts by `priority * model_markup` ascending). Key A: priority=1, Key B: priority=10. Issue 5 requests. Assert ALL hit Key A (lowest cost score).
  - `test_latency_first_picks_lowest_p50`: Seed 2 keys under `strategy="latency_first"`. Key A scenario: `Static{status=200, delay_ms=500}`. Key B scenario: `Static{status=200, delay_ms=10}`. Issue 10+ requests to build latency window, then issue 5 more. Assert later requests increasingly favor Key B. Check metric or hit distribution.

  **Must NOT do**:
  - Do not use entity ID prefix collision — use `cf_` for cost-first, `lf_` for latency-first

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 6-10)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/strategy/cost_first.rs` — sorts by `priority * model_markup` ascending; lowest first
  - `crates/gw-routing/src/strategy/latency_first.rs` — uses P50 TTFT from sliding window; cold-start falls back to priority desc

  **API/Type References**:
  - `seed_vendor_key(..., priority=N)` — priority field in vendor key payload
  - `seed_pricing(..., input_micro=N, output_micro=N)` — pricing affects cost calculation

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Cost-first always picks cheapest key
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_strategies.py::test_cost_first_picks_cheapest_key -v
    Expected Result: PASSED — all 5 requests hit Key A
    Evidence: .omo/evidence/task-5-cost-first.txt

  Scenario: Latency-first converges to fastest key
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_strategies.py::test_latency_first_picks_lowest_p50 -v
    Expected Result: PASSED — later requests favor Key B (>= 70% of last 5)
    Evidence: .omo/evidence/task-5-latency-first.txt
  ```

  **Commit**: YES (C3, grouped with Tasks 4, 6)
  - Files: `tests/e2e/client/scenarios/test_routing_strategies.py`

- [x] 6. Routing Strategies — LoadBalance + CacheAffinity

  **What to do**:
  - Add to `test_routing_strategies.py`:
  - `test_load_balance_picks_least_loaded`: Seed 2 keys under `strategy="load_balance"`. Key A scenario: `Sse` with `SlowBodyBytePerSecond{bps=50}` (long-running). Key B scenario: happy `Static{status=200}`. Issue 1 streaming request to occupy Key A, then immediately issue 1 non-stream request. Assert second request hits Key B (lower inflight score). Check via per-key hit counters.
  - `test_cache_affinity_pins_same_content`: Seed 2 keys under `strategy="cache_affinity"`. Issue 5 requests with identical message content. Assert ALL hit the same key (deterministic hash). Then issue 5 requests with different content — at least 2 distinct keys should be hit.

  **Must NOT do**:
  - Entity ID prefix: `lb_` for load-balance, `ca_` for cache-affinity

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5, 7-10)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/strategy/load_balance.rs` — 0.40*inflight + 0.35*p50 + 0.25*error_rate scoring
  - `crates/gw-routing/src/strategy/cache_affinity.rs` — SHA-256(model+message_hash) mod len

  **API/Type References**:
  - `SlowBodyBytePerSecond` ResponseSpec — creates long-running response to occupy a key slot
  - fake-upstream scenario supports concurrent requests on different routes

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Load-balance routes to least-loaded key
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_strategies.py::test_load_balance_picks_least_loaded -v
    Expected Result: PASSED — second request hits Key B while Key A busy
    Evidence: .omo/evidence/task-6-load-balance.txt

  Scenario: Cache-affinity pins same content to same key
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_strategies.py::test_cache_affinity_pins_same_content -v
    Expected Result: PASSED — same-content requests all hit one key; varied content hits >= 2
    Evidence: .omo/evidence/task-6-cache-affinity.txt
  ```

  **Commit**: YES (C3, grouped with Tasks 4, 5)
  - Files: `tests/e2e/client/scenarios/test_routing_strategies.py`

- [x] 7. Failover Retry — 5xx Failover + 429 Cooldown Recovery

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_failover_retry.py`
  - `test_5xx_failover_recovers_to_next_key`: Seed 2 keys A/B under fixed_order (priority: A=10, B=5). Key A scenario: `Static{status=502}`. Key B scenario: happy `Sse`. Issue 1 stream request. Assert client gets 200 SSE stream. Assert `gw_routing_failover_total{reason="retryable_5xx"}` incremented by 1 via `label_delta()`. Assert `gw_retry_attempt_total{outcome="recovered"}` incremented.
  - `test_429_cooldown_then_recovery`: Seed 1 key. Scenario: `StatusWithRetryAfter{status=429, retry_after_secs=2}` for first request. Issue first request — assert 429 response with error code `GW_UPSTREAM_429` or appropriate. Wait 3s (via retry loop, not sleep). Change scenario to happy `Static{status=200}`. Issue second request — assert 200 success. Assert key was cooled and recovered.
  - `test_5xx_exhausts_retries_returns_502`: Seed 2 keys, both scenario `Static{status=500}`. Issue request. Assert final response is 502/503 with `GW_UPSTREAM_5XX`. Assert `gw_retry_attempt_total{outcome="exhausted"}` incremented.

  **Must NOT do**:
  - Entity ID prefix: `fo_` for failover tests
  - Do not use `time.sleep()` for cooldown wait — use a polling loop with short interval checking metrics or just wait slightly longer than retry_after

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4-6, 8-10)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_routing_retry.py:test_connect_refused_first_key_falls_through_to_second` — existing failover test pattern (but for connect-refused, not 5xx)
  - `crates/gw-routing/src/failover.rs` — `classify_for_failover`: 429→Retryable429, 500-599→Retryable5xx, body_started→NotRetryable

  **API/Type References**:
  - `StatusWithRetryAfter{status, retry_after_secs}` ResponseSpec — fake-upstream returns status with `Retry-After` header
  - `gw_routing_failover_total{reason}` metric — reason values: `retryable_5xx`, `retryable_429`, `connect_failed`
  - `gw_retry_attempt_total{outcome}` metric — outcome values: `recovered`, `exhausted`
  - Error code: `GW_UPSTREAM_5XX` in `crates/gw-core/src/error/codes.rs`

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: 5xx on first key fails over to second key successfully
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_failover_retry.py::test_5xx_failover_recovers_to_next_key -v
    Expected Result: PASSED — 200 response, failover metric incremented
    Evidence: .omo/evidence/task-7-5xx-failover.txt

  Scenario: 429 triggers cooldown then key recovers
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_failover_retry.py::test_429_cooldown_then_recovery -v
    Expected Result: PASSED — first request gets 429, second request (after cooldown) gets 200
    Evidence: .omo/evidence/task-7-429-cooldown.txt

  Scenario: All keys 5xx exhausts retries
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_failover_retry.py::test_5xx_exhausts_retries_returns_502 -v
    Expected Result: PASSED — final response 502/503, exhausted metric incremented
    Evidence: .omo/evidence/task-7-5xx-exhausted.txt
  ```

  **Commit**: YES (C4)
  - Message: `test(e2e): add 5xx failover and 429 cooldown recovery tests`
  - Files: `tests/e2e/client/scenarios/test_failover_retry.py`

- [x] 8. Permissions — 5-Tier Merge + ReDoS Budget

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_permissions.py`
  - `test_permission_merge_model_intersection`: Seed user-group allowing models `["gpt-4", "gpt-3.5-turbo"]`, api-key allowing `["gpt-4", "claude-3"]`. Request `gpt-4` — should succeed (intersection). Request `gpt-3.5-turbo` — should fail with permission denied (not in api-key list). Request `claude-3` — should fail (not in user-group list).
  - `test_permission_merge_rpm_min_positive`: Seed user-group with `rpm_limit=100`, api-key with `rpm_limit=50`. Fire 51 requests rapidly. Assert 51st gets 429 with `GW_RATE_LIMIT_RPM`. Confirms min-positive merge (50, not 100).
  - `test_permission_merge_markup_product`: Seed user-group with `markup_percent=10`, api-key with `markup_percent=20`. Issue request. Check `usage:records` for effective markup = 1.10 * 1.20 = 1.32 (product, not sum).
  - `test_redos_pathological_regex_killed_within_budget`: Seed api-key with `model_whitelist_regex="(a+)+$"`. Request model `"aaaaaaaaaaaaaaaaab"`. Assert response is permission denied (regex timeout, returns None). Assert `gw_redos_timeout_total{api_key_id="..."}` incremented by 1. Assert response latency < 200ms (budget is 100ms + overhead).
  - `test_redos_auto_disable_after_threshold`: Fire 11 requests (TIMEOUT_THRESHOLD=10) with pathological regex. Assert 11th request is permission denied (auto-disabled key's regex returns None, denying all models). Assert `gw_redos_timeout_total{api_key_id="..."}` >= 10. Confirm by requesting a non-pathological model name that would normally match — it should also be denied since the regex evaluator is disabled.

  **Must NOT do**:
  - Entity ID prefix: `pm_` for permission merge, `rd_` for ReDoS
  - Do not test all 5 tiers exhaustively — focus on 2-tier intersection and min-positive

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4-7, 9-10)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/merge.rs:28-169` — INTERSECTION for models/channels/groups (empty=unrestricted, skipped), MIN positive for rpm/tpm, PRODUCT for markup
  - `crates/gw-auth/src/model_permissions.rs` — BUDGET_MS=100, TIMEOUT_THRESHOLD=10, `RedosTracker` auto-disable flag
  - `tests/e2e/client/scenarios/test_quotas.py:test_rpm_exceeded_rejects_429` — existing RPM test pattern

  **API/Type References**:
  - `gw_redos_timeout_total{api_key_id}` metric
  - `seed_user_apikey(...)` — `allowed_models` in api-key payload limits model access
  - User-group entity must be seeded via `fake_web` PUT; check if `seed_user_group()` exists or use raw PUT

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Model intersection permits only shared models
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_permissions.py::test_permission_merge_model_intersection -v
    Expected Result: PASSED — gpt-4 succeeds, gpt-3.5-turbo and claude-3 rejected
    Evidence: .omo/evidence/task-8-model-intersection.txt

  Scenario: RPM merge uses minimum positive value
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_permissions.py::test_permission_merge_rpm_min_positive -v
    Expected Result: PASSED — 429 at request 51 (limit=50, not 100)
    Evidence: .omo/evidence/task-8-rpm-min.txt

  Scenario: ReDoS regex killed within budget
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_permissions.py::test_redos_pathological_regex_killed_within_budget -v
    Expected Result: PASSED — permission denied, metric incremented, latency < 200ms
    Evidence: .omo/evidence/task-8-redos-budget.txt
  ```

  **Commit**: YES (C5)
  - Message: `test(e2e): add 5-tier permission merge and ReDoS budget tests`
  - Files: `tests/e2e/client/scenarios/test_permissions.py`

- [x] 9. Observability — usage:detailed 5-Surface Decode

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_observability.py`
  - `test_detailed_log_emits_all_5_surfaces`: Issue 1 happy non-stream OpenAI request. Use `redis_xrange_decoded()` on `usage:detailed` stream filtering by the request's correlation ID (from response headers or usage:records). Assert exactly 5 entries with `kind` values: `downstream_request`, `upstream_request`, `upstream_response`, `downstream_response`, `summary`. Decode each payload using `decode_detailed_record()`. Assert each decoded payload is valid JSON with non-empty content.
  - `test_detailed_log_encoding_matches_size_thresholds`: Issue a request with large prompt (>1KB to trigger lz4, or >64KB for zstd). Assert `encoding` field matches expected tier. Assert `gw_detailed_log_emit_total{kind="downstream_request",encoding="lz4+json"}` incremented.
  - `test_detailed_log_metric_counters`: Issue 1 request. Assert `gw_detailed_log_emit_total` has 5 increments (one per surface). Assert `gw_detailed_log_drop_total` is 0 for this request.

  **Must NOT do**:
  - Entity ID prefix: `obs_` for observability
  - Do not test drop-on-outage behavior (would need Redis shutdown, too complex for this task)

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
  - Reason: Requires understanding of detailed archive encoding, redis stream structure, and multi-surface correlation

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4-8, 10)
  - **Blocks**: Task 14 (forced_params + TPM additions)
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-ledger/src/detailed_archive.rs` — XADD key `usage:detailed`, fields: `ts_ms`, `request_id`, `trace_id`, `kind`, `encoding`, `payload`
  - `tests/e2e/client/conftest.py:wait_for_records()` — how to wait for redis stream entries

  **API/Type References**:
  - 5 surface kinds: `downstream_request`, `upstream_request`, `upstream_response`, `downstream_response`, `summary`
  - Encoding tiers: `raw+json` (<1024 bytes), `lz4+json` (1024-65536), `zstd+json` (>=65536)
  - Metrics: `gw_detailed_log_emit_total{kind,encoding}`, `gw_detailed_log_drop_total{reason,kind}`
  - Config: `detailed_log.enabled` must be true (check `config/default.toml` or docker-compose env)

  **WHY Each Reference Matters**:
  - `detailed_archive.rs` defines the exact field names and encoding decision logic the test must validate
  - `wait_for_records()` pattern ensures test doesn't race against async XADD

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: All 5 surfaces emitted for a single request
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_observability.py::test_detailed_log_emits_all_5_surfaces -v
    Expected Result: PASSED — 5 entries decoded, all valid JSON
    Evidence: .omo/evidence/task-9-detailed-5-surfaces.txt

  Scenario: Encoding tier matches payload size
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_observability.py::test_detailed_log_encoding_matches_size_thresholds -v
    Expected Result: PASSED — large payload uses lz4+json encoding
    Evidence: .omo/evidence/task-9-encoding-tier.txt
  ```

  **Commit**: YES (C6, grouped with Task 14)
  - Message: `test(e2e): add usage:detailed 5-surface decode and forced_params tests`
  - Files: `tests/e2e/client/scenarios/test_observability.py`

- [x] 10. Resilience — Response Deadline + /readyz Semantics

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_resilience.py`
  - `test_response_deadline_exceeded_on_slow_upstream`: Seed 1 key with `seed_pricing(..., stream_total_timeout_ms=2000)` to set a 2s total stream timeout per-model. Key scenario: `SlowBodyBytePerSecond{bps=10}` on a response body large enough to exceed 2s. Assert response fails with `GW_RESPONSE_DEADLINE_EXCEEDED`. Assert `gw_billing_upstream_cancellation_total{cause="deadline"}` incremented.
  - `test_readyz_returns_200_when_healthy`: GET `/readyz` on health port. Assert 200 and body "ok".
  - `test_readyz_semantics_differ_from_livez`: GET `/livez` → always 200. GET `/readyz` → 200 when healthy. Document that we cannot easily test `/readyz` returning 503 without killing PG (tracked as future enhancement).
  - NOTE: For testing /readyz=503 when PG is down, this would require `docker compose stop postgres` mid-test — mark as P2 future enhancement with a comment in the test file.

  **Must NOT do**:
  - Entity ID prefix: `res_` for resilience
  - Do not kill postgres in this test (too disruptive to shared compose stack)
  - Do not modify gateway env vars in docker-compose.yml for deadline test — use `seed_pricing()` timeout overrides if possible, or add a docker-compose override note

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4-9)
  - **Blocks**: Task 15 (graceful shutdown addition)
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/cancellation.rs` — deadline enforcement: timer starts after upstream connect, fires `GW_RESPONSE_DEADLINE_EXCEEDED`
  - `crates/gw-proxy/src/timeouts.rs` — `billing.response_deadline_ms` default 300000, `cancel_grace_ms` default 200
  - `crates/gw-gateway/src/health.rs` — `/readyz`: READY AtomicBool, returns 200 "ok" or 503 "not ready". `/livez`: always 200 "ok"
  - `tests/e2e/client/scenarios/test_mgmt_token.py:test_livez_still_no_auth` — existing livez assertion pattern

  **API/Type References**:
  - `seed_pricing(..., stream_total_timeout_ms=N, read_timeout_ms=N, total_timeout_ms=N)` — per-model timeout overrides
  - Error code: `GW_RESPONSE_DEADLINE_EXCEEDED` in `crates/gw-core/src/error/codes.rs`
  - Metrics: `gw_billing_upstream_cancellation_total{cause}`, `gw_billing_grace_yielded_final_usage_total`
  - Health port: 8080 (`/livez`, `/readyz`)

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Slow upstream triggers response deadline error
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_resilience.py::test_response_deadline_exceeded_on_slow_upstream -v
    Expected Result: PASSED — GW_RESPONSE_DEADLINE_EXCEEDED returned, metric incremented
    Evidence: .omo/evidence/task-10-deadline.txt

  Scenario: /readyz returns 200 when healthy
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_resilience.py::test_readyz_returns_200_when_healthy -v
    Expected Result: PASSED — 200 "ok"
    Evidence: .omo/evidence/task-10-readyz.txt
  ```

  **Commit**: YES (C7)
  - Message: `test(e2e): add response deadline and readyz semantics tests`
  - Files: `tests/e2e/client/scenarios/test_resilience.py`

- [x] 11. Cross-Protocol Extensions — Non-Stream Matrix + Lossy Metric

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_cross_protocol.py`
  - Add parametrized `test_cross_protocol_non_stream_matrix[from-to]` covering all 6 directional pairs: OAI→Anth, OAI→Gemini, Anth→OAI, Anth→Gemini, Gemini→OAI, Gemini→Anth. Each: client sends in `from` protocol, upstream is `to` protocol (via `protocol_conversion=True` vendor key), non-stream. Assert 200 response with valid content. Assert `gw_codec_cross_render_nonstream_total{from="X",to="Y"}` incremented.
  - `test_cross_protocol_lossy_metric_fires_on_known_drop`: Identify a known lossy field (e.g., Anthropic `system` prompt format vs OpenAI `system` role). Send request with that field via cross-protocol path. Assert `gw_codec_cross_render_lossy_total{from,to,field}` incremented by ≥1.

  **Must NOT do**:
  - Entity ID prefix: `cpn_` for cross-protocol non-stream
  - Do not duplicate existing stream cross-protocol tests

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 12-15)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_cross_protocol.py:test_openai_client_anthropic_upstream_cross_protocol_stream` — existing cross-protocol test pattern with `protocol_conversion=True`
  - `tests/e2e/client/scenarios/test_non_stream.py:test_cross_protocol_non_stream_round_trips` — existing single-pair non-stream cross test

  **API/Type References**:
  - `gw_codec_cross_render_nonstream_total{from,to}` — v2 non-stream cross-render metric
  - `gw_codec_cross_render_lossy_total{from,to,field}` — lossy field-drop metric
  - `seed_vendor_key(..., protocol_conversion=True)` — enables cross-protocol rendering

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: All 6 non-stream cross-protocol pairs work
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_cross_protocol.py -k "non_stream_matrix" -v
    Expected Result: 6 PASSED — each direction renders correctly
    Evidence: .omo/evidence/task-11-nonstream-matrix.txt

  Scenario: Lossy metric fires for known field drop
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_cross_protocol.py::test_cross_protocol_lossy_metric_fires_on_known_drop -v
    Expected Result: PASSED — lossy metric incremented
    Evidence: .omo/evidence/task-11-lossy-metric.txt
  ```

  **Commit**: YES (C8)
  - Message: `test(e2e): extend cross-protocol non-stream matrix and lossy metric coverage`
  - Files: `tests/e2e/client/scenarios/test_cross_protocol.py`

- [x] 12. Auth Extensions — HMAC Body-Tamper + JWT nbf/leeway

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_auth_failures.py`
  - `test_hmac_valid_sig_for_different_body_rejected`: Compute valid HMAC signature for body A. Send body B with that signature + valid timestamp. Assert 401 with appropriate error code (HMAC verification failure). This tests body-integrity, not just sig-presence.
  - `test_jwt_nbf_in_future_rejected`: Generate JWT with `nbf` = now + 600s (well beyond leeway). Assert 401.
  - `test_jwt_leeway_at_boundary`: Generate JWT with `nbf` = now + (leeway - 1s). Assert 200 (within leeway). Generate JWT with `nbf` = now + (leeway + 60s). Assert 401. Leeway configured in gateway config — check `config/default.toml` for value.

  **Must NOT do**:
  - Entity ID prefix: `hmac2_` for HMAC body tests, `jwt2_` for JWT nbf tests
  - Do not break existing HMAC/JWT tests — add new test functions only

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 11, 13-15)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_auth_failures.py:test_hmac_*` — existing HMAC signing pattern (signature generation, timestamp header, signature header)
  - `tests/e2e/client/scenarios/test_auth_failures.py:test_jwt_*` — existing JWT generation and assertion pattern
  - `crates/gw-auth/src/hmac.rs` — HMAC body verify: SHA256 of canonical string including request body

  **API/Type References**:
  - `config/default.toml` — JWT leeway configuration: `auth.jwt_leeway_secs` or similar
  - HMAC canonical string format: check `crates/gw-auth/src/hmac.rs` for exact fields

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Valid HMAC sig on wrong body is rejected
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_auth_failures.py::test_hmac_valid_sig_for_different_body_rejected -v
    Expected Result: PASSED — 401 error
    Evidence: .omo/evidence/task-12-hmac-body-tamper.txt

  Scenario: JWT with future nbf rejected
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_auth_failures.py::test_jwt_nbf_in_future_rejected -v
    Expected Result: PASSED — 401 error
    Evidence: .omo/evidence/task-12-jwt-nbf.txt
  ```

  **Commit**: YES (C9)
  - Message: `test(e2e): extend auth failures with HMAC body-tamper and JWT nbf/leeway`
  - Files: `tests/e2e/client/scenarios/test_auth_failures.py`

- [x] 13. Capability Resolver — Positive Match

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_routing_retry.py`
  - `test_capability_resolver_positive_match`: Seed 2 vendor keys — Key A has capabilities `["vision"]`, Key B has no capabilities. Seed model group requiring `["vision"]`. Issue request to that model. Assert request succeeds AND hits Key A (not Key B). Assert via per-key hit counters or `usage:records` key_id.

  **Must NOT do**:
  - Entity ID prefix: `cap_`
  - Do not modify existing capability negative test

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 11, 12, 14, 15)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_routing_retry.py:test_no_key_advertises_required_capability` — existing negative capability test; shows how to seed `force_capabilities` on model group
  - `crates/gw-capability/src/resolver.rs` — CapabilityResolver filters keys by required capabilities

  **API/Type References**:
  - `seed_model_group(..., force_capabilities=["vision"])` — requires matching capability on vendor key
  - `seed_vendor_key(..., capabilities=["vision"])` — advertises capability

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Capable key selected over incapable key
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_retry.py::test_capability_resolver_positive_match -v
    Expected Result: PASSED — request hits Key A with vision capability
    Evidence: .omo/evidence/task-13-capability-positive.txt
  ```

  **Commit**: YES (C10)
  - Message: `test(e2e): add capability resolver positive match`
  - Files: `tests/e2e/client/scenarios/test_routing_retry.py`

- [x] 14. Observability Extensions — forced_params + TPM

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_observability.py`
  - `test_forced_params_temperature_applied`: Seed model group with `forced_params={"temperature": 0.5}`. Issue request with `temperature=0.9`. Assert `gw_forced_params_applied_total{field="temperature"}` incremented. Assert upstream request (via `usage:detailed` `upstream_request` surface if readable) has `temperature=0.5`.
  - `test_forced_params_top_p_applied`: Same pattern for `top_p`.
  - `test_tpm_deferred_freeze_and_release`: Issue 1 streaming request. During stream, check `billing:inflight:*` Redis HASH exists (frozen_micro > 0). After stream completes, check frozen_micro is released (HASH deleted or frozen_micro=0). Assert `usage:records` final charge matches.

  **Must NOT do**:
  - Entity ID prefix: `fp_` for forced_params, `tpm_` for TPM
  - Do not check Go-side `deducted_micro` (Go not running in e2e)

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 11-13, 15)
  - **Blocks**: None
  - **Blocked By**: Task 9 (same file)

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/upstream.rs:438-563` — forced_params mutation: reads `ModelGroup.forced_params`, overwrites IR fields, increments `gw_forced_params_applied_total{field}`
  - `crates/gw-ledger/src/freeze.rs` — RequestFreeze: atomic frozen_micro; `crates/gw-ledger/src/inflight.rs` — InflightLedger Redis HASH `billing:inflight:{instance}:{request}:{attempt}`

  **API/Type References**:
  - `seed_model_group(..., forced_params={"temperature": 0.5})` — forced_params JSON in model group payload
  - `gw_forced_params_applied_total{field}` metric — field values: `temperature`, `top_p`
  - Redis key pattern: `billing:inflight:*` — HGETALL returns `frozen_micro` field

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: forced_params overrides client temperature
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_observability.py::test_forced_params_temperature_applied -v
    Expected Result: PASSED — metric incremented, upstream got forced value
    Evidence: .omo/evidence/task-14-forced-params.txt

  Scenario: TPM freeze created during stream, released after
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_observability.py::test_tpm_deferred_freeze_and_release -v
    Expected Result: PASSED — inflight key exists during stream, cleaned after
    Evidence: .omo/evidence/task-14-tpm-freeze.txt
  ```

  **Commit**: YES (C6, grouped with Task 9)
  - Files: `tests/e2e/client/scenarios/test_observability.py`

- [x] 15. Resilience Extensions — Graceful Shutdown

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_resilience.py`
  - `test_graceful_shutdown_drains_inflight`: Issue 3 concurrent streaming requests (use `SlowBodyBytePerSecond{bps=100}` for ~5s streams). Send `docker compose kill -s SIGTERM gateway`. Assert all 3 client connections complete without error (streams finish, not abruptly closed). Assert `usage:records` has 3 records with valid charges. Assert no `billing:inflight:*` keys remain in Redis.
  - NOTE: After SIGTERM, gateway container stops — subsequent tests won't work. This test MUST be the last test in the file (or use a fixture that restarts gateway after).

  **Must NOT do**:
  - Entity ID prefix: `gs_`
  - Do not use `docker compose stop` (sends SIGTERM + timeout + SIGKILL) — use `docker compose kill -s SIGTERM` for explicit signal
  - Do not skip gateway restart after this test

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
  - Reason: Complex orchestration: concurrent streams + container lifecycle + post-drain assertions

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 11-14)
  - **Blocks**: None
  - **Blocked By**: Task 10 (same file)

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_reliability.py:test_gateway_container_restart_recovers` — existing container lifecycle test pattern
  - `crates/gw-gateway/src/main.rs` — Pingora graceful shutdown: SIGTERM triggers drain mode

  **API/Type References**:
  - `docker compose -f ../docker-compose.yml kill -s SIGTERM gateway` — send SIGTERM
  - `docker compose -f ../docker-compose.yml up -d gateway` — restart after test
  - Redis key pattern: `billing:inflight:*` — should be empty after clean drain
  - `SlowBodyBytePerSecond{bps=100}` — creates ~5s stream for a 500-byte body

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: SIGTERM drains in-flight requests cleanly
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_resilience.py::test_graceful_shutdown_drains_inflight -v
    Expected Result: PASSED — all 3 streams complete, no inflight leaks
    Evidence: .omo/evidence/task-15-graceful-shutdown.txt

  Scenario: Gateway restarts after graceful shutdown test
    Tool: Bash (curl)
    Steps:
      1. After test, run: curl -sf http://localhost:8080/readyz
    Expected Result: 200 "ok" — gateway back up
    Evidence: .omo/evidence/task-15-restart-verify.txt
  ```

  **Commit**: YES (C12, grouped with Task 17)
  - Message: `test(e2e): add graceful shutdown and KEK rotation tests`
  - Files: `tests/e2e/client/scenarios/test_resilience.py`

- [x] 16. Tunnel Mode Tests

  **What to do**:
  - Create `tests/e2e/client/scenarios/test_tunnel.py`
  - NOTE: Tunnel requires gateway built with `--features tunnel` and a running tunnel node. The e2e docker-compose may need a `gateway-tunnel` service variant or env override. If `--features tunnel` is not buildable in current e2e infra, mark tests as `@pytest.mark.skip(reason="requires --features tunnel build")` and document setup requirements.
  - `test_tunnel_no_node_returns_hard_error`: Seed vendor key with `tunnel_preference={"require_tunnel": true}`. No tunnel node registered. Issue request. Assert response status 502 with error code `GW_TUNNEL_NO_NODE`. Assert `gw_tunnel_resolve_total{result="no_node"}` incremented.
  - `test_tunnel_target_denied`: Seed vendor key with `require_tunnel=true`. Register a tunnel node but with `target_whitelist` that excludes the upstream domain. Issue request. Assert `GW_TUNNEL_TARGET_DENIED` (502).
  - `test_tunnel_direct_vs_tunnel_byte_identity`: Seed 2 vendor keys — one direct, one tunnel (with valid node). Issue same request to both. Assert response bodies are byte-identical.

  **Must NOT do**:
  - Entity ID prefix: `tn_`
  - Do not modify `docker-compose.yml` for tunnel unless absolutely necessary — prefer env var override or skip marker
  - Do not block the PR on tunnel tests — if infra not ready, skip-mark is acceptable

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
  - Reason: Requires understanding of tunnel feature gate, docker-compose service setup, and WebSocket transport

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 17-19)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/tunnel_stack.rs` — `TunnelStack::resolve` per-request routing
  - `crates/e2e-fake-upstream/src/tunnel_node.rs` — existing tunnel node stub in fake-upstream
  - `docs/superpowers/specs/2026-05-22-tunnel-t-design.md` — tunnel architecture spec

  **API/Type References**:
  - Error codes: `GW_TUNNEL_NO_NODE`, `GW_TUNNEL_NO_LIVE`, `GW_TUNNEL_TARGET_DENIED` — all 502
  - Metric: `gw_tunnel_resolve_total{result}` — result values: `direct`, `tunnel`, `no_node`, `no_live`, `denied`
  - Config: `tunnel.enabled=true` (boot-time), `tunnel.target_whitelist`
  - VendorKey payload: `tunnel_preference: { require_tunnel: true }`

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Missing tunnel node returns hard error
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_tunnel.py::test_tunnel_no_node_returns_hard_error -v
    Expected Result: PASSED (or SKIPPED if tunnel feature not built)
    Evidence: .omo/evidence/task-16-tunnel-no-node.txt

  Scenario: Direct vs tunnel byte identity
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_tunnel.py::test_tunnel_direct_vs_tunnel_byte_identity -v
    Expected Result: PASSED — responses byte-equal (or SKIPPED)
    Evidence: .omo/evidence/task-16-tunnel-identity.txt
  ```

  **Commit**: YES (C11)
  - Message: `test(e2e): add tunnel mode tests (no-node hard error, direct-vs-tunnel identity)`
  - Files: `tests/e2e/client/scenarios/test_tunnel.py`

- [x] 17. KEK Rotation Test

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_resilience.py`
  - `test_kek_rotation_dual_decrypt_window`: First, read `crates/gw-gateway/src/kms.rs` and `crates/gw-store/src/encrypt.rs` (dual-decrypt evidence at `crates/gw-store/tests/encrypt_dual_read.rs`) to determine if dual-decrypt (try old KEK then new KEK) is implemented. If YES: Seed a vendor key with `signature_secret` (encrypted at rest under KEK K1). Read the key successfully. Restart gateway container with new `GW_KMS__KEK_HEX=K2`. Assert the old key can still be read (dual-decrypt). Seed a new key under K2. Assert both old and new keys readable. If dual-decrypt is NOT implemented: mark test `@pytest.mark.skip(reason="KEK dual-decrypt not implemented — single-KEK only")` and add a code comment documenting the finding.
  - This test requires gateway container restart with new KEK. Use `@pytest.mark.kek_rotation` marker and exclude from default runs via `pytest.ini`.

  **Must NOT do**:
  - Entity ID prefix: `kek_`
  - Do not test KEK rotation without understanding if dual-decrypt is implemented — read `crates/gw-kms/` first
  - If dual-decrypt not implemented, skip test and document as known gap

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 16, 18, 19)
  - **Blocks**: None
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `crates/gw-kms/` — KEK management, encryption/decryption of `signature_secret`
  - `docs/architecture.md` §KEK rotation — documented rotation semantics

  **API/Type References**:
  - `GW_KMS__KEK_HEX` env var — 32-byte hex key
  - `api_keys.signature_secret` — encrypted column

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: KEK rotation allows reading keys encrypted under old KEK
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_resilience.py::test_kek_rotation_dual_decrypt_window -v
    Expected Result: PASSED if dual-decrypt implemented; SKIPPED with reason if not
    Failure Indicators: 500 error on key load after KEK rotation = dual-decrypt missing
    Evidence: .omo/evidence/task-17-kek-rotation.txt

  Scenario: Verify KMS crate was read to ground the test
    Tool: Bash (grep)
    Steps:
      1. Run: grep -r "dual\|old_kek\|previous" crates/gw-gateway/src/kms.rs crates/gw-store/src/encrypt.rs | head -10
    Expected Result: Output reveals whether dual-decrypt logic exists (informs skip/test decision)
    Evidence: .omo/evidence/task-17-kms-investigation.txt
  ```

  **Commit**: YES (C12, grouped with Task 15)
  - Files: `tests/e2e/client/scenarios/test_resilience.py`

- [x] 18. Routing Strategies — Sticky TTL Edge Cases

  **What to do**:
  - Extend `tests/e2e/client/scenarios/test_routing_strategies.py`
  - `test_sticky_rebinds_after_ttl_expiry`: Seed 2 keys under `strategy="sticky"`. Issue request with conversation_id="conv-1". Assert binds to Key A (or B). Wait for sticky TTL to expire (currently 2s in docker-compose via `GW_ROUTING__STICKY_TTL_SECS=2`). Issue another request with same conversation_id. Assert `gw_sticky_bind_total` incremented again (rebind, not hit). NOTE: May bind to same key — the point is that the binding was re-evaluated, not cached.
  - `test_sticky_different_conversations_different_keys`: Issue 10 requests each with unique conversation_id. Assert at least 2 distinct keys used (not all sticky to one). Assert `gw_sticky_bind_total` incremented 10 times.

  **Must NOT do**:
  - Entity ID prefix: `stk_`
  - Do not reduce `STICKY_TTL_SECS` further — 2s is already short for CI

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 16, 17, 19)
  - **Blocks**: None
  - **Blocked By**: Task 4 (same file)

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_routing_retry.py:test_sticky_strategy_binds_user_to_same_key_then_expires` — existing sticky test (basic bind + expire)
  - `crates/gw-routing/src/strategy/sticky.rs` — moka cache with TTL, `gw_sticky_hit_total`, `gw_sticky_bind_total`

  **API/Type References**:
  - `GW_ROUTING__STICKY_TTL_SECS=2` — docker-compose env override
  - Metrics: `gw_sticky_hit_total` (cache hit), `gw_sticky_bind_total` (new binding)

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Sticky rebinds after TTL expiry
    Tool: Bash (pytest)
    Steps:
      1. Run: pytest scenarios/test_routing_strategies.py::test_sticky_rebinds_after_ttl_expiry -v
    Expected Result: PASSED — rebind metric incremented after TTL wait
    Evidence: .omo/evidence/task-18-sticky-rebind.txt
  ```

  **Commit**: YES (C3, grouped with Tasks 4-6)
  - Files: `tests/e2e/client/scenarios/test_routing_strategies.py`

- [x] 19. Documentation Finalization

  **What to do**:
  - Review all new test files and ensure they follow documentation conventions
  - If any new test markers were added (e.g., `@pytest.mark.kek_rotation`, `@pytest.mark.tunnel`), update `pytest.ini` to register them (avoid `PytestUnknownMarkWarning`)
  - Update `docs/architecture.md` §10.3 if new make targets or test infrastructure was added
  - Verify CLAUDE.md soak recommendation (from Task 3) is consistent with actual test layout
  - Add module docstrings to all new test files explaining what capability surface they cover

  **Must NOT do**:
  - Do not create new markdown files outside `.omo/`
  - Do not modify workflow.md unless filter behavior changed (it didn't — tests only)

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 16-18)
  - **Blocks**: None
  - **Blocked By**: Task 3 (CLAUDE.md), all Wave 2-3 test files

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_happy_streaming.py` — example module docstring format
  - `pytest.ini` — existing marker registrations and addopts

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: No pytest marker warnings
    Tool: Bash (pytest)
    Steps:
      1. Run: cd tests/e2e/client && .venv/bin/python3 -m pytest --co -q 2>&1 | grep -i "unknown mark"
    Expected Result: No output (no unknown mark warnings)
    Evidence: .omo/evidence/task-19-no-marker-warnings.txt

  Scenario: All new test files have module docstrings
    Tool: Bash (grep)
    Steps:
      1. Run: for f in test_routing_strategies.py test_failover_retry.py test_permissions.py test_observability.py test_resilience.py test_tunnel.py; do head -5 tests/e2e/client/scenarios/$f; done
    Expected Result: Each file starts with triple-quoted docstring
    Evidence: .omo/evidence/task-19-docstrings.txt
  ```

  **Commit**: YES (part of final squash or individual)
  - Message: `test(e2e): add pytest markers and module docstrings for new test files`
  - Files: `pytest.ini`, `docs/architecture.md` (if changed), new test files (docstrings)

---

## Final Verification Wave

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in `.omo/evidence/`. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run linter. Review all new test files for: duplicate test IDs, `time.sleep()` usage, missing `_error_code()` assertions, hardcoded ports, missing `wait_for_records()`. Check naming conventions match existing tests. Verify no gateway Rust source modified.
  Output: `Lint [PASS/FAIL] | Conventions [N clean/N issues] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Run `make e2e` — capture full output. Run `make e2e-soak-smoke` — capture output. Verify all new tests pass. Count total pass/fail/skip/deselect. Save to `.omo/evidence/final-qa/`.
  Output: `e2e [N/N pass] | soak-smoke [PASS/FAIL] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff. Verify 1:1 — everything in spec was built, nothing beyond spec was built. Check "Must NOT do" compliance (no Rust source changes, no make e2e soak inclusion). Detect unaccounted changes. Flag any new Python dependencies.
  Output: `Tasks [N/N compliant] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

- **C1**: `test(e2e): add conftest helpers for detailed-log decode and permission merge seeds` — conftest.py
- **C2**: `docs: add soak smoke recommendation to CLAUDE.md` — CLAUDE.md
- **C3**: `test(e2e): add routing strategy tests (round-robin, weighted-rr, cost-first, latency-first, load-balance, cache-affinity)` — test_routing_strategies.py
- **C4**: `test(e2e): add 5xx failover and 429 cooldown recovery tests` — test_failover_retry.py
- **C5**: `test(e2e): add 5-tier permission merge and ReDoS budget tests` — test_permissions.py
- **C6**: `test(e2e): add usage:detailed 5-surface decode and forced_params tests` — test_observability.py
- **C7**: `test(e2e): add response deadline and readyz semantics tests` — test_resilience.py
- **C8**: `test(e2e): extend cross-protocol non-stream matrix and lossy metric coverage` — test_cross_protocol.py
- **C9**: `test(e2e): extend auth failures with HMAC body-tamper and JWT nbf/leeway` — test_auth_failures.py
- **C10**: `test(e2e): add capability resolver positive match` — test_routing_retry.py
- **C11**: `test(e2e): add tunnel mode tests (no-node hard error, direct-vs-tunnel identity)` — test_tunnel.py (conditional)
- **C12**: `test(e2e): add graceful shutdown and KEK rotation tests` — test_resilience.py
- **Final**: Squash or keep per user preference at PR time

---

## Success Criteria

### Verification Commands
```bash
make e2e                        # Expected: all pass, 0 failures
make e2e-soak-smoke             # Expected: 1 passed
cargo fmt --all -- --check      # Expected: clean
bash scripts/check-invariants.sh # Expected: all hold
```

### Final Checklist
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All new tests pass in `make e2e`
- [ ] Soak smoke still green
- [ ] No Rust source modifications
- [ ] CLAUDE.md soak recommendation added
- [ ] All entity IDs globally unique across test suite
