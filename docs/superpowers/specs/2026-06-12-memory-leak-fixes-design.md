# Memory Leak Fixes — Design Spec

**Date**: 2026-06-12
**Status**: Approved (brainstorming)
**Scope**: 12 P0/P1 memory-leak / resource-release defects identified by the 2026-06-12 audit, fixed across 7 crates over 7 sequential PRs, dependency-bottom-up order, single feature branch per crate, hard "zero behavior regression" constraint.

## Motivation

A targeted memory-leak audit (2026-06-12, 5 parallel Explore agents covering static patterns, long-lived containers, async/channel lifecycle, per-request release paths, and connection pools) identified the following:

| Tier | Count | Examples |
|------|-------|----------|
| 🚨 P0 — real leak | 7 categories | `Box::leak` per unknown capability, `LeaseGuard` no `Drop`, `SpillFile` no `Drop`, multiple unbounded DashMaps |
| ⚠️ P1 — bounded-fragile / design risk | 5 categories | Tunnel probe task never cancelled, `BillingBacklog` unbounded by design, Pingora pool not explicitly configured, manual-sweep IP limiters |

None of the leaks are exponential; all are O(time) or O(distinct_keys) linear. Two of them — `Box::leak` in `gw-capability` and `RedosTracker` in `gw-auth` — are externally amplifiable (attacker-controlled keyspace), making them the highest-risk among linear leaks.

This spec covers the fix design for all 12 categories. Implementation is delivered as 7 sequential, per-crate PRs against `main`.

## §1 Goals & Constraints

**Goals**:
1. Close all 12 leak categories with verifiable test coverage.
2. Land changes incrementally so partial completion is itself a monotone safety improvement.
3. Keep CLAUDE.md sync-rule fully compliant per PR (CLAUDE.md ⇄ AGENTS.md byte-identical; `docs/spec/`, `docs/architecture.md`, `docs/ROADMAP.md` updated in the same commit).

**Hard constraints**:
1. **Zero behavior regression** on existing happy paths. New `Drop` / sweep / cap logic only fires on anomaly or saturation paths.
2. **Zero hidden issues** — every fix MUST be preceded by a failing test (TDD) that demonstrates the leak in the unfixed code and disappears once fixed. Implementation review without an exercising test is not accepted.
3. **One PR per crate**, merged before starting the next crate. Single feature branch per crate, re-cut from latest `main` for each.

**Explicitly NOT changed**:

| Category | Specifics |
|----------|-----------|
| Protocol / codec | OpenAI/Anthropic/Gemini parsing, cross-render, SSE framing |
| Pingora filter chain | request_filter / upstream_peer / response_filter / logging order and behavior |
| Routing | The 8 strategies' key-selection algorithms, health detection logic, cooldown windows |
| Billing | `BillingBacklog` must-deliver invariant, `UsageRecord` schema, Redis stream key layout (we add a bytes-size gauge only) |
| Pingora internals | conn pool algorithm, HTTP state machine (we only configure `Opt` parameters) |
| PgSQL pool | already-reasonable parameters (`idle_timeout=600s`, `min_connections=2`) |
| soak harness | `tests/e2e/client/soak/` (the harness lags the dev branch; modifying it is out of scope) |
| Healthy P2 items | `RegexCache`, `Sticky`, Moka entity caches |

## §2 Per-Crate Execution Sequence

Execution order is **dependency bottom-up**: lower-DAG crates first, so callers above are never editing against an in-flux dependency.

**Pre-implementation verification (mandatory for every crate)**: the implementer subagent re-grep's its target files against the current `main` and reports any item from the list below that has been partially or fully addressed by a recently-landed change (notably the resilience refactor in commit `25487be`). Already-addressed items are removed from the crate's work scope and noted in the PR description. The remaining design below is the audit's view as of 2026-06-12; small drift is expected.

### Index Matrix

| # | Crate | Items | Pattern | Model | Special gate |
|---|-------|-------|---------|-------|--------------|
| 1 | gw-runtime (+ gw-proxy callers) | 2 | RAII `Drop` + caller reset | sonnet | — |
| 2 | gw-store | 3 | `AbortOnDropJoinHandle` + GC self-restart + TTL sweep | sonnet | — |
| 3 | gw-ledger | 4 | `Drop` rollback + sweep + observability gauge | **opus** | **design-only gate + Go-side companion change** |
| 4 | gw-auth | 4 | Moka cache + single sweep loop | sonnet (design phase may upgrade to opus) | RPM TTI-boundary semantic change documented as deliberate behavior |
| 5 | gw-routing | 5 | Single sweep loop + semantic adjustment (`InFlightStore.count`) | sonnet | — |
| 6 | gw-capability | 1 | `&'static str` → `Arc<str>` + Moka | sonnet | Public enum boundary, requires full-workspace grep |
| 7 | gw-gateway | 3 | ShutdownCoordinator wiring + explicit pool config | sonnet | — |

