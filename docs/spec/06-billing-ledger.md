# 06 — Billing & Ledger (as-built)
- Family: C (billing) · Backing crates: gw-ledger, gw-billing · Source-of-truth: `crates/gw-ledger/src/{usage.rs,flusher.rs,freeze.rs,deduct_gc.rs,inflight.rs,inflight_reaper.rs,inflight_reconciler.rs,lease.rs,coef.rs,credit_estimator.rs,counter_handler.rs,protection_config.rs,detailed_archive.rs}`, `crates/gw-billing/src/{formulas.rs,lib.rs}`, `crates/gw-proxy/src/billing_lifecycle.rs`, `crates/gw-proxy/src/filters/request.rs` · Status: Rust emits usage to a Redis stream and reserves freezes/leases in-process; it performs no per-token cost math (Go owns cost).

## Purpose

Rust accounts request resource consumption and protects against overdraft, but never computes monetary cost. It produces a `UsageRecord` per attempt, ships it to the `usage:records` Redis stream, and leaves cost computation and balance decrement to the Go billing service that consumes that stream. Two overdraft defenses exist: a legacy in-process soft `frozen_micro` reservation (`RequestFreeze`), and an optional hard per-Pod credit-lease layer for users Go has pushed into protected mode. A separate per-surface wire-fidelity archive (`usage:detailed`) is emitted for forensic/replay purposes.

## As-built surface

### UsageRecord schema — `crates/gw-ledger/src/usage.rs`
`UsageRecord` carries identity/trace, subjects, protocol path, result, the 6-dim `UsageSnapshot` (`usage`), and billing-reconciliation fields. There is **no `cost_micro` field** and **no `usage_source` field** — both are absence-asserted by the `usage_record_json_shape` test (`crates/gw-ledger/src/usage.rs`). Notable fields:
- `vendor_exposure_state: VendorExposureState` (enum `Settled` / `Terminated` / `CrashFallback`, serde `snake_case`) — tells Go whether to bill, reconcile, or treat the record as crash recovery.
- `usage_provenance: UsageProvenance` — per-dimension `ProvenanceTag` (`input`/`output`/`cache_read`/`cache_creation`/`thinking`/`multimodal_input`) frozen at emission. Tags serialize as `upstream_final`, `cli_extracted`, `upstream_partial`, `estimated`, `crash_fallback`, and `unknown`.
- `attempt_index: u32` — 0-based; matches the inflight Redis key for that attempt. Records are **per-attempt** (a failed attempt emits its own record before retry).
- `freeze_amount_micro: u64` and `estimate_pricing_version: u64` (the latter is the loaded `entity:estimate_pricing` version, or `0` when the legacy fallback estimate is used).
- `markup_chain_snapshot: MarkupChainSnapshot` — Go re-derives cost from this.
- `pod_id: String`, and the credit-lease pair `credits_consumed: Option<i64>` / `coef_table_version: Option<u64>` (both `skip_serializing_if = "Option::is_none"`).
- `operation_id: Option<String>` for long-running operations (skip-if-none).

### Per-attempt record construction — `crates/gw-proxy/src/billing_lifecycle.rs`
`build_attempt_record(ctx)` builds a `UsageRecord` from `ReqCtx`, mapping `ctx.vendor_exposure` (`VendorExposure::Settled` → `Settled`, all else → `Terminated`). `on_retry_sync` forces `Terminated`, snapshots the record, then resets per-attempt ctx fields and bumps `attempt_index`. `on_retry` / `settle_prior_attempt_spawn` perform the async half: `InflightLedger::mark_settled(Terminated)` then `Ledger::emit` carrying a `ClearKey` for the prior attempt. The lifecycle state machine (`VendorExposure` enum: `Idle`/`Sent`/`Streaming`/`Settled`/`Terminated`) is validated by `can_transition`; `transition_to` records `sent_ms`/`settled_ms` timings and emits `gw_billing_exposure_transitions_total{from,to}`.

- **CLI-extracted usage outranks partial and estimated usage.** CLI body/stream extractors parse usage emitted by upstream CLI wire traffic, so their dimensions are tagged `cli_extracted`. Merge and Go dedupe priority is `upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown`.

