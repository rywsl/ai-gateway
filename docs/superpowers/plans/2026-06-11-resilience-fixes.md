# Resilience Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a shared `gw-runtime` crate that provides Supervisor, ShutdownCoordinator, sharded SpillFile, RegexCache, and a force-majeure BackpressureGate; then apply those primitives to fix billing data loss, blocking I/O, unbounded archive memory, regex re-compilation, and naked `tokio::spawn` across the gateway.

**Architecture:** Platform-first (Approach B). Sequential delivery: T1 builds primitives, T2 wires SIGTERM/SIGINT + ShutdownCoordinator, T3 wraps every long-running spawn site in Supervisor, T4 makes DLQ async over SpillFile and adds an unbounded BillingBacklog, T5 caps archive memory with disk spill, T6 caches compiled regex. The L1/L2/L3 elasticity model (self-heal → observe → shed) keeps internal buffers from ever dropping billing data; only true force-majeure (memory/fd/disk ≥99%, Redis OOM) triggers an opaque 503.

**Tech Stack:** Rust 1.88, tokio, Pingora 0.8, fred (Redis), sqlx (Postgres), moka, arc-swap, parking_lot, tracing, metrics. New crate `gw-runtime` at workspace bottom.

**Reference spec:** `docs/superpowers/specs/2026-06-11-resilience-fixes-design.md`

---

## File Structure

**New files** (created during this plan):

```
crates/gw-runtime/
├── Cargo.toml
├── src/
│   ├── lib.rs                     module re-exports
│   ├── supervisor.rs              Supervisor, RestartPolicy, ShutdownToken, SupervisorError
│   ├── shutdown.rs                ShutdownCoordinator, two-phase drain
│   ├── spill_io.rs                SpillRoot, SpillFile (sharded), quota enforcer
│   ├── regex_cache.rs             RegexCache (Moka-backed)
│   └── health/
│       ├── mod.rs                 HealthHook trait, HealthSignal, SystemHealthMonitor, BackpressureGate
│       ├── memory.rs              MemoryPressureHook
│       ├── fd.rs                  FdPressureHook
│       ├── disk.rs                SpillDiskFullHook
│       └── redis_oom.rs           RedisOomHook
└── tests/
    ├── supervisor_integration.rs
    ├── shutdown_integration.rs
    └── spill_io_integration.rs

crates/gw-ledger/src/backlog.rs    BillingBacklog (new module)
```

**Modified files**:

```
Cargo.toml                                  add gw-runtime to workspace members
config/default.toml                         add [runtime.*] + [proxy.archive] blocks
scripts/check-invariants.sh                 allow gw-runtime as bottom dep

crates/gw-egress-impersonate/
└── src/supervisor.rs                       delete impl; re-export gw_runtime::Supervisor

crates/gw-ledger/
├── Cargo.toml                              + gw-runtime
├── src/lib.rs                              + backlog module
├── src/dlq.rs                              EmergencyDlqWriter -> SpillFile (async)
├── src/flusher.rs                          cascade Redis -> SpillFile -> BillingBacklog;
│                                           wrap spawns in Supervisor
└── src/inflight_reaper.rs                  wrap spawn in Supervisor

crates/gw-task-worker/
├── Cargo.toml                              + gw-runtime
└── src/lib.rs                              wrap 5 core loops + health server in Supervisor

crates/gw-proxy/
├── Cargo.toml                              + gw-runtime
├── src/ctx.rs                              archive fields -> ArchiveState enum
├── src/filters/early.rs                    + BackpressureGate admit check
├── src/filters/request.rs:1651             ModelPermission via RegexCache
├── src/filters/response.rs                 archive flow into ArchiveState + spill
└── src/filters/logging.rs                  emit_response_surfaces reads ArchiveState

crates/gw-gateway/
├── Cargo.toml                              + gw-runtime
├── src/main.rs                             SIGTERM/SIGINT handler -> drain
└── src/lib.rs                              bootstrap: SpillRoot, RegexCache,
                                            ShutdownCoordinator, SystemHealthMonitor,
                                            BackpressureGate; wrap all spawns

docs/architecture.md                        + gw-runtime in workspace layout
docs/workflow.md                            + early.rs backpressure + shutdown sequence
docs/spec/06-billing-ledger.md              DLQ cascade -> SpillFile -> BillingBacklog
docs/ROADMAP.md                             remove implemented items
CLAUDE.md / AGENTS.md                       mirror any agent-facing changes
```

---

## Task 1.1: Create `gw-runtime` crate skeleton

**Files:**
- Create: `crates/gw-runtime/Cargo.toml`
- Create: `crates/gw-runtime/src/lib.rs`
- Modify: `Cargo.toml` (root workspace members)
- Modify: `scripts/check-invariants.sh` (allow `gw-runtime` as bottom layer)

- [ ] **Step 1: Add new crate to workspace**

Open `Cargo.toml` (root), find the `members = [` block, and append `"crates/gw-runtime",` (preserve trailing-comma style). After the edit the array contains `gw-runtime` as the **last** crate before the e2e crates.

- [ ] **Step 2: Create `crates/gw-runtime/Cargo.toml`**

```toml
[package]
name = "gw-runtime"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Shared runtime primitives: Supervisor, ShutdownCoordinator, SpillFile, RegexCache, health hooks."

[dependencies]
tokio = { workspace = true, features = ["full"] }
tokio-util = { workspace = true, features = ["rt"] }
tracing = "0.1"
metrics = "0.23"
moka = { workspace = true }
arc-swap = { workspace = true }
parking_lot = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
futures = { workspace = true }
regex = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["full", "test-util"] }
tempfile = "3"
```

> If any workspace dep listed above isn't declared in the root `Cargo.toml`, copy its existing version from another crate that uses it (e.g. `gw-egress-impersonate` already has `moka`, `parking_lot`, `dashmap`, `metrics`).

- [ ] **Step 3: Create `crates/gw-runtime/src/lib.rs`**

```rust
//! Shared runtime primitives for the gateway.
//!
//! Modules:
//! - [`supervisor`] — bounded restart for long-running tasks.
//! - [`shutdown`] — two-phase drain coordinator.
//! - [`spill_io`] — async sharded append-only files for DLQ / archive overflow.
//! - [`regex_cache`] — Moka-backed compiled-regex cache.
//! - [`health`] — `HealthHook` trait, `SystemHealthMonitor`, `BackpressureGate`.

pub mod health;
pub mod regex_cache;
pub mod shutdown;
pub mod spill_io;
pub mod supervisor;

pub use shutdown::{ShutdownCoordinator, ShutdownToken};
pub use supervisor::{RestartPolicy, Supervisor, SupervisorError};
```

(Stub child modules now so the crate compiles before later steps fill them in.)

- [ ] **Step 4: Create stub module files**

```rust
// crates/gw-runtime/src/supervisor.rs
//! Bounded-restart supervisor — filled in by task 1.2.

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("task returned error: {0}")]
    Task(#[from] anyhow::Error),
}

pub struct RestartPolicy;
pub struct Supervisor;
```

```rust
// crates/gw-runtime/src/shutdown.rs
//! ShutdownCoordinator — filled in by task 1.3.

pub struct ShutdownCoordinator;

#[derive(Clone)]
pub struct ShutdownToken;
```

```rust
// crates/gw-runtime/src/spill_io.rs
//! SpillFile primitives — filled in by task 1.4.
```

```rust
// crates/gw-runtime/src/regex_cache.rs
//! RegexCache — filled in by task 1.5.
```

```rust
// crates/gw-runtime/src/health/mod.rs
//! Health hooks + monitor + backpressure gate — filled in by tasks 1.6 / 1.7.
```

```rust
// crates/gw-runtime/src/health/memory.rs   (empty stub)
// crates/gw-runtime/src/health/fd.rs       (empty stub)
// crates/gw-runtime/src/health/disk.rs     (empty stub)
// crates/gw-runtime/src/health/redis_oom.rs(empty stub)
```

Each of the four hook files is just an empty `//! ...` doc-comment line; they will be populated in task 1.7.

- [ ] **Step 5: Update `scripts/check-invariants.sh`**

Find the section listing allowed bottom-layer crates (search for `gw-core`). Add `gw-runtime` in the same set, and add a positive lint that rejects any upper crate importing `gw_egress_impersonate::supervisor::supervise` after task 3 lands (T3 deletes the local impl). Concretely append:

```bash
# gw-runtime is a bottom-layer primitive crate; nothing upper may re-export
# its own Supervisor.
if grep -RInE 'pub use .*::supervisor::Supervisor' crates/ \
    --include='*.rs' \
    --exclude-dir=gw-runtime; then
    echo "INVARIANT FAIL: Supervisor must come from gw_runtime; remove the re-export above."
    exit 1
fi
```

- [ ] **Step 6: Compile to confirm the skeleton is valid**

Run: `cargo check -p gw-runtime`
Expected: builds clean (no warnings other than unused-imports from stubs are tolerated; we'll fill them in next tasks).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/gw-runtime/ scripts/check-invariants.sh
git commit -m "feat(gw-runtime): scaffold runtime primitives crate"
```

---

## Task 1.2: Supervisor — bounded restart loop

**Files:**
- Modify: `crates/gw-runtime/src/supervisor.rs`
- Modify: `crates/gw-runtime/src/lib.rs` (re-exports already declared)
- Test: `crates/gw-runtime/tests/supervisor_integration.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/gw-runtime/tests/supervisor_integration.rs`:

```rust
use gw_runtime::supervisor::{RestartPolicy, Supervisor};
use gw_runtime::shutdown::ShutdownToken;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

fn fast_policy() -> RestartPolicy {
    RestartPolicy {
        max_restarts: 3,
        window: Duration::from_secs(1),
        backoff_initial: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        backoff_multiplier: 2.0,
        backoff_jitter: 0.0,
    }
}

#[tokio::test]
async fn restarts_on_returned_error() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("test", fast_policy(), token.clone())
        .spawn(move |tok| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(anyhow::anyhow!("boom"))
                } else {
                    // Stay alive until shutdown.
                    tok.cancelled().await;
                    Ok(())
                }
            }
        });

    tokio::time::sleep(Duration::from_millis(300)).await;
    token.cancel();
    handle.await.unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn restarts_on_panic() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("panicker", fast_policy(), token.clone())
        .spawn(move |tok| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    panic!("crash");
                }
                tok.cancelled().await;
                Ok(())
            }
        });

    tokio::time::sleep(Duration::from_millis(200)).await;
    token.cancel();
    handle.await.unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn marks_unhealthy_after_max_restarts() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let policy = RestartPolicy {
        max_restarts: 2,
        window: Duration::from_secs(10),
        ..fast_policy()
    };
    let c = counter.clone();
    let sup = Supervisor::new("flaky", policy, token.clone());
    let unhealthy = sup.unhealthy_flag();
    let handle = sup.spawn(move |_| {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("always fail"))
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.await.unwrap();
    assert!(unhealthy.load(Ordering::SeqCst));
    // Exactly max_restarts + 1 = 3 runs before giving up.
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn shutdown_stops_restarts() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("test", fast_policy(), token.clone())
        .spawn(move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("err"))
            }
        });

    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();
    handle.await.unwrap();
    let final_count = counter.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(counter.load(Ordering::SeqCst), final_count, "no restart after shutdown");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gw-runtime --test supervisor_integration`
Expected: compile error (`RestartPolicy` fields / `Supervisor::new` / `spawn` / `ShutdownToken::new` not defined).

- [ ] **Step 3: Implement `ShutdownToken` skeleton needed by Supervisor**

Replace `crates/gw-runtime/src/shutdown.rs` with the minimal pieces required now (full ShutdownCoordinator lands in 1.3):

```rust
//! ShutdownToken — full ShutdownCoordinator is in task 1.3; this module
//! provides the cancellation primitive Supervisor depends on.

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct ShutdownToken {
    inner: CancellationToken,
}

impl ShutdownToken {
    pub fn new() -> Self {
        Self { inner: CancellationToken::new() }
    }

    pub fn child(&self) -> Self {
        Self { inner: self.inner.child_token() }
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

// ShutdownCoordinator stub — populated in task 1.3.
pub struct ShutdownCoordinator;
```

- [ ] **Step 4: Implement `Supervisor` and `RestartPolicy`**

Replace `crates/gw-runtime/src/supervisor.rs`:

```rust
//! Bounded-restart supervisor for long-running tasks.
//!
//! Wraps a task factory in a restart loop:
//! - Each invocation catches `Err` and `panic` (via `catch_unwind`).
//! - Backoff between restarts is `backoff_initial`, scaled by
//!   `backoff_multiplier` up to `backoff_max`, with `backoff_jitter`
//!   applied symmetrically.
//! - If more than `max_restarts` failures occur within `window`, the task
//!   is marked unhealthy and the loop exits.
//! - `ShutdownToken` cancellation breaks the loop immediately.

use crate::shutdown::ShutdownToken;
use futures::FutureExt;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("task returned error: {0}")]
    Task(#[from] anyhow::Error),
}

#[derive(Clone, Debug)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub window: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    pub backoff_multiplier: f64,
    pub backoff_jitter: f64, // 0.0 .. 1.0
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(60),
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            backoff_jitter: 0.2,
        }
    }
}

pub struct Supervisor {
    name: &'static str,
    policy: RestartPolicy,
    token: ShutdownToken,
    unhealthy: Arc<AtomicBool>,
}

impl Supervisor {
    pub fn new(name: &'static str, policy: RestartPolicy, token: ShutdownToken) -> Self {
        Self { name, policy, token, unhealthy: Arc::new(AtomicBool::new(false)) }
    }

    pub fn unhealthy_flag(&self) -> Arc<AtomicBool> {
        self.unhealthy.clone()
    }

    pub fn spawn<F, Fut>(self, factory: F) -> JoinHandle<()>
    where
        F: Fn(ShutdownToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
    {
        let Self { name, policy, token, unhealthy } = self;
        tokio::spawn(async move {
            let mut failures: VecDeque<Instant> = VecDeque::new();
            let mut backoff = policy.backoff_initial;

            loop {
                if token.is_cancelled() {
                    tracing::info!(target: "gw_runtime::supervisor", task = name, "shutdown requested");
                    break;
                }

                let child_token = token.child();
                let fut = factory(child_token);
                let result = AssertUnwindSafe(fut).catch_unwind().await;

                let outcome = match result {
                    Ok(Ok(())) => {
                        tracing::info!(target: "gw_runtime::supervisor", task = name, "task exited cleanly");
                        if token.is_cancelled() {
                            break;
                        }
                        "ok"
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(target: "gw_runtime::supervisor", task = name, error = %err, "task returned error");
                        "error"
                    }
                    Err(panic) => {
                        let payload = panic_payload_string(&panic);
                        tracing::error!(target: "gw_runtime::supervisor", task = name, panic = %payload, "task panicked");
                        "panic"
                    }
                };

                metrics::counter!("gw_supervisor_restart_total", "task" => name, "reason" => outcome).increment(1);

                let now = Instant::now();
                failures.push_back(now);
                while let Some(front) = failures.front() {
                    if now.duration_since(*front) > policy.window {
                        failures.pop_front();
                    } else {
                        break;
                    }
                }

                if failures.len() as u32 > policy.max_restarts {
                    tracing::error!(
                        target: "gw_runtime::supervisor",
                        task = name,
                        failures = failures.len(),
                        "exceeded restart budget; marking unhealthy and giving up"
                    );
                    unhealthy.store(true, Ordering::SeqCst);
                    metrics::gauge!("gw_supervisor_unhealthy", "task" => name).set(1.0);
                    break;
                }

                // Apply backoff with token-aware sleep.
                let delay = apply_jitter(backoff, policy.backoff_jitter);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = token.cancelled() => break,
                }
                backoff = next_backoff(backoff, &policy);
            }
        })
    }
}

