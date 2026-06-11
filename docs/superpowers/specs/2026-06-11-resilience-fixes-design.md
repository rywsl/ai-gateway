# Resilience Fixes — Design Spec

**Date**: 2026-06-11
**Status**: Approved (brainstorming)
**Scope**: Single comprehensive design covering 6 resilience fixes; platform-first delivery order (Approach B); pre-launch project, no production rollback concerns.

## Motivation

A pre-launch audit (阻塞 / 流式 / 异步任务 / 高并发) surfaced the following defects, ordered by ROI:

1. **No SIGTERM/SIGINT handler** in `gw-gateway/src/main.rs`. Kill loses in-flight billing.
2. **`EmergencyDlqWriter` does `std::fs::*` under `std::sync::Mutex`** in async context (`gw-ledger/src/dlq.rs`).
3. **`ModelPermission` recompiles regex per request** (`gw-proxy/src/filters/request.rs:1651`).
4. **`archive_upstream/downstream_response_body` `BytesMut` grow unbounded** until EOS (`gw-proxy/src/ctx.rs`, `gw-proxy/src/filters/response.rs`).
5. **Only `gw-egress-impersonate` has a Supervisor**; ledger flusher, inflight reaper, task worker loops, health server, stream consumer, DeductGc are bare `tokio::spawn` and silently die on panic.
6. **Billing data loss possible** under DLQ disk failure cascade (no in-memory final fallback).

We deliver these as one platform-first plan: build shared runtime primitives first, then apply them. The plan is single-PR but sequential tasks (T1–T6).

## §1 Architecture & Layering

One new crate, several extensions, no public-trait changes.

```
crates/gw-runtime/             [NEW]   Long-running task runtime primitives
  ├── src/supervisor.rs               General Supervisor (lifted from gw-egress-impersonate)
  ├── src/shutdown.rs                 ShutdownCoordinator (CancellationToken + JoinSet)
  ├── src/spill_io.rs                 Async sharded spill primitive (tokio::fs)
  ├── src/regex_cache.rs              Moka-backed compiled-regex cache
  └── src/health/                     Hooks + SystemHealthMonitor + BackpressureGate

crates/gw-egress-impersonate/         Existing
  └── src/supervisor.rs              ─► delete local impl; re-export gw-runtime::Supervisor

crates/gw-ledger/                     Existing
  ├── src/dlq.rs                     ─► use gw-runtime::spill_io
  ├── src/flusher.rs                 ─► wire ShutdownCoordinator + Supervisor
  ├── src/inflight_reaper.rs         ─► wrap in Supervisor
  └── src/backlog.rs            [NEW] BillingBacklog growable VecDeque + recovery

crates/gw-task-worker/                Existing
  └── src/lib.rs                     ─► wrap 5 core loops in Supervisor

crates/gw-proxy/                      Existing
  ├── src/filters/response.rs        ─► archive cap + spill
  ├── src/ctx.rs                     ─► ArchiveState enum (InMemory/Spilled/Truncated)
  ├── src/filters/request.rs         ─► ModelPermission via RegexCache
  └── src/filters/early.rs           ─► BackpressureGate admit check

crates/gw-gateway/                    Existing
  ├── src/main.rs                    ─► SIGTERM/SIGINT signal handler
  └── src/lib.rs                     ─► bootstrap wiring (Supervisor + Coordinator + RegexCache + Monitor)
```

**Workspace layering**: `gw-runtime` sits at the bottom of the dependency DAG. It depends only on `tokio`, `tracing`, `moka`, `arc-swap`, `parking_lot`, `metrics`. Every upper crate may depend on it; it depends on no upper crate. `scripts/check-invariants.sh` adds `gw-runtime` to the allowed-bottom-layer set.

**Task delivery order** (Approach B — platform first):

