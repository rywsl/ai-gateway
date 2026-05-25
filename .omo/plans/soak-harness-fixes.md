# Soak Harness Soft-Warning Fixes

## TL;DR

> **Quick Summary**: Fix 4 non-blocking soft warnings from e2e soak harness. All are harness defects — zero gateway code changes. Seed under-provisioning → multi-key round_robin; hard-coded threshold → read from table; RSS None→0 conflation → proper tri-state; docs sync.
> 
> **Deliverables**:
> - Seed provisions 8 vendor keys per model group with `round_robin` strategy
> - `soft_records_to_2xx_ratio()` reads threshold from `_SOFT_THRESHOLDS[chaos_mode]`; heavy raised to 1.30
> - `gateway_rss_bytes()` uses `/proc/1/status` VmRSS; propagates `None` on failure; invariant returns `passed: None` with reason
> - Docs updated with resolution markers
> 
> **Estimated Effort**: Short (4 commits, harness-only)
> **Parallel Execution**: YES — 3 waves
> **Critical Path**: Task 1 (seed) → Task 5 (smoke verify) → Task 6 (docs)

---

## Context

### Original Request
Run e2e soak, record defects. Soak passed all hard invariants. 4 non-blocking soft warnings discovered:
1. chaos-off 2xx rate 33% (threshold 99.9%) — seed under-provisioning
2. chaos-off p99 5,705ms (threshold 200ms) — same root cause
3. heavy chaos records_to_2xx_ratio 1.171 (threshold 1.10) — hard-coded threshold ignores table
4. RSS 0/0 vacuously passing — None→0 conflation at call site

### Interview Summary
**Key Discussions**:
- All 4 problems are harness defects, not gateway bugs
- Problems 1+2 share root cause: single vendor key can't sustain 833 rps (Little's Law: need concurrency=1666, have 512)
- User initially chose Branch C (scale fake-upstream), rejected after analysis showed per-key in-flight cap is the real bottleneck
- User confirmed Branch A: 8 keys per group, round_robin, preserving C5

**Research Findings**:
- `gateway_rss_bytes()` at `conftest.py:1098-1129`: parses `docker stats --no-stream`, returns `None` on failure
- Call site at `test_chaos_soak.py:161-162`: `rss_before = gateway_rss_bytes() or 0` coalesces None→0
- `invariants.py:366-376`: `soft_records_to_2xx_ratio()` hard-codes `th = 1.10`
- `invariants.py:308-321`: `_SOFT_THRESHOLDS` table correctly defined but unused by ratio function
- `seed.py:87-115`: 4 groups × 1 key, `strategy="fixed_order"`
- Other soft functions (`soft_success_rate`, `soft_latency_p99`, `soft_completion_rate`) correctly read from table

### Metis Review
**Identified Gaps** (addressed):
- Commit ordering: Metis recommended seed→ratio→RSS→docs (not RSS first) so each smoke validates against increasingly correct baseline
- RSS `/proc/1/status` returns kB → must multiply by 1024
- `passed: None` consumers need tri-state handling verification
- 8-key round_robin may change chaos failure distribution (diluted vs concentrated)
- Threshold 1.30 needs empirical rationale in comment
- Seed determinism: fresh UUIDs each run acceptable (no cross-run correlation needed)

---

## Work Objectives

### Core Objective
Fix 4 non-blocking soak soft warnings by correcting harness defects: seed capacity, threshold implementation, RSS measurement, and docs sync.

### Concrete Deliverables
- `tests/e2e/client/soak/seed.py`: 8 vendor keys per model group, `strategy="round_robin"`
- `tests/e2e/client/soak/invariants.py`: parameterized `soft_records_to_2xx_ratio()`, heavy threshold 1.30
- `tests/e2e/client/conftest.py`: `/proc/1/status` VmRSS-based `gateway_rss_bytes()`
- `tests/e2e/client/scenarios/test_chaos_soak.py`: propagate `None` (no `or 0`), pass `chaos_mode`
- `docs/issues/2026-05-25-soak-soft-warnings.md`: 4 resolution markers
- Soak spec `docs/superpowers/specs/2026-05-22-chaos-soak-design.md` §4/§11.2 updated

### Definition of Done
- [ ] `make e2e-soak-smoke` exits 0
- [ ] `make e2e-soak` (full) exits 0 with zero soft warnings for these 4 issues
- [ ] All hard invariants still pass
- [ ] `scripts/check-invariants.sh` still passes