### Flusher — `crates/gw-ledger/src/flusher.rs`
`Ledger::start` / `Ledger::start_with_spill_root` spawn one background task that batches `LedgerItem`s from an mpsc channel and writes them to `usage:records` (`USAGE_STREAM`) via a pipelined `XADD`. Balance decrement is **not** performed here. `emit` is the must-deliver path: on a full channel it falls back to a synchronous direct `XADD` (`sync_xadd`); on `Closed` it likewise bypasses. Primary stream failure routes through `DlqEnvelope { dlq_meta, usage_record }` (`crates/gw-ledger/src/dlq.rs`): first to `usage:records:dlq` (`DLQ_STREAM`, `DLQ_MAXLEN = 100_000`), then to an in-memory retry queue drained every 30 s, then to a sharded `SpillFile` under `${runtime.spill_io.root_dir}/dlq.emergency.<uuid>.<shard>.jsonl` (shard ∈ `0..runtime.spill_io.dlq_shards-1`, async append via `gw_runtime::spill_io`, created with `SpillPolicy::KeepOnDrop` so the shards survive process exit and are picked up by `BacklogRecovery::load_into` on the next boot via its `starts_with("dlq.")` filter); if the emergency file write also fails, Rust attempts `usage:records:dlq:permanent_failed` before incrementing `gateway_usage_records_lost_total`. The DEL rule: an inflight key is deleted (`inflight_key`) only after the matching record's `XADD` is confirmed; DLQ / serialize-fail / queue-fail paths leave the key for the reconciler.

### DLQ cascade — `crates/gw-ledger/src/{flusher.rs,dlq.rs,backlog.rs}`

The full failure cascade for a `UsageRecord` is:

```
XADD usage:records
  │ failure
  ▼
XADD usage:records:dlq (DLQ stream, MAXLEN 100_000)
  │ failure
  ▼
SpillFile async write (sharded, under runtime.spill_io.root_dir)
  │ failure
  ▼
BillingBacklog::push (unbounded VecDeque, in-process)
  │ flush-on-drain or periodic-backlog-drainer
  ▼
XADD usage:records (retry from backlog)
  │ failure
  ▼
XADD usage:records:dlq:permanent_failed
  │ failure
  ▼
gateway_usage_records_lost_total++
```

**Must-deliver boundary**: drop is acceptable only if `BackpressureGate` already rejected the request *before* a `UsageRecord` existed (i.e. the 503 at `early_request_filter` step 0). Once a `UsageRecord` is constructed (in `logging` or `on_retry`), the cascade guarantees delivery through at least `BillingBacklog` — which is unbounded by construction. The metric `gw_billing_backlog_pending_count` rising signals pressure; it never signals data loss.

### BillingBacklog — `crates/gw-ledger/src/backlog.rs`

`BillingBacklog` is an unbounded `VecDeque<DlqEnvelope>` guarded by a `Mutex`. It is the final in-memory safety net: any record that fails all Redis and disk paths lands here. A periodic backlog drainer task (`BacklogDrainer`) re-attempts XADD to `usage:records` for each entry, removing it on success. On process shutdown (phase 2 of `ShutdownCoordinator`), the backlog is serialized to `backlog_shutdown_<unix_ts>.jsonl` in `spill_io.root_dir`.

### BacklogRecovery — `crates/gw-ledger/src/backlog.rs`

On bootstrap, `BacklogRecovery::recover` scans `spill_io.root_dir` for `backlog_shutdown_*.jsonl` files, deserializes each `DlqEnvelope` line-by-line, and re-enqueues them into the `BillingBacklog` for eventual `usage:records` XADD. Corrupt files (malformed JSON, truncated lines) are moved to `<spill_root>/corrupt/` and counted by `gw_billing_backlog_corrupt_total`. This ensures that billing records survive a full process restart.

### Freeze (soft defense) — `crates/gw-ledger/src/freeze.rs` + `crates/gw-ledger/src/deduct_gc.rs`
`RequestFreeze::new` reserves `amount` micro-units against the per-user in-process `DeductionState.frozen_micro` (`AtomicI64` in the global `DEDUCT` DashMap) and enqueues `FrozenDelta { uid, +amount }` to the ledger frozen-delta worker. Release is idempotent via a CAS on `released`; it decrements local `frozen_micro` and enqueues `-amount`. `Drop` rolls back any un-released freeze, enqueues the matching negative delta, and increments `gateway_freeze_leak_recovered_total`. The worker asynchronously `INCRBY counter:user:<uid>:frozen_micro <delta>` as a scalar Redis counter; enqueue/Redis outcomes are counted by `gw_frozen_delta_enqueue_total` and `gw_frozen_delta_redis_total`. This module never writes `deducted_micro` (Go is the exclusive writer). `TaskFreeze` is the task-path analogue, delegating release to an injected `TaskFreezeRelease`.