| # | Task | Scope | Depends |
|---|------|-------|---------|
| T1 | Build `gw-runtime` primitives | Supervisor, ShutdownCoordinator, SpillFile, RegexCache, Health hooks, BackpressureGate | — |
| T2 | SIGTERM/SIGINT + ShutdownCoordinator wiring | Cancel propagation chain | T1 |
| T3 | Apply Supervisor everywhere | Ledger flusher, InflightReaper, TaskWorker ×5, Health server, Stream consumer, DeductGc, egress-impersonate (migration) | T1, T2 |
| T4 | DLQ async-ization + BillingBacklog | `EmergencyDlqWriter` over SpillFile; growable backlog; recovery on boot | T1 |
| T5 | archive cap + disk spill | `ArchiveState` enum; soft/hard caps; SpillFile per request | T1, T4 |
| T6 | Regex cache | `ModelPermission` via shared `RegexCache` | T1 |

## §2 Per-Task Detailed Design

### T1 — `gw-runtime` primitives

**Supervisor** (`gw-runtime::supervisor`)
```rust
pub struct Supervisor { name: &'static str, policy: RestartPolicy, shutdown: ShutdownToken }
pub struct RestartPolicy { max_restarts: u32, window: Duration, backoff: ExponentialBackoff }
impl Supervisor {
    pub fn spawn<F, Fut>(self, factory: F) -> JoinHandle<()>
    where F: Fn(ShutdownToken) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = Result<(), SupervisorError>> + Send;
}
```
- Each `Err` or panic (via `catch_unwind`) triggers backoff restart.
- Exceeds `max_restarts/window` → mark unhealthy (affects `/readyz`), stop restarting.
- `ShutdownToken` cancelled → no more restarts; factory observes token to exit cleanly.
- Default policy: `max_restarts=5, window=60s, backoff=100ms..30s ×2 jitter ±20%`.

**ShutdownCoordinator** (`gw-runtime::shutdown`)
```rust
pub struct ShutdownCoordinator { token: CancellationToken, tasks: JoinSet<()>, phase: AtomicU8 }
impl ShutdownCoordinator {
    pub fn token(&self) -> ShutdownToken;
    pub fn register(&mut self, h: JoinHandle<()>);
    pub async fn drain(self, hard_deadline: Duration);
}
```
- Two-phase: phase 1 (accept-stop, default 5s) — stops accepting and triggers Pingora `shutdown_after`; phase 2 (task-drain, default 25s) — `JoinSet::join_next` until tasks exit; over `hard_deadline` → `abort_all` + log.
- `/readyz` reads `phase: AtomicU8` and returns 503 when `phase ≥ 1`.

**SpillFile** (`gw-runtime::spill_io`)
```rust
pub struct SpillFile {
    base_path: PathBuf,
    shards: Vec<Mutex<BufWriter<tokio::fs::File>>>,
    next_shard: AtomicUsize,
}
impl SpillFile {
    pub async fn create(root: &SpillRoot, key: &str, shards: NonZeroUsize) -> io::Result<Self>;
    pub async fn append(&self, bytes: &[u8]) -> io::Result<()>;
    pub async fn read_all(&self) -> io::Result<Vec<Vec<u8>>>;
}
pub struct SpillRoot { root: PathBuf, max_total_bytes: u64 }
impl SpillRoot {
    pub async fn create(&self, key: &str, shards: NonZeroUsize) -> io::Result<SpillFile>;
    pub fn enforce_quota(self: Arc<Self>) -> JoinHandle<()>;  // background LRU cleanup (supervised)
}
```
- Multi-shard `Mutex<BufWriter<tokio::fs::File>>`; round-robin via `next_shard.fetch_add(Relaxed)`.
- Shard count configurable per call site: DLQ shared file uses `shards=4`; per-request archive uses `shards=1`.
- Lock held only for the syscall, never across business `.await`.
- Replay reads shards in order; intra-shard order preserved, inter-shard near-time ordering acceptable.