### Must Have
- 8 vendor keys per model group with `round_robin` strategy
- `soft_records_to_2xx_ratio()` accepts `chaos_mode` parameter, reads from `_SOFT_THRESHOLDS`
- Heavy `records_to_2xx_ratio_max` = 1.30 with empirical rationale comment
- `gateway_rss_bytes()` uses `docker exec <cid> cat /proc/1/status | grep VmRSS`
- `resource_rss_growth_under_threshold()` accepts `Optional[int]`, returns `passed: None` with reason
- VmRSS kB→bytes conversion (×1024)
- C5 compliance: `retry_policy.enabled=false` on all seeded keys
- Resolution markers in docs issue file

### Must NOT Have (Guardrails)
- ANY changes under `crates/` — zero gateway code modifications
- Changes to `tests/e2e/docker-compose.yml` — no compose modifications
- Changes to `SoakConfig` defaults (`target_rps`, `n_threads`, `max_inflight_per_thread`)
- Modifications to hard invariants (account_consistency, process_alive, resource_release, error_code_compliance, chaos_causal, min_completion)
- Modifications to `_SOFT_THRESHOLDS` entries other than `records_to_2xx_ratio_max` for heavy mode
- Retries, fallback strategies, or changes to cross_vendor_fallback
- Refactoring of `soft_success_rate`, `soft_latency_p99`, `soft_completion_rate` — they already work correctly
- New soft warnings, invariants, or metrics
- Re-tuning `light` or `off` chaos thresholds

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (pytest harness)
- **Automated tests**: YES (tests-after — the soak IS the test)
- **Framework**: pytest + make targets
- **No TDD**: changes are to test harness itself, not production code

### QA Policy
Every task includes agent-executed QA scenarios.
Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Harness changes**: Use Bash — run pytest/make, parse JSON reports, assert fields
- **Docs changes**: Use Bash (grep) — verify resolution markers

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — independent fixes, MAX PARALLEL):
├── Task 1: Seed multi-key provisioning [quick]
├── Task 2: records_to_2xx_ratio threshold parameterization [quick]
├── Task 3: RSS measurement + tri-state invariant [unspecified-high]

Wave 2 (After Wave 1 — orchestrator integration):
├── Task 4: test_chaos_soak.py integration (depends: 1, 2, 3) [quick]

Wave 3 (After Wave 2 — docs + verification):
├── Task 5: Smoke verification run (depends: 4) [quick]
├── Task 6: Docs sync (depends: 4) [writing]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── F1: Plan compliance audit (oracle)
├── F2: Code quality review (unspecified-high)
├── F3: Real manual QA (unspecified-high)
└── F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

Critical Path: Task 1 → Task 4 → Task 5 → F1-F4 → user okay
Parallel Speedup: ~50% faster than sequential (Wave 1 has 3 parallel tasks)
Max Concurrent: 3 (Wave 1)

### Dependency Matrix

| Task | Depends On | Blocks |
|------|-----------|--------|
| 1 | — | 4, 5 |
| 2 | — | 4, 5 |
| 3 | — | 4, 5 |
| 4 | 1, 2, 3 | 5, 6 |
| 5 | 4 | F1-F4 |
| 6 | 4 | F1-F4 |

### Agent Dispatch Summary

