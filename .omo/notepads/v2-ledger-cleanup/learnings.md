# Learnings — v2-ledger-cleanup

## 2026-05-24 Session Init
- freeze.rs:60-61 has `TODO(memory)` — DEDUCT entries never reclaimed
- freeze.rs:126-159 TaskFreeze release() is silent no-op (Phase 1b placeholder)
- logging.rs:315 `HandlerCtx::stub()` — T6-B follow-up, 6 listeners get empty ctx
- ledger.rs:12 LedgerListener `on_outcome` is no-op
- DeductionState has 3 AtomicI64: base_micro, deducted_micro, frozen_micro
- DEDUCT is `Lazy<DashMap<UserId, Arc<DeductionState>>>`
- HandlerCtx fields: request_id, api_key_id, vendor_key_id, api_format, key_health, entity_store, resilience
- HandlerCtx::stub() returns all None/empty
- LlmGateway has: store (EntityStore), health (KeyHealthStore), resilience (ResilienceState), detailed_archive (DetailedArchive)
- gw-task-runtime does not exist yet; check-invariants.sh:96-104 has trivially-true assertions
- Workspace has 13 crates currently, next crate = gw-task-runtime (SUPPORTING layer)
- Redis hash-tags for Cluster: `{user:<uid>}` prefix
- From S-routing: PickCtx now has 7 fields, ReqCtx has strategy_name/picked_candidates/failover_attempt/body_started

## 2026-05-24 T3: DEDUCT GC landed
- Created `crates/gw-ledger/src/deduct_gc.rs`: `DeductGc::sweep()` uses `DashMap::retain` + `Ordering::Relaxed`
- Metric: `gw_deduct_gc_removed_total` (counter, incremented per sweep batch)
- `DeductGc::start(interval)` spawns tokio loop; `start_default()` uses 60s
- `sweep()` is public + synchronous for testability; background loop calls it
- Tests use `Mutex<()>` serialization because `sweep()` touches global DEDUCT and parallel tests race
- Tests use key-prefix isolation (`gc1_`, `gc2_`, `gc3_`) instead of `DEDUCT.clear()` to avoid stomping concurrent freeze.rs tests
- Deleted `TODO(memory)` from freeze.rs:60-61

## 2026-05-24 T2: TaskFreeze Lua release landed
- `gw-task-runtime::RedisTaskFreezeRelease::new(pool)` loads `task_freeze_release.lua` with `SCRIPT LOAD` and release uses `EVALSHA` with `EVAL` fallback on NOSCRIPT.
- Release keys are cluster co-located via `{user:<uid>}`: `:tasks:leases`, `:entity:task:<task_id>`, `:counter:task_frozen_micro`.
- Counter key is a HASH and the script mutates field `frozen_micro` via `HINCRBY`; tests seed/read that field, not string `GET`.
- `gw-ledger::TaskFreeze::release()` remains synchronous and spawns the async releaser; `None` releaser is a test/no-op path.
