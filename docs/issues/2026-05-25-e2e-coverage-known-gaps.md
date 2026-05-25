# E2E Coverage Known Gaps

Recorded 2026-05-25 after the `feat/e2e-coverage-gaps` merge (`22dca03`).

These are limitations in the current Rust gateway that prevent full e2e test coverage. Each has a corresponding `xfail`, `skip`, or degraded-assertion test that documents the expected behavior once the gap is resolved.

---

## 1. ReDoS budget guard — `model_whitelist_regex` on ApiKeyEntry

**Status**: **Resolved** (2026-05-25)

**What**: `model_whitelist_regex` is now wired on `ApiKeyEntry` and evaluated in `request_filter` step 11 via `check_model_whitelist_regex()`. The pattern compiles to `ModelPermission` and runs through `check_with_budget()` (100 ms ReDoS budget, `RedosTracker` auto-disable after 10 timeouts, fail-closed deny on disable).

**Tests**: `xfail` markers removed. Both `test_redos_pathological_regex_killed_within_budget` and `test_redos_auto_disable_after_threshold` pass.

---

## 2. LatencyFirst strategy — black-box p50 observable via `/healthz/strategies`

**Status**: **Resolved** (2026-05-25)

**What**: `/healthz/strategies` endpoint now exposes per-(key, format) p50 TTFT from the `LatencyTracker`'s `by_key_format` DashMap. E2e tests can query this mgmt-gated endpoint to verify the gateway's internal latency state, making the `LatencyFirst` preference provable in black-box tests.

**Test**: `xfail` marker removed. `test_latency_first_picks_lowest_p50` queries `/healthz/strategies` after warmup to confirm p50 ordering.

---

## 3. CacheAffinity strategy — `message_hash` wired from cached_body SHA-256

**Status**: **Resolved** (2026-05-25)

**What**: `PickCtx.message_hash` is now populated in `upstream.rs` via `body_hash_u64()`, which computes SHA-256 of the raw `ctx.cached_body` bytes and takes the first 8 bytes as `u64` LE. Empty body produces `None`. `CacheAffinity` hashes `model_name || ":" || message_hash` to pin content-identical requests to the same key, and different content distributes across keys.

**Test**: `test_cache_affinity_pins_same_content` updated to assert same-content pinning and varied-content distribution (>= 2 distinct keys).

---

## 4. Failover metric labels — actual vs. plan naming

**Status**: resolved (not a gap — tests match reality)

**What**: The plan spec referenced `reason="retryable_5xx"` / `reason="retryable_429"` and a separate `gw_retry_attempt_total` metric. The actual gateway implementation in `crates/gw-routing/src/failover.rs:87-92` uses `reason="upstream_5xx"` / `reason="upstream_429"` on `gw_routing_failover_total`. There is no separate `gw_retry_attempt_total` metric.

**Tests**: `test_5xx_failover_recovers_to_next_key`, `test_429_cooldown_then_recovery`, `test_5xx_exhausts_retries_returns_502` — all correctly assert the actual metric labels.

**Resolution**: None needed. The plan spec was inaccurate; the tests correctly reflect the real gateway behavior.

---

## 5. `time.sleep()` in e2e tests — polling intervals

**Status**: present (25 occurrences across 9 test files)

**What**: E2e tests use `time.sleep()` for three purposes:
1. **Polling intervals** (0.05s–0.3s): loop sleep between metric/Redis checks
2. **Async propagation waits** (0.5s): wait for Redis XADD to propagate after request completes
3. **TTL expiry waits** (2.5s–3.0s): wait for sticky TTL or cooldown to expire

All are in test code only — no `time.sleep()` in production Rust code.

**Assessment**: These are inherent to black-box e2e testing where the gateway's internal state is not directly observable. The alternative (pure polling with timeout) would make tests more complex without meaningful reliability improvement for sub-second sleeps. The 2.5s–3.0s TTL waits are unavoidable without a test-only TTL override endpoint.

**Resolution**: No action needed unless test flakiness is observed. If flaky, convert to polling loops with exponential backoff. Pre-existing tests (`test_quotas.py`, `test_reliability.py`, `test_redis_l1_invalidation.py`) use the same pattern.