fn next_backoff(current: Duration, policy: &RestartPolicy) -> Duration {
    let next = current.mul_f64(policy.backoff_multiplier);
    if next > policy.backoff_max { policy.backoff_max } else { next }
}

fn apply_jitter(d: Duration, jitter: f64) -> Duration {
    if jitter <= 0.0 {
        return d;
    }
    // Cheap deterministic-ish jitter: use nanoseconds of `Instant::now` as
    // a pseudo-random source; we don't need cryptographic quality here.
    let nanos = Instant::now().elapsed().as_nanos() as u64;
    let frac = (nanos % 1_000) as f64 / 1_000.0; // 0.0..1.0
    let signed = (frac - 0.5) * 2.0 * jitter;    // -jitter..+jitter
    let scaled = d.as_secs_f64() * (1.0 + signed);
    Duration::from_secs_f64(scaled.max(0.0))
}

fn panic_payload_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p gw-runtime --test supervisor_integration`
Expected: all 4 tests PASS.

- [ ] **Step 6: Lint clean**

Run: `cargo clippy -p gw-runtime --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-runtime/src/supervisor.rs crates/gw-runtime/src/shutdown.rs crates/gw-runtime/tests/supervisor_integration.rs
git commit -m "feat(gw-runtime): Supervisor with bounded-restart policy"
```

---

## Task 1.3: ShutdownCoordinator — two-phase drain

**Files:**
- Modify: `crates/gw-runtime/src/shutdown.rs` (replace stub)
- Test: `crates/gw-runtime/tests/shutdown_integration.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/gw-runtime/tests/shutdown_integration.rs`:

```rust
use gw_runtime::shutdown::{ShutdownCoordinator, ShutdownPhase};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn two_phase_drain_completes_clean() {
    let mut coord = ShutdownCoordinator::new();
    let token = coord.token();
    let done = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let tok = token.clone();
        let d = done.clone();
        coord.register(tokio::spawn(async move {
            tok.cancelled().await;
            // Small work to simulate cleanup.
            tokio::time::sleep(Duration::from_millis(20)).await;
            d.fetch_add(1, Ordering::SeqCst);
        }));
    }

    let result = coord.drain(
        Duration::from_millis(50),
        Duration::from_millis(500),
        Duration::from_secs(2),
    ).await;

    assert_eq!(done.load(Ordering::SeqCst), 3);
    assert!(matches!(result, gw_runtime::shutdown::DrainOutcome::Clean));
}

#[tokio::test]
async fn phase_transitions_observable() {
    let mut coord = ShutdownCoordinator::new();
    let token = coord.token();
    let phase_handle = coord.phase_handle();

    coord.register(tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }));

    assert_eq!(phase_handle.current(), ShutdownPhase::Running);
    let drain = tokio::spawn(async move {
        coord.drain(
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(500),
        ).await
    });

    // After a moment, we should be in Phase1 (accept-stop).
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(phase_handle.current() >= ShutdownPhase::Phase1AcceptStop);

    drain.await.unwrap();
    assert_eq!(phase_handle.current(), ShutdownPhase::Stopped);
}

#[tokio::test]
async fn hard_deadline_aborts_hung_task() {
    let mut coord = ShutdownCoordinator::new();
    let _token = coord.token();

    // A task that ignores cancellation and hangs.
    coord.register(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }));

    let outcome = coord.drain(
        Duration::from_millis(20),
        Duration::from_millis(100),
        Duration::from_millis(200),
    ).await;

    assert!(matches!(outcome, gw_runtime::shutdown::DrainOutcome::Forced));
}

#[tokio::test]
async fn second_drain_call_emergency_aborts() {
    let mut coord = ShutdownCoordinator::new();
    let _token = coord.token();

    coord.register(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }));

    // Trigger immediate emergency abort without waiting through phases.
    coord.emergency_abort();
    // Emergency abort sets phase to Stopped synchronously; outstanding
    // tasks are aborted.
    assert_eq!(coord.phase_handle().current(), gw_runtime::shutdown::ShutdownPhase::Stopped);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gw-runtime --test shutdown_integration`
Expected: compile failure — `ShutdownCoordinator::new` and friends do not exist.

- [ ] **Step 3: Implement `ShutdownCoordinator`**

Replace `crates/gw-runtime/src/shutdown.rs`:

```rust
//! Two-phase shutdown coordinator.
//!
//! Phase 0 (Running) → Phase 1 (AcceptStop) → Phase 2 (TaskDrain) → Stopped.
//!
//! Phase 1 flips `/readyz` to 503 so external load balancers (k8s) stop
//! sending new traffic. Pingora `shutdown_after` should be wired to the
//! same token so it stops accepting new connections.
//!
//! Phase 2 waits for every `register`-ed JoinHandle to complete. After
//! `hard_deadline` it `abort()`s outstanding tasks and returns `Forced`.

use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ShutdownPhase {
    Running = 0,
    Phase1AcceptStop = 1,
    Phase2TaskDrain = 2,
    Stopped = 3,
}

impl ShutdownPhase {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Running,
            1 => Self::Phase1AcceptStop,
            2 => Self::Phase2TaskDrain,
            _ => Self::Stopped,
        }
    }
}

#[derive(Clone, Default)]
pub struct ShutdownToken {
    inner: CancellationToken,
}

impl ShutdownToken {
    pub fn new() -> Self {
        Self { inner: CancellationToken::new() }
    }
    pub fn child(&self) -> Self {
        Self { inner: self.inner.child_token() }
    }
    pub fn cancel(&self) { self.inner.cancel(); }
    pub fn is_cancelled(&self) -> bool { self.inner.is_cancelled() }
    pub async fn cancelled(&self) { self.inner.cancelled().await; }
}

#[derive(Clone)]
pub struct PhaseHandle {
    state: Arc<AtomicU8>,
}

impl PhaseHandle {
    pub fn current(&self) -> ShutdownPhase {
        ShutdownPhase::from_u8(self.state.load(Ordering::Acquire))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    Clean,
    Forced,
}

pub struct ShutdownCoordinator {
    token: ShutdownToken,
    handles: Vec<JoinHandle<()>>,
    phase: Arc<AtomicU8>,
    // Hold an ArcSwap for additional shutdown listeners that need a clone
    // of the handle but can also be appended after construction.
    _phase_swap: Arc<ArcSwap<ShutdownPhase>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            token: ShutdownToken::new(),
            handles: Vec::new(),
            phase: Arc::new(AtomicU8::new(ShutdownPhase::Running as u8)),
            _phase_swap: Arc::new(ArcSwap::from_pointee(ShutdownPhase::Running)),
        }
    }

    pub fn token(&self) -> ShutdownToken { self.token.clone() }

    pub fn phase_handle(&self) -> PhaseHandle {
        PhaseHandle { state: self.phase.clone() }
    }

    pub fn register(&mut self, h: JoinHandle<()>) {
        self.handles.push(h);
    }

    pub async fn drain(
        mut self,
        phase1: Duration,
        phase2: Duration,
        hard_deadline: Duration,
    ) -> DrainOutcome {
        let start = Instant::now();

        // Phase 1 — stop accepting new work.
        self.phase.store(ShutdownPhase::Phase1AcceptStop as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(1.0);
        self.token.cancel();
        tokio::time::sleep(phase1).await;

        // Phase 2 — wait for registered tasks to exit.
        self.phase.store(ShutdownPhase::Phase2TaskDrain as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(2.0);

        let phase2_deadline = Instant::now() + phase2;
        for h in std::mem::take(&mut self.handles) {
            let remaining = phase2_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { h.abort(); continue; }
            if let Err(_) = tokio::time::timeout(remaining, h).await {
                // exceeded phase2 budget
            }
        }

        // Determine if we are still within the hard deadline.
        let hard_left = hard_deadline.saturating_sub(start.elapsed());
        let mut outcome = DrainOutcome::Clean;
        if hard_left.is_zero() {
            metrics::counter!("gw_shutdown_forced_total").increment(1);
            outcome = DrainOutcome::Forced;
        }

        self.phase.store(ShutdownPhase::Stopped as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(3.0);
        outcome
    }

    /// Emergency: skip phases, abort everything, set Stopped immediately.
    pub fn emergency_abort(&mut self) {
        self.token.cancel();
        for h in std::mem::take(&mut self.handles) { h.abort(); }
        self.phase.store(ShutdownPhase::Stopped as u8, Ordering::Release);
        metrics::counter!("gw_shutdown_forced_total").increment(1);
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 4: Update `crates/gw-runtime/src/lib.rs` exports**

Ensure `lib.rs` exports the new types:

```rust
pub use shutdown::{DrainOutcome, PhaseHandle, ShutdownCoordinator, ShutdownPhase, ShutdownToken};
pub use supervisor::{RestartPolicy, Supervisor, SupervisorError};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p gw-runtime --test shutdown_integration`
Expected: 4 tests PASS.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p gw-runtime --all-targets -- -D warnings
git add crates/gw-runtime/src/shutdown.rs crates/gw-runtime/src/lib.rs crates/gw-runtime/tests/shutdown_integration.rs
git commit -m "feat(gw-runtime): ShutdownCoordinator with two-phase drain"
```

---

## Task 1.4: SpillFile — sharded async append-only storage

**Files:**
- Modify: `crates/gw-runtime/src/spill_io.rs`
- Modify: `crates/gw-runtime/src/lib.rs` (add re-exports)
- Test: `crates/gw-runtime/tests/spill_io_integration.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/gw-runtime/tests/spill_io_integration.rs`:

```rust
use gw_runtime::spill_io::{SpillRoot, SpillRootConfig};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tempfile::TempDir;

fn root(tmp: &TempDir) -> Arc<SpillRoot> {
    Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 16 * 1024 * 1024,
        buffer_size: 4096,
    }))
}

#[tokio::test]
async fn single_shard_append_and_read_back() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root.create("test", NonZeroUsize::new(1).unwrap()).await.unwrap();
    file.append(b"hello\n").await.unwrap();
    file.append(b"world\n").await.unwrap();
    file.flush().await.unwrap();
    let all = file.read_all().await.unwrap();
    let concat: Vec<u8> = all.into_iter().flatten().collect();
    assert_eq!(concat, b"hello\nworld\n");
}

#[tokio::test]
async fn sharded_round_robin_distributes_writes() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root.create("test", NonZeroUsize::new(4).unwrap()).await.unwrap();
    for i in 0..40u32 {
        file.append(format!("{i}\n").as_bytes()).await.unwrap();
    }
    file.flush().await.unwrap();
    // 4 shards * 10 lines each
    let shards = file.read_all().await.unwrap();
    assert_eq!(shards.len(), 4);
    for s in &shards {
        let lines = String::from_utf8_lossy(s).lines().count();
        assert_eq!(lines, 10, "expected 10 lines per shard, got {lines}");
    }
}

#[tokio::test]
async fn concurrent_appends_no_data_loss() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = Arc::new(root.create("concurrent", NonZeroUsize::new(4).unwrap()).await.unwrap());

    let mut joins = Vec::new();
    for w in 0..16 {
        let f = file.clone();
        joins.push(tokio::spawn(async move {
            for i in 0..100u32 {
                f.append(format!("{w}-{i}\n").as_bytes()).await.unwrap();
            }
        }));
    }
    for j in joins { j.await.unwrap(); }
    file.flush().await.unwrap();

    let total_lines: usize = file.read_all().await.unwrap()
        .iter()
        .map(|s| String::from_utf8_lossy(s).lines().count())
        .sum();
    assert_eq!(total_lines, 16 * 100);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p gw-runtime --test spill_io_integration`
Expected: compile errors — `SpillRoot::new` / `create` / `SpillFile::append` undefined.

- [ ] **Step 3: Implement SpillRoot / SpillFile**

Replace `crates/gw-runtime/src/spill_io.rs`:

```rust
//! Sharded async append-only spill files.
//!
//! Each `SpillFile` is backed by N shard files (`<base>.0.jsonl`,
//! `<base>.1.jsonl`, ...). Concurrent appends pick a shard via round-robin
//! `AtomicUsize::fetch_add(Relaxed)` and serialise into that shard's
//! `tokio::sync::Mutex<BufWriter<tokio::fs::File>>`. Locks are held only
//! across the syscall, never across business `.await`.

use std::io::{self};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct SpillRootConfig {
    pub root_dir: PathBuf,
    pub max_total_bytes: u64,
    pub buffer_size: usize,
}

impl Default for SpillRootConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("/data/cnexus/spill"),
            max_total_bytes: 10 * 1024 * 1024 * 1024, // 10 GiB
            buffer_size: 64 * 1024,
        }
    }
}

pub struct SpillRoot {
    config: SpillRootConfig,
}

impl SpillRoot {
    pub fn new(config: SpillRootConfig) -> Self { Self { config } }

    pub fn root_dir(&self) -> &Path { &self.config.root_dir }

    pub async fn ensure_dir(&self) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.config.root_dir).await
    }

    pub async fn create(&self, key: &str, shards: NonZeroUsize) -> io::Result<SpillFile> {
        self.ensure_dir().await?;
        let shard_n = shards.get();
        let mut shard_files = Vec::with_capacity(shard_n);
        let mut shard_paths = Vec::with_capacity(shard_n);
        for i in 0..shard_n {
            let path = self.config.root_dir.join(format!("{key}.{i}.jsonl"));
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&path)
                .await?;
            shard_files.push(Mutex::new(BufWriter::with_capacity(self.config.buffer_size, file)));
            shard_paths.push(path);
        }
        Ok(SpillFile {
            base_key: key.to_string(),
            shard_paths,
            shards: shard_files,
            next_shard: AtomicUsize::new(0),
        })
    }

    /// Background LRU enforcer task that prunes oldest files until total
    /// size fits below `max_total_bytes`. Returns its JoinHandle so the
    /// caller can register it with the ShutdownCoordinator / Supervisor.
    pub fn spawn_quota_enforcer(self: Arc<Self>, interval: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                if let Err(e) = self.enforce_quota_once().await {
                    metrics::counter!("gw_spill_quota_enforce_fail_total").increment(1);
                    tracing::warn!(target: "gw_runtime::spill_io", error = %e, "quota enforce iteration failed");
                }
            }
        })
    }

    async fn enforce_quota_once(&self) -> io::Result<()> {
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&self.config.root_dir).await?;
        let mut total: u64 = 0;
        while let Some(ent) = dir.next_entry().await? {
            let meta = match ent.metadata().await { Ok(m) => m, Err(_) => continue };
            if !meta.is_file() { continue; }
            total += meta.len();
            let mtime = meta.modified().ok();
            entries.push((mtime, ent.path(), meta.len()));
        }
        if total <= self.config.max_total_bytes { return Ok(()); }
        entries.sort_by_key(|(t, _, _)| *t);
        let mut removed: u64 = 0;
        for (_, p, len) in entries {
            if total - removed <= self.config.max_total_bytes { break; }
            if tokio::fs::remove_file(&p).await.is_ok() {
                removed += len;
            }
        }
        Ok(())
    }
}

pub struct SpillFile {
    base_key: String,
    shard_paths: Vec<PathBuf>,
    shards: Vec<Mutex<BufWriter<File>>>,
    next_shard: AtomicUsize,
}

impl SpillFile {
    pub fn key(&self) -> &str { &self.base_key }
    pub fn shard_count(&self) -> usize { self.shards.len() }
    pub fn shard_paths(&self) -> &[PathBuf] { &self.shard_paths }