**DeductGc lifecycle.** The periodic DEDUCT sweep is supervised by `gw_runtime::Supervisor` (catch_unwind + exponential-backoff restart) and registered with the `ShutdownCoordinator`. A panic inside the sweep no longer silently disables eviction. The same per-tick body also calls `gw_store::deduction::sweep_user_circuit()` to evict `UserCircuit::Open { until }` entries past `until + USER_CIRCUIT_STALE_GRACE` (5 min default). `UserCircuit::Closed` entries are NOT evicted — currently the workspace has no `USER_CIRCUIT.insert(_, Closed)` writers, and `Closed` carries no creation timestamp.

### Freeze estimate (cost boundary) — `crates/gw-proxy/src/filters/request.rs`
`freeze_estimate_with_pricing(est_input_tokens, request_max_tokens, markup, pricing)` uses `entity:estimate_pricing:<model>` when available. `EstimatePricing` (`crates/gw-core/src/entity.rs`) carries `conservative_per_token_micro`, `safety_factor`, `default_max_tokens`, and `hard_cap_tokens`; `compute_freeze_estimate` (`crates/gw-ledger/src/estimate_pricing.rs`) computes `(input + output_cap) × conservative_per_token_micro × safety_factor × markup`, with `output_cap` bounded by both request/default output and the total `hard_cap_tokens`. `ReqCtx.estimate_pricing_version` is propagated into the emitted `UsageRecord`. If the entity is absent or cold-load misses, Rust falls back to the legacy flat estimate: `est_input_tokens × max(markup, 1.0)`, floored at 1. Step 9 checks the estimate against hard caps (`check_cost`); step 10 constructs the `RequestFreeze` with `freeze_estimate_micro` and parks it on `ctx.freeze_guard`. This is a conservative freeze estimate, not Go's per-token cost formula.

### Billing robustness (inflight lifecycle) — `crates/gw-ledger/src/{inflight.rs,inflight_reaper.rs,inflight_reconciler.rs}`
`InflightLedger` keeps a per-request/attempt Redis HASH at `billing:inflight:<instance>:<request_id>:<attempt>` (`test_key`), with `register` (state `sent`), `mark_settled` (`Settled`/`Terminated`), `clear` (DEL), and a 24 h `EXPIRE` safety net. `inflight_reaper::spawn` runs the T0 emit-lag watchdog: it `scan`s own-instance keys, and for any key whose `ts_settled_ms` lag exceeds `emit_lag_t0_secs` it fires `gw_billing_emit_lag_t0_total{request_id,attempt_index}` — it does **not** delete the key and does **not** emit a fallback record. `inflight_reconciler::run` is the startup crash-recovery path and the **only** place that emits fallback records: it builds a `CrashFallback` `UsageRecord` (`input = prompt_estimate_tokens`, `output = 0`, all provenance `CrashFallback`, status `Error`, `error_code = "crash_fallback"`) via `build_fallback_record`, emits it, then DELs the key and bumps `gw_billing_inflight_recovered_total{exposure_state="crash_fallback"}`.

### Boot wiring — `crates/gw-gateway/src/lib.rs`
Bootstrap resolves a stable `instance_id` (`instance_id::resolve`) before `Ledger::start`, then starts `DetailedArchive::start`, `DeductGc::spawn(shutdown_token)` (Supervisor-wrapped, registered with `ShutdownCoordinator`), `instance_id::spawn_heartbeat_task`, `inflight_reaper::spawn` (using `cfg.billing.reaper_interval_secs` / `cfg.billing.emit_lag_t0_secs`), constructs `InflightLedger`, runs `inflight_reconciler::run` before traffic starts, and calls `gw_ledger::install_counter_handler()` (`crates/gw-gateway/src/lib.rs`). Resilience-fixes additions: `SpillRoot::open(cfg.runtime.spill_io)` for DLQ and archive overflow, `RegexCache::new(cfg.runtime.regex_cache)` for shared model-permission regex, `ShutdownCoordinator::new(cfg.runtime.shutdown)` for SIGTERM-driven drain, `SystemHealthMonitor::new(cfg.runtime.health)` with 4 force-majeure hooks (memory, FD, disk, Redis-OOM), `BackpressureGate::new(monitor)` wired into `early_request_filter`, and `BacklogRecovery::recover(spill_root)` to re-enqueue any records surviving a previous process crash/shutdown.

