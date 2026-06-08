# 03 — Key Health (as-built)
- Family: K (key-health) · Backing crate: gw-health · Source-of-truth: `crates/gw-health/src/{format_health.rs,store.rs,cooldown.rs,sliding_window.rs,quota_probe.rs,lib.rs}`, `crates/gw-proxy/src/filters/logging.rs`, `crates/gw-proxy/src/error/listeners/key_health.rs` · Status: K-A multi-dim per-`(key, format)` circuit + cooldown state machine is in production; the cooldown-recovery probe exists but is default-disabled.

## Purpose

`gw-health` tracks the live serviceability of each vendor key, sliced per wire format. It answers one question for the router — "may this `(KeyId, ApiFormat)` be used right now?" — and absorbs two independent failure signals: an automatic sliding-window circuit breaker, and explicit cooldown directives produced by the error classifier. State is per-replica and in-memory only; there is no persistence (`crates/gw-health/src/lib.rs` module doc).

## As-built surface

**`FormatHealth` — per-`(key, fmt)` state cell** (`crates/gw-health/src/format_health.rs`). Holds a `Mutex<SlidingWindow>` plus five atomics: `circuit: AtomicU8`, `open_until_ms: AtomicU64`, `cooldown_until_ms: AtomicU64`, `cooldown_reason: AtomicU16`, and `consecutive_failures: AtomicU32`. Three concerns are layered:

- **Sliding window** (`crates/gw-health/src/sliding_window.rs`): a 30-second `VecDeque<(Instant, bool)>` of outcomes (`WINDOW_DURATION = 30s`), evicting on each `record`. `failure_rate()` returns `(total_samples, rate)`.
- **3-state circuit** (`CircuitState::{Closed, Open, HalfOpen}` in `crates/gw-health/src/cooldown.rs`): the trip rule in `FormatHealth::record` is `total >= SAMPLE_FLOOR (20)` and `rate > FAILURE_RATE_TRIP (0.5)` ⇒ `Closed → Open` for `OPEN_HOLD_MS (60_000)`. `is_available` CAS-claims the half-open probe slot (`Open → HalfOpen`) once `open_until_ms` has passed; only the CAS winner sees `true`, concurrent callers see `false`. A successful half-open sample CAS-transitions `HalfOpen → Closed`; a failed half-open sample CAS-transitions `HalfOpen → Open` and re-arms the 60 s hold (`crates/gw-health/src/format_health.rs` lines ~116–151).
- **Explicit cooldown**: `cooldown_until_ms` + `cooldown_reason`. `apply_cooldown` grows the deadline monotonically (CAS up to `max(current, candidate)`); the reason is only written when the deadline actually advanced. `reset_consecutive` is always honoured even when the deadline did not grow. `clear_cooldown` zeroes the deadline and resets the reason to `None` without touching the circuit or window.

`is_available()` ORs the two signals: an active cooldown (`cooldown_until_ms > now`) returns `false` before the circuit is even consulted (`crates/gw-health/src/format_health.rs` lines ~66–89).

**`KeyHealthStore` — the public face** (`crates/gw-health/src/store.rs`). Owns `by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>` and exposes the verbs callers use: `is_available`, `peek` (read-only `HealthSnapshot`, no half-open CAS), `record`, `apply_cooldown`, `clear_cooldown`, plus snapshot helpers (`snapshot_entries`, `snapshot_entries_for_key`, `snapshot_for_pairs`) and JSON renderers (`snapshot_json`, `key_snapshot_json`). `apply_cooldown` emits `gw_health_apply_cooldown_total{reason}`.

**`CooldownReason`** (`crates/gw-health/src/cooldown.rs`): a stable `#[repr(u16)]` enum, 16 variants (`None=0` … `SlidingWindowTrip=15`), with `from_u16` (unknown → `None`, forward-compatible), `as_u16`, and `label()` for metrics/tracing. `CooldownDirective { reason, ttl_secs, reset_consecutive }` is the apply payload.

**Routing consumer** (`crates/gw-routing/src/...`): candidate filtering calls `KeyHealthStore::is_available` — `crates/gw-routing/src/pipeline/mod.rs` (`if !health.is_available(...)`), `crates/gw-routing/src/strategy/round_robin.rs`, `fixed_order.rs`, `cost_first.rs`, `latency_first.rs`. Routing only reads; it never records or applies cooldowns.

**Outcome recorder** (`crates/gw-proxy/src/filters/logging.rs`): after each request, builds an `OutcomeKind` — `Success` on `ok`, else `Failure { http_status, retry_after_secs: None }` — and calls `health.record(&pk.key_id, api_fmt, outcome)` (lines ~194–202). `api_fmt` is resolved from the upstream codec kind (falling back to `resolve_upstream_format`) so the correct per-format bucket is updated.

**Cooldown applier** (`crates/gw-proxy/src/error/listeners/key_health.rs`): `KeyHealthListener` implements `ErrorListener`. On a `ClassifierOutcome` carrying a `cooldown`, it maps `gw_core::error::CooldownDirective` → `gw_health::CooldownDirective` (decoding `reason_code` via `CooldownReason::from_u16`) and calls `key_health.apply_cooldown`. The classifier already folds an upstream `Retry-After` header into `ttl_secs`; the listener emits `gw_retry_after_consumed_total{source=header|default}` and `gw_classifier_listener_observed_total{listener="key_health", kind=wired|unwired}`.

**Admin observability** (`crates/gw-gateway/src/health.rs`): `/healthz/circuits` renders `state.key_health.snapshot_json()`; `/healthz/key/<id>` renders `key_snapshot_json(key_id)`. Per the doc comment in `store.rs`, this JSON is observability-only and not a wire contract.