    pub async fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let n = self.shards.len();
        let idx = self.next_shard.fetch_add(1, Ordering::Relaxed) % n;
        let mut guard = self.shards[idx].lock().await;
        guard.write_all(bytes).await?;
        metrics::counter!("gw_spill_bytes_total", "site" => self.base_key.clone())
            .increment(bytes.len() as u64);
        Ok(())
    }

    pub async fn flush(&self) -> io::Result<()> {
        for s in &self.shards {
            let mut g = s.lock().await;
            g.flush().await?;
        }
        Ok(())
    }

    pub async fn fsync(&self) -> io::Result<()> {
        for s in &self.shards {
            let mut g = s.lock().await;
            g.flush().await?;
            g.get_mut().sync_all().await?;
        }
        Ok(())
    }

    pub async fn read_all(&self) -> io::Result<Vec<Vec<u8>>> {
        // Flush before read so buffered data is visible.
        for s in &self.shards {
            let mut g = s.lock().await;
            g.flush().await?;
        }
        let mut out = Vec::with_capacity(self.shards.len());
        for path in &self.shard_paths {
            let mut f = File::open(path).await?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).await?;
            out.push(buf);
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Update `lib.rs` re-exports**

```rust
pub use spill_io::{SpillFile, SpillRoot, SpillRootConfig};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p gw-runtime --test spill_io_integration`
Expected: 3 tests PASS.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p gw-runtime --all-targets -- -D warnings
git add crates/gw-runtime/src/spill_io.rs crates/gw-runtime/src/lib.rs crates/gw-runtime/tests/spill_io_integration.rs
git commit -m "feat(gw-runtime): sharded async SpillFile + quota enforcer"
```

---

## Task 1.5: RegexCache — Moka-backed compiled-regex cache

**Files:**
- Modify: `crates/gw-runtime/src/regex_cache.rs`
- Modify: `crates/gw-runtime/src/lib.rs`

- [ ] **Step 1: Write inline unit tests**

Replace `crates/gw-runtime/src/regex_cache.rs`:

```rust
//! Compiled-regex cache. Moka `sync::Cache` storing `Arc<Regex>` keyed by
//! `Arc<str>` pattern. Compile errors are NOT cached.

use moka::sync::Cache;
use regex::Regex;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("regex compile failed: {0}")]
    Regex(#[from] regex::Error),
}

pub struct RegexCacheConfig {
    pub capacity: u64,
    pub tti: Duration,
}

impl Default for RegexCacheConfig {
    fn default() -> Self {
        Self { capacity: 4096, tti: Duration::from_secs(3600) }
    }
}

pub struct RegexCache {
    inner: Cache<Arc<str>, Arc<Regex>>,
}

impl RegexCache {
    pub fn new(cfg: RegexCacheConfig) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(cfg.capacity)
                .time_to_idle(cfg.tti)
                .build(),
        }
    }

    pub fn get_or_compile(&self, pattern: &str) -> Result<Arc<Regex>, CompileError> {
        let key: Arc<str> = Arc::from(pattern);
        if let Some(re) = self.inner.get(&key) {
            metrics::counter!("gw_regex_cache_hit_total").increment(1);
            return Ok(re);
        }
        metrics::counter!("gw_regex_cache_miss_total").increment(1);
        match Regex::new(pattern) {
            Ok(re) => {
                let arc = Arc::new(re);
                self.inner.insert(key, arc.clone());
                Ok(arc)
            }
            Err(e) => {
                metrics::counter!("gw_regex_cache_compile_fail_total").increment(1);
                Err(e.into())
            }
        }
    }

    pub fn len(&self) -> u64 { self.inner.entry_count() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_returns_same_arc() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        let a = cache.get_or_compile("^foo$").unwrap();
        let b = cache.get_or_compile("^foo$").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn compile_error_not_cached() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        assert!(cache.get_or_compile("[invalid").is_err());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn distinct_patterns_distinct_arcs() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        let a = cache.get_or_compile("^foo$").unwrap();
        let b = cache.get_or_compile("^bar$").unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }
}
```

- [ ] **Step 2: Update `lib.rs` re-exports**

```rust
pub use regex_cache::{CompileError, RegexCache, RegexCacheConfig};
```

- [ ] **Step 3: Run and verify**

Run: `cargo test -p gw-runtime regex_cache`
Expected: 3 tests PASS.

- [ ] **Step 4: Lint and commit**

```bash
cargo clippy -p gw-runtime --all-targets -- -D warnings
git add crates/gw-runtime/src/regex_cache.rs crates/gw-runtime/src/lib.rs
git commit -m "feat(gw-runtime): Moka-backed RegexCache"
```

---

## Task 1.6: Health framework — HealthHook / SystemHealthMonitor / BackpressureGate

**Files:**
- Modify: `crates/gw-runtime/src/health/mod.rs`
- Modify: `crates/gw-runtime/src/lib.rs`

- [ ] **Step 1: Write inline unit tests in the new module**

Replace `crates/gw-runtime/src/health/mod.rs`:

```rust
//! Health hooks + system monitor + backpressure gate.
//!
//! Design (see `docs/superpowers/specs/2026-06-11-resilience-fixes-design.md`):
//! - L1 buffers self-heal silently.
//! - L2 metrics observe.
//! - L3 force-majeure hooks (this module) flip a global BackpressureGate.

pub mod disk;
pub mod fd;
pub mod memory;
pub mod redis_oom;

use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HealthSignal { Healthy, Critical }

pub trait HealthHook: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self) -> HealthSignal;
}

pub struct SystemHealthMonitor {
    hooks: Vec<Arc<dyn HealthHook>>,
    state: Arc<AtomicU8>,           // 0 healthy / 1 critical
    healthy_streak: Arc<AtomicU8>,  // consecutive healthy ticks
    hysteresis_ticks: u8,
}

impl SystemHealthMonitor {
    pub fn new(hysteresis_ticks: u8) -> Self {
        Self {
            hooks: Vec::new(),
            state: Arc::new(AtomicU8::new(0)),
            healthy_streak: Arc::new(AtomicU8::new(hysteresis_ticks)),
            hysteresis_ticks,
        }
    }

    pub fn add_hook<H: HealthHook>(&mut self, h: H) -> &mut Self {
        self.hooks.push(Arc::new(h)); self
    }

    pub fn state_handle(&self) -> Arc<AtomicU8> { self.state.clone() }

    pub fn evaluate(&self) -> HealthSignal {
        let mut any_crit = None;
        for h in &self.hooks {
            if h.check() == HealthSignal::Critical {
                metrics::gauge!("gw_health_hook_critical", "hook" => h.name()).set(1.0);
                any_crit = Some(h.name());
            } else {
                metrics::gauge!("gw_health_hook_critical", "hook" => h.name()).set(0.0);
            }
        }
        let was_critical = self.state.load(Ordering::Acquire) == 1;
        if let Some(name) = any_crit {
            if !was_critical {
                tracing::warn!(target: "gw_runtime::health", hook = name, "system entered critical state");
                metrics::counter!("gw_health_hook_transition_total", "hook" => name, "to" => "critical").increment(1);
            }
            self.state.store(1, Ordering::Release);
            self.healthy_streak.store(0, Ordering::Release);
            metrics::gauge!("gw_health_state").set(1.0);
            HealthSignal::Critical
        } else {
            let streak = self.healthy_streak.fetch_add(1, Ordering::AcqRel).saturating_add(1);
            if was_critical && streak >= self.hysteresis_ticks {
                tracing::info!(target: "gw_runtime::health", "system recovered to healthy");
                metrics::counter!("gw_health_hook_transition_total", "hook" => "_aggregate", "to" => "healthy").increment(1);
                self.state.store(0, Ordering::Release);
            }
            metrics::gauge!("gw_health_state").set(self.state.load(Ordering::Acquire) as f64);
            HealthSignal::Healthy
        }
    }

    /// Spawn the periodic evaluator loop. Returns the JoinHandle to register
    /// with ShutdownCoordinator; callers can wrap this in `Supervisor::spawn`
    /// for restart-on-panic instead of relying on the bare task.
    pub fn into_loop(self, interval: Duration) -> JoinHandle<()> {
        let monitor = Arc::new(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                monitor.evaluate();
            }
        })
    }
}

#[derive(Clone)]
pub struct BackpressureGate {
    state: Arc<AtomicU8>,
}

pub enum Admit { Pass, Reject }

impl BackpressureGate {
    pub fn from_state(state: Arc<AtomicU8>) -> Self { Self { state } }

    pub fn admit(&self) -> Admit {
        match self.state.load(Ordering::Acquire) {
            1 => {
                metrics::counter!("gw_backpressure_rejects_total").increment(1);
                metrics::gauge!("gw_backpressure_active").set(1.0);
                Admit::Reject
            }
            _ => {
                metrics::gauge!("gw_backpressure_active").set(0.0);
                Admit::Pass
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysCritical;
    impl HealthHook for AlwaysCritical {
        fn name(&self) -> &'static str { "always_crit" }
        fn check(&self) -> HealthSignal { HealthSignal::Critical }
    }
    struct AlwaysHealthy;
    impl HealthHook for AlwaysHealthy {
        fn name(&self) -> &'static str { "always_ok" }
        fn check(&self) -> HealthSignal { HealthSignal::Healthy }
    }

    #[test]
    fn any_critical_makes_aggregate_critical() {
        let mut m = SystemHealthMonitor::new(3);
        m.add_hook(AlwaysHealthy).add_hook(AlwaysCritical);
        assert_eq!(m.evaluate(), HealthSignal::Critical);
        let gate = BackpressureGate::from_state(m.state_handle());
        assert!(matches!(gate.admit(), Admit::Reject));
    }

    #[test]
    fn hysteresis_requires_consecutive_healthy_ticks() {
        let mut m = SystemHealthMonitor::new(3);
        m.add_hook(AlwaysCritical);
        m.evaluate(); // critical
        assert_eq!(m.state_handle().load(Ordering::Acquire), 1);

        // Remove the critical hook and add a healthy one.
        let mut m2 = SystemHealthMonitor::new(3);
        m2.add_hook(AlwaysHealthy);
        // Force state into critical to start with.
        m2.state_handle().store(1, Ordering::Release);
        m2.healthy_streak.store(0, Ordering::Release);
        assert_eq!(m2.evaluate(), HealthSignal::Healthy);
        assert_eq!(m2.state_handle().load(Ordering::Acquire), 1, "still critical after 1 tick");
        m2.evaluate();
        assert_eq!(m2.state_handle().load(Ordering::Acquire), 1, "still critical after 2 ticks");
        m2.evaluate();
        assert_eq!(m2.state_handle().load(Ordering::Acquire), 0, "healthy after 3 ticks");
    }
}
```

- [ ] **Step 2: Update `lib.rs` re-exports**

```rust
pub use health::{Admit, BackpressureGate, HealthHook, HealthSignal, SystemHealthMonitor};
```

- [ ] **Step 3: Run and verify**

Run: `cargo test -p gw-runtime health::tests`
Expected: 2 tests PASS.

- [ ] **Step 4: Lint and commit**

```bash
cargo clippy -p gw-runtime --all-targets -- -D warnings
git add crates/gw-runtime/src/health/mod.rs crates/gw-runtime/src/lib.rs
git commit -m "feat(gw-runtime): health monitor + backpressure gate skeleton"
```

---

## Task 1.7: Concrete health hooks — Memory / FD / Disk / Redis OOM

**Files:**
- Modify: `crates/gw-runtime/src/health/memory.rs`
- Modify: `crates/gw-runtime/src/health/fd.rs`
- Modify: `crates/gw-runtime/src/health/disk.rs`
- Modify: `crates/gw-runtime/src/health/redis_oom.rs`

> Each hook reports `Critical` only when "one step from explosion" (99% / OOM-class signal). Hooks are I/O-light — they read pseudo-filesystem files on each `check()`. If a read fails, the hook returns `Healthy` (refusal to invent risk) and emits a metric.

- [ ] **Step 1: Implement `MemoryPressureHook`**

`crates/gw-runtime/src/health/memory.rs`:

```rust
//! MemoryPressureHook — cgroup v2 first, fall back to RSS / RLIMIT_AS.

use super::{HealthHook, HealthSignal};
use std::path::PathBuf;

pub struct MemoryPressureHook {
    cgroup_current: PathBuf,
    cgroup_max: PathBuf,
    threshold_ratio: f64, // 0.99 = 99%
}

impl MemoryPressureHook {
    pub fn new() -> Self {
        Self {
            cgroup_current: PathBuf::from("/sys/fs/cgroup/memory.current"),
            cgroup_max: PathBuf::from("/sys/fs/cgroup/memory.max"),
            threshold_ratio: 0.99,
        }
    }

    pub fn with_paths(current: PathBuf, max: PathBuf, threshold_ratio: f64) -> Self {
        Self { cgroup_current: current, cgroup_max: max, threshold_ratio }
    }

    fn read_u64(path: &PathBuf) -> Option<u64> {
        let s = std::fs::read_to_string(path).ok()?;
        let trimmed = s.trim();
        if trimmed == "max" { return Some(u64::MAX); }
        trimmed.parse::<u64>().ok()
    }
}

impl HealthHook for MemoryPressureHook {
    fn name(&self) -> &'static str { "memory" }
    fn check(&self) -> HealthSignal {
        let current = match Self::read_u64(&self.cgroup_current) { Some(v) => v, None => return HealthSignal::Healthy };
        let max = match Self::read_u64(&self.cgroup_max) { Some(v) => v, None => return HealthSignal::Healthy };
        if max == u64::MAX || max == 0 { return HealthSignal::Healthy; }
        let ratio = current as f64 / max as f64;
        metrics::gauge!("gw_memory_usage_ratio").set(ratio);
        if ratio >= self.threshold_ratio { HealthSignal::Critical } else { HealthSignal::Healthy }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &PathBuf, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn under_threshold_healthy() {
        let tmp = TempDir::new().unwrap();
        let cur = tmp.path().join("current");
        let max = tmp.path().join("max");
        write(&cur, "100"); write(&max, "1000");
        let hook = MemoryPressureHook::with_paths(cur, max, 0.99);
        assert_eq!(hook.check(), HealthSignal::Healthy);
    }

    #[test]
    fn over_threshold_critical() {
        let tmp = TempDir::new().unwrap();
        let cur = tmp.path().join("current");
        let max = tmp.path().join("max");
        write(&cur, "995"); write(&max, "1000");
        let hook = MemoryPressureHook::with_paths(cur, max, 0.99);
        assert_eq!(hook.check(), HealthSignal::Critical);
    }
}
```

- [ ] **Step 2: Implement `FdPressureHook`**

`crates/gw-runtime/src/health/fd.rs`:

```rust
//! FdPressureHook — Linux only; reads /proc/self/limits and counts /proc/self/fd.

use super::{HealthHook, HealthSignal};

pub struct FdPressureHook {
    fd_dir: std::path::PathBuf,
    limit_path: std::path::PathBuf,
    threshold_ratio: f64,
}

impl FdPressureHook {
    pub fn new() -> Self {
        Self {
            fd_dir: std::path::PathBuf::from("/proc/self/fd"),
            limit_path: std::path::PathBuf::from("/proc/self/limits"),
            threshold_ratio: 0.99,
        }
    }

    pub fn with_paths(fd_dir: std::path::PathBuf, limit_path: std::path::PathBuf, threshold_ratio: f64) -> Self {
        Self { fd_dir, limit_path, threshold_ratio }
    }

    fn read_limit(&self) -> Option<u64> {
        let s = std::fs::read_to_string(&self.limit_path).ok()?;
        // Match a line like: "Max open files            1024                 1024                 files"
        for line in s.lines() {
            if line.starts_with("Max open files") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                // [..., "Max", "open", "files", soft, hard, unit]; the soft limit is what
                // setrlimit enforces. Pick the soft limit (parts[3]).
                if let Some(soft) = parts.get(3) {
                    if *soft == "unlimited" { return Some(u64::MAX); }
                    return soft.parse::<u64>().ok();
                }
            }
        }
        None
    }

    fn count_open(&self) -> Option<u64> {
        let mut n: u64 = 0;
        let rd = std::fs::read_dir(&self.fd_dir).ok()?;
        for ent in rd { if ent.is_ok() { n += 1; } }
        Some(n)
    }
}

impl HealthHook for FdPressureHook {
    fn name(&self) -> &'static str { "fd" }
    fn check(&self) -> HealthSignal {
        let open = match self.count_open() { Some(v) => v, None => return HealthSignal::Healthy };
        let limit = match self.read_limit() { Some(v) if v != u64::MAX => v, _ => return HealthSignal::Healthy };
        if limit == 0 { return HealthSignal::Healthy; }
        let ratio = open as f64 / limit as f64;
        metrics::gauge!("gw_fd_usage_ratio").set(ratio);
        if ratio >= self.threshold_ratio { HealthSignal::Critical } else { HealthSignal::Healthy }
    }
}
```

- [ ] **Step 3: Implement `SpillDiskFullHook`**

`crates/gw-runtime/src/health/disk.rs`:

```rust
//! SpillDiskFullHook — Linux only; uses libc::statvfs.

use super::{HealthHook, HealthSignal};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

pub struct SpillDiskFullHook {
    path: PathBuf,
    threshold_used_ratio: f64, // 0.99 = 99%
}

impl SpillDiskFullHook {
    pub fn new(path: PathBuf) -> Self { Self { path, threshold_used_ratio: 0.99 } }
    pub fn with_threshold(path: PathBuf, ratio: f64) -> Self { Self { path, threshold_used_ratio: ratio } }

    fn statvfs(&self) -> Option<(u64, u64)> {
        let c = CString::new(self.path.as_os_str().as_bytes()).ok()?;
        // SAFETY: zero-initialised struct passed to libc::statvfs.
        let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: c is a valid C string for the duration of the call.
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut buf) };
        if rc != 0 { return None; }
        let total = (buf.f_blocks as u64) * (buf.f_frsize as u64);
        let free  = (buf.f_bavail as u64) * (buf.f_frsize as u64);
        Some((total, free))
    }
}

impl HealthHook for SpillDiskFullHook {
    fn name(&self) -> &'static str { "spill_disk" }
    fn check(&self) -> HealthSignal {
        let (total, free) = match self.statvfs() { Some(v) => v, None => return HealthSignal::Healthy };
        if total == 0 { return HealthSignal::Healthy; }
        let used_ratio = 1.0 - (free as f64 / total as f64);
        metrics::gauge!("gw_spill_disk_free_bytes").set(free as f64);
        if used_ratio >= self.threshold_used_ratio { HealthSignal::Critical } else { HealthSignal::Healthy }
    }
}
```

Add `libc = "0.2"` to `gw-runtime/Cargo.toml` `[dependencies]` (it's already in the workspace; copy the version from another crate that uses it).

- [ ] **Step 4: Implement `RedisOomHook`**

`crates/gw-runtime/src/health/redis_oom.rs`:

```rust
//! RedisOomHook — rolling 5 s window of explicit Redis OOM errors.
//!
//! Callers (gw-store / gw-ledger fred wrappers) MUST invoke `record()` whenever
//! a fred error string matches "OOM command not allowed when used memory".

use super::{HealthHook, HealthSignal};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct RedisOomHook {
    inner: Arc<Mutex<VecDeque<Instant>>>,
    window: Duration,
}

impl RedisOomHook {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(VecDeque::new())), window: Duration::from_secs(5) }
    }
    pub fn handle(&self) -> RedisOomReporter {
        RedisOomReporter { inner: self.inner.clone(), window: self.window }
    }
}

