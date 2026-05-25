# Learnings

## 2026-05-25 Plan Start
- All entities use JSONB payload column — no DDL migration needed
- MergedPermissions is NOT serde-derived — constructed programmatically during merge
- RedosTracker + check_with_budget() fully implemented in gw-auth but never wired
- LatencyTracker in Wired but not in HealthState
- PickCtx.message_hash exists but hardcoded None at upstream.rs:228
- Hash computed before forced_params mutation (line 224 vs 519) — correct by construction
- Admin endpoints use manual JSON building (no serde_json in health server)
- no_v1_markers_in_prod lint is error-mode — no TODO/unimplemented allowed

## 2026-05-25 Doc Sync (T7)
- workflow.md §2.2 step 11: expanded model permission with full ReDoS budget flow (compile, check_with_budget, auto-disable)
- architecture.md §8: added ReDoS auto-disable fail-closed as deliberate behaviour
- architecture.md §6.1 + workflow.md §6: added /healthz/strategies to listener table + observability surface
- architecture.md §6.1: expanded admin route description with /healthz/strategies return shape
- workflow.md §2.3 step 4: documented PickCtx.message_hash populated from SHA-256 of cached_body (body_hash_u64)
- issues/e2e-coverage-known-gaps.md: gaps #1 (ReDoS), #2 (LatencyFirst), #3 (CacheAffinity) marked Resolved
- No new files created, no code files modified, no over-expansion

## 2026-05-25 T7: xfail removal + assertion updates
- Removed xfail from `test_redos_pathological_regex_killed_within_budget` (test_permissions.py) — ReDoS budget guard now wired via check_with_budget(); pathological regex → 403 GW_REQUEST_MODEL_NOT_FOUND within 100ms budget
- Removed xfail from `test_redos_auto_disable_after_threshold` (test_permissions.py) — auto-disable after 10 timeouts → fail-closed 403 even for non-pathological model names
- Removed xfail from `test_latency_first_picks_lowest_p50` (test_routing_strategies.py) — /healthz/strategies endpoint now populates P50 TTFT data; warmup→measure pattern works reliably
- Updated `test_cache_affinity_pins_same_content` (test_routing_strategies.py) — Phase 1 assertion unchanged (same body→same key); Phase 2 assertion changed from "all pin to 1 key (degraded)" to "spread across ≥2 keys" since message_hash is now populated from cached_body SHA-256
- All 3 xfail markers corresponded to coverage gaps T4/T5/T6 which are now fully implemented
- Test assertions verified against: 403 status code for ReDoS deny, GW_REQUEST_MODEL_NOT_FOUND error code, gw_redos_timeout_total metric, ≥70% fast-key preference for latency_first