### Credit-lease (hard overdraft protection) — `crates/gw-ledger/src/{lease.rs,coef.rs,credit_estimator.rs,counter_handler.rs,protection_config.rs}`
- `lease.rs`: `LEASE_POOL` (global `DashMap<UserId, Arc<LeaseState>>`). `LeaseGuard::try_acquire(uid, requested, now_ms_val, grace_ms)` returns `LeaseError::NoLease` (no entry), `LeaseError::Expired` (`now_ms_val > expires_at.saturating_add(grace_ms)`), or `LeaseError::Insufficient` (pool `< requested`); on success CAS-decrements `credits`. `settle_actual(actual)` refunds `reserved - actual` only when `actual ≤ reserved` (no double-charge on over-spend; over-spend is reconciled by Go via `credits_consumed`). `now_ms()` returns `0` on a pre-1970 clock (fail-closed: a 0 timestamp makes any lease read as expired). `upsert_protected` overwrites the user's protected lease state; there is no Rust-side unprotected mode.
- `coef.rs`: RCU-style versioned `ModelCoefs` cache (`COEF_CACHE: ArcSwap<CoefSnapshot>`, `COEF_STAGING`). Coef entries stage via `apply_coef_entry`; `apply_coef_commit(version, model_count)` activates only when `staged ≥ model_count` and `version > active` (`CommitOutcome::{Activated, Incomplete, Stale}`). All values are unitless `credits / token` — never currency.
- `credit_estimator.rs`: `estimate_request_credits` (acquire-time, 2 dims: input + max-output) and `actual_usage_credits` (settle-time, 6 dims). Both look up coefs via the lease's `coef_version`; `version == 0` → `EstimatorError::UnknownCoefVersion`, missing model → `EstimatorError::UnknownModel`.
- `counter_handler.rs`: implements `gw_store::counter_dispatch::CounterHandler` for the `events:counter` namespaces `protected_lease`, `coef_table`, `coef_table_commit`; `install_counter_handler()` registers it at boot. Coef-commit outcomes drive `gw_credit_coef_table_incomplete_total`.
- `protection_config.rs`: `[protection]` table — `enabled` (default `true`, kill switch), `lease_grace_ms` (default 30000), `default_output_ceiling_tokens` (default 8192).
- Admission (`crates/gw-proxy/src/filters/request.rs` step 8.5): only when `protection_cfg.enabled` and the user's `LEASE_POOL` mode is `Protected`. Resolves `max_out` (request `max_tokens` else `default_output_ceiling_tokens`, bumping `gw_credit_default_ceiling_used_total`), estimates credits at the lease's `coef_table_version`, and acquires with `protection_cfg.lease_grace_ms`. **Fail-closed**: missing coefs (`estimate_request_credits` error) hard-reject with HTTP 503 `GW_QUOTA_COEF_TABLE_NOT_READY`. `LeaseError::Insufficient` → HTTP 402 `GW_QUOTA_BALANCE_INSUFFICIENT`; `LeaseError::Expired` → HTTP 503 `GW_QUOTA_BALANCE_CACHE_UNAVAILABLE`; `LeaseError::NoLease` (warmup/race) falls through to the legacy step 9–10 soft-defense path and bumps `gw_credit_lease_warmup_pending_total`. On success the `LeaseGuard` parks on `ctx.lease_guard` and steps 9–10 are bypassed. Codes are defined in `crates/gw-core/src/error/codes.rs`.

**Ledger sweep cadence.** A `ledger_sweep` background task (Supervisor-wrapped, 60s tick, ShutdownCoordinator-registered) periodically evicts:
- `COEF_STAGING` entries whose version is strictly less than `active_version - COEF_STAGING_SAFETY_MARGIN` (100 default). `apply_coef_commit`'s existing `Stale` and `Incomplete` branches remain unchanged; this sweep covers the orphan case (entries inserted by `apply_coef_entry` whose commit event never arrives). `LEASE_POOL` no longer has an unprotected idle state to evict.

**BillingBacklog observability.** In addition to the existing `gw_billing_backlog_pending_bytes` and `gw_billing_backlog_pending_count` gauges, `push` now emits `gw_billing_backlog_high_watermark_count` (the maximum pending count seen) and `gw_billing_backlog_over_threshold_total` (a counter that fires once per upward crossing of `BILLING_BACKLOG_OVER_THRESHOLD`, default 10_000). The crossing flag resets when `pop` drops the count below the threshold, so subsequent crossings count again.