#[derive(Clone)]
pub struct RedisOomReporter {
    inner: Arc<Mutex<VecDeque<Instant>>>,
    window: Duration,
}

impl RedisOomReporter {
    pub fn record(&self) {
        let now = Instant::now();
        let mut g = self.inner.lock();
        g.push_back(now);
        // Trim while we hold the lock.
        while let Some(front) = g.front() {
            if now.duration_since(*front) > self.window { g.pop_front(); } else { break; }
        }
        metrics::counter!("gw_redis_oom_errors_total").increment(1);
    }
}

impl HealthHook for RedisOomHook {
    fn name(&self) -> &'static str { "redis_oom" }
    fn check(&self) -> HealthSignal {
        let now = Instant::now();
        let mut g = self.inner.lock();
        while let Some(front) = g.front() {
            if now.duration_since(*front) > self.window { g.pop_front(); } else { break; }
        }
        if g.is_empty() { HealthSignal::Healthy } else { HealthSignal::Critical }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_healthy() {
        let h = RedisOomHook::new();
        assert_eq!(h.check(), HealthSignal::Healthy);
    }

    #[test]
    fn record_triggers_critical_then_self_clears() {
        let h = RedisOomHook { inner: Arc::new(Mutex::new(VecDeque::new())), window: Duration::from_millis(50) };
        let r = h.handle();
        r.record();
        assert_eq!(h.check(), HealthSignal::Critical);
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(h.check(), HealthSignal::Healthy);
    }
}
```

- [ ] **Step 5: Compile, test, lint**

```bash
cargo test -p gw-runtime
cargo clippy -p gw-runtime --all-targets -- -D warnings
```

Expected: all tests across the module pass; no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-runtime/src/health/ crates/gw-runtime/Cargo.toml
git commit -m "feat(gw-runtime): force-majeure health hooks (memory/fd/disk/redis_oom)"
```

---

## Task 2.1: Wire ShutdownCoordinator + SystemHealthMonitor into bootstrap

**Files:**
- Modify: `crates/gw-gateway/Cargo.toml` (add `gw-runtime`)
- Modify: `crates/gw-gateway/src/lib.rs`

> Goal of T2: only stand up the shutdown infrastructure and BackpressureGate plumbing. Application of the gate to `early.rs` and adoption by long-running tasks happens in T2.2 / T2.3 / T3.

- [ ] **Step 1: Add `gw-runtime` dependency**

Edit `crates/gw-gateway/Cargo.toml`. Under `[dependencies]` append:

```toml
gw-runtime = { path = "../gw-runtime" }
```

- [ ] **Step 2: Construct primitives in bootstrap**

Open `crates/gw-gateway/src/lib.rs`. Find the section that builds the runtime state (look for where stores/redis/etc. are constructed; near `pub fn run()` or `wire(...)`). Add the following block right after config is loaded, before any task is spawned:

```rust
use gw_runtime::{
    BackpressureGate, RegexCache, RegexCacheConfig, ShutdownCoordinator,
    SpillRoot, SpillRootConfig, SystemHealthMonitor,
};
use gw_runtime::health::{
    disk::SpillDiskFullHook, fd::FdPressureHook, memory::MemoryPressureHook,
    redis_oom::RedisOomHook,
};
use std::num::NonZeroUsize;
use std::sync::Arc;

let mut shutdown = ShutdownCoordinator::new();
let shutdown_token = shutdown.token();
let phase_handle = shutdown.phase_handle();

let spill_root = Arc::new(SpillRoot::new(SpillRootConfig {
    root_dir: cfg.runtime.spill_io.root_dir.clone(),
    max_total_bytes: cfg.runtime.spill_io.max_total_bytes,
    buffer_size: cfg.runtime.spill_io.buffer_size,
}));
spill_root.ensure_dir().await.expect("spill root must be writable");

let regex_cache = Arc::new(RegexCache::new(RegexCacheConfig {
    capacity: cfg.runtime.regex_cache.capacity,
    tti: std::time::Duration::from_secs(cfg.runtime.regex_cache.tti_secs),
}));

let redis_oom_hook = RedisOomHook::new();
let redis_oom_reporter = redis_oom_hook.handle();

let mut monitor = SystemHealthMonitor::new(cfg.runtime.health.hysteresis_healthy_ticks);
monitor
    .add_hook(MemoryPressureHook::new())
    .add_hook(FdPressureHook::new())
    .add_hook(SpillDiskFullHook::new(spill_root.root_dir().to_path_buf()))
    .add_hook(redis_oom_hook);

let backpressure_state = monitor.state_handle();
let backpressure_gate = BackpressureGate::from_state(backpressure_state.clone());
let monitor_handle = monitor.into_loop(std::time::Duration::from_millis(
    cfg.runtime.health.evaluate_interval_ms,
));
shutdown.register(monitor_handle);
```

- [ ] **Step 3: Add config struct fields**

In the same file (or wherever the `Config` struct is defined — search for `struct Config` or `runtime` in `gw-gateway/src/config.rs` / `gw-core/src/config.rs`), add the new sections. The figment loader will pick them up from `config/default.toml`. Use this Rust block as the template:

```rust
#[derive(Debug, serde::Deserialize)]
pub struct RuntimeConfig {
    pub supervisor: SupervisorConfig,
    pub shutdown: ShutdownConfig,
    pub spill_io: SpillIoConfig,
    pub regex_cache: RegexCacheConfig,
    pub health: HealthConfig,
}

#[derive(Debug, serde::Deserialize)]
pub struct SupervisorConfig {
    pub default_max_restarts: u32,
    pub default_window_ms: u64,
    pub default_backoff_initial_ms: u64,
    pub default_backoff_max_ms: u64,
    pub default_backoff_multiplier: f64,
    pub default_backoff_jitter: f64,
}

#[derive(Debug, serde::Deserialize)]
pub struct ShutdownConfig {
    pub phase1_accept_stop_ms: u64,
    pub phase2_task_drain_ms: u64,
    pub hard_deadline_ms: u64,
    pub billing_drain_ms: u64,
}

#[derive(Debug, serde::Deserialize)]
pub struct SpillIoConfig {
    pub root_dir: std::path::PathBuf,
    pub max_total_bytes: u64,
    pub buffer_size: usize,
    pub dlq_shards: usize,
}

#[derive(Debug, serde::Deserialize)]
pub struct RegexCacheConfig {
    pub capacity: u64,
    pub tti_secs: u64,
}

#[derive(Debug, serde::Deserialize)]
pub struct HealthConfig {
    pub evaluate_interval_ms: u64,
    pub hysteresis_healthy_ticks: u8,
}

#[derive(Debug, serde::Deserialize)]
pub struct ProxyArchiveConfig {
    pub soft_cap_bytes: usize,
    pub hard_cap_bytes: usize,
    pub spill_dir: std::path::PathBuf,
}
```

Then add `pub runtime: RuntimeConfig,` to the top-level `Config` struct and `pub archive: ProxyArchiveConfig,` to the existing `proxy` config struct (or add a new `proxy` block if it doesn't have one).

- [ ] **Step 4: Update `config/default.toml`**

Append to `config/default.toml`:

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
max_total_bytes = 10737418240
buffer_size = 65536
dlq_shards = 4

[runtime.regex_cache]
capacity = 4096
tti_secs = 3600

[runtime.health]
evaluate_interval_ms = 1000
hysteresis_healthy_ticks = 3

[proxy.archive]
soft_cap_bytes = 4194304
hard_cap_bytes = 8388608
spill_dir = "/data/cnexus/spill/archive"
```

- [ ] **Step 5: Pass primitives through to wiring**

The bootstrap block from step 2 must hand off:
- `Arc<RegexCache>`, `Arc<BackpressureGate>`, `shutdown_token`, `phase_handle`, `Arc<SpillRoot>`, `redis_oom_reporter`

Add these as fields to the gateway's existing wiring/state struct (search for `struct Wired` in `lib.rs`). Wherever filters are constructed, thread them through. Example for the `Wired` struct:

```rust
pub struct Wired {
    // ... existing fields ...
    pub regex_cache: Arc<RegexCache>,
    pub backpressure_gate: Arc<BackpressureGate>,
    pub shutdown_token: gw_runtime::ShutdownToken,
    pub phase_handle: gw_runtime::PhaseHandle,
    pub spill_root: Arc<SpillRoot>,
    pub redis_oom_reporter: gw_runtime::health::redis_oom::RedisOomReporter,
}
```

- [ ] **Step 6: Compile**

Run: `cargo check -p gw-gateway`
Expected: builds clean. (Filter wiring for early.rs / request.rs / etc. comes in later tasks; for now only the bootstrap compiles.)

- [ ] **Step 7: Commit**

```bash
git add crates/gw-gateway/ config/default.toml
git commit -m "feat(gw-gateway): bootstrap ShutdownCoordinator + SystemHealthMonitor + BackpressureGate"
```

---

## Task 2.2: Install SIGTERM/SIGINT signal handler

**Files:**
- Modify: `crates/gw-gateway/src/main.rs`
- Modify: `crates/gw-gateway/src/lib.rs` (expose `run` that returns coordinator)

- [ ] **Step 1: Replace `main.rs` body**

`crates/gw-gateway/src/main.rs`:

```rust
//! gw-gateway binary entrypoint.
//!
//! Installs jemalloc, then runs the gateway with a SIGTERM/SIGINT-aware
//! tokio runtime that calls `ShutdownCoordinator::drain` on signal.

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    gw_gateway::run_with_signal_handler()
}
```

- [ ] **Step 2: Add `run_with_signal_handler` to `lib.rs`**

```rust
use tokio::signal::unix::{signal, SignalKind};

pub fn run_with_signal_handler() -> anyhow::Result<()> {
    // The existing `run()` builds the multi-threaded tokio runtime
    // internally for Pingora. We wrap it so the signal handler can
    // intercept SIGTERM/SIGINT and drive the ShutdownCoordinator.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let bootstrap = bootstrap_gateway().await?; // returns (Wired, ShutdownCoordinator)
        let (wired, mut coordinator) = bootstrap;

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut second_signal_count = 0u32;

        // Spawn the actual gateway loop (Pingora bind + serve).
        let serve_handle = tokio::spawn(serve_gateway(wired));
        coordinator.register(serve_handle);

        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    tracing::info!(target: "gw_gateway", "SIGTERM received");
                    second_signal_count += 1;
                    if second_signal_count >= 2 {
                        coordinator.emergency_abort();
                        break;
                    }
                    break;
                }
                _ = sigint.recv() => {
                    tracing::info!(target: "gw_gateway", "SIGINT received");
                    second_signal_count += 1;
                    if second_signal_count >= 2 {
                        coordinator.emergency_abort();
                        break;
                    }
                    break;
                }
            }
        }

        let cfg = current_config(); // helper that returns the cached Config
        let outcome = coordinator.drain(
            Duration::from_millis(cfg.runtime.shutdown.phase1_accept_stop_ms),
            Duration::from_millis(cfg.runtime.shutdown.phase2_task_drain_ms),
            Duration::from_millis(cfg.runtime.shutdown.hard_deadline_ms),
        ).await;

        match outcome {
            gw_runtime::DrainOutcome::Clean => Ok(()),
            gw_runtime::DrainOutcome::Forced => {
                std::process::exit(1);
            }
        }
    })
}
```

> Note: refactor the existing `run()` to expose two halves — `bootstrap_gateway()` returns `(Wired, ShutdownCoordinator)` after the bootstrap block from T2.1 runs; `serve_gateway(Wired)` owns the Pingora server start. Wire `Wired::shutdown_token` into the Pingora server via `Server::run_forever()` replacement (Pingora 0.8 doesn't expose a stable `shutdown_after` for embedded use — instead, spawn the Pingora server with `tokio::task::spawn_blocking` from `serve_gateway` and use a `tokio::select!` between server completion and shutdown_token, then cancel the blocking task on shutdown_token via `server.cancel()` if available, or rely on Pingora's own SIGINT handler).

If the existing `run()` cannot be cleanly split, an alternative is to spawn a dedicated thread for the Pingora server (mirroring the current "bootstrap independent thread" pattern in `gw-gateway/src/lib.rs:239`) and have signal handling live entirely in the main tokio runtime.

- [ ] **Step 3: Integration test for SIGTERM**

Create `crates/gw-gateway/tests/shutdown.rs`:

```rust
//! SIGTERM end-to-end: spawn a bootstrap, send SIGTERM via Coordinator's
//! token (no real signal), verify clean drain.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gw_runtime::{ShutdownCoordinator, Supervisor, RestartPolicy};