**RegexCache** (`gw-runtime::regex_cache`)
```rust
pub struct RegexCache { inner: moka::sync::Cache<Arc<str>, Arc<Regex>>, redos_budget: Duration }
impl RegexCache {
    pub fn get_or_compile(&self, pattern: &str) -> Result<Arc<Regex>, CompileError>;
}
```
- Default capacity 4096, TTI 1h. Compile errors not cached. Global singleton wired by `gw-gateway` bootstrap.

**Health hooks + monitor + gate** (`gw-runtime::health`)
```rust
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum HealthSignal { Healthy, Critical }   // no Warning state in decision path

pub trait HealthHook: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self) -> HealthSignal;
}

pub struct SystemHealthMonitor {
    hooks: Vec<Arc<dyn HealthHook>>,
    state: AtomicU8,
    last_change_ns: AtomicU64,
}
impl SystemHealthMonitor {
    pub fn evaluate(&self) -> HealthSignal { /* any Critical → Critical */ }
}

pub struct BackpressureGate { state: Arc<AtomicU8>, healthy_streak: AtomicU8 }
impl BackpressureGate {
    pub fn admit(&self) -> Admit;   // Pass | Reject
}
pub enum Admit { Pass, Reject }
```
- Background task (supervised) runs `evaluate` every 1s.
- Hysteresis: `Critical → Healthy` requires 3 consecutive `Healthy` ticks before gate releases.

### T2 — SIGTERM/SIGINT + ShutdownCoordinator wiring

- `gw-gateway/src/main.rs`: `tokio::signal::unix::{signal, SignalKind}` for SIGTERM and SIGINT → `ShutdownCoordinator::drain`.
- `gw-gateway/src/lib.rs`: inject `ShutdownToken` into Ledger flusher (drain pending batch + sync XADD), DetailedArchive (drain channel), Health server (200→503 switch), Stream consumer, egress-impersonate, TaskWorker.
- Pingora: `Server::shutdown_after(token)` continues processing established connections until they finish or hard-deadline expires.
- Defaults: `phase1=5s, phase2=25s, hard_deadline=35s, billing_drain=20s` (soft target, not hard).
- Double SIGTERM → immediate `abort_all` (emergency path).

### T3 — Supervisor application

Mechanical wrap of every long-running spawn site. Each task factory is rewritten as `Fn(ShutdownToken) -> Future<Output=Result<...>>`, with `tokio::select! { _ = token.cancelled() => break, ... }` for clean exit.

| Crate | File | spawn count |
|-------|------|-------------|
| gw-ledger | flusher.rs | 1 |
| gw-ledger | inflight_reaper.rs | 1 (continuous scan loop; startup reconciler stays one-shot) |
| gw-task-worker | lib.rs | 5 |
| gw-gateway | lib.rs (health / stream consumer / DeductGc) | 3 |
| gw-egress-impersonate | supervisor.rs | delete local impl, re-export `gw-runtime::Supervisor` |

Ledger flusher's existing per-tick `catch_unwind` stays; outer Supervisor only handles task-level panic.

### T4 — DLQ async-ization + BillingBacklog

**Billing write path**:
```
emit(UsageRecord)
  ├──> primary:   Redis XADD usage:records
  │     fail ↓
  ├──> secondary: SpillFile::append (sharded, async)
  │     fail ↓
  └──> tertiary:  BillingBacklog (growable VecDeque, in-memory)
        background retry worker (Supervisor) drains back to Redis/disk
```

**BillingBacklog** (`crates/gw-ledger/src/backlog.rs`):
```rust
pub struct BillingBacklog {
    queue: tokio::sync::Mutex<VecDeque<DlqEnvelope>>,
    pending_count: AtomicUsize,
    pending_bytes: AtomicU64,
    oldest_at: AtomicU64,
}
```
- VecDeque grows on demand; **never drops** an envelope.
- Backlog "overload" is not a parameter; it manifests only when the underlying resource (memory) is near exhaustion, caught by `MemoryPressureHook`.
- Background retry worker (Supervisor) tries Redis first, falls back to SpillFile; success → pop.

