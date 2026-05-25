# E2E Coverage Known Gaps

Recorded 2026-05-25 after the `feat/e2e-coverage-gaps` merge (`22dca03`).

These are limitations in the current Rust gateway that prevent full e2e test coverage. Each has a corresponding `xfail`, `skip`, or degraded-assertion test that documents the expected behavior once the gap is resolved.

---

## 1. ReDoS budget guard — `model_whitelist_regex` not on ApiKeyEntry

**Status**: xfail (2 tests in `test_permissions.py`)

**What**: The Rust `regex` crate uses RE2-style linear matching (no catastrophic backtracking), and `model_whitelist_regex` is not wired as a field on `ApiKeyEntry`. The gateway's ReDoS budget guard (`BUDGET_MS=100`, `TIMEOUT_THRESHOLD=10`, `RedosTracker` auto-disable) cannot be exercised end-to-end because there is no per-key regex evaluation path exposed to the e2e harness.

**Tests**:
- `test_redos_pathological_regex_killed_within_budget` — documents expected budget enforcement
- `test_redos_auto_disable_after_threshold` — documents expected auto-disable after 10 timeouts

**Resolution**: When `model_whitelist_regex` (or equivalent per-key regex field) lands on `ApiKeyEntry` and is evaluated in the request path, remove the `xfail` markers.

---

## 2. LatencyFirst strategy — black-box p50 preference unprovable

**Status**: xfail (1 test in `test_routing_strategies.py`)

**What**: `latency_first` picks the key with the lowest p50 latency from the sliding window. In a black-box e2e test, we can configure two fake-upstream routes with different delays (e.g., 500ms vs 10ms via `auth_check` per-key routing), but the gateway's internal p50 sliding window is not observable externally. After warmup, the fast key should dominate, but timing jitter in Docker makes the assertion unreliable (≥70% threshold fails intermittently).

**Test**: `test_latency_first_picks_lowest_p50` — documents expected distribution bias

**Resolution**: Either expose a `/debug/strategy-state` endpoint for test inspection, or accept this as an inherent black-box limitation. The unit tests in `crates/gw-routing/tests/strategies.rs` cover the algorithm deterministically.

---

## 3. CacheAffinity strategy — `message_hash` not wired in proxy

**Status**: degraded assertion (1 test in `test_routing_strategies.py`)

**What**: `CacheAffinity` in `crates/gw-routing/src/strategy/cache_affinity.rs` hashes `model_name || ":" || message_hash` to pin requests to a key. However, `crates/gw-proxy/src/filters/upstream.rs:228` always sets `message_hash: None`, causing the strategy to degrade to first-available (all requests pin to the same key regardless of content).

**Test**: `test_cache_affinity_pins_same_content` — asserts the degraded single-key behavior and documents the expected varied-content distribution once `message_hash` is populated.

**Resolution**: Wire `message_hash` computation (e.g., SHA-256 of request body or conversation content) in `upstream.rs` before calling the routing strategy. Then update the test to assert ≥2 distinct keys for different content.

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