#[tokio::test]
async fn coordinator_drains_supervised_task() {
    let mut coord = ShutdownCoordinator::new();
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();
    let h = Supervisor::new("test", RestartPolicy::default(), coord.token())
        .spawn(move |tok| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                tok.cancelled().await;
                Ok(())
            }
        });
    coord.register(h);

    let outcome = coord.drain(
        Duration::from_millis(50),
        Duration::from_millis(500),
        Duration::from_secs(2),
    ).await;
    assert!(matches!(outcome, gw_runtime::DrainOutcome::Clean));
}
```

- [ ] **Step 4: Compile & test**

```bash
cargo check -p gw-gateway
cargo test -p gw-gateway --test shutdown
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-gateway/src/main.rs crates/gw-gateway/src/lib.rs crates/gw-gateway/tests/shutdown.rs
git commit -m "feat(gw-gateway): SIGTERM/SIGINT handler triggers ShutdownCoordinator drain"
```

---

## Task 2.3: Health endpoint reflects backpressure + shutdown phase

**Files:**
- Modify: `crates/gw-health/src/format_health.rs` or the gateway's `run_health_server` entry (search for `run_health_server`).
- Modify: `crates/gw-gateway/src/lib.rs` (pass state into health server)

- [ ] **Step 1: Add inputs to the health-server entry**

Currently `run_health_server(port, state, store, blacklist)`. Extend with `phase_handle` and `backpressure_state`:

```rust
pub async fn run_health_server(
    port: u16,
    state: HealthState,
    store: Arc<Store>,
    blacklist: Arc<MgmtBlacklist>,
    phase: gw_runtime::PhaseHandle,
    backpressure_state: Arc<std::sync::atomic::AtomicU8>,
    supervisor_unhealthy: Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> { /* existing routing + new logic below */ }
```

- [ ] **Step 2: `/readyz` returns 503 when any of these are true**

In the `/readyz` handler:

```rust
use gw_runtime::ShutdownPhase;
use std::sync::atomic::Ordering;

let phase = phase.current();
let bp = backpressure_state.load(Ordering::Acquire) == 1;
let unhealthy = supervisor_unhealthy.load(Ordering::Acquire);

if phase >= ShutdownPhase::Phase1AcceptStop || bp || unhealthy {
    return Response::builder().status(503).body(Body::empty()).unwrap();
}
```

`/livez` keeps its current behaviour — only returns 200/non-200 based on the listener being alive.

- [ ] **Step 3: Pass state at construction**

In `gw-gateway/src/lib.rs` where `run_health_server` is spawned (line ~905 today), include the new args. Coalesce all `Supervisor::unhealthy_flag()` outputs into one combined `AtomicBool` (set when any supervised task gives up). For now provide a single shared `supervisor_unhealthy = Arc::new(AtomicBool::new(false))`; T3 will OR-aggregate per-task flags into it.

- [ ] **Step 4: Compile and commit**

```bash
cargo check -p gw-gateway -p gw-health
git add crates/gw-health/ crates/gw-gateway/src/lib.rs
git commit -m "feat(health): /readyz reflects shutdown phase, backpressure, supervisor unhealthy"
```

---

## Task 3.1: Migrate `gw-egress-impersonate::supervise` to `gw_runtime::Supervisor`

**Files:**
- Modify: `crates/gw-egress-impersonate/src/supervisor.rs`
- Modify: `crates/gw-egress-impersonate/Cargo.toml` (add `gw-runtime`)
- Modify: `crates/gw-egress-impersonate/src/lib.rs` (re-export)
- Modify: every call site that constructed the local `RestartLimit` (search for `supervise(`)

- [ ] **Step 1: Replace local impl with thin wrapper**

`crates/gw-egress-impersonate/src/supervisor.rs`:

```rust
//! Thin compatibility wrapper around `gw_runtime::Supervisor`. Existing call
//! sites continue to call `supervise(bind, pool, shutdown_rx)`; internally
//! this now uses the shared Supervisor primitive.

use crate::handle::EgressBind;
use crate::pool::ClientPool;
use crate::server::Listener;
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::sync::Arc;

pub async fn supervise(
    bind: EgressBind,
    pool: Arc<ClientPool>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let token = ShutdownToken::new();
    let token_for_listener = token.clone();
    // Bridge the broadcast receiver to the new token.
    let token_for_bridge = token.clone();
    tokio::spawn(async move {
        let _ = shutdown_rx.recv().await;
        token_for_bridge.cancel();
    });

    let bind = Arc::new(bind);
    let pool_clone = pool.clone();
    let handle = Supervisor::new("egress_impersonate", RestartPolicy::default(), token_for_listener)
        .spawn(move |tok| {
            let bind = bind.clone();
            let pool = pool_clone.clone();
            async move {
                Listener::bind_and_serve((*bind).clone(), pool, tok).await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        });
    let _ = handle.await;
}
```

> If `Listener::bind_and_serve` does not currently take a `ShutdownToken`, adapt it to accept either a token or fall back to the broadcast receiver. The intent is one supervised task whose `Err`/panic restarts via `gw_runtime::Supervisor`.

- [ ] **Step 2: Add dependency and re-export**

In `crates/gw-egress-impersonate/Cargo.toml` `[dependencies]`:

```toml
gw-runtime = { path = "../gw-runtime" }
```

In `crates/gw-egress-impersonate/src/lib.rs`:

```rust
pub use gw_runtime::Supervisor;
```

- [ ] **Step 3: Compile**

Run: `cargo check -p gw-egress-impersonate -p gw-gateway`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-egress-impersonate/
git commit -m "refactor(gw-egress-impersonate): use gw_runtime::Supervisor"
```

---

## Task 3.2: Supervise `gw-ledger` flusher, frozen-delta worker, DLQ retry worker, reaper

**Files:**
- Modify: `crates/gw-ledger/Cargo.toml`
- Modify: `crates/gw-ledger/src/flusher.rs` (3 spawn sites: 107, 323, 369)
- Modify: `crates/gw-ledger/src/inflight_reaper.rs` (spawn site: 18)
- Modify: `crates/gw-ledger/src/lib.rs` (public entry points accept a `ShutdownToken`)

- [ ] **Step 1: Add `gw-runtime` to `gw-ledger/Cargo.toml`**

```toml
gw-runtime = { path = "../gw-runtime" }
```

- [ ] **Step 2: Refactor flusher to take a `ShutdownToken`**

In `crates/gw-ledger/src/flusher.rs:107`, the bare `tokio::spawn` becomes a Supervisor::spawn. Replace the spawn block with:

```rust
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};

let worker_redis_clone = worker_redis.clone();
let worker_instance_clone = worker_instance.clone();
let worker_dlq_clone = worker_dlq.clone();
let rx = Arc::new(tokio::sync::Mutex::new(rx)); // mpsc::Receiver is !Sync; wrap.

let flusher_handle = Supervisor::new("billing_flusher", RestartPolicy::default(), shutdown.clone())
    .spawn(move |tok| {
        let redis = worker_redis_clone.clone();
        let instance = worker_instance_clone.clone();
        let dlq = worker_dlq_clone.clone();
        let rx = rx.clone();
        async move {
            let mut buf: Vec<LedgerItem> = Vec::with_capacity(batch_size);
            let mut ticker = interval(flush_interval);
            loop {
                let mut rx_guard = rx.lock().await;
                tokio::select! {
                    _ = tok.cancelled() => {
                        // Final drain on shutdown.
                        while let Ok(item) = rx_guard.try_recv() {
                            buf.push(item);
                        }
                        if !buf.is_empty() {
                            let _ = AssertUnwindSafe(
                                flush(&redis, &instance, &dlq, &mut buf)
                            ).catch_unwind().await;
                        }
                        return Ok(());
                    }
                    maybe_item = rx_guard.recv() => {
                        match maybe_item {
                            Some(item) => {
                                buf.push(item);
                                if buf.len() >= batch_size {
                                    let mut taken = std::mem::take(&mut buf);
                                    let result = AssertUnwindSafe(
                                        flush(&redis, &instance, &dlq, &mut taken)
                                    ).catch_unwind().await;
                                    if let Err(_panic) = result {
                                        metrics::counter!("gw_billing_flusher_panic_total").increment(1);
                                        tracing::error!(
                                            target: "gw_ledger",
                                            "flusher: flush() panicked; batch lost (inflight keys remain for reconciler)"
                                        );
                                    }
                                }
                            }
                            None => return Ok(()),
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() {
                            let mut taken = std::mem::take(&mut buf);
                            let result = AssertUnwindSafe(
                                flush(&redis, &instance, &dlq, &mut taken)
                            ).catch_unwind().await;
                            if let Err(_panic) = result {
                                metrics::counter!("gw_billing_flusher_panic_total").increment(1);
                            }
                        }
                    }
                }
            }
        }
    });
```

`shutdown: ShutdownToken` is now a new parameter on the public flusher entry point (`pub fn spawn_flusher(...)` or equivalent). Thread it from `gw-gateway/src/lib.rs` (the bootstrap holds `shutdown_token`).

- [ ] **Step 3: Same treatment for frozen-delta worker (`flusher.rs:323`) and DLQ retry worker (`flusher.rs:369`)**

Pattern:

```rust
fn spawn_frozen_delta_worker(redis: RedisCtx, mut rx: mpsc::Receiver<FrozenDelta>, shutdown: ShutdownToken) {
    Supervisor::new("frozen_delta_worker", RestartPolicy::default(), shutdown)
        .spawn(move |tok| {
            let redis = redis.clone();
            async move {
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        Some(delta) = rx.recv() => { /* existing body */ }
                        else => return Ok(()),
                    }
                }
            }
        });
}
```

The DLQ retry worker (`flusher.rs:369`) gets the same wrapper.

- [ ] **Step 4: Supervise the inflight reaper (`inflight_reaper.rs:18`)**

`crates/gw-ledger/src/inflight_reaper.rs`:

```rust
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};

pub fn spawn(
    pool: RedisPool,
    instance_id: String,
    interval_secs: u64,
    emit_lag_t0_secs: u64,
    shutdown: ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("inflight_reaper", RestartPolicy::default(), shutdown)
        .spawn(move |tok| {
            let pool = pool.clone();
            let instance_id = instance_id.clone();
            async move {
                let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = tick.tick() => {
                            if let Err(e) = run_once(&pool, &instance_id, emit_lag_t0_secs).await {
                                tracing::warn!(error = ?e, "inflight reaper iteration failed");
                            }
                        }
                    }
                }
            }
        })
}
```

- [ ] **Step 5: Register flusher / worker / reaper JoinHandles with the ShutdownCoordinator**

In `gw-gateway/src/lib.rs` bootstrap, capture each `JoinHandle` returned by the spawn helpers and call `shutdown.register(handle)` so they participate in drain.

- [ ] **Step 6: Test — supervised flusher restarts on panic**

`crates/gw-ledger/tests/supervised_flusher.rs`:

```rust
//! Verify that the supervised flusher continues processing after a panic
//! injected via a poisoned flush function. This test stubs Redis with a
//! fake client that panics on the first batch and succeeds afterwards.
//!
//! Marked `#[ignore]` because it needs the testcontainers Redis already
//! wired by other --ignored tests. Mirror the existing pattern.

#[tokio::test]
#[ignore]
async fn flusher_restarts_after_panic() {
    // Use the existing test harness in this crate; copy the pattern from
    // crates/gw-ledger/tests/<existing>.rs and inject a panicking flush.
    // Assertions:
    // 1. `gw_supervisor_restart_total{task="billing_flusher",reason="panic"}` > 0
    // 2. After restart, subsequent XADDs succeed (Redis stream has expected length)
    todo!("flesh out using existing test harness");
}
```

> The `todo!()` is acceptable here only because the test pattern depends on a harness this plan doesn't fully spec. The implementer must replace with real test code mirroring the existing `crates/gw-ledger/tests/*.rs` setup. Spec compliance reviewer should flag if the test stays as `todo!()`.

- [ ] **Step 7: Compile, lint, commit**

```bash
cargo check -p gw-ledger -p gw-gateway
cargo clippy -p gw-ledger -p gw-gateway --all-targets -- -D warnings
git add crates/gw-ledger/ crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-ledger): supervise flusher / frozen-delta / dlq-retry / reaper"
```

---

## Task 3.3: Supervise `gw-task-worker` core loops

**Files:**
- Modify: `crates/gw-task-worker/Cargo.toml`
- Modify: `crates/gw-task-worker/src/lib.rs` (spawn sites at lines 453–458, 515)

- [ ] **Step 1: Add `gw-runtime` dependency**

In `crates/gw-task-worker/Cargo.toml`:

```toml
gw-runtime = { path = "../gw-runtime" }
```

- [ ] **Step 2: Wrap the 5 core loops + health server spawn**

Replace lines 453–458 of `crates/gw-task-worker/src/lib.rs`:

```rust
use gw_runtime::{RestartPolicy, Supervisor};

// `shutdown: ShutdownToken` is now an argument to whichever function owns
// this spawn block (search upward — typically `TaskWorkerRuntime::run` or
// `TaskWorker::start`).

let handles = vec![
    Supervisor::new("task_exec_loop", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| {
            let inner = exec_loop.clone();
            async move {
                inner.run_with_shutdown(tok).await.map_err(anyhow::Error::from)
            }
        }),
    Supervisor::new("task_cancel_loop", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| { let inner = cancel_loop.clone(); async move { inner.run_with_shutdown(tok).await.map_err(anyhow::Error::from) } }),
    Supervisor::new("task_watchdog_loop", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| { let inner = watchdog_loop.clone(); async move { inner.run_with_shutdown(tok).await.map_err(anyhow::Error::from) } }),
    Supervisor::new("task_heartbeat_loop", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| { let inner = heartbeat_loop.clone(); async move { inner.run_with_shutdown(tok).await.map_err(anyhow::Error::from) } }),
    Supervisor::new("task_registration_loop", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| { let inner = registration_loop.clone(); async move { inner.run_with_shutdown(tok).await.map_err(anyhow::Error::from) } }),
    Supervisor::new("task_health_server", RestartPolicy::default(), shutdown.clone())
        .spawn(move |tok| { let port = cfg.server.health_port; async move { run_health_server(port, tok).await.map_err(anyhow::Error::from) } }),
];
```

Each `*_loop.run()` must grow a `run_with_shutdown(token)` method that returns `Result<(), Error>`; default implementation is `tokio::select! { _ = token.cancelled() => Ok(()), r = self.run() => r }`.

- [ ] **Step 3: Spawn site at line 515 (worker-specific spawn inside loop)**

Wrap with `Supervisor` only if it's a long-running task. If it's a per-task fire-and-forget spawn (one-off task execution), keep it as `tokio::spawn` but wrap the body in `AssertUnwindSafe::catch_unwind` and increment `gw_task_worker_inflight_panic_total` on panic.

- [ ] **Step 4: Compile, lint, commit**

```bash
cargo check -p gw-task-worker
cargo clippy -p gw-task-worker --all-targets -- -D warnings
git add crates/gw-task-worker/
git commit -m "feat(gw-task-worker): supervise core loops + health server"
```

---

## Task 3.4: Supervise `gw-gateway` background spawns

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs` (spawn sites at lines 905, 997, 1131, 1141)

- [ ] **Step 1: Wrap health server (line 905)**

```rust
Supervisor::new("health_server", RestartPolicy::default(), shutdown_token.clone())
    .spawn(move |tok| {
        let health_state = health_state.clone();
        let health_store = health_store.clone();
        let health_blacklist = health_blacklist.clone();
        let phase = phase_handle.clone();
        let bp = backpressure_state.clone();
        let supervisor_unhealthy = supervisor_unhealthy.clone();
        async move {
            run_health_server(health_port, health_state, health_store, health_blacklist, phase, bp, supervisor_unhealthy)
                .await
                .map_err(anyhow::Error::from)
        }
    });
```

- [ ] **Step 2: Wrap tunnel-store-refresher (line 997)**

```rust
Supervisor::new("tunnel_store_refresher", RestartPolicy::default(), shutdown_token.clone())
    .spawn(move |tok| {
        let stack = stack_clone.clone();
        let store = store_clone.clone();
        let kek = tunnel_kek.clone();
        async move {
            let mut last_signature: u64 = 0;
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        if let Some(nodes) = store.proxy_nodes_list_or_load().await {
                            // ... existing signature logic ...
                        }
                    }
                }
            }
        }
    });
```

- [ ] **Step 3: Wrap adaptive-RPM decay loop (line 1131) and IP ratelimit sweep (line 1141)**

```rust
Supervisor::new("adaptive_rpm_decay", RestartPolicy::default(), shutdown_token.clone())
    .spawn({
        let manager = manager.clone();
        move |tok| {
            let m = manager.clone();
            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = interval.tick() => m.tick_decay(),
                    }
                }
            }
        }
    });

Supervisor::new("ip_ratelimit_sweep", RestartPolicy::default(), shutdown_token.clone())
    .spawn({
        let guard = guard.clone();
        move |tok| {
            let g = guard.clone();
            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(sweep_interval_secs));
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = interval.tick() => {
                            let removed = g.sweep();
                            if removed > 0 {
                                tracing::info!(target: "gw_gateway", removed, active = g.len(), "ip ratelimit swept");
                            }
                        }
                    }
                }
            }
        }
    });
```

- [ ] **Step 4: Register each `JoinHandle` with the coordinator**

For every Supervisor `spawn` above, capture the returned `JoinHandle` and call `coordinator.register(handle)` before serving begins.

- [ ] **Step 5: Combine per-supervisor `unhealthy_flag()` into the shared `supervisor_unhealthy` AtomicBool**

After construction:

```rust
let unhealthy_flags = vec![
    health_supervisor.unhealthy_flag(),
    tunnel_supervisor.unhealthy_flag(),
    // ...
];

tokio::spawn({
    let token = shutdown_token.clone();
    let supervisor_unhealthy = supervisor_unhealthy.clone();
    async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tick.tick() => {
                    let any = unhealthy_flags.iter().any(|f| f.load(std::sync::atomic::Ordering::Acquire));
                    supervisor_unhealthy.store(any, std::sync::atomic::Ordering::Release);
                }
            }
        }
    }
});
```

The Supervisor builder must therefore expose its flag *before* `spawn` is called — adjust to:

```rust
let sup = Supervisor::new("name", RestartPolicy::default(), token.clone());
let flag = sup.unhealthy_flag();
let handle = sup.spawn(factory);
```

- [ ] **Step 6: Compile, lint, commit**

```bash
cargo check -p gw-gateway
cargo clippy -p gw-gateway --all-targets -- -D warnings
git add crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-gateway): supervise health/tunnel-refresher/rpm-decay/ip-sweep"
```

---

## Task 4.1: Migrate `EmergencyDlqWriter` to async `SpillFile`

**Files:**
- Modify: `crates/gw-ledger/src/dlq.rs`
- Modify: `crates/gw-ledger/Cargo.toml` (already has `gw-runtime` from T3.2)
- Modify: `crates/gw-ledger/src/flusher.rs` (call site `writer.append(...)`)

- [ ] **Step 1: Rewrite `EmergencyDlqWriter` over `SpillFile`**

Replace the `EmergencyDlqWriter` struct and impl in `crates/gw-ledger/src/dlq.rs`:

```rust
use gw_runtime::{SpillFile, SpillRoot};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::OnceCell;

pub struct EmergencyDlqWriter {
    root: Arc<SpillRoot>,
    shards: NonZeroUsize,
    file: OnceCell<SpillFile>,
}

impl EmergencyDlqWriter {
    pub fn new(root: Arc<SpillRoot>, shards: NonZeroUsize) -> Self {
        Self { root, shards, file: OnceCell::new() }
    }

    async fn file(&self) -> std::io::Result<&SpillFile> {
        self.file
            .get_or_try_init(|| async { self.root.create("dlq", self.shards).await })
            .await
    }

    pub async fn append(&self, envelope: &DlqEnvelope) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(envelope)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        bytes.push(b'\n');
        let file = self.file().await?;
        file.append(&bytes).await?;
        metrics::counter!("gw_dlq_envelopes_appended_total").increment(1);
        Ok(())
    }

    pub async fn flush(&self) -> std::io::Result<()> {
        if let Some(f) = self.file.get() { f.flush().await? }
        Ok(())
    }

    pub async fn read_all(&self) -> std::io::Result<Vec<Vec<u8>>> {
        match self.file.get() {
            Some(f) => f.read_all().await,
            None => Ok(Vec::new()),
        }
    }
}
```

> Remove the old `std::fs::*` / `std::sync::Mutex<File>` imports — they should now be unused.

- [ ] **Step 2: Update construction in `flusher.rs`**

Search for `Arc<EmergencyDlqWriter>` construction (typically in the public `spawn_flusher` entry). Take `Arc<SpillRoot>` and a `dlq_shards: usize` from config instead of a `dlq_dir: PathBuf`:

```rust
let writer = Arc::new(EmergencyDlqWriter::new(
    spill_root.clone(),
    NonZeroUsize::new(cfg.runtime.spill_io.dlq_shards).expect("dlq_shards >= 1"),
));
```

- [ ] **Step 3: Update all call sites of `writer.append(...)` to `.await`**

```rust
if let Err(err) = writer.append(&disk_envelope).await {
    metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "error").increment(1);
    tracing::warn!(target: "gw_ledger", error = %err, "dlq write failed; falling back to in-memory backlog");
    // Fallback to BillingBacklog — implemented in task 4.2.
    backlog.push(disk_envelope).await;
} else {
    metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "ok").increment(1);
}
```

- [ ] **Step 4: Unit test the async writer**

`crates/gw-ledger/src/dlq.rs` `#[cfg(test)] mod tests` block:

```rust
#[tokio::test]
async fn dlq_writer_round_trip() {
    use gw_runtime::{SpillRoot, SpillRootConfig};
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1024 * 1024,
        buffer_size: 4096,
    }));
    let writer = EmergencyDlqWriter::new(root, NonZeroUsize::new(2).unwrap());

    let env = DlqEnvelope::new(
        UsageRecord::default(), "test", 0, DlqLayer::Redis, 0,
    );
    writer.append(&env).await.unwrap();
    writer.append(&env).await.unwrap();
    writer.flush().await.unwrap();
    let lines: usize = writer.read_all().await.unwrap()
        .iter().map(|s| String::from_utf8_lossy(s).lines().count()).sum();
    assert_eq!(lines, 2);
}
```

- [ ] **Step 5: Compile, test, commit**

```bash
cargo test -p gw-ledger dlq::tests::dlq_writer_round_trip
cargo clippy -p gw-ledger --all-targets -- -D warnings
git add crates/gw-ledger/src/dlq.rs crates/gw-ledger/src/flusher.rs
git commit -m "feat(gw-ledger): async DLQ writer backed by SpillFile"
```

---

## Task 4.2: Implement `BillingBacklog` — growable in-memory fallback

**Files:**
- Create: `crates/gw-ledger/src/backlog.rs`
- Modify: `crates/gw-ledger/src/lib.rs` (add `pub mod backlog`)

- [ ] **Step 1: Write the failing tests**

```rust
// crates/gw-ledger/src/backlog.rs
//! BillingBacklog — unbounded in-memory fallback when both Redis XADD and
//! the SpillFile DLQ have failed. Guarantees the must-deliver billing
//! invariant: never drops an envelope. The retry worker (next step)
//! drains it back to Redis / disk.

use crate::dlq::DlqEnvelope;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct BillingBacklog {
    queue: Mutex<VecDeque<(Instant, DlqEnvelope)>>,
    pending_count: AtomicUsize,
    pending_bytes: AtomicU64,
    high_watermark_count: AtomicUsize,
    oldest_ns: AtomicU64,
}

impl BillingBacklog {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            pending_count: AtomicUsize::new(0),
            pending_bytes: AtomicU64::new(0),
            high_watermark_count: AtomicUsize::new(0),
            oldest_ns: AtomicU64::new(0),
        }
    }

    pub fn pending_count(&self) -> usize { self.pending_count.load(Ordering::Acquire) }
    pub fn pending_bytes(&self) -> u64 { self.pending_bytes.load(Ordering::Acquire) }
    pub fn high_watermark(&self) -> usize { self.high_watermark_count.load(Ordering::Acquire) }

    pub async fn push(&self, env: DlqEnvelope) {
        let bytes = serde_json::to_vec(&env).map(|b| b.len() as u64).unwrap_or(0);
        let mut q = self.queue.lock().await;
        let now = Instant::now();
        if q.is_empty() {
            self.oldest_ns.store(elapsed_ns(now), Ordering::Release);
        }
        q.push_back((now, env));
        let count = self.pending_count.fetch_add(1, Ordering::AcqRel) + 1;
        self.pending_bytes.fetch_add(bytes, Ordering::AcqRel);
        let mut hwm = self.high_watermark_count.load(Ordering::Acquire);
        while count > hwm {
            match self.high_watermark_count.compare_exchange(hwm, count, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => hwm = actual,
            }
        }
        metrics::gauge!("gw_billing_backlog_pending_count").set(count as f64);
        metrics::gauge!("gw_billing_backlog_pending_bytes").set(self.pending_bytes() as f64);
    }

    pub async fn pop(&self) -> Option<DlqEnvelope> {
        let mut q = self.queue.lock().await;
        let (_, env) = q.pop_front()?;
        let new_oldest = q.front().map(|(t, _)| elapsed_ns(*t)).unwrap_or(0);
        self.oldest_ns.store(new_oldest, Ordering::Release);
        let bytes = serde_json::to_vec(&env).map(|b| b.len() as u64).unwrap_or(0);
        self.pending_count.fetch_sub(1, Ordering::AcqRel);
        self.pending_bytes.fetch_sub(bytes, Ordering::AcqRel);
        metrics::gauge!("gw_billing_backlog_pending_count").set(self.pending_count() as f64);
        metrics::gauge!("gw_billing_backlog_oldest_age_seconds")
            .set(if new_oldest == 0 { 0.0 } else { (elapsed_ns(Instant::now()) - new_oldest) as f64 / 1e9 });
        Some(env)
    }

    pub async fn drain_to_vec(&self) -> Vec<DlqEnvelope> {
        let mut q = self.queue.lock().await;
        let out: Vec<DlqEnvelope> = q.drain(..).map(|(_, e)| e).collect();
        self.pending_count.store(0, Ordering::Release);
        self.pending_bytes.store(0, Ordering::Release);
        self.oldest_ns.store(0, Ordering::Release);
        out
    }
}

fn elapsed_ns(t: Instant) -> u64 {
    // Use Instant elapsed since epoch-equivalent — we only ever compute
    // relative deltas so this is fine; pick a fixed monotonic baseline.
    static BASE: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);
    t.duration_since(*BASE).as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlq::{DlqEnvelope, DlqLayer};
    use crate::usage::UsageRecord;

    fn env() -> DlqEnvelope {
        DlqEnvelope::new(UsageRecord::default(), "test", 0, DlqLayer::Redis, 0)
    }

    #[tokio::test]
    async fn push_and_pop_preserves_order() {
        let b = BillingBacklog::new();
        for _ in 0..3 { b.push(env()).await; }
        assert_eq!(b.pending_count(), 3);
        for _ in 0..3 { assert!(b.pop().await.is_some()); }
        assert_eq!(b.pending_count(), 0);
    }

    #[tokio::test]
    async fn drain_clears_state() {
        let b = BillingBacklog::new();
        for _ in 0..10 { b.push(env()).await; }
        let drained = b.drain_to_vec().await;
        assert_eq!(drained.len(), 10);
        assert_eq!(b.pending_count(), 0);
        assert_eq!(b.pending_bytes(), 0);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/gw-ledger/src/lib.rs` add:

```rust
pub mod backlog;
```

- [ ] **Step 3: Run, lint, commit**

```bash
cargo test -p gw-ledger backlog::tests
cargo clippy -p gw-ledger --all-targets -- -D warnings
git add crates/gw-ledger/src/backlog.rs crates/gw-ledger/src/lib.rs
git commit -m "feat(gw-ledger): BillingBacklog growable VecDeque fallback"
```

---

## Task 4.3: Cascade Redis → SpillFile → BillingBacklog in the billing write path

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`

> Goal: every failed XADD goes to SpillFile; every failed SpillFile append goes to BillingBacklog. The background retry worker (next step) drains BillingBacklog back to Redis.

- [ ] **Step 1: Plumb BillingBacklog into `DlqFallback`**

Extend `DlqFallback`:

```rust
#[derive(Clone)]
pub struct DlqFallback {
    pub writer: Arc<EmergencyDlqWriter>,
    pub retry_tx: mpsc::Sender<DlqEnvelope>,
    pub backlog: Arc<BillingBacklog>,
}
```

- [ ] **Step 2: Update the failure cascade**

Wherever the existing flusher routes a failed XADD to the DLQ (search for `DlqLayer::Disk` and `writer.append`), replace with:

```rust
async fn route_to_dlq(fallback: &DlqFallback, mut env: DlqEnvelope) {
    // Try retry channel first (in-memory bounded mpsc — fast path).
    env.dlq_meta.dlq_layer = DlqLayer::Mem;
    if fallback.retry_tx.try_send(env.clone()).is_ok() {
        return;
    }
    // Then disk via SpillFile.
    let disk = env.with_layer(DlqLayer::Disk);
    if fallback.writer.append(&disk).await.is_ok() {
        return;
    }
    // Last fallback: unbounded BillingBacklog. Never drops.
    fallback.backlog.push(disk).await;
}
```

- [ ] **Step 3: Background retry worker drains BillingBacklog**

Add a supervised task next to the flusher:

```rust
pub fn spawn_backlog_drainer(
    redis: RedisCtx,
    writer: Arc<EmergencyDlqWriter>,
    backlog: Arc<BillingBacklog>,
    shutdown: ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("billing_backlog_drainer", RestartPolicy::default(), shutdown)
        .spawn(move |tok| {
            let redis = redis.clone();
            let writer = writer.clone();
            let backlog = backlog.clone();
            async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = tick.tick() => {
                            while let Some(env) = backlog.pop().await {
                                match dlq_xadd(&redis, &env).await {
                                    Ok(_) => continue,
                                    Err(_) => {
                                        let disk = env.with_layer(DlqLayer::Disk);
                                        if writer.append(&disk).await.is_err() {
                                            // Put it back at the head — never drop.
                                            backlog.push(disk).await;
                                            break; // back off until next tick
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
}
```

- [ ] **Step 4: Wire backlog + drainer into bootstrap**

In `gw-gateway/src/lib.rs`, where the flusher is constructed:

```rust
let backlog = Arc::new(BillingBacklog::new());
let dlq_writer = Arc::new(EmergencyDlqWriter::new(
    spill_root.clone(),
    NonZeroUsize::new(cfg.runtime.spill_io.dlq_shards).expect("dlq_shards >= 1"),
));

let fallback = DlqFallback { writer: dlq_writer.clone(), retry_tx, backlog: backlog.clone() };
let flusher_handle = spawn_flusher(/* ..., */ fallback.clone(), shutdown_token.clone());
let drainer_handle = spawn_backlog_drainer(redis.clone(), dlq_writer.clone(), backlog.clone(), shutdown_token.clone());
shutdown.register(flusher_handle);
shutdown.register(drainer_handle);
```

- [ ] **Step 5: Compile, lint, commit**

```bash
cargo check -p gw-ledger -p gw-gateway
cargo clippy -p gw-ledger -p gw-gateway --all-targets -- -D warnings
git add crates/gw-ledger/src/flusher.rs crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-ledger): cascade Redis -> SpillFile -> BillingBacklog with drainer"
```

---

## Task 4.4: BacklogRecovery — load persisted backlog on bootstrap

**Files:**
- Modify: `crates/gw-ledger/src/backlog.rs` (add `BacklogRecovery`)
- Modify: `crates/gw-gateway/src/lib.rs` (run recovery at bootstrap before flusher starts)

- [ ] **Step 1: Implement recovery**

Append to `crates/gw-ledger/src/backlog.rs`:

```rust
use std::path::Path;

pub struct BacklogRecovery {
    pub root: Arc<gw_runtime::SpillRoot>,
}

impl BacklogRecovery {
    pub fn new(root: Arc<gw_runtime::SpillRoot>) -> Self { Self { root } }

    pub async fn load_into(&self, backlog: &BillingBacklog) -> std::io::Result<usize> {
        self.root.ensure_dir().await?;
        let mut dir = tokio::fs::read_dir(self.root.root_dir()).await?;
        let mut loaded: usize = 0;
        let mut corrupt: Vec<std::path::PathBuf> = Vec::new();
        let corrupt_dir = self.root.root_dir().join("corrupt");
        tokio::fs::create_dir_all(&corrupt_dir).await.ok();

        while let Some(ent) = dir.next_entry().await? {
            let path = ent.path();
            let is_dlq = path.file_name().and_then(|n| n.to_str())
                .map(|s| s.starts_with("dlq.") || s.starts_with("backlog_shutdown_"))
                .unwrap_or(false);
            if !is_dlq { continue; }
            match self.replay_file(&path, backlog).await {
                Ok(n) => loaded += n,
                Err(e) => {
                    tracing::warn!(target: "gw_ledger::backlog", path = %path.display(), error = %e, "backlog file corrupt; quarantining");
                    corrupt.push(path);
                }
            }
        }

        for p in corrupt {
            let dest = corrupt_dir.join(p.file_name().unwrap());
            let _ = tokio::fs::rename(&p, &dest).await;
            metrics::counter!("gw_billing_recovery_corrupt_files_total").increment(1);
        }
        metrics::counter!("gw_billing_recovery_loaded_total").increment(loaded as u64);
        Ok(loaded)
    }

    async fn replay_file(&self, path: &Path, backlog: &BillingBacklog) -> std::io::Result<usize> {
        let bytes = tokio::fs::read(path).await?;
        let mut count = 0usize;
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() { continue; }
            let env: DlqEnvelope = serde_json::from_slice(line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            backlog.push(env).await;
            count += 1;
        }
        // Successful replay deletes the file (the data is now in the
        // backlog and will be persisted again on drain if needed).
        let _ = tokio::fs::remove_file(path).await;
        Ok(count)
    }
}
```

- [ ] **Step 2: Run recovery before starting the drainer**

In `gw-gateway/src/lib.rs` bootstrap, AFTER constructing `backlog` and `spill_root` but BEFORE spawning the flusher / drainer:

```rust
let recovery = gw_ledger::backlog::BacklogRecovery::new(spill_root.clone());
match recovery.load_into(&backlog).await {
    Ok(loaded) => tracing::info!(target: "gw_gateway", loaded, "billing backlog recovery complete"),
    Err(e) => tracing::error!(target: "gw_gateway", error = %e, "backlog recovery failed; continuing with empty backlog"),
}
```

- [ ] **Step 3: Test**

`crates/gw-ledger/tests/backlog_recovery.rs`:

```rust
use gw_ledger::backlog::{BillingBacklog, BacklogRecovery};
use gw_ledger::dlq::{DlqEnvelope, DlqLayer};
use gw_ledger::usage::UsageRecord;
use gw_runtime::{SpillRoot, SpillRootConfig};
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn recovery_loads_and_quarantines() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1024 * 1024,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    // Valid file.
    let mut env = DlqEnvelope::new(UsageRecord::default(), "x", 0, DlqLayer::Disk, 0);
    let line = serde_json::to_vec(&env).unwrap();
    let mut data = Vec::new();
    data.extend_from_slice(&line); data.push(b'\n');
    data.extend_from_slice(&line); data.push(b'\n');
    tokio::fs::write(tmp.path().join("dlq.0.jsonl"), data).await.unwrap();

    // Corrupt file.
    tokio::fs::write(tmp.path().join("dlq.1.jsonl"), b"not-json\n").await.unwrap();

    let backlog = BillingBacklog::new();
    let recovery = BacklogRecovery::new(root);
    let loaded = recovery.load_into(&backlog).await.unwrap();
    assert_eq!(loaded, 2);

    // Corrupt file moved.
    assert!(tmp.path().join("corrupt").join("dlq.1.jsonl").exists());
}
```

- [ ] **Step 4: Run, lint, commit**

```bash
cargo test -p gw-ledger --test backlog_recovery
cargo clippy -p gw-ledger --all-targets -- -D warnings
git add crates/gw-ledger/src/backlog.rs crates/gw-ledger/tests/backlog_recovery.rs crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-ledger): backlog recovery on bootstrap with corrupt-file quarantine"
```

---

## Task 4.5: Shutdown dumps un-drained backlog to disk

**Files:**
- Modify: `crates/gw-ledger/src/backlog.rs` (add `dump_to_shutdown_file`)
- Modify: `crates/gw-gateway/src/lib.rs` (wire dump into shutdown drain)

- [ ] **Step 1: Implement the dump**

Append to `crates/gw-ledger/src/backlog.rs`:

```rust
impl BillingBacklog {
    pub async fn dump_to_shutdown_file(&self, root: &gw_runtime::SpillRoot) -> std::io::Result<usize> {
        let drained = self.drain_to_vec().await;
        if drained.is_empty() { return Ok(0); }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key = format!("backlog_shutdown_{ts}");
        let file = root.create(&key, std::num::NonZeroUsize::new(1).unwrap()).await?;
        for env in &drained {
            let mut bytes = serde_json::to_vec(env)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            bytes.push(b'\n');
            file.append(&bytes).await?;
        }
        file.fsync().await?;
        metrics::gauge!("gw_billing_backlog_shutdown_dump_bytes").set(drained.len() as f64);
        Ok(drained.len())
    }
}
```

- [ ] **Step 2: Invoke the dump during shutdown drain**

In `gw-gateway/src/lib.rs`, immediately after `coordinator.drain(...)` returns:

```rust
let dump_result = backlog.dump_to_shutdown_file(&spill_root).await;
match dump_result {
    Ok(n) if n > 0 => tracing::info!(target: "gw_gateway", n, "shutdown: backlog dumped to disk"),
    Ok(_) => {},
    Err(e) => {
        tracing::error!(target: "gw_gateway", error = %e, "shutdown: backlog dump FAILED; exiting 2");
        metrics::counter!("gw_billing_backlog_shutdown_dump_failed_total").increment(1);
        std::process::exit(2);
    }
}
```

- [ ] **Step 3: Test the round-trip**

Add to `crates/gw-ledger/tests/backlog_recovery.rs`:

```rust
#[tokio::test]
async fn dump_then_recover_round_trip() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1024 * 1024,
        buffer_size: 4096,
    }));

    let backlog1 = BillingBacklog::new();
    for _ in 0..5 {
        backlog1.push(DlqEnvelope::new(UsageRecord::default(), "x", 0, DlqLayer::Disk, 0)).await;
    }
    let dumped = backlog1.dump_to_shutdown_file(&root).await.unwrap();
    assert_eq!(dumped, 5);

    let backlog2 = BillingBacklog::new();
    let loaded = BacklogRecovery::new(root.clone()).load_into(&backlog2).await.unwrap();
    assert_eq!(loaded, 5);
    assert_eq!(backlog2.pending_count(), 5);
}
```

- [ ] **Step 4: Run, lint, commit**

```bash
cargo test -p gw-ledger --test backlog_recovery
cargo clippy -p gw-ledger -p gw-gateway --all-targets -- -D warnings
git add crates/gw-ledger/ crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-ledger): shutdown dump + exit code 2 if dump fails"
```

---

## Task 5.1: Introduce `ArchiveState` enum in `ReqCtx`

**Files:**
- Modify: `crates/gw-proxy/Cargo.toml` (add `gw-runtime`)
- Modify: `crates/gw-proxy/src/ctx.rs` (lines 330, 332, 455, 456)

- [ ] **Step 1: Add `gw-runtime` dep**

`crates/gw-proxy/Cargo.toml`:

```toml
gw-runtime = { path = "../gw-runtime" }
```

- [ ] **Step 2: Define `ArchiveState`**

At the top of `crates/gw-proxy/src/ctx.rs` (or in a new sibling module `archive_state.rs` and `pub mod archive_state;` from `ctx.rs`):

```rust
use bytes::BytesMut;
use gw_runtime::SpillFile;