**Recovery**: bootstrap scans `spill_root` for `dlq.*.jsonl` and `backlog_shutdown_*.jsonl`, loads into BillingBacklog. Corrupt files are renamed to `corrupt/` with log; **no fail-fast**.

`EmergencyDlqWriter` API changes from sync `append` to `async append`; existing call sites at `gw-ledger/src/flusher.rs:515` get `.await`.

### T5 — archive cap + disk spill

`ReqCtx` (`gw-proxy/src/ctx.rs`) replaces `BytesMut` archive fields with:
```rust
enum ArchiveState {
    InMemory(BytesMut),
    Spilled { file: SpillFile, written: u64 },
    Truncated { written: u64, dropped: u64 },
}
```

Thresholds (config `[proxy.archive]`):
- `soft_cap_bytes = 4 MiB` → on first overflow, create `SpillFile` (shards=1) and flush in-memory bytes into it; state → `Spilled`.
- `hard_cap_bytes = 8 MiB` → close SpillFile; subsequent chunks only increment `dropped`; state → `Truncated`.

`response_body_filter`:
- Same-protocol pass-through is unchanged (archive is a side-channel; never blocks the response).
- Cross-render path: archive is decoupled from rendering. Rendering runs the IR stream live, archive flows into `ArchiveState`.

`logging.rs::emit_response_surfaces`:
- `InMemory` → bytes as today.
- `Spilled` → one-shot `SpillFile::read_all` (via `spawn_blocking` if needed).
- `Truncated` → attach `truncated=true, dropped_bytes=N` metadata; emit what was captured; **usage:records is unaffected (must-deliver invariant preserved)**.

All spill failures emit `tracing::warn!` with `request_id`, `surface`, `bytes_written`, `bytes_dropped`, `error`. Truncation alone does not warn (expected behavior; metric only).

### T6 — `ModelPermission` regex cache

- `ModelPermission::new(pattern, effect)` takes `&RegexCache`; internally calls `get_or_compile(pattern)` and stores `Arc<Regex>`.
- Call site `gw-proxy/src/filters/request.rs:1651` retrieves `Arc<RegexCache>` from filter ctx (injected at bootstrap).
- ReDoS budget check stays (post-hoc observation + auto-disable), but compilation happens once per unique pattern.

## §3 Configuration & Defaults

Adds to `config/default.toml`:

```toml
[runtime.supervisor]
default_max_restarts = 5
default_window_ms = 60000
default_backoff_initial_ms = 100
default_backoff_max_ms = 30000
default_backoff_multiplier = 2.0
default_backoff_jitter = 0.2

[runtime.shutdown]
phase1_accept_stop_ms = 5000
phase2_task_drain_ms = 25000
hard_deadline_ms = 35000
billing_drain_ms = 20000

[runtime.spill_io]
root_dir = "/data/cnexus/spill"
max_total_bytes = 10737418240        # 10 GiB total quota (LRU cleanup)
buffer_size = 65536                   # BufWriter capacity per shard
dlq_shards = 4

[runtime.regex_cache]
capacity = 4096
tti_secs = 3600

[runtime.health]
evaluate_interval_ms = 1000
hysteresis_healthy_ticks = 3

[proxy.archive]
soft_cap_bytes = 4194304             # 4 MiB → spill
hard_cap_bytes = 8388608             # 8 MiB → truncate
spill_dir = "/data/cnexus/spill/archive"
```

Bootstrap wiring:
1. Read config → construct `SpillRoot`, `RegexCache`, `ShutdownCoordinator`, `SystemHealthMonitor`, `BackpressureGate`.
2. Inject `Arc<RegexCache>` and `Arc<BackpressureGate>` into `gw-auth` and `gw-proxy` filter ctx.
3. Inject `ShutdownToken` + default `RestartPolicy` into every Supervisor creation site.
4. Main thread `tokio::signal` awaits → triggers `ShutdownCoordinator::drain`.
5. Drain returns → exit (code 0 if clean, 1 if hard-deadline forced, 2 if backlog dump on shutdown failed).