### Crate 1 — gw-runtime (+ gw-proxy callers)

**Items**:
- `SpillFile` has no `Drop` impl → shard files leak on non-clean exit (`crates/gw-runtime/src/spill_io.rs:132`)
- `reset_response_attempt_state` does not reset `archive_upstream_response_body` / `archive_downstream_response_body` → archive data contamination across retries + file leak (`crates/gw-proxy/src/gateway.rs:131-145`)

**Pattern**: RAII Drop + caller-side reset
- `SpillFile::Drop` issues `tokio::spawn(tokio::fs::remove_file(...))` per shard (sync drop context cannot block).
- `reset_response_attempt_state` adds `ctx.archive_*_response_body = ArchiveState::default()`; the default variant assignment drops the live `Spilled` variant, triggering the new `Drop`.

**Subtle correctness**:
- Spawned cleanup errors (file-not-found, permission) emit a metric and silently return; they never panic (would abort drop path).
- Coexists with existing `spawn_quota_enforcer` LRU sweep — the new `Drop` is **timely** cleanup, the LRU is **fallback** cleanup. Both paths must be idempotent.

**Required tests** (TDD, fail before fix):
1. Write 1 MB across multiple shards → drop → assert shard files removed.
2. Manually `unlink` a shard between creation and drop → assert no panic.
3. Simulate attempt-1 archive write, run `reset_response_attempt_state`, write attempt-2 → assert attempt-2 archive contains no attempt-1 bytes.
4. Drop + concurrent LRU sweep → idempotent (no double-unlink errors).

**Docs**: `docs/spec/06-billing-ledger.md` (archive section), `docs/architecture.md` §Deliberate behaviours (SpillFile cleanup ownership), `docs/ROADMAP.md` (close item).

### Crate 2 — gw-store

**Items**:
- `DeductGc` JoinHandle discarded via `let _`, no panic recovery (`crates/gw-gateway/src/lib.rs:1057`)
- `USER_CIRCUIT: DashMap<UserId, UserCircuit>` no eviction (`crates/gw-store/src/deduction.rs:48`)
- `DEDUCT: DashMap<UserId, Arc<DeductionState>>` only safe while GC runs; degrades to unbounded leak if GC dies (same site)

**Pattern**:
- Reuse `gw-runtime::Supervisor` (already in workspace; provides catch_unwind + exponential-backoff restart by `RestartPolicy`).
- Wrap the existing DeductGc spawn site in `Supervisor::new("deduct_gc", RestartPolicy::default(), token).spawn(...)`; register the returned `JoinHandle` on `ShutdownCoordinator`.
- `USER_CIRCUIT` swept inside the same GC loop, by closed-state-age TTL.

**Pre-implementation verification** (the implementer subagent confirms): inspect whether DeductGc is already partially Supervisor-wrapped — if so, scope reduces to ShutdownCoordinator registration and `USER_CIRCUIT` TTL only.

**Subtle correctness**:
- GC cadence (60s) **must not change** — downstream code may assume the timing.
- `USER_CIRCUIT` sweep only removes entries that are **in closed state and older than TTL**; do not evict open-state entries (would incorrectly reset the circuit).

**Required tests**:
1. Inject a panic into the GC iteration → assert task restart counter increments and DEDUCT continues being swept.
2. `USER_CIRCUIT` TTL eviction with mock clock.
3. `AbortOnDropJoinHandle` drop semantics — assert spawned task is aborted.

**Docs**: `docs/spec/06-billing-ledger.md` (DEDUCT / USER_CIRCUIT lifecycle), `docs/architecture.md` §Deliberate behaviours (GC self-restart).

### Crate 3 — gw-ledger (opus + design-only gate + Go companion)

**Items**:
- `LeaseGuard` has no `Drop` impl → on panic between `try_acquire` and `logging`, credits are permanently removed from the in-memory lease pool (`crates/gw-ledger/src/lease.rs:78-84`)
- `LEASE_POOL: DashMap<UserId, Arc<LeaseState>>` no eviction (`crates/gw-ledger/src/lease.rs:69`)
- `COEF_STAGING: DashMap<u64, StagingState>` accumulates orphan entries from incomplete commits (`crates/gw-ledger/src/coef.rs:85`)
- `BillingBacklog` has no size-bytes gauge or upper-bound alarm metric (`crates/gw-ledger/src/backlog.rs:16`) — **observation only, must-deliver semantics preserved**