pub enum ArchiveState {
    InMemory(BytesMut),
    Spilled {
        file: Box<SpillFile>,
        written: u64,
    },
    Truncated {
        written: u64,
        dropped: u64,
    },
}

impl Default for ArchiveState {
    fn default() -> Self { Self::InMemory(BytesMut::new()) }
}

pub struct ArchiveCaps {
    pub soft_cap_bytes: usize,
    pub hard_cap_bytes: usize,
}

impl ArchiveState {
    pub fn len(&self) -> u64 {
        match self {
            Self::InMemory(b) => b.len() as u64,
            Self::Spilled { written, .. } => *written,
            Self::Truncated { written, .. } => *written,
        }
    }

    pub fn dropped(&self) -> u64 {
        match self { Self::Truncated { dropped, .. } => *dropped, _ => 0 }
    }

    pub fn is_truncated(&self) -> bool { matches!(self, Self::Truncated { .. }) }
}
```

- [ ] **Step 3: Replace the two `BytesMut` fields in `ReqCtx`**

In `crates/gw-proxy/src/ctx.rs:330,332`:

```rust
pub archive_upstream_response_body: ArchiveState,
pub archive_downstream_response_body: ArchiveState,
```

And in the `Default` / `new` constructor (currently lines 455–456):

```rust
archive_upstream_response_body: ArchiveState::default(),
archive_downstream_response_body: ArchiveState::default(),
```

- [ ] **Step 4: Add helper method for chunk append with cap enforcement**

Append to the `ArchiveState` impl block:

```rust
use gw_runtime::SpillRoot;
use std::sync::Arc;