### Detailed-log archive — `crates/gw-ledger/src/detailed_archive.rs`
`DetailedArchive` captures up to 5 per-surface `CapturedSurface`s per request (`Kind::{DownstreamRequest, UpstreamRequest, UpstreamResponse, DownstreamResponse, Summary}`), each `XADD`-ed to `usage:detailed` by a worker pool. Bodies are base64-encoded, then encoded via tiered compression `raw`/`lz4`/`zstd` (`encode` + `CompressionCfg` from `crates/gw-ledger/src/encoding.rs`). It is **drop-on-outage**: a full channel, oversize payload, compress failure, or `XADD` failure increments `gw_detailed_log_drop_total{reason,kind}` and discards the surface — there is no DLQ and no redaction.

### Archive buffer cap + spill — `crates/gw-proxy/src/ctx.rs` (ArchiveState)
Per-request archive accumulation is capped by `[proxy.archive]` configuration. `ArchiveState` has three states: `InMemory` (below `soft_cap_bytes`), `Spilling` (between soft and hard cap — excess bytes written to `spill_dir`), and `Dropped` (above `hard_cap_bytes` — further appends are discarded). `append_chunk` returns the current state; `emit_response_surfaces` (in the logging filter) reads from whichever storage the state indicates. This prevents unbounded per-request memory consumption on large upstream responses.

**Spill-file lifetime.** A spilled archive's shard files are unlinked the moment the owning `ArchiveState::Spilled` variant drops — either when the request's `ReqCtx` drops or when `reset_response_attempt_state` reassigns the accumulator on response-side failover. Unlink is synchronous (`std::fs::remove_file`) and best-effort; the `SpillRoot` LRU enforcer is the fallback. Reset on failover prevents both data contamination and disk leak across retries.

### gw-billing (task-level formula crate) — `crates/gw-billing/src/{formulas.rs,lib.rs}`
`gw-billing` legitimately exists as the **task-level formula crate** — it computes freeze/settle **multipliers** for the long-running task path, not per-token request cost. `calculate_freeze_amount` / `calculate_settle_amount` (`crates/gw-billing/src/formulas.rs`) scale a `base_micro` by `kind_multiplier` (`Image`=3, `Video`=6, `Audio`=3, `Batch`=2, `LroResponses`/`Generic`=1), a vendor multiplier, and a `duration_multiplier` (step function on `estimated_duration_secs`). It contains no `cost_per_token` / `price_per_token` — enforced by the `no_billing_cost_per_token` lint — and is allowed only as a workspace/business dependency, never a dependency of supporting crates (`no_billing_crate` lint). This resolves the historical "gw-billing forbidden" contradiction: the crate is permitted **because** it is a task-formula crate, not a request cost engine.

## Deliberate behaviours

- **No Rust cost math.** Rust never computes monetary cost. `UsageRecord` carries no `cost_micro`; the freeze uses either Go-pushed conservative `EstimatePricing` or the legacy flat fallback. Go owns actual cost from `usage:records`. Enforced by `no_cost_micro` (`scripts/check-invariants.sh`).
- **Per-attempt records.** Each attempt emits its own `UsageRecord`; a failed attempt is settled (`Terminated`) and emitted before the retry register clobbers the next `attempt_index` (`crates/gw-proxy/src/billing_lifecycle.rs`).
- **Three-tier usage DLQ → cascade.** `usage:records` failure goes to Redis DLQ, then memory retry queue, then sharded `SpillFile`, then unbounded `BillingBacklog`; only after all fallbacks including backlog drainer retry fail does `gateway_usage_records_lost_total` increment (`crates/gw-ledger/src/{flusher.rs,dlq.rs,backlog.rs}`). Drop is acceptable only if `BackpressureGate` already rejected the request before any `UsageRecord` existed.
- **BillingBacklog is unbounded.** `BillingBacklog` uses an unbounded `VecDeque`; pressure shows as `gw_billing_backlog_pending_count` rising, never as dropped envelopes. On shutdown, the backlog is serialized to `backlog_shutdown_<ts>.jsonl`; on next bootstrap, `BacklogRecovery` re-enqueues. Corrupt recovery files are quarantined to `<spill_root>/corrupt/`.
- **Detailed-log drops on outage.** Forensic archive is best-effort; it never blocks the request and never retries (`crates/gw-ledger/src/detailed_archive.rs`).
- **Freeze leak recovery.** A `RequestFreeze` dropped without `release()` rolls back its reservation and counts it (`gateway_freeze_leak_recovered_total`) rather than leaking frozen credits (`crates/gw-ledger/src/freeze.rs`).
- **Credit-lease fail-closed.** Protected mode without coefficient data hard-rejects rather than admitting; lease expiry includes `lease_grace_ms` for small clock skew; a pre-1970 clock reads every lease as expired; only `LeaseError::NoLease` (a benign race) falls through to soft defense (`crates/gw-ledger/src/lease.rs`, `crates/gw-proxy/src/filters/request.rs`).
- **Over-spend not double-charged.** `LeaseGuard::settle_actual` refunds only on under-spend; over-spend is left to Go's outstanding-ledger reconciliation via `credits_consumed` (`crates/gw-ledger/src/lease.rs`).
- **T0 watchdog is forensic.** The reaper never deletes keys or emits fallback records — only the startup reconciler emits `CrashFallback` (`crates/gw-ledger/src/inflight_reaper.rs`, `crates/gw-ledger/src/inflight_reconciler.rs`).