**Mandatory design-only gate** — before any implementer dispatch, produce `docs/superpowers/specs/2026-06-XX-gw-ledger-design.md` (`XX` is filled in with the gate's actual draft date) enumerating:

1. `LeaseGuard::Drop` rollback policy choice (one of):
   - (a) Refund all reserved credits;
   - (b) Refund only the unsettled portion (when `settle_actual` was partially applied);
   - (c) No refund — emit a `gw_lease_panic_leak_total` metric only and let the Go-side lease pool consistency scan repair from authoritative state.
2. Companion Go-side change in `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/` — three styles to consider:
   - Active scan reconciler;
   - Passive metric consumption / alerting;
   - Schema extension (new `panic_recovered` field on lease record).
3. Whether `COEF_STAGING` orphan eviction uses `version < active_version - safety_margin` (preserves recently committed) or `wall-clock TTL` (simpler).

**User must approve the gate's design doc before implementer dispatch.**

**Subtle correctness**:
- `LeaseGuard::Drop` and `release` / `settle_actual` interlock via `AtomicBool` flip — at-most-once rollback; idempotent.
- `COEF_STAGING` eviction MUST NOT remove a `version` that has been committed but not yet propagated to all instances — the safety margin parameter must reflect this.
- `BillingBacklog` size gauge uses `AtomicUsize` maintained at push/pop sites; never call `VecDeque::len()` from the gauge read path (locks the drainer hot path).

**Required tests**:
1. `LeaseGuard::Drop` rolls back on panic path (chosen policy).
2. After `release` / `settle_actual` → drop is a no-op (idempotent).
3. `LEASE_POOL` sweep removes already-transitioned protected users.
4. `COEF_STAGING` orphan-version sweep (mock clock + version progression).
5. `BillingBacklog` size gauge tracks queue bytes accurately.
6. **Go-side integration**: round-trip test that Rust emits new metric / field and Go consumes it.

**Docs** (4 files — more than other crates):
- `docs/spec/06-billing-ledger.md` (lease state machine, backlog observability)
- `docs/architecture.md` §Deliberate behaviours (`LeaseGuard::Drop` semantics)
- `docs/go-contracts/contract-change/2026-06-XX-leaseguard-drop.md` (**new file** — Go contract change; the `XX` is filled in with the Crate 3 PR open date)
- `docs/ROADMAP.md`

**Go-side changes**: in the same PR, modify `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/` lease pool consistency scan code per the chosen design-gate option. Follow the Go project's own `CLAUDE.md` / `.claude/` rules.

### Crate 4 — gw-auth (sonnet; design phase may upgrade to opus)

**Items**:
- `RedosTracker` HashMap unbounded, **attacker-controlled keyspace** (`crates/gw-auth/src/model_permissions.rs:122`)
- `RateLimitGuard` 5 unbounded DashMaps (`crates/gw-auth/src/ratelimit/mod.rs:28-32`)
- `AdaptiveRpmManager.evict()` exists but has no scheduled caller (`crates/gw-auth/src/ratelimit/adaptive.rs:204`)
- `IpRateLimitGuard` / `AuthFailureIpLimiter` manual sweep, no timer (`ip_ratelimit.rs`, `auth_ip_ratelimit.rs`)

**Pattern**:
- Replace `RedosTracker` and 5 `RateLimitGuard` DashMaps with Moka caches (`max_capacity` + TTI).
- For sweep wiring: helper functions `spawn_rpm_decay` and `spawn_ip_ratelimit_sweep` already exist in `crates/gw-gateway/src/lib.rs` (the second is `#[allow(dead_code)]`, indicating it is **defined but not called**). The scope here is to:
  1. Wire `spawn_ip_ratelimit_sweep` into bootstrap (drop the dead_code attribute).
  2. Add `spawn_auth_failure_ip_sweep` if not present and wire it.
  3. Confirm `spawn_rpm_decay` is wired (verified: it ticks `AdaptiveRpmManager.tick_decay`; we also need it or a sibling to call `evict()` periodically).
- All sweep tasks register their `JoinHandle` on `ShutdownCoordinator`.

**Subtle correctness — this is the most behavior-sensitive crate**:
> Replacing `Arc<DefaultDirectRateLimiter>` with Moka means a new entry's limiter state starts empty. **A user who exhausts their RPM, then idles longer than the TTI, then returns, receives an apparently fresh window.** This is a deliberate behavior change. It MUST be written into `docs/spec/05-rate-limit.md` and `docs/architecture.md` §Deliberate behaviours. Spec-compliance reviewer confirms the doc reflects this exactly.

**Required tests**:
1. Each cache: TTL / capacity boundary.
2. Sweep loop fires periodically and reaches each target (`AdaptiveRpmManager.evict`, both IP sweeps).
3. **Boundary test**: user hits RPM limit → idle past TTI → re-enters → assert the documented "fresh window" semantics.

**Docs**: `docs/spec/05-rate-limit.md` (TTI boundary semantic change), `docs/architecture.md` §Deliberate behaviours, `docs/ROADMAP.md`.

### Crate 5 — gw-routing (sonnet)

**Items**:
- `ModelFrequencyTracker.counters` no eviction (`crates/gw-routing/src/reservation.rs:45`)
- `ReservationGlobalView` two unbounded DashMaps; accumulates across pod restarts (`crates/gw-routing/src/reservation.rs:104-105`)
- `InFlightStore.by_key` uses `get_or_create` semantics on read (`crates/gw-routing/src/in_flight.rs:48`)
- `LatencyTracker.evict_stale()` exists but has no timer (`crates/gw-routing/src/latency.rs:132`)
- `AffinityManager.sweep_expired()` same
- `UNKNOWN_PRESETS_WARNED: DashSet` unbounded (`crates/gw-routing/src/strategy/set.rs:48`)

**Pattern**:
- Add a `spawn_routing_sweep` helper in `crates/gw-gateway/src/lib.rs` wrapped in `gw-runtime::Supervisor`; the loop drives all 5 sweep targets per tick.
- `InFlightStore.count()` semantic adjustment: **reading a non-existent key returns 0 without creating an entry**. Acquire-path entry creation unchanged.
- `UNKNOWN_PRESETS_WARNED` replaced with Moka (cap 1000).
- `ReservationGlobalView.by_model_instance` eviction by instance heartbeat: an instance not seen in `instance_expiry_secs` (default 3600s, configurable) is removed.

**Subtle correctness**:
- The `InFlightStore.count()` semantic change requires a full-workspace grep for `.count(` callers; reviewer enumerates them and confirms none depend on the old "read-creates-entry" behavior.
- Cluster scale-in causes many instances to disappear simultaneously; the sweep loop must remain responsive (batch eviction, no long lock holds).

**Required tests**:
1. Each sweep target: mock-clock test that advances time and asserts eviction.
2. `InFlightStore.count()` on non-existent key returns 0 and does not create an entry.
3. `ReservationGlobalView` after instance expiry: routing decisions remain correct.
4. `UNKNOWN_PRESETS_WARNED`: over-cap warns still increment the metric but the set evicts oldest.

**Docs**: `docs/spec/03-routing.md`, `docs/architecture.md` §Deliberate behaviours (`InFlightStore.count` semantic), `docs/ROADMAP.md`.

### Crate 6 — gw-capability (sonnet)

**Items**:
- `Box::leak` produces a permanent allocation per distinct unknown capability string (`crates/gw-capability/src/resolver.rs:119`).

**Pattern**:
- `Capability::Tagged` storage type changes from `&'static str` to `Arc<str>`.
- Introduce a Moka cache (`max_capacity 10_000`, TTI 1h) mapping tag string → `Arc<str>` so repeated tags reuse the same `Arc`.

**Subtle correctness — public-API boundary**:
- `Capability` is a workspace-public enum. All matches on `Capability::Tagged(s)` need a full-workspace grep and update; `s: &str` access stays compatible via `Arc<str>` deref.
- Serialization / deserialization paths unchanged (only the in-memory storage type changes).

**Required tests**:
1. Two requests with the same tag share an `Arc<str>` (`Arc::strong_count` or pointer equality).
2. Cache LRU eviction under load.
3. TTI expiry under mock clock.
4. **Attack scenario**: 1000 distinct tags → total memory bounded by `max_capacity × avg_str_len`.

**Docs**: `docs/spec/02-capability.md`, `docs/architecture.md` §Deliberate behaviours, `docs/ROADMAP.md`.

### Crate 7 — gw-gateway (sonnet)

**Items**:
- Tunnel `_probe_handle` never cancelled, holds `Arc<TunnelStack>` (`crates/gw-gateway/src/lib.rs:1369`)
- Redis pool has no `pool.quit()` invocation on shutdown
- Pingora upstream conn pool relies on defaults (no explicit `Opt`)

**Pattern**:
- `probe_handle` is registered into the existing `ShutdownCoordinator`; its loop body wraps `tokio::select!` on the shutdown cancellation token.
- During drain, the coordinator hook calls `RedisClient::quit()` after all dependent tasks have aborted.
- Set explicit `Opt` for Pingora: idle timeout and max-idle-per-backend.

**Subtle correctness**:
- Redis `quit()` ordering: must run after all XREADGROUP / consumer loops are aborted; otherwise, in-flight consumers may surface errors that mask shutdown intent.
- Pingora pool config must not conflict with per-request `Timeouts::apply_to_peer` (which sets per-peer timeouts).

**Required tests**:
1. Probe handle aborts when shutdown is signalled.
2. Integration: Redis `quit()` is invoked after dependent task abort in the drain hook order.
3. Pingora idle config takes effect (may require a mock peer).

**Docs**: `docs/spec/09-tunnel.md` (probe lifecycle), `docs/architecture.md` §Bootstrap / §Shutdown, `docs/ROADMAP.md`.

## §3 Workflow Per Crate

All 7 crates use the same execution loop. Differences are only the model choice and the gw-ledger design-only gate.

```
┌─ Enter Crate N ───────────────────────────────────────────────┐
│                                                                │
│  Step 0 (gw-ledger only): design-only gate                    │
│     · I (orchestrator, opus) draft the design doc             │
│       listing the 3 LeaseGuard::Drop options                  │
│       and the 3 Go-side change styles                         │
│     · → User approves → commit design doc → Step 1            │
│                                                                │
│  Step 1: Dispatch implementer subagent (sonnet / opus)         │
│     Prompt carries:                                            │
│       · §2 fix list verbatim for this crate                   │
│       · Failing-test-first TDD requirement                    │
│       · CLAUDE.md sync-rule digest + exact docs paths         │
│       · Cross-crate touchpoint files                          │
│       · In-crate reference patterns (e.g. RequestFreeze::Drop)│
│       · Completion gate commands                              │
│     Subagent works on fix/memory-leaks-<crate-name>           │
│                                                                │
│  Step 2: Handle implementer status                            │
│     · DONE                  → Step 3                          │
│     · DONE_WITH_CONCERNS    → read concerns;                  │
│                               correctness ones go to Step 3   │
│     · NEEDS_CONTEXT         → supply context, redispatch      │
│     · BLOCKED               → STOP, ask user                  │
│                                                                │
│  Step 3: spec-compliance reviewer (sonnet)                    │
│     Verifies: all §2 items done? Nothing extra?               │
│     Docs synced? CLAUDE.md ⇄ AGENTS.md byte-identical?       │
│     · ✅ → Step 4                                              │
│     · ❌ → minor issue: fix inline, continue                   │
│         major issue: bounce to Step 1 (same implementer)      │
│                                                                │
│  Step 4: code-quality reviewer (sonnet; opus for gw-ledger)   │
│     Verifies: TDD test coverage? edge cases?                  │
│     Anything that could leave a hidden issue?                 │
│     Focus: exception paths, Drop order, AtomicBool idempotence│
│            concurrency / shutdown timing                      │
│     · ✅ → Step 5                                              │
│     · ❌ → same inline / bounce rule as Step 3                 │
│                                                                │
│  Step 5: Completion gate (run by orchestrator)                │
│     · cargo test -p <crate>                                   │
│     · cargo clippy --workspace --all-targets -- -D warnings   │
│     · cargo fmt --all -- --check                              │
│     · bash scripts/check-invariants.sh                        │
│     All green → Step 6; any red → bounce to Step 1            │
│                                                                │
│  Step 6: Open PR; wait for user review + merge                │
│     PR title: "fix(<crate>): close <N> memory leaks"          │
│     PR body: §2 excerpt + design-doc link                     │
│                                                                │
│  Step 7: After merge, checkout main + cut next branch         │
│     fix/memory-leaks-<crate N+1>                              │
│                                                                │
└────────────────────────────────────────────────────────────────┘

Final (after Crate 7 merged):
  · cargo test --workspace (with --ignored if relevant)
  · If green: done. If red: regression analysis, follow-up PR.
```

**Operational discipline**:

1. **One implementer per crate, reused across review rounds**. Crate switching is the only point a new implementer is spawned. Avoids context loss between bounces.

2. **No silent fix-and-continue across review tiers**. spec-compliance pass MUST be ✅ before code-quality starts. Major issues found in code-quality bounce to implementer; trivial issues are fixed inline by the reviewer.

3. **Reviewer inline-fix rule** (from prior session memory):
   - **Inline-fix** (do not bounce; after the inline fix, the reviewer **proceeds to its own ✅ decision without re-running the full review**): single-line typo, comment polish, metric label fix, missing log field, variable rename for clarity, clippy / fmt remnants, doc wording, obvious `let _` / `?` / `.expect()` omissions.
   - **Bounce to implementer** (full re-review on return): missing §2 item, missing TDD test, cross-crate file not updated, public API / signature / trait change, docs-sync gap (CLAUDE.md / docs/spec / Go contracts), Drop / AtomicBool / concurrency correctness, > 5 LoC change OR > 1 file change, any behavior implication.
   - **When unsure: bounce** (conservative default consistent with "no hidden issues").

4. **PR awaits user merge**. Orchestrator never self-merges. Only state preparation is automated.

5. **BLOCKED is a true stop**. Orchestrator does not switch models, reorder, or abandon fixes autonomously.

6. **gw-ledger design-only gate is a hard stop**. Step 1 does not begin until the design doc is approved.

7. **2-round bounce threshold**: if a crate's review bounces twice without converging, stop and surface to the user — usually indicates a §2-design problem.

## §4 Risk Register

| Risk | Trigger | Mitigation |
|------|---------|------------|
| Moka replacement changes RPM behavior at TTI boundary | gw-auth user exhausts RPM → idle > TTI → returns | Documented as deliberate behavior (`docs/spec/05-rate-limit.md` + `docs/architecture.md`); explicit boundary test |
| `InFlightStore.count()` semantic adjustment breaks a caller | Caller relies on "read-creates-entry" | Pre-implementation full-workspace grep `\.count\(`; reviewer enumerates all callers and confirms |
| `LeaseGuard::Drop` and `settle_actual` race | Rare panic mid-settle | `AtomicBool` flip + idempotent rollback; test cover |
| Rust ↔ Go change ordering mismatch | Two-side commit out of sync | Single PR contains both Rust and Go changes for gw-ledger |
| Redis `quit()` ordering bug | drainer races against consumer | Explicit ShutdownCoordinator hook ordering: dependent tasks aborted first, then `pool.quit()` |
| `SpillFile::Drop` spawn outlives process | Shutdown faster than cleanup task | Acceptable — LRU enforcer cleans on next start; no data-correctness impact |
| Orchestrator time budget overruns | 7 PRs × user review | Per-crate PR — user can pause at any merge point |

**Conditions under which the orchestrator stops and asks**:
- Implementation reveals a §2 omission.
- Docs sync need exceeds §2's estimate (e.g. requires editing `docs/workflow.md` phase descriptions).
- Any BLOCKED status from a subagent.
- gw-ledger design-only gate (mandatory stop).
- 2-round review bounce without convergence (signal that §2 design may be wrong).

## §5 Validation Matrix

| Phase | Commands | Purpose |
|-------|----------|---------|
| Implementer iteration | `cargo test -p <crate>` + new tests | Fast feedback loop |
| Per-crate completion gate | + `cargo clippy --workspace --all-targets -- -D warnings`<br>+ `cargo fmt --all -- --check`<br>+ `bash scripts/check-invariants.sh` | Required before PR open |
| Final (post-Crate-7) | + `cargo test --workspace` (optionally `-- --ignored`) | Single full-workspace gate at the end |

**Soak harness is intentionally excluded** — the e2e soak driver lags the dev branch (per user note 2026-06-12). The TDD-required failing test per fix replaces soak coverage for the specific leak being closed; broader integration is covered by `--workspace` at the final step.

## §6 Open Items / Future Considerations

- After Crate 7 merges, run a fresh memory-leak audit (same 5-agent template as 2026-06-12) to confirm no new categories emerged from the refactor itself.
- If `cargo test --workspace` final gate reveals integration-level regression, file a follow-up PR — do NOT inline-fix as the 8th item of this work.
- The `BillingBacklog` upper-bound gauge added in Crate 3 enables a future conversation about whether the must-deliver invariant should be relaxed under extreme outage — out of scope here, but the observability lays the groundwork.

## Sign-Off

This spec captures all decisions from the brainstorming session on 2026-06-12.
Approved by user: pending review of this written spec.