impl ArchiveState {
    /// Append a chunk; transition InMemory → Spilled at soft cap,
    /// Spilled → Truncated at hard cap. Never returns error to the caller —
    /// failures are logged + counted but the response path is not blocked.
    pub async fn append_chunk(
        &mut self,
        chunk: &[u8],
        caps: &ArchiveCaps,
        spill_root: &Arc<SpillRoot>,
        request_id: &str,
        surface: &'static str,
    ) {
        let new_total = self.len().saturating_add(chunk.len() as u64);

        match self {
            Self::InMemory(buf) => {
                if new_total <= caps.soft_cap_bytes as u64 {
                    buf.extend_from_slice(chunk);
                    return;
                }
                // Transition to Spilled.
                let key = format!("archive-{request_id}-{surface}");
                match spill_root.create(&key, std::num::NonZeroUsize::new(1).unwrap()).await {
                    Ok(file) => {
                        if let Err(e) = file.append(buf).await {
                            tracing::warn!(target: "gw_proxy::archive",
                                request_id, surface,
                                bytes_written = buf.len(),
                                bytes_dropped = chunk.len(),
                                error = %e,
                                "archive_spill_append_failed");
                            *self = Self::Truncated { written: buf.len() as u64, dropped: chunk.len() as u64 };
                            metrics::counter!("gw_archive_truncated_total").increment(1);
                            return;
                        }
                        if let Err(e) = file.append(chunk).await {
                            tracing::warn!(target: "gw_proxy::archive",
                                request_id, surface,
                                bytes_written = buf.len(),
                                bytes_dropped = chunk.len(),
                                error = %e,
                                "archive_spill_append_failed");
                            *self = Self::Truncated { written: buf.len() as u64, dropped: chunk.len() as u64 };
                            metrics::counter!("gw_archive_truncated_total").increment(1);
                            return;
                        }
                        let written = (buf.len() + chunk.len()) as u64;
                        *self = Self::Spilled { file: Box::new(file), written };
                        metrics::counter!("gw_archive_spilled_total").increment(1);
                    }
                    Err(e) => {
                        tracing::warn!(target: "gw_proxy::archive",
                            request_id, surface,
                            size = buf.len(),
                            error = %e,
                            "archive_spill_create_failed");
                        *self = Self::Truncated { written: buf.len() as u64, dropped: chunk.len() as u64 };
                        metrics::counter!("gw_archive_truncated_total").increment(1);
                        metrics::counter!("gw_archive_disk_full_total").increment(1);
                    }
                }
            }
            Self::Spilled { file, written } => {
                if new_total > caps.hard_cap_bytes as u64 {
                    *self = Self::Truncated { written: *written, dropped: chunk.len() as u64 };
                    metrics::counter!("gw_archive_truncated_total").increment(1);
                    return;
                }
                if let Err(e) = file.append(chunk).await {
                    tracing::warn!(target: "gw_proxy::archive",
                        request_id, surface,
                        bytes_written = *written,
                        bytes_dropped = chunk.len(),
                        error = %e,
                        "archive_spill_append_failed");
                    *self = Self::Truncated { written: *written, dropped: chunk.len() as u64 };
                    metrics::counter!("gw_archive_truncated_total").increment(1);
                } else {
                    *written += chunk.len() as u64;
                }
            }
            Self::Truncated { dropped, .. } => {
                *dropped += chunk.len() as u64;
            }
        }
    }

    pub async fn read_for_emit(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::InMemory(b) => Ok(b.to_vec()),
            Self::Spilled { file, .. } => {
                let shards = file.read_all().await?;
                Ok(shards.into_iter().next().unwrap_or_default())
            }
            Self::Truncated { .. } => Ok(Vec::new()),
        }
    }
}
```

- [ ] **Step 5: Compile**

Run: `cargo check -p gw-proxy`
Expected: errors at the three `extend_from_slice` call sites in `filters/response.rs` — fixed in 5.2.

- [ ] **Step 6: Commit (intermediate — code still doesn't compile fully)**

Skip commit until 5.2 makes it compile.

---

## Task 5.2: Wire `append_chunk` into `response_body_filter`

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs` (lines 149, 201, 256)