If `spill_io.root_dir` does not exist, bootstrap creates it. Permission failure → fail-fast (surface ops issues early at pre-launch).

`/readyz` behavior:
- 503 when `ShutdownCoordinator::phase ≥ 1`.
- 503 when any Supervisor is marked unhealthy.
- 503 when `BackpressureGate::admit() == Reject`.

`/livez` stays 200 throughout drain (until `hard_deadline_ms` triggers process exit).

## §4 Error Handling & Failure Modes

### Elasticity tiers

| Tier | Role | Examples |
|------|------|----------|
| **L1 Internal buffer** | Self-heal silently | `BillingBacklog` VecDeque auto-grow; Redis temp fail → SpillFile; SpillFile shard sharding; Supervisor panic restart; Moka cache; regex cache |
| **L2 Observation only** | Metrics/alerts; no admission impact | Restart rate, upstream latency, backlog size, Redis fail rate |
| **L3 Force-majeure shedding** | Reject new traffic to save existing work | Memory near OOM, FD near ulimit, spill disk near full, Redis OOM |

Only L3 triggers `BackpressureGate`. L1 always heals; L2 never affects request path.

### Health hooks (final set)

Exactly 4 hooks, each Critical only when "one step from explosion":

| Hook | Source | Critical |
|------|--------|----------|
| `MemoryPressureHook` | `/proc/self/status` RSS or cgroup `memory.current` vs limit | ≥ **99%** |
| `FdPressureHook` | `getrlimit(RLIMIT_NOFILE)` vs `/proc/self/fd` count | ≥ **99%** |
| `SpillDiskFullHook` | `statfs(spill_root)` used/total ratio | ≥ **99%** |
| `RedisOomHook` | fred client returns `OOM command not allowed when used memory > 'maxmemory'` | any OOM error within a 5 s sliding window (self-clears when window empties) |

**Explicitly excluded** (not Critical signals):
- Generic Redis failure rate → degrades to SpillFile, self-heals.
- BillingBacklog age/growth → symptom; if it actually threatens memory, `MemoryPressureHook` catches it.
- Supervisor restart storm → restarts don't directly harm resources; if they consume memory/fd, corresponding hook catches it.
- Spill disk write latency / partial fill → degrades but doesn't fail.

### Aggregation rule

```rust
impl SystemHealthMonitor {
    pub fn evaluate(&self) -> HealthSignal {
        for h in &self.hooks {
            if h.check() == HealthSignal::Critical { return HealthSignal::Critical; }
        }
        HealthSignal::Healthy
    }
}
```

Any one Critical → BackpressureGate activates. Hysteresis: 3 consecutive Healthy ticks (3s) to release.

### Client-facing response under backpressure

`gw-proxy/src/filters/early.rs`:
```rust
if let Admit::Reject = ctx.backpressure_gate.admit() {
    return Err(GatewayError::ServiceUnavailable);   // reuse existing error
}
```

- HTTP 503
- Body: `{"error":{"type":"service_unavailable","message":"Service temporarily unavailable. Please retry later."}}`
- `Retry-After: 2`
- No header/body leakage of internal cause.
- Internal `tracing::warn!` logs which hook tripped, for ops.

### Per-task failure matrix