- **Wave 1**: 3 tasks — T1 → `quick`, T2 → `quick`, T3 → `unspecified-high`
- **Wave 2**: 1 task — T4 → `quick`
- **Wave 3**: 2 tasks — T5 → `quick`, T6 → `writing`
- **FINAL**: 4 tasks — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [ ] 1. Seed multi-key provisioning (8 keys per model group, round_robin)

  **What to do**:
  - In `tests/e2e/client/soak/seed.py`, modify each of the 4 `seed_vendor_key()` calls to provision **8 vendor keys** per model group instead of 1
  - Use a loop: `for i in range(8): seed_vendor_key(key_id=f"vk-soak-{kind}-{i}", ...)` for each of the 4 groups (open_ai, anthropic, gemini, cross)
  - Change `strategy="fixed_order"` to `strategy="round_robin"` in each `seed_model_group()` call
  - Keep `cross_vendor_fallback=False` unchanged
  - Keep `retry_policy` disabled (C5 compliance)
  - All 8 keys per group point to the same `base = "http://fake-upstream:9000"` and same vendor
  - Each key gets the same capability profile as the original single key

  **Must NOT do**:
  - Change `SoakConfig` defaults (target_rps, n_threads, max_inflight_per_thread)
  - Add retry logic or fallback strategies
  - Modify docker-compose
  - Change vendor definitions or user definitions

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single-file change, pattern-repetitive (loop over existing structure)
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - `playwright`: No UI involved
    - `git-master`: Commit handled by orchestrator

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3)
  - **Blocks**: Task 4 (orchestrator integration)
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `tests/e2e/client/soak/seed.py:87-115` — Current seed_vendor_key() calls, one per group. Follow existing parameter patterns for the 7 additional keys per group.
  - `tests/e2e/client/soak/seed.py:117-140` — Current seed_model_group() calls with `strategy="fixed_order"`. Change to `"round_robin"`.

  **API/Type References**:
  - `tests/e2e/client/soak/seed.py:seed_vendor_key()` function signature — parameters: `key_id`, `vendor_id`, `base_url`, `model_pattern`, `capabilities`
  - `tests/e2e/client/soak/seed.py:seed_model_group()` function signature — parameters include `strategy`, `cross_vendor_fallback`

  **WHY Each Reference Matters**:
  - seed.py is self-contained: both the function definitions and their call sites are in the same file
  - The existing single-key pattern shows exactly what parameters each new key needs
  - C5 compliance depends on `cross_vendor_fallback=False` + retry disabled — verify these are preserved

  **Acceptance Criteria**:
  - [ ] `grep -c "round_robin" tests/e2e/client/soak/seed.py` returns 4 (one per model group call)
  - [ ] After seeding, DB shows 32 vendor keys total (8 per group × 4 groups)
  - [ ] `grep "fixed_order" tests/e2e/client/soak/seed.py` returns empty (no more fixed_order)
  - [ ] `grep "cross_vendor_fallback=False" tests/e2e/client/soak/seed.py` still present for all groups

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: 8 vendor keys seeded per model group
    Tool: Bash
    Preconditions: `make e2e-up` running, seed applied
    Steps:
      1. Run `cd tests/e2e/client && python -c "from soak.seed import seed_all; seed_all()" 2>&1`
      2. Run `python -c "import httpx; r = httpx.get('http://localhost:8080/admin/vendor-keys'); print(len(r.json()))"` (or equivalent DB query)
      3. Assert total vendor keys = 32 (8 × 4 groups)
    Expected Result: 32 vendor keys provisioned, all pointing to fake-upstream:9000
    Failure Indicators: KeyError on seed, count != 32, any key missing capabilities
    Evidence: .omo/evidence/task-1-seed-count.txt

  Scenario: C5 compliance — no retry enabled
    Tool: Bash
    Preconditions: seed.py modified
    Steps:
      1. `grep -n "retry" tests/e2e/client/soak/seed.py`
      2. Assert no `retry_policy.*enabled.*true` or `retry_policy.*enabled=True`
    Expected Result: Zero lines matching retry enabled
    Failure Indicators: Any retry_policy with enabled=True
    Evidence: .omo/evidence/task-1-c5-compliance.txt
  ```

  **Commit**: YES (commit 1)
  - Message: `fix(e2e): provision 8 vendor keys per model group with round_robin`
  - Files: `tests/e2e/client/soak/seed.py`
  - Pre-commit: `make e2e-soak-smoke`

- [ ] 2. Parameterize `soft_records_to_2xx_ratio()` threshold from `_SOFT_THRESHOLDS`

  **What to do**:
  - In `tests/e2e/client/soak/invariants.py`, add `chaos_mode: str` parameter to `soft_records_to_2xx_ratio()` function signature
  - Replace hard-coded `th = 1.10` (line ~369) with `th = _SOFT_THRESHOLDS[chaos_mode]["records_to_2xx_ratio_max"]`
  - In `_SOFT_THRESHOLDS` dict (lines 308-321), change the `heavy` mode's `records_to_2xx_ratio_max` from `1.10` to `1.30`
  - Add inline comment on the 1.30 value: `# Empirical: observed 1.171 under heavy chaos (2026-05-25); 1.30 allows ~30% billable non-2xx fraction from upstream-attempted failures (502/429)`
  - Keep `off` and `light` modes at `1.10` unchanged
  - Follow the pattern of `soft_success_rate()`, `soft_latency_p99()`, `soft_completion_rate()` which already read from `_SOFT_THRESHOLDS[chaos_mode]`

  **Must NOT do**:
  - Modify `_SOFT_THRESHOLDS` entries other than `records_to_2xx_ratio_max` for heavy
  - Refactor other soft_* functions — they already work correctly
  - Add new soft warnings or invariants
  - Re-tune `light` or `off` thresholds

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single-file, 5-line change following existing pattern
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3)
  - **Blocks**: Task 4 (orchestrator integration)
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `tests/e2e/client/soak/invariants.py:329-341` — `soft_success_rate(rpt, *, chaos_mode)`: Follow this exact pattern for adding `chaos_mode` parameter and reading from `_SOFT_THRESHOLDS`
  - `tests/e2e/client/soak/invariants.py:343-353` — `soft_latency_p99(rpt, *, chaos_mode)`: Second example of same pattern
  - `tests/e2e/client/soak/invariants.py:355-365` — `soft_completion_rate(rpt, *, chaos_mode)`: Third example

  **API/Type References**:
  - `tests/e2e/client/soak/invariants.py:308-321` — `_SOFT_THRESHOLDS` dict structure: `{mode: {metric_name: value}}`
  - `tests/e2e/client/soak/invariants.py:366-376` — Current `soft_records_to_2xx_ratio()` with hard-coded `th = 1.10`

  **WHY Each Reference Matters**:
  - The 3 existing soft_* functions show the EXACT pattern to follow (parameter name, lookup syntax, return structure)
  - The `_SOFT_THRESHOLDS` dict already has the `records_to_2xx_ratio_max` key — just need to read it
  - Line 369's `th = 1.10` is the specific line to replace

  **Acceptance Criteria**:
  - [ ] `grep "1.10" tests/e2e/client/soak/invariants.py` matches only `off` and `light` entries in `_SOFT_THRESHOLDS`, NOT the function body
  - [ ] `grep "1.30" tests/e2e/client/soak/invariants.py` matches the `heavy` entry in `_SOFT_THRESHOLDS`
  - [ ] `grep "chaos_mode" tests/e2e/client/soak/invariants.py` matches in `soft_records_to_2xx_ratio` function signature

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Threshold reads from _SOFT_THRESHOLDS for heavy mode
    Tool: Bash
    Preconditions: invariants.py modified
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         from soak.invariants import _SOFT_THRESHOLDS
         assert _SOFT_THRESHOLDS['heavy']['records_to_2xx_ratio_max'] == 1.30, 'heavy threshold not 1.30'
         assert _SOFT_THRESHOLDS['off']['records_to_2xx_ratio_max'] == 1.10, 'off threshold changed'
         assert _SOFT_THRESHOLDS['light']['records_to_2xx_ratio_max'] == 1.10, 'light threshold changed'
         print('PASS: thresholds correct')
         "
    Expected Result: Prints "PASS: thresholds correct", exit 0
    Failure Indicators: AssertionError, import error, KeyError
    Evidence: .omo/evidence/task-2-threshold-values.txt

  Scenario: Function uses chaos_mode parameter (not hard-coded)
    Tool: Bash
    Preconditions: invariants.py modified
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         import inspect
         from soak.invariants import soft_records_to_2xx_ratio
         sig = inspect.signature(soft_records_to_2xx_ratio)
         assert 'chaos_mode' in sig.parameters, f'chaos_mode not in params: {sig}'
         print('PASS: chaos_mode parameter exists')
         "
    Expected Result: Prints "PASS: chaos_mode parameter exists", exit 0
    Failure Indicators: AssertionError showing missing parameter
    Evidence: .omo/evidence/task-2-chaos-mode-param.txt

  Scenario: Missing chaos_mode raises KeyError (not silent default)
    Tool: Bash
    Preconditions: invariants.py modified
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         from soak.invariants import soft_records_to_2xx_ratio
         try:
             soft_records_to_2xx_ratio(None, records_delta=10, chaos_mode='nonexistent')
             print('FAIL: no error raised')
         except KeyError as e:
             print(f'PASS: KeyError raised for unknown mode: {e}')
         "
    Expected Result: Prints "PASS: KeyError raised for unknown mode: 'nonexistent'"
    Failure Indicators: No error raised, or wrong error type
    Evidence: .omo/evidence/task-2-unknown-mode-error.txt
  ```

  **Commit**: YES (commit 2)
  - Message: `fix(e2e): parameterize records_to_2xx_ratio threshold from _SOFT_THRESHOLDS`
  - Files: `tests/e2e/client/soak/invariants.py`
  - Pre-commit: `make e2e-soak-smoke`

- [ ] 3. RSS measurement: `/proc/1/status` VmRSS + tri-state invariant

  **What to do**:
  - In `tests/e2e/client/conftest.py`, replace the `docker stats --no-stream` parsing in `gateway_rss_bytes()` with:
    1. Get container ID: `docker compose -f tests/e2e/docker-compose.yml ps -q gateway`
    2. Run `docker exec <cid> cat /proc/1/status` and grep for `VmRSS:` line
    3. Parse the kB value and multiply by 1024 to get bytes
    4. Return `None` with logged reason on any failure: container missing, exec failed, VmRSS line absent, parse error
    5. Keep `docker stats` as a **fallback** if `docker exec` fails (defense in depth)
  - In `tests/e2e/client/soak/invariants.py`, modify `resource_rss_growth_under_threshold()`:
    1. Change signature: `(before_bytes: Optional[int], after_bytes: Optional[int], threshold_mib: int = 100)`
    2. If either is `None`: return `{"passed": None, "raw": {"reason": "RSS measurement unavailable: before={before_bytes}, after={after_bytes}", "before_bytes": before_bytes, "after_bytes": after_bytes}}`
    3. Keep existing logic for when both are valid ints

  **Must NOT do**:
  - Replace `docker stats` elsewhere — only the RSS sampler
  - Modify hard invariants
  - Change threshold_mib default

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Two files, error handling paths, unit conversion, needs careful edge case coverage
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2)
  - **Blocks**: Task 4 (orchestrator integration)
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `tests/e2e/client/conftest.py:1098-1129` — Current `gateway_rss_bytes()` implementation. Replace the `docker stats` core but preserve the container-ID lookup pattern.
  - `tests/e2e/client/soak/invariants.py:185-205` — Current `resource_rss_growth_under_threshold()`. Add Optional handling at the top, keep existing calculation below.
  - `tests/e2e/client/soak/invariants.py:15` — "data unavailable" convention: `passed: None` with `raw.reason` — follow this exactly for RSS measurement failure.

  **External References**:
  - `/proc/<pid>/status` format: `VmRSS:    12345 kB` — always in kB, parse second token, multiply by 1024

  **WHY Each Reference Matters**:
  - `conftest.py:1098` shows the exact function to modify and its callers
  - `invariants.py:185` shows the exact function that consumes the RSS values
  - Line 15 convention ensures consistency with how other invariants handle missing data

  **Acceptance Criteria**:
  - [ ] `grep "VmRSS" tests/e2e/client/conftest.py` matches in `gateway_rss_bytes()`
  - [ ] `grep "1024" tests/e2e/client/conftest.py` matches (kB→bytes conversion)
  - [ ] `grep "Optional" tests/e2e/client/soak/invariants.py` matches in `resource_rss_growth_under_threshold` signature
  - [ ] `grep '"passed": None' tests/e2e/client/soak/invariants.py` or equivalent matches in the function

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: RSS returns positive int on healthy container
    Tool: Bash
    Preconditions: `make e2e-up` running
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         from conftest import gateway_rss_bytes
         v = gateway_rss_bytes()
         assert v is not None and v > 1024*1024, f'Expected >1MiB, got {v!r}'
         print(f'PASS: RSS = {v} bytes ({v/1024/1024:.1f} MiB)')
         "
    Expected Result: Prints positive MiB value, exit 0
    Failure Indicators: None returned, value < 1MiB, parse error
    Evidence: .omo/evidence/task-3-rss-positive.txt

  Scenario: RSS kB→bytes conversion is correct (within 20% of cross-check)
    Tool: Bash
    Preconditions: `make e2e-up` running
    Steps:
      1. CID=$(docker compose -f tests/e2e/docker-compose.yml ps -q gateway)
      2. CROSS=$(docker exec $CID cat /proc/1/status | grep VmRSS | awk '{print $2 * 1024}')
      3. cd tests/e2e/client && python -c "
         from conftest import gateway_rss_bytes
         v = gateway_rss_bytes()
         cross = int('$CROSS')
         ratio = abs(v - cross) / cross if cross > 0 else 999
         assert ratio < 0.20, f'Drift {ratio:.2%}: got={v}, cross={cross}'
         print(f'PASS: ratio={ratio:.2%}')
         "
    Expected Result: Within 20% of direct /proc read
    Failure Indicators: ratio >= 20%, meaning conversion bug
    Evidence: .omo/evidence/task-3-rss-crosscheck.txt

  Scenario: Invariant returns passed=None when RSS unavailable
    Tool: Bash
    Preconditions: invariants.py modified
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         from soak.invariants import resource_rss_growth_under_threshold
         out = resource_rss_growth_under_threshold(before_bytes=None, after_bytes=None)
         assert out['passed'] is None, f'Expected None, got {out[\"passed\"]}'
         assert 'reason' in out['raw'], f'Missing reason in raw: {out[\"raw\"]}'
         print(f'PASS: passed=None, reason={out[\"raw\"][\"reason\"]}')
         "
    Expected Result: passed=None with reason string
    Failure Indicators: passed is True/False instead of None, missing reason key
    Evidence: .omo/evidence/task-3-rss-none-handling.txt

  Scenario: Invariant still works normally with valid ints
    Tool: Bash
    Preconditions: invariants.py modified
    Steps:
      1. cd tests/e2e/client
      2. python -c "
         from soak.invariants import resource_rss_growth_under_threshold
         ok = resource_rss_growth_under_threshold(before_bytes=100*1024*1024, after_bytes=150*1024*1024)
         assert ok['passed'] is True, f'Expected True for 50MiB growth, got {ok}'
         bad = resource_rss_growth_under_threshold(before_bytes=100*1024*1024, after_bytes=300*1024*1024)
         assert bad['passed'] is False, f'Expected False for 200MiB growth, got {bad}'
         print('PASS: normal int path works')
         "
    Expected Result: True for <100MiB growth, False for >100MiB growth
    Failure Indicators: Wrong boolean for valid inputs
    Evidence: .omo/evidence/task-3-rss-normal-path.txt
  ```

  **Commit**: YES (commit 3, combined with Task 4)
  - Message: `fix(e2e): use /proc/1/status VmRSS for RSS measurement, propagate None`
  - Files: `tests/e2e/client/conftest.py`, `tests/e2e/client/soak/invariants.py`
  - Pre-commit: `make e2e-soak-smoke`