- [ ] **Step 1: Pass `spill_root` and `caps` through the filter ctx**

Wherever `ReqCtx` is constructed (search for `ReqCtx::new` or similar in `gw-proxy/src/ctx.rs`), accept `spill_root: Arc<SpillRoot>` and `archive_caps: ArchiveCaps` as inputs. Thread these from `Wired` in `gw-gateway/src/lib.rs`.

- [ ] **Step 2: Replace the 3 `extend_from_slice` call sites**

`crates/gw-proxy/src/filters/response.rs:149`:

```rust
ctx.archive_upstream_response_body
    .append_chunk(b, &ctx.archive_caps, &ctx.spill_root, &ctx.request_id, "upstream")
    .await;
```

`crates/gw-proxy/src/filters/response.rs:201`:

```rust
ctx.archive_downstream_response_body
    .append_chunk(b, &ctx.archive_caps, &ctx.spill_root, &ctx.request_id, "downstream")
    .await;
```

`crates/gw-proxy/src/filters/response.rs:256`: same as 201.

> The first two sites are inside `response_body_filter` which is `async fn` — `await` is legal there. If a site is inside a non-async closure, refactor: this is a known issue around Pingora filter signatures; in 0.8 the body filter is async-capable.

- [ ] **Step 3: Compile**

Run: `cargo check -p gw-proxy`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/response.rs crates/gw-proxy/Cargo.toml
git commit -m "feat(gw-proxy): archive ArchiveState with soft/hard cap and disk spill"
```

---

## Task 5.3: `emit_response_surfaces` reads `ArchiveState` instead of `BytesMut`

**Files:**
- Modify: `crates/gw-proxy/src/filters/logging.rs` (find the `emit_response_surfaces` fn and any reference to `archive_downstream_response_body` / `archive_upstream_response_body`)

- [ ] **Step 1: Replace direct field reads with `read_for_emit`**

For each surface that previously did:

```rust
let bytes = ctx.archive_downstream_response_body.split().freeze();
```

replace with:

```rust
let bytes = match ctx.archive_downstream_response_body.read_for_emit().await {
    Ok(b) => bytes::Bytes::from(b),
    Err(e) => {
        tracing::warn!(target: "gw_proxy::archive",
            request_id = %ctx.request_id, surface = "downstream",
            error = %e, "archive_emit_read_failed");
        bytes::Bytes::new()
    }
};
let truncated = ctx.archive_downstream_response_body.is_truncated();
let dropped = ctx.archive_downstream_response_body.dropped();
```

Add `truncated` / `dropped` to the detailed-log payload as metadata fields. Confirm the schema field names align with §06-billing-ledger.md updates from task 7.

- [ ] **Step 2: After emit, drop the SpillFile to delete it**

```rust
ctx.archive_downstream_response_body = ArchiveState::default();
ctx.archive_upstream_response_body = ArchiveState::default();
```

(Best-effort cleanup; the quota enforcer covers anything that leaks.)

- [ ] **Step 3: Compile, lint, commit**

```bash
cargo check -p gw-proxy
cargo clippy -p gw-proxy --all-targets -- -D warnings
git add crates/gw-proxy/src/filters/logging.rs
git commit -m "feat(gw-proxy): emit reads ArchiveState; truncated flag flows to detailed log"
```

---

## Task 5.4: Integration test — archive lifecycle across InMemory/Spilled/Truncated

**Files:**
- Create: `crates/gw-proxy/tests/archive_state.rs`

- [ ] **Step 1: Write the test**

```rust
use bytes::BytesMut;
use gw_proxy::ctx::{ArchiveCaps, ArchiveState};
use gw_runtime::{SpillRoot, SpillRootConfig};
use std::sync::Arc;
use tempfile::TempDir;

fn caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps { soft_cap_bytes: soft, hard_cap_bytes: hard }
}

#[tokio::test]
async fn stays_in_memory_under_soft_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    let mut a = ArchiveState::default();
    let small = vec![0u8; 1024];
    a.append_chunk(&small, &caps(4096, 8192), &root, "req1", "upstream").await;
    assert!(matches!(a, ArchiveState::InMemory(_)));
    assert_eq!(a.len(), 1024);
}

#[tokio::test]
async fn spills_to_disk_past_soft_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    let mut a = ArchiveState::default();
    let chunk = vec![1u8; 4096];
    a.append_chunk(&chunk, &caps(2048, 8192), &root, "req2", "upstream").await;
    assert!(matches!(a, ArchiveState::Spilled { .. }));
    let read_back = a.read_for_emit().await.unwrap();
    assert_eq!(read_back.len(), 4096);
}

#[tokio::test]
async fn truncates_past_hard_cap() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    let mut a = ArchiveState::default();
    let chunk = vec![2u8; 5000];
    a.append_chunk(&chunk, &caps(2048, 4096), &root, "req3", "upstream").await;
    // Should jump straight from InMemory -> Truncated because writing the
    // initial chunk would exceed the hard cap and the spill attempt fails
    // OR succeeds but the next chunk crosses hard.
    // The assertion is that further appends only count `dropped`.
    a.append_chunk(&chunk, &caps(2048, 4096), &root, "req3", "upstream").await;
    assert!(a.is_truncated());
    assert!(a.dropped() > 0);
}
```

- [ ] **Step 2: Run, commit**

```bash
cargo test -p gw-proxy --test archive_state
git add crates/gw-proxy/tests/archive_state.rs
git commit -m "test(gw-proxy): ArchiveState lifecycle integration"
```

---

## Task 6.1: Wire `RegexCache` into request filter ctx

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs` (add `regex_cache` field)
- Modify: `crates/gw-proxy/src/filters/request.rs:1651`

- [ ] **Step 1: Add `regex_cache` to `ReqCtx`**

```rust
use gw_runtime::RegexCache;
use std::sync::Arc;

pub struct ReqCtx {
    // ... existing fields ...
    pub regex_cache: Arc<RegexCache>,
}
```

Construct from `Wired::regex_cache` wherever `ReqCtx` is built.

- [ ] **Step 2: Modify `ModelPermission::new` to use the cache**

In `crates/gw-auth/src/model_permissions.rs` find `impl ModelPermission { pub fn new(pattern, effect) }`. Either:

A) Add a parallel constructor `new_with_cache(cache: &RegexCache, pattern: &str, effect)` that uses the shared regex, OR
B) Replace the underlying field type from `Regex` to `Arc<Regex>` and require all constructors to take a cache reference.

Choose **B** to ensure no caller bypasses the cache. Concretely:

```rust
use gw_runtime::{RegexCache, CompileError};
use regex::Regex;
use std::sync::Arc;

pub struct ModelPermission {
    pattern: Arc<str>,
    regex: Arc<Regex>,
    effect: PermissionEffect,
}

impl ModelPermission {
    pub fn new(cache: &RegexCache, pattern: &str, effect: PermissionEffect) -> Result<Self, CompileError> {
        let regex = cache.get_or_compile(pattern)?;
        Ok(Self { pattern: Arc::from(pattern), regex, effect })
    }

    pub fn regex(&self) -> &Regex { &self.regex }
}
```

`gw-auth/Cargo.toml`: add `gw-runtime = { path = "../gw-runtime" }`.

- [ ] **Step 3: Update call site in `request.rs:1651`**

```rust
let permission = match ModelPermission::new(&ctx.regex_cache, pattern, PermissionEffect::Allow) {
    Ok(permission) => permission,
    Err(err) => {
        tracing::warn!(
            target: "gw_proxy::request_filter",
            error = %err,
            pattern,
            "model_whitelist_regex compile failed; denying request"
        );
        return Err(model_whitelist_regex_denied_error(
            "model_whitelist_regex compile failed",
        ));
    }
};
```

- [ ] **Step 4: Update every other caller of `ModelPermission::new`**

Run: `grep -RIn 'ModelPermission::new' crates/`
Expected: a small number of sites. Each needs a `&RegexCache` available — thread it from the nearest ctx/state.

- [ ] **Step 5: Compile, lint, test**

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p gw-auth
cargo test -p gw-proxy
```

Expected: clean.

- [ ] **Step 6: Add cache-hit unit test**

In `crates/gw-runtime/src/regex_cache.rs` `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn concurrent_first_compile_only_once() {
    use std::sync::Arc;
    use std::thread;
    let cache = Arc::new(RegexCache::new(RegexCacheConfig::default()));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let c = cache.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                c.get_or_compile("^foo-(bar|baz)-\\d+$").unwrap();
            }
        }));
    }
    for h in handles { h.join().unwrap(); }
    // Moka doesn't expose internal compile-count, but cache size must be 1.
    assert_eq!(cache.len(), 1);
}
```

Run: `cargo test -p gw-runtime regex_cache`.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-auth/ crates/gw-proxy/ crates/gw-runtime/src/regex_cache.rs
git commit -m "feat(gw-auth,gw-proxy): ModelPermission via shared RegexCache"
```

---

## Task 7.1: Apply BackpressureGate at `early.rs`

**Files:**
- Modify: `crates/gw-proxy/src/filters/early.rs`
- Modify: `crates/gw-proxy/src/ctx.rs` (add `backpressure_gate` field)

- [ ] **Step 1: Add field to `ReqCtx`**

```rust
use gw_runtime::BackpressureGate;
pub struct ReqCtx {
    // ...
    pub backpressure_gate: Arc<BackpressureGate>,
}
```

Construct from `Wired::backpressure_gate`.

- [ ] **Step 2: Check at the earliest possible filter point**

Find the entry function in `crates/gw-proxy/src/filters/early.rs`. It already does an existing check (search for `total_streams` / `GLOBAL_STREAM_CAP`). Add the gate check first:

```rust
use gw_runtime::Admit;

if matches!(ctx.backpressure_gate.admit(), Admit::Reject) {
    // Opaque 503 — do not leak which hook tripped.
    let body = serde_json::json!({
        "error": {
            "type": "service_unavailable",
            "message": "Service temporarily unavailable. Please retry later."
        }
    });
    let mut resp = pingora::http::ResponseHeader::build(503, None)?;
    resp.insert_header("retry-after", "2")?;
    resp.insert_header("content-type", "application/json")?;
    session.write_response_header(Box::new(resp), false).await?;
    session.write_response_body(Some(bytes::Bytes::from(body.to_string().into_bytes())), true).await?;
    // Internal trace — useful for ops; not sent to client.
    tracing::warn!(target: "gw_proxy::backpressure", "request rejected due to backpressure");
    return Ok(true); // short-circuit
}
```

> The exact API for writing a synthetic response in this codebase is whatever the existing `early.rs` already uses for the `GLOBAL_STREAM_CAP` early-reject path. Mirror that.

- [ ] **Step 3: Compile, lint**

```bash
cargo check -p gw-proxy
cargo clippy -p gw-proxy --all-targets -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/early.rs crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-proxy): BackpressureGate admission check at early.rs (opaque 503)"
```

---

## Task 8.1: Documentation sync

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: `docs/ROADMAP.md`
- Modify: `CLAUDE.md` and `AGENTS.md` (mirror)

> Per CLAUDE.md sync rule: doc updates land in the same commit as the matching code change. This task is the final consolidation — if earlier tasks omitted incidental doc edits, fix them here.

- [ ] **Step 1: Update `docs/architecture.md`**

In §Workspace layout add `gw-runtime` to the dependency DAG. In §Configuration document each new `[runtime.*]` and `[proxy.archive]` table with column 1 = key, column 2 = default, column 3 = description.

In §Invariants add:
```
- Every long-running tokio task runs under `gw_runtime::Supervisor`.
- The 503 returned by BackpressureGate must never reveal which hook tripped.
- BillingBacklog is unbounded by construction — pressure shows as
  `gw_billing_backlog_pending_count` rising, not as dropped envelopes.
```

In §Deliberate behaviours add:
```
- `/livez` remains 200 during drain so k8s does not restart the Pod and
  lose the in-memory BillingBacklog. `/readyz` is the only health endpoint
  k8s should consult for traffic.
- 503 under backpressure carries `Retry-After: 2`. The 2-second window is
  deliberately short: we want clients to retry quickly so existing in-flight
  work can drain the resource pressure.
```

- [ ] **Step 2: Update `docs/workflow.md`**

In the Pingora filter chain section add early.rs's BackpressureGate check as the very first filter step. In a new "Shutdown sequence" subsection document phase 0 → 1 → 2 → Stopped, the role of each, what `/readyz` and `/livez` report at each, and the dump-on-exit behaviour.

- [ ] **Step 3: Update `docs/spec/06-billing-ledger.md`**

- DLQ cascade: Redis XADD → SpillFile (sharded) → BillingBacklog (unbounded).
- Must-deliver contract reaffirmed with the boundary condition: drop is only acceptable if BackpressureGate already rejected the request *before* a UsageRecord existed.
- BacklogRecovery on bootstrap; `backlog_shutdown_<ts>.jsonl` round-trip.

- [ ] **Step 4: Update `docs/ROADMAP.md`**

Remove the now-implemented items: SIGTERM handler, supervisor universalisation, DLQ async I/O, archive buffer cap, regex cache. Leave the out-of-scope items from spec §Appendix as the still-deferred list.

- [ ] **Step 5: Update `CLAUDE.md` and `AGENTS.md`**

Mirror any newly-added "Where to look first when…" entries (e.g. "Debugging a 503 with no reason → check `gw_backpressure_active` and the four hook gauges"). Keep the two files byte-identical except their title.

- [ ] **Step 6: Run invariant check + final test sweep**

```bash
bash scripts/check-invariants.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace -- --ignored
```

Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add docs/ CLAUDE.md AGENTS.md
git commit -m "docs: sync architecture/workflow/spec/roadmap with resilience fixes"
```

---

## Self-Review Checklist (run after all tasks complete)

- [ ] Every `- [ ]` step has executable content (code, command, file paths) — no "TBD".
- [ ] Every spec section has at least one task implementing it. Confirmed:
  - §1 Architecture → T1.1
  - §2 T1 primitives → T1.2–T1.7
  - §2 T2 SIGTERM → T2.1–T2.3
  - §2 T3 Supervisor application → T3.1–T3.4
  - §2 T4 DLQ + Backlog → T4.1–T4.5
  - §2 T5 archive cap + spill → T5.1–T5.4
  - §2 T6 regex cache → T6.1
  - §3 Configuration → T2.1 (config struct) + T2.1 (default.toml)
  - §4 Backpressure → T7.1
  - §5 Testing → unit tests inline in T1.2/1.4/1.5/1.6/1.7/4.2/5.4/6.1; integration tests in T2.2/3.2/4.4/4.5/5.4
  - §6 Doc sync → T8.1
- [ ] Type/method names consistent across tasks (e.g. `ShutdownToken`, `Supervisor::new`, `RegexCache::get_or_compile`, `BillingBacklog::push`, `ArchiveState::append_chunk`).
- [ ] No placeholder tasks. (T3.2 step 6 contains a `todo!()` test stub — explicitly flagged in the step.)
- [ ] Each task ends with a commit. No batched commits.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-11-resilience-fixes.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Controller dispatches a fresh subagent per task, two-stage review (spec compliance → code quality) between tasks. Best for this plan: tasks are mostly mechanical after T1 and benefit from focused subagent attention.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`. Batched checkpoints; useful if you want to keep the loop tight in one place.

**Which approach?**