| Stage | Failure | Behavior |
|-------|---------|----------|
| Redis XADD | temporary fail | → SpillFile; `RedisOomHook` only triggers on specific OOM error |
| SpillFile::append | fail | → BillingBacklog (in-memory growable) |
| BillingBacklog | grows | auto-expand VecDeque; **never drops** |
| SpillDiskFullHook | Critical (disk ≥ 99%) | BackpressureGate ON → 503 + Retry-After:2 |
| MemoryPressureHook | Critical (≥99%) | BackpressureGate ON → 503 |
| FdPressureHook | Critical (≥99%) | BackpressureGate ON → 503 |
| RedisOomHook | Critical (OOM in 5s window) | BackpressureGate ON → 503 |
| Hook clears | 3s healthy streak | BackpressureGate OFF; traffic resumes |
| Supervisor task panic | — | catch_unwind → backoff restart |
| Supervisor restart storm | exceeds policy | task marked unhealthy → `/readyz` 503 |
| Drain shutdown disk fails | — | dump backlog to `spill_root/backlog_shutdown_<ts>.jsonl`; if dump fails → fatal log + exit 2 |
| Bootstrap backlog file corrupt | — | rename to `corrupt/`; log; **continue startup** |
| Archive spill creation fails | — | state → Truncated; `tracing::warn!`; client gets normal response |
| Archive spill append fails | — | state → Truncated; `tracing::warn!`; client unaffected |
| Archive emit read fails | — | detailed log marks `archive_unavailable=true`; usage:records unaffected |
| Regex compile fail | — | not cached; ModelPermission falls back to current deny + auto-disable |

### Billing must-deliver invariant

**Hard rule**: no UsageRecord may be silently dropped. Drop is only acceptable when:
- `BackpressureGate` already rejected the request before it generated work (no UsageRecord exists to drop).
- Process is forcibly killed (SIGKILL / OOM kill) **after** in-memory backlog accumulated — and even then, anything flushed to disk is recovered on next boot.

In normal operation, every accepted request produces exactly one UsageRecord that lands in Redis eventually (possibly after disk + memory layers).

### New metrics

```
# System health
gw_health_state                                    gauge 0 healthy / 1 critical
gw_health_hook_critical{hook}                      gauge 0/1
gw_health_hook_transition_total{hook,to}           counter
gw_backpressure_active                             gauge 0/1
gw_backpressure_rejects_total                      counter
gw_memory_usage_ratio                              gauge
gw_fd_usage_ratio                                  gauge
gw_spill_disk_free_bytes                           gauge
gw_redis_oom_errors_total                          counter

# Runtime primitives
gw_supervisor_restart_total{task,reason}           counter
gw_supervisor_unhealthy{task}                      gauge
gw_shutdown_phase                                  gauge 0/1/2
gw_shutdown_forced_total                           counter
gw_spill_append_fail_total{site}                   counter
gw_spill_disk_full_total{site}                     counter
gw_spill_bytes_total{site}                         counter
gw_spill_write_latency_seconds{site}               histogram
gw_spill_quota_enforce_fail_total                  counter
gw_regex_cache_hit_total                           counter
gw_regex_cache_miss_total                          counter
gw_regex_cache_compile_fail_total                  counter

# Billing observability (not driving decisions)
gw_billing_backlog_pending_count                   gauge
gw_billing_backlog_oldest_age_seconds              gauge
gw_billing_recovery_loaded_total                   counter
gw_billing_recovery_corrupt_files_total            counter
gw_dlq_envelopes_pending                           gauge
gw_archive_spilled_total                           counter
gw_archive_truncated_total                         counter
gw_archive_disk_full_total                         counter
```

## §5 Testing & Verification

### Testing pyramid

```
┌──────────────────────────┐
│  Integration (--ignored) │   testcontainers Redis+PG; shutdown drain;
│                          │   Supervisor restart; BillingBacklog recovery
├──────────────────────────┤
│  Unit                    │   per-crate fast tests
└──────────────────────────┘
```