- [ ] 4. Orchestrator integration: wire chaos_mode + remove `or 0` in test_chaos_soak.py

  **What to do**:
  - In `tests/e2e/client/scenarios/test_chaos_soak.py`:
    1. Find `rss_before = gateway_rss_bytes() or 0` (~line 161) → change to `rss_before = gateway_rss_bytes()` (remove `or 0`)
    2. Find `rss_after = gateway_rss_bytes() or 0` (~line 162) → same treatment
    3. Find the call to `soft_records_to_2xx_ratio(driver_report, records_delta=...)` (~line 181-184) → add `chaos_mode=chaos_mode` parameter
    4. Verify `chaos_mode` variable is available in scope (it's a parameter of `_run_soak()` — confirm)
    5. Pass `rss_before` and `rss_after` (now potentially None) to `resource_rss_growth_under_threshold()` — the function now accepts Optional[int] from Task 3

  **Must NOT do**:
  - Change `SoakConfig` defaults
  - Add new parameters to `_run_soak()` beyond what's needed
  - Modify driver.py

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Small surgical edits in one file, 3-4 line changes
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 2 (sequential after Wave 1)
  - **Blocks**: Tasks 5, 6
  - **Blocked By**: Tasks 1, 2, 3

  **References**:

  **Pattern References**:
  - `tests/e2e/client/scenarios/test_chaos_soak.py:161-162` — `rss_before = gateway_rss_bytes() or 0` lines to modify
  - `tests/e2e/client/scenarios/test_chaos_soak.py:181-184` — `soft_records_to_2xx_ratio()` call site to add `chaos_mode`
  - `tests/e2e/client/scenarios/test_chaos_soak.py:75-80` — `_run_soak()` function signature — verify `chaos_mode` is a parameter

  **WHY Each Reference Matters**:
  - Lines 161-162 are the exact `or 0` conflation sites
  - Lines 181-184 are where `chaos_mode` needs to be threaded through
  - The function signature confirms `chaos_mode` is already available in scope

  **Acceptance Criteria**:
  - [ ] `grep "or 0" tests/e2e/client/scenarios/test_chaos_soak.py` returns empty (no more None→0 conflation)
  - [ ] `grep "chaos_mode" tests/e2e/client/scenarios/test_chaos_soak.py` matches in the `soft_records_to_2xx_ratio()` call

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: No "or 0" pattern remains in test_chaos_soak.py
    Tool: Bash
    Preconditions: test_chaos_soak.py modified
    Steps:
      1. grep -n "or 0" tests/e2e/client/scenarios/test_chaos_soak.py
      2. Assert exit code 1 (no matches)
    Expected Result: grep returns no matches
    Failure Indicators: Any line containing "or 0" pattern
    Evidence: .omo/evidence/task-4-no-or-zero.txt

  Scenario: chaos_mode passed to soft_records_to_2xx_ratio
    Tool: Bash
    Preconditions: test_chaos_soak.py modified
    Steps:
      1. grep -n "soft_records_to_2xx_ratio" tests/e2e/client/scenarios/test_chaos_soak.py
      2. Assert output contains "chaos_mode"
    Expected Result: Call site includes chaos_mode= parameter
    Failure Indicators: Call site missing chaos_mode parameter
    Evidence: .omo/evidence/task-4-chaos-mode-wired.txt

  Scenario: Smoke run passes with all changes integrated
    Tool: Bash
    Preconditions: All Wave 1+2 changes applied, `make e2e-up` running
    Steps:
      1. make e2e-soak-smoke
      2. Assert exit 0
      3. Parse report JSON: all hard invariants passed=true
    Expected Result: exit 0, all hard invariants pass
    Failure Indicators: Non-zero exit, any hard invariant failed
    Evidence: .omo/evidence/task-4-smoke-integrated.txt
  ```

  **Commit**: YES (commit 3, combined with Task 3)
  - Message: (same commit as Task 3) `fix(e2e): use /proc/1/status VmRSS for RSS measurement, propagate None`
  - Files: `tests/e2e/client/scenarios/test_chaos_soak.py`
  - Pre-commit: `make e2e-soak-smoke` 

- [ ] 5. Smoke verification run (post-integration gate)

  **What to do**:
  - Run `make e2e-soak-smoke` (100 rps × 10s) and verify exit 0
  - Parse the report JSON and assert:
    - All hard invariants: `passed: true`
    - `account_consistency.passed: true` (C5 still holds with 8 keys)
    - RSS values are non-zero positive (not 0/0)
  - If smoke fails, diagnose and report — do NOT proceed to docs

  **Must NOT do**:
  - Skip this step — it gates the docs commit
  - Modify any source files — this is verification only

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Run command, parse output, verify conditions
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (parallel with Task 6)
  - **Parallel Group**: Wave 3 (with Task 6)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 4

  **References**:

  **Pattern References**:
  - `tests/e2e/client/.soak_report/` — Report output directory, JSON format
  - `Makefile` — `e2e-soak-smoke` target definition

  **WHY Each Reference Matters**:
  - Report JSON structure determines what jq queries to run for assertions

  **Acceptance Criteria**:
  - [ ] `make e2e-soak-smoke` exits 0
  - [ ] Report JSON shows all hard invariants `passed: true`
  - [ ] RSS values in report are non-zero

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Smoke run succeeds with all fixes
    Tool: Bash
    Preconditions: All Tasks 1-4 changes committed
    Steps:
      1. make e2e-soak-smoke
      2. REPORT=$(ls -t tests/e2e/client/.soak_report/*smoke*.json | head -1)
      3. jq '.invariants | to_entries | map(select(.value.passed != true)) | length' $REPORT
      4. Assert result is 0 (all invariants pass)
    Expected Result: exit 0, 0 failed invariants
    Failure Indicators: Non-zero exit, any invariant failed
    Evidence: .omo/evidence/task-5-smoke-pass.json (copy of report)

  Scenario: RSS values are non-zero in report
    Tool: Bash
    Preconditions: Smoke run completed
    Steps:
      1. REPORT=$(ls -t tests/e2e/client/.soak_report/*smoke*.json | head -1)
      2. jq '.resource_rss_growth.raw.before_bytes // 0, .resource_rss_growth.raw.after_bytes // 0' $REPORT
      3. Assert both values > 0 OR passed is null with reason
    Expected Result: Positive RSS values, or explicit None with reason
    Failure Indicators: 0/0 with passed=true (the original bug)
    Evidence: .omo/evidence/task-5-rss-values.txt
  ```

  **Commit**: NO (verification only)

- [ ] 6. Docs sync: resolution markers + spec updates

  **What to do**:
  - In `docs/issues/2026-05-25-soak-soft-warnings.md`:
    1. Add `RESOLVED` marker to each of the 4 warning sections
    2. Add brief resolution description and commit reference for each
  - In `docs/superpowers/specs/2026-05-22-chaos-soak-design.md`:
    1. §4 (seed design): document that soak seed now provisions 8 keys per group with `round_robin`, C5 still holds
    2. §11.2 (thresholds): document heavy `records_to_2xx_ratio_max` raised to 1.30 with rationale

  **Must NOT do**:
  - Modify spec sections other than §4 and §11.2
  - Add new spec sections
  - Change any non-docs files

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: Documentation updates, prose-heavy
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (parallel with Task 5)
  - **Parallel Group**: Wave 3 (with Task 5)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 4

  **References**:

  **Pattern References**:
  - `docs/issues/2026-05-25-soak-soft-warnings.md` — Current issue file with 4 warnings. Add resolution markers to each.
  - `docs/superpowers/specs/2026-05-22-chaos-soak-design.md` — Soak spec. Only modify §4 and §11.2.

  **WHY Each Reference Matters**:
  - Issue file is the direct target for resolution markers
  - Spec is the authoritative design doc — CLAUDE.md sync rule requires spec updates when behavior changes

  **Acceptance Criteria**:
  - [ ] `grep -c "RESOLVED" docs/issues/2026-05-25-soak-soft-warnings.md` returns exactly 4
  - [ ] `grep "round_robin" docs/superpowers/specs/2026-05-22-chaos-soak-design.md` matches in §4
  - [ ] `grep "1.30" docs/superpowers/specs/2026-05-22-chaos-soak-design.md` matches in §11.2

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: All 4 warnings marked RESOLVED
    Tool: Bash
    Preconditions: docs updated
    Steps:
      1. grep -c "RESOLVED" docs/issues/2026-05-25-soak-soft-warnings.md
      2. Assert result is 4
    Expected Result: Exactly 4 RESOLVED markers
    Failure Indicators: Count != 4
    Evidence: .omo/evidence/task-6-resolved-count.txt

  Scenario: Spec §4 documents multi-key seed
    Tool: Bash
    Preconditions: spec updated
    Steps:
      1. grep -A5 "round_robin" docs/superpowers/specs/2026-05-22-chaos-soak-design.md
      2. Assert output mentions 8 keys per group and C5 compliance
    Expected Result: §4 describes 8-key round_robin seed with C5 note
    Failure Indicators: Missing multi-key documentation, missing C5 reference
    Evidence: .omo/evidence/task-6-spec-seed.txt
  ```

  **Commit**: YES (commit 4)
  - Message: `docs: resolve soak soft warnings, update spec thresholds`
  - Files: `docs/issues/2026-05-25-soak-soft-warnings.md`, `docs/superpowers/specs/2026-05-22-chaos-soak-design.md`
  - Pre-commit: `grep -c "RESOLVED" docs/issues/2026-05-25-soak-soft-warnings.md` returns 4

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, grep for pattern). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

  **Acceptance Criteria**:
  - [ ] All "Must Have" items verified present (8/8)
  - [ ] All "Must NOT Have" items verified absent (9/9)
  - [ ] All evidence files from Tasks 1-6 exist in `.omo/evidence/`
  - [ ] VERDICT is APPROVE

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `python -m py_compile` on all changed .py files. Review for: bare except clauses, unused imports, hardcoded magic numbers without comments, inconsistent parameter naming. Check AI slop: excessive comments, over-abstraction, generic variable names.
  Output: `Compile [PASS/FAIL] | Files [N clean/N issues] | VERDICT`

  **Acceptance Criteria**:
  - [ ] `python -m py_compile tests/e2e/client/soak/seed.py` exits 0
  - [ ] `python -m py_compile tests/e2e/client/soak/invariants.py` exits 0
  - [ ] `python -m py_compile tests/e2e/client/conftest.py` exits 0
  - [ ] `python -m py_compile tests/e2e/client/scenarios/test_chaos_soak.py` exits 0
  - [ ] Zero bare except clauses in changed files
  - [ ] VERDICT is APPROVE

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state (`make e2e-down && make e2e-up`). Run `make e2e-soak-smoke`. Verify all soft warnings in report JSON are `status: "ok"`. Run `make e2e-soak` (full). Capture complete report. Verify 4 previously-failing soft warnings now pass. Save to `.omo/evidence/final-qa/`.
  Output: `Smoke [PASS/FAIL] | Full Soak [PASS/FAIL] | Soft Warnings [N/4 resolved] | VERDICT`

  **Acceptance Criteria**:
  - [ ] `make e2e-soak-smoke` exits 0
  - [ ] `make e2e-soak` (full 833 rps × 60s × off+heavy) exits 0
  - [ ] All 4 previously-failing soft warnings now pass in report JSON
  - [ ] All hard invariants still pass
  - [ ] Complete report saved to `.omo/evidence/final-qa/`
  - [ ] VERDICT is APPROVE

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git diff). Verify 1:1 — everything in spec was built, nothing beyond spec was built. Check "Must NOT do" compliance: zero changes under `crates/`, zero docker-compose changes, no SoakConfig changes. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Scope [CLEAN/N violations] | VERDICT`

  **Acceptance Criteria**:
  - [ ] `git diff --name-only` shows only files listed in plan deliverables
  - [ ] Zero files under `crates/` modified
  - [ ] Zero changes to `tests/e2e/docker-compose.yml`
  - [ ] Zero changes to `driver.py` SoakConfig defaults
  - [ ] VERDICT is APPROVE

---

## Commit Strategy

| # | Scope | Message | Files | Pre-commit check |
|---|-------|---------|-------|-----------------|
| 1 | seed | `fix(e2e): provision 8 vendor keys per model group with round_robin` | `tests/e2e/client/soak/seed.py` | `make e2e-soak-smoke` |
| 2 | threshold | `fix(e2e): parameterize records_to_2xx_ratio threshold from _SOFT_THRESHOLDS` | `tests/e2e/client/soak/invariants.py` | `make e2e-soak-smoke` |
| 3 | RSS+orchestrator | `fix(e2e): use /proc/1/status VmRSS for RSS measurement, propagate None` | `tests/e2e/client/conftest.py`, `tests/e2e/client/scenarios/test_chaos_soak.py`, `tests/e2e/client/soak/invariants.py` | `make e2e-soak-smoke` |
| 4 | docs | `docs: resolve soak soft warnings, update spec thresholds` | `docs/issues/2026-05-25-soak-soft-warnings.md`, `docs/superpowers/specs/2026-05-22-chaos-soak-design.md` | `grep -c RESOLVED docs/issues/2026-05-25-soak-soft-warnings.md` returns 4 |

---

## Success Criteria

### Verification Commands
```bash
make e2e-soak-smoke   # Expected: exit 0, all hard invariants passed
make e2e-soak         # Expected: exit 0, zero soft warnings for the 4 fixed issues
scripts/check-invariants.sh  # Expected: all hold
```

### Final Checklist
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] `make e2e-soak-smoke` passes
- [ ] `make e2e-soak` passes with resolved soft warnings
- [ ] C5 compliance verified (8 keys × retry=false)
- [ ] Docs resolution markers present