## Cross-process touchpoints

- **`usage:records` (Redis stream).** Rust `XADD`s one `UsageRecord` per attempt (field `payload`, JSON); Go consumes, computes cost, decrements balance. Schema: `crates/gw-ledger/src/usage.rs`.
- **`usage:records:dlq` (Redis stream).** DLQ stream wrapping `{ dlq_meta, usage_record }`, where `dlq_meta` includes `first_failure_ts_ms`, `primary_failure_reason`, `retry_count_before_dlq`, and `dlq_layer` (`redis`/`mem`/`disk`) (`crates/gw-ledger/src/{flusher.rs,dlq.rs}`).
- **`usage:records:dlq:permanent_failed` (Redis stream).** Last-chance stream for records whose Redis DLQ and emergency file fallback both fail.
- **`usage:detailed` (Redis stream).** 5 per-surface XADDs/request with fields `ts_ms`/`request_id`/`trace_id`/`kind`/`encoding`/`payload` (`crates/gw-ledger/src/detailed_archive.rs`).
- **`billing:inflight:<instance>:<request_id>:<attempt>` (Redis HASH).** Written/cleared by Rust; the W4 startup reconciler reads residuals on the same instance after a crash (`crates/gw-ledger/src/{inflight.rs,inflight_reconciler.rs}`).
- **`events:counter` (Redis, Go → Rust push).** Namespaces consumed by `counter_handler.rs`: `user_balance` (authoritative `value_after` overwrite for local `base_micro` / `deducted_micro`), `protected_lease` (mode + `lease_credits` + `lease_id` + `expires_at_ms` + `coef_table_version`), `coef_table` (per `(version, model_id, dim)` `coef_per_token`), `coef_table_commit` (`version` + `model_count`). Go guarantees `Σ outstanding ≤ balance × (1 + overcommit_ratio)`; Rust holds the per-Pod scalar pool.
- **`counter:user:<uid>:frozen_micro` (Redis STRING integer).** Rust asynchronously `INCRBY`s on request freeze/release so other Pods and Go can observe soft-reservation pressure with `GET`.
- **`entity:estimate_pricing:<model>` (Redis HASH / store entity).** Rust reads a conservative per-model freeze estimate entity through `EntityStore.estimate_pricing_or_load`.
- **`usage:records.credits_consumed` + `coef_table_version`.** Rust reports per-request credit consumption and the coef version it priced under so Go reconciles the outstanding ledger at the same version (`crates/gw-ledger/src/usage.rs`).

## Invariants & lints

Enforced by `scripts/check-invariants.sh`:
- `no_cost_micro` — no `cost_micro` field anywhere in `crates/` (test absence-assertions whitelisted).
- `no_billing_crate` — `gw-billing` must not be a dependency of the supporting crates (`gw-core`, `gw-codec`, `gw-codec-macros`, `gw-store`, `gw-auth`, `gw-ledger`).
- `no_billing_cost_per_token` — `gw-billing/src` must not contain `cost_per_token` / `price_per_token`.

In-code invariants: freeze release is idempotent + leak-recovered on `Drop`; the flusher DELs an inflight key only after XADD-confirmed delivery; the startup reconciler is the sole fallback-record emitter; acquire-time and settle-time credit math must use the same `coef_version`; coef commits activate only forward (`version > active`) and complete (`staged ≥ model_count`).

## Not built → ROADMAP

No open ROADMAP items in this domain.