- No new soak scenarios (current soak has already drifted from system).
- No Loom in CI (runner can't sustain it).
- `make e2e-soak-smoke` stays as a local convenience; not a CI gate.

### Unit tests

**`gw-runtime`**:
- `supervisor`: restarts_on_panic, respects_max_restarts, shutdown_stops_restarts, backoff_with_jitter
- `shutdown`: two_phase_drain, hard_deadline_aborts, double_signal_emergency
- `spill_io`: sharded_append_round_robin, append_after_disk_full, read_all_preserves_shard_order, quota_enforcer_lru_cleanup
- `regex_cache`: hit_skips_recompile, compile_error_not_cached, tti_eviction
- `health`: each hook mock-tested for Critical threshold (99% / 5s window); `SystemHealthMonitor::evaluate` aggregation; `BackpressureGate` hysteresis (3 healthy ticks to release); `admit` Pass/Reject

### Integration tests (`--ignored`, testcontainers)

**Shutdown (`crates/gw-gateway/tests/shutdown.rs`)**:
- sigterm_drains_in_flight_streams
- sigterm_drains_billing_pipeline
- readyz_flips_503_immediately
- livez_remains_200_until_hard_deadline
- hard_deadline_forces_exit
- double_sigterm_aborts_immediately

**Supervisor application**:
- ledger_flusher_restarts_on_panic
- inflight_reaper_survives_redis_blip
- task_worker_loops_supervised
- health_server_resilient
- supervisor_marks_unhealthy_when_thrashing

**DLQ + BillingBacklog**:
- dlq_append_async
- dlq_sharded_concurrent_writes
- dlq_replay_after_restart
- dlq_corrupt_file_rename_continues
- dlq_disk_full_falls_back_to_memory_and_trips_hook

**Archive**:
- archive_under_soft_cap_stays_in_memory
- archive_soft_cap_triggers_spill
- archive_hard_cap_triggers_truncate
- archive_spill_create_failure_truncates
- archive_emit_reads_spill_file
- archive_spill_cleanup_on_request_end
- archive_truncated_metadata_propagates

**Regex cache**:
- regex_cache_hit_no_recompile
- regex_cache_concurrent_compile_collapsed
- regex_cache_redos_budget_still_triggers
- model_permission_check_with_cache

### CI gates (unchanged set)

- `cargo test --workspace`
- `cargo test --workspace -- --ignored`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `bash scripts/check-invariants.sh` (extended with `gw-runtime` bottom-layer rule)

## §6 Documentation Sync (per CLAUDE.md sync rule)

Each commit touching the relevant boundary updates the matching doc in the same commit:

- **`docs/architecture.md`**: §Workspace layout adds `gw-runtime`; §Configuration adds `[runtime.*]` and `[proxy.archive]` blocks; §Invariants adds Supervisor / BackpressureGate / health-hook invariants; §Deliberate behaviours adds "503 body never reveals internal cause".
- **`docs/workflow.md`**: `early.rs` adds BackpressureGate check; shutdown drain sequence (phase 1/2/hard-deadline) documented.
- **`docs/spec/06-billing-ledger.md`**: DLQ rewritten over `SpillFile`; BillingBacklog as final fallback; must-deliver contract reaffirmed.
- **`docs/spec/09-tunnel.md`** and other specs: unchanged.
- **`docs/go-contracts/`**: unchanged (Redis stream payload, counter keys, encrypted columns, KEK rotation — all untouched).
- **`docs/ROADMAP.md`**: remove items implemented by this work.
- **`scripts/check-invariants.sh`**: extend with `gw-runtime` allowed-bottom-layer; ensure no upper crate exports its own Supervisor.
- **`CLAUDE.md` ⇄ `AGENTS.md`**: kept byte-identical except title.

## Open / Deferred

None. The 6 fixes are scoped, the platform crate is defined, the failure model is decided, the test surface is enumerated.

## Appendix — Out-of-Scope (deliberate)

- `LatencyTracker::Mutex<VecDeque>` lock contention — flagged in the audit as a contention hotspot, but not in this plan's ROI window.
- `EntityStore` per-(user, api-key) merged-permission cache — flagged as a CPU win, but orthogonal to resilience.
- `GLOBAL_STREAM_CAP` / `PER_KEY_INFLIGHT_CAP` hardcoded — pre-launch; revisit once load profile is real.
- egress impersonate UDS `max_sessions=64` and `ClientPool::Mutex<HashMap>` — same reason.
- New cross-protocol streaming event mappings (Anthropic/Gemini → OpenAI Chat losing `ThinkingDelta` / `ToolCallStart`) — separate codec design effort.