**Cooldown-recovery probe** (`crates/gw-health/src/quota_probe.rs`). This is a *cooldown-recovery probe*, not a quota-remaining probe: it only probes keys already in cooldown and clears that cooldown early on a 2xx. `QuotaProbeRunner::spawn` returns `None` when disabled (zero overhead). When enabled, a background tokio task sleeps `interval_secs` then runs `probe_cycle`, which snapshots entries, skips any not `in_cooldown`, calls `ProbeTransport::probe` (e.g. `GET /v1/models`), and on `ProbeResult::Success` calls `clear_cooldown`; any other result leaves the original TTL intact. Each probe emits `gw_quota_probe_total{key_id, result=success|failure}`. Probe requests bypass the billing/ledger pipeline (no `usage:records`). Bootstrap wiring (`crates/gw-gateway/src/lib.rs` lines ~433–451) only spawns the runner when `cfg.health.quota_probe.enabled` is set; `QuotaProbeCfg::default` has `enabled = false`. The concrete `HttpProbeTransport` lives in `crates/gw-gateway/src/quota_probe_transport.rs`.

## Deliberate behaviours

- **`record()` reduces the outcome to a bool.** `KeyHealthStore::record` computes `ok = matches!(outcome, OutcomeKind::Success)` and discards the rest (`let _ = outcome;` in `crates/gw-health/src/store.rs`). The `OutcomeKind::Failure { http_status, retry_after_secs }` payload is currently inert at the health-store layer — all status-driven cooldown decisions are taken upstream by the error classifier and delivered through `KeyHealthListener::apply_cooldown`. The window/circuit only need the boolean.
- **Cooldown overrides the circuit.** `is_available` checks the cooldown deadline first and returns `false` before consulting circuit state; the two signals are OR'd (either non-available makes the pair unavailable) (`crates/gw-health/src/format_health.rs`).
- **The sliding-window trip does not write a cooldown reason.** Tripping `Closed → Open` leaves `cooldown_reason` untouched; circuit-open is separately observable via the snapshot's `circuit` field. The `SlidingWindowTrip` reason variant is reserved for explicit applier calls (`crates/gw-health/src/format_health.rs` comment lines ~112–115).
- **Cooldown deadline is monotonic.** `apply_cooldown` only ever grows `cooldown_until_ms`; a later, shorter directive cannot shorten an in-flight cooldown, and a directive that loses the max race does not overwrite the still-active reason (`crates/gw-health/src/format_health.rs` lines ~157–187).
- **A failed half-open probe returns to Open and re-arms the hold.** Without this transition a `(key, fmt)` could be stuck `HalfOpen` until process restart (the post-hold window is near-empty, so the trip branch cannot re-fire on a single sample). `open_until_ms` is re-armed only on CAS success so a concurrent winner does not double-arm (`crates/gw-health/src/format_health.rs` lines ~126–151).
- **Probe failures preserve the cooldown.** A non-2xx probe is logged and leaves the original TTL untouched; only a 2xx clears it early (`crates/gw-health/src/quota_probe.rs` `probe_cycle`).

## Cross-process touchpoints

Key health is per-replica and in-memory; it is **not** persisted and **not** shared across Pods (`crates/gw-health/src/lib.rs`: "No persistence in v1 … each gateway replica maintains its own snapshot"). The only cross-process surface is the upstream HTTP call the cooldown-recovery probe makes to the vendor (`ProbeTransport::probe` in `crates/gw-health/src/quota_probe.rs`); that call deliberately produces no Redis `usage:records` entry. No `counter:*` keys, Redis stream payloads, or PgSQL entities are owned by this family.

## Invariants & lints

- `CooldownReason` discriminants are stable and additive — never renumber existing variants (`crates/gw-health/src/cooldown.rs` module doc); observability dashboards key off the numeric label, and `from_u16` decodes unknown values to `None` for cross-version forward-compatibility.
- `peek` must not trigger the half-open CAS — it is observation-only (`crates/gw-health/src/store.rs` `HealthSnapshot` doc); only `is_available` admits a half-open probe.
- `snapshot_json` / `key_snapshot_json` are observability JSON, not a wire contract — production code must not parse them (`crates/gw-health/src/store.rs` `snapshot_json` doc).
- The `no_cost_micro` / `no_billing_crate` workspace lints in `scripts/check-invariants.sh` apply here too: probe requests bypass billing and `gw-health` performs no cost computation.

## Not built → ROADMAP

These are not present in `crates/gw-health/` or its consumers and are documented here only as forward pointers:

- **K-B vendor-specific error analyzers.** Per-vendor parsing that maps upstream error bodies/headers to specific `CooldownReason` variants and graded TTLs. Today the mapping is generic: `KeyHealthListener` decodes whatever `reason_code` the shared classifier produced (`crates/gw-proxy/src/error/listeners/key_health.rs`), and the `SlidingWindowTrip` reason variant exists in `cooldown.rs` but is never written by any analyzer.
- **K-C in-Rust OAuth refresh.** The `OAuthRefreshFailed` cooldown reason (`crates/gw-health/src/cooldown.rs`) is defined, but no Rust code performs an OAuth token refresh or calls `clear_cooldown` on refresh success. Cross-ref the auth spec (spec 07).
- **K-D quota-remaining probe + `QuotaState` entity fields.** The current probe is a cooldown-recovery probe only (`crates/gw-health/src/quota_probe.rs`). A true quota-remaining probe that reads and stores remaining vendor quota — together with `QuotaState` entity fields to persist it — does not exist; `FormatHealth` carries no quota-remaining field.
