# WS-4 Panic Recovery & Supervisor Invariant — TDD Sub-Plan

> **Status**: TDD sub-plan, derived from `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-4 and §5, and `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 (重要 1/2), §4 WS-4 REV-FINAL-V8..V12, §4 WS-2 REV-FINAL-W11 (Rust portion absorbed into this WS).
> **Mirror**: byte-identical at `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-4-panic-supervisor.md` except this header (single-line title).
> **Audience**: executor with no Rust/Go expertise. Every task is self-contained — 8 sections, literal file:line, literal code snippets, literal commands.
> **Drafted**: 2026-06-17
>
> **Source verification policy (v2 §0 重要 1+2)**: every audit/critique claim below has been Read-verified against the actual source file before being lifted into a task. Where v1/v2 wording diverges from the source the discrepancy is recorded in the **【源码验证】** section. Candidate fixes from v2 REV-FINAL-* are listed as alternatives with a short ADR-style reason for the choice. If the executor finds further drift during TDD, **source wins** — record the deviation in the same section and adjust.

---

## 0. Scope & coverage

| Finding | Source | One-liner | Task(s) |
|---|---|---|---|
| H1 | Go audit P0-HIGH | scheduler-runner panic kills process | T7 (via Supervised migration) |
| H2 | Go audit P0-HIGH | archive drainer/sink panic kills process | T7 (via Supervised migration) |
| Panic-site 1 | Go audit P1-MED (downgraded) | `business.ShardFleet.Run` shard goroutines bare `go func` | T7 |
| Panic-site 2 | Go audit P1-MED (downgraded) | `billing.Settler.drainFleetAcks` started with bare `go func() { _ = ... }()` | T7 |
| Panic-site 5 | Go audit P1-MED (downgraded) | `cache.Invalidator.Run` started bare in `binaries/web/main.go` | T2, T7 |
| S3 | Go new finding (sonnet `ab6bb6e04ee3458b7`) | all-in-one single errgroup makes worker errors kill HTTP listener | T3 |
| S4 | Go new finding | binaries/web cache invalidator is a bare goroutine outside any supervisor | T2 |
| R16 | Rust new finding (sonnet `ab9a0b56c409b36bd`) | `DetailedArchive::start` workers spawned with bare `tokio::spawn` | T9 |
| R17 | Rust new finding | `QuotaStateProbeRunner::start` spawned bare | T9 |
| R18 | Rust new finding | `spawn_heartbeat_task` (instance_id) spawned bare; panic loses TTL, TTL-expiry triggers dead-instance misclassification | T9 |
| REV-FINAL-V8 | v2 §4 WS-4 | Go `Supervised` with **concrete** `RestartPolicy` fields; Rust `Supervisor` mirror; `unhealthyFlag` on `/readyz` | T1, T4, T8 |
| REV-FINAL-V9 | v2 §4 WS-4 | CI lint via `go/analysis` AST walker, **not regex**; Rust `syn` parse | T5, T11 |
| REV-FINAL-V10 | v2 §4 WS-4 | `binaries/web/main.go` 3 bare `go func` rewritten (verified: lines 67, 74, 79) | T2 |
| REV-FINAL-V11 | v2 §4 WS-4 | `/readyz` aggregates `SupervisorRegistry.AllHealthy()` | T6 |
| REV-FINAL-V12 | v2 §4 WS-4 | Rust `supervised_spawn` vs `one_shot_spawn` dual API; lint exempts `#[cfg(test)]` | T8, T9, T11 |
| REV-FINAL-W11 | v2 §4 WS-2 (Rust audit owned by WS-4) | Existing `gw-runtime/src/supervisor.rs:51` `Supervisor` already covers panic + RestartPolicy + ShutdownToken; the gap is a `one_shot_spawn` sibling + a registry surface for `/readyz` + a thin `supervised_spawn` helper that wraps the existing builder. **Extend**, do not new-crate. | T8 |

**Out of scope** (handled in sibling WS):
- Generic 工作流 supervisor design for *all* Go workers (WS-2 §S2.1/S2.2 owns the broader sink/drainer lifecycle; WS-4 only carves the `Supervised`/`OneShot` API surface so WS-4 migrations land first).
- Tunnel/JoinSet/scopeguard work (WS-2 REV-FINAL-W8..W10).
- `cancel-on-error` semantics for billing settler streams (WS-1 / WS-2).

**Dependency on WS-2**: WS-2 owns the broader migration of every sink/drainer to `Supervised`. WS-4 lands the **interface and the proof points** (Panic-site 1/2/5, S3, S4, R16/17/18); WS-2 then sweeps remaining call sites. Per v2 REV-003d the supervisor interface must be merged by D1 so WS-4 can start D3.

---

## 1. Architecture summary (executor reads once, then dives into tasks)

### 1.1 Go side

- New file `shared/runtime/supervisor.go` adds **two** top-level functions and a registry:
  - `Supervised(name string, policy RestartPolicy, f func(ctx context.Context) error) Worker` — long-running tasks. Panic-recover + restart with backoff + ctx-aware exit. Failure budget controls when the worker stops restarting and flips its `unhealthy` flag.
  - `OneShot(name string, f func(ctx context.Context) error) Worker` — explicitly **no** restart. CI lint allows this only for boot/shutdown sequences and idempotent migrations.
  - `SupervisorRegistry` — process-singleton; every `Supervised`/`OneShot` registers; `/readyz` aggregates via `AllHealthy()`.
- The **existing** `RunWorkers` (`shared/runtime/supervisor.go:12`) becomes a thin adapter: it accepts `[]Worker` and wraps each in `Supervised(name, defaultPolicy, w.Run)` unless the worker already opted in.
- `binaries/web/main.go` (3 bare `go func` at lines 67, 74, 79) and `binaries/all-in-one/main.go` (single errgroup) are rewritten: HTTP listener gets its **own** errgroup; workers get their **own** errgroup; worker errgroup error never cancels HTTP errgroup. Shutdown order is strict (S2 fix): `srv.Shutdown` → cancel worker ctx → `wg.Wait` → `pool.Close`.
- `/readyz` upgrades from `200 always` to `200 ⇔ SupervisorRegistry.AllHealthy() && deps healthy`.
- CI lint (`scripts/check-goroutines/`) is a `go/analysis` analyzer. AST-walks the package, flags any `*ast.GoStmt` whose callee is **not** `runtime.Supervised(...)` or `runtime.OneShot(...)`. Allowlist file lets std-lib helpers (signal.Notify, httptest, etc.) pass.

### 1.2 Rust side

- `crates/gw-runtime/src/supervisor.rs:51` already implements `Supervisor` with `RestartPolicy` (max_restarts, window, backoff_initial, backoff_max, backoff_multiplier, backoff_jitter), `catch_unwind`, ShutdownToken propagation, and an `unhealthy` flag. **REV-FINAL-W11 confirmed**: this is the right base; do not new-crate.
- Add **two** convenience entry points in the same file:
  - `supervised_spawn(name, policy, token, factory) -> JoinHandle<()>` — small free function that today is the inline `Supervisor::new(...).spawn(...)` boilerplate.
  - `one_shot_spawn(name, token, fut) -> JoinHandle<Result<(), anyhow::Error>>` — explicit single-shot. Panics in the future are caught and logged; restart is **not** attempted; the unhealthy flag flips iff the future returned `Err` or panicked.
- Add a `SupervisorRegistry` (process-singleton) that holds every `unhealthy: Arc<AtomicBool>` flag; `/readyz` (health server in `gw-gateway/src/health.rs`) aggregates.
- `crates/gw-store/src/managed_loop.rs:33` `Supervisor::spawn` already covers tick-driven loops. **Do not duplicate**; instead make `managed_loop::Supervisor::spawn` register itself with the new `SupervisorRegistry` so `/readyz` sees it. The `Fatal` path flips unhealthy.
- The 3 bare `tokio::spawn` sites (R16/R17/R18) are migrated:
  - R16 `DetailedArchive::start` (`crates/gw-ledger/src/detailed_archive.rs:134`)
  - R17 `QuotaStateProbeRunner::start` (`crates/gw-store/src/quota_state.rs:141`)
  - R18 `spawn_heartbeat_task` (`crates/gw-ledger/src/instance_id.rs:100`)
- CI lint (`scripts/check-tokio-spawn.rs`) parses with `syn` (Rust crate `syn`) and flags `tokio::spawn` outside an allowed-callsite list. Exempts `#[cfg(test)]` items.

### 1.3 What `RestartPolicy` looks like, concretely

```go
// shared/runtime/supervisor.go
type PanicAction int

const (
    PanicActionRestart       PanicAction = iota // default — log + sleep + restart
    PanicActionFatalErrgroup                    // bubble up; caller errgroup decides
)

type RestartPolicy struct {
    MaxConsecutivePanics int           // 0 = use default (5)
    BackoffInitial       time.Duration // 0 = use default (100ms)
    BackoffMax           time.Duration // 0 = use default (30s)
    JitterFrac           float64       // 0..1, default 0.2
    PanicAction          PanicAction
    BudgetWindow         time.Duration // 0 = use default (60s) — failures within window count toward budget
}
```

Per-worker recipes (T4 produces this table):

| Worker | Policy preset | Reason |
|---|---|---|
| billing.Settler | `RestartPolicy{MaxConsecutivePanics: 10, BackoffInitial: 200*time.Millisecond, BackoffMax: 10*time.Second, JitterFrac: 0.3}` | needs to recover from XAUTOCLAIM Redis blips quickly |
| billing.ShardFleet shard | inherits parent (settler) `Supervised` envelope; per-shard panics restart only that shard | shards are independent CRC32 buckets, isolation by design |
| billing.drainFleetAcks | `RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 100*time.Millisecond, BackoffMax: 5*time.Second}` | bounded by AckChannel depth |
| cache.Invalidator | `RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 500*time.Millisecond, BackoffMax: 30*time.Second}` | downstream is Redis XREADGROUP; long backoff acceptable |
| scheduler.worker (H1) | `RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 500*time.Millisecond, BackoffMax: 30*time.Second}` | recurring jobs; budget burn should page |
| archive.drainer (H2) | `RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 200*time.Millisecond, BackoffMax: 10*time.Second}` | sink-coupled |
| monitoring/audit sink | default | low-traffic |

---

## 2. Task list (each task uses the 8-section format)

> Tasks ordered so dependencies chain. Numbered T1..T13. After T1 the executor can parallelize T2/T3/T4 (Go side) and T8/T9 (Rust side).

---

### TASK T1 — Implement `Supervised` / `OneShot` / `SupervisorRegistry` in `shared/runtime/supervisor.go`

**【是什么 What it IS】**
- Add three new top-level symbols to the file `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/supervisor.go` (currently 31 lines): `Supervised`, `OneShot`, `SupervisorRegistry`.
- Add concrete `RestartPolicy` struct as defined in §1.3.
- Keep the existing `RunWorkers` function compatible; this task does **not** rewrite call sites — it only adds the new surface.

**【不是什么 What it is NOT】**
- ❌ Not a rewrite of `RunWorkers` (that is T7).
- ❌ Not a change to `binaries/web/main.go` or `binaries/all-in-one/main.go` (those are T2, T3).
- ❌ Not the lint analyzer (T5).
- ❌ Not the `/readyz` aggregation (T6).
- ❌ Not the Rust supervisor (T8).

**【为什么 Why this exists / why bug occurs】**
- v1 §4 WS-4 and v2 REV-FINAL-V8 require a concrete `RestartPolicy` with named fields, not the vague `Safely(...)` that v1 originally proposed (打回 by adversarial sonnet `a107a6254359f127c`: "errgroup connascence kills siblings").
- Without this interface there is nowhere for T2/T3/T7 to migrate `go func() { ... }()` call sites to, and no place for T6 `/readyz` to read healthiness from.

**【做什么 DO】**
1. Edit `shared/runtime/supervisor.go`:
   - Add the `RestartPolicy` struct (§1.3 above, literal).
   - Add `PanicAction` enum (§1.3, literal).
   - Add `SupervisorRegistry` (a `sync.Map[string, *atomic.Bool]`; `Register(name) *atomic.Bool`, `AllHealthy() bool`, `Unhealthy() []string`).
   - Add `var DefaultSupervisorRegistry = NewSupervisorRegistry()` package var (testable indirection — production uses default, tests inject).
   - Add `func Supervised(name string, policy RestartPolicy, run func(ctx context.Context) error) Worker` that returns a `Worker{Name: name, Run: <wrapped>}`. The wrapper:
     - On entry: register `name` with registry → get `*atomic.Bool` healthy flag (default true).
     - Loop: `catch panic in run(ctx)`; on panic increment counter + sleep backoff; on `ctx.Err()` return immediately.
     - When consecutive panics exceed `policy.MaxConsecutivePanics` within `policy.BudgetWindow`, set healthy=false and return; do **not** keep restarting forever.
     - On clean `run` return without ctx cancel: reset counter, sleep `policy.BackoffInitial`, retry (typical loop-tick worker semantics).
     - Use `time.AfterFunc`/`time.NewTimer` with jitter via `policy.JitterFrac` (deterministic — seed from `time.Now().UnixNano()`).
   - Add `func OneShot(name string, run func(ctx context.Context) error) Worker`: registers; runs once; panic → log + healthy=false + return error; success → healthy stays true.
2. Add a new test file `shared/runtime/supervisor_health_test.go` covering:
   - Panic in run → restart counter increments → healthy stays true until budget burn.
   - Budget burn → healthy=false → `Worker.Run` returns.
   - Ctx cancel → exits without flipping healthy=false.
   - `OneShot` panic → healthy=false + Worker.Run returns error.

**【不做什么 DON'T】**
- ❌ Don't use `golang.org/x/sync/errgroup` directly inside `Supervised` — it conflates "this worker failed" with "this errgroup must cancel", which is exactly the connascence the original `Safely(...)` proposal was打回 for.
- ❌ Don't swallow `context.Canceled` / `context.DeadlineExceeded` as panics — those are clean exits.
- ❌ Don't use `recover()` outside a deferred function inside the worker goroutine — Go semantics require deferred recover.
- ❌ Don't register the worker name multiple times — second registration must be a hard `panic("supervisor: duplicate worker name " + name)` to surface accidental boot-time collisions.
- ❌ Don't add metrics yet (T13 wires Prom metric).

**【怎么做 HOW (literal)】**

Patch `shared/runtime/supervisor.go` — append after the existing `RunWorkers` (line 31). Final file shape (illustrative — preserve current lines 1-31 verbatim, append below):

```go
// --- Supervised / OneShot / SupervisorRegistry --------------------------------

type PanicAction int

const (
    PanicActionRestart PanicAction = iota
    PanicActionFatalErrgroup
)

type RestartPolicy struct {
    MaxConsecutivePanics int
    BackoffInitial       time.Duration
    BackoffMax           time.Duration
    JitterFrac           float64
    PanicAction          PanicAction
    BudgetWindow         time.Duration
}

func (p RestartPolicy) withDefaults() RestartPolicy {
    if p.MaxConsecutivePanics <= 0 {
        p.MaxConsecutivePanics = 5
    }
    if p.BackoffInitial <= 0 {
        p.BackoffInitial = 100 * time.Millisecond
    }
    if p.BackoffMax <= 0 {
        p.BackoffMax = 30 * time.Second
    }
    if p.JitterFrac <= 0 {
        p.JitterFrac = 0.2
    }
    if p.BudgetWindow <= 0 {
        p.BudgetWindow = 60 * time.Second
    }
    return p
}

type SupervisorRegistry struct {
    mu      sync.Mutex
    healthy map[string]*atomic.Bool
}

func NewSupervisorRegistry() *SupervisorRegistry {
    return &SupervisorRegistry{healthy: map[string]*atomic.Bool{}}
}

func (r *SupervisorRegistry) Register(name string) *atomic.Bool {
    r.mu.Lock()
    defer r.mu.Unlock()
    if _, ok := r.healthy[name]; ok {
        panic("supervisor: duplicate worker name " + name)
    }
    b := &atomic.Bool{}
    b.Store(true)
    r.healthy[name] = b
    return b
}

func (r *SupervisorRegistry) AllHealthy() bool {
    r.mu.Lock()
    defer r.mu.Unlock()
    for _, b := range r.healthy {
        if !b.Load() {
            return false
        }
    }
    return true
}

func (r *SupervisorRegistry) Unhealthy() []string {
    r.mu.Lock()
    defer r.mu.Unlock()
    var out []string
    for n, b := range r.healthy {
        if !b.Load() {
            out = append(out, n)
        }
    }
    sort.Strings(out)
    return out
}

var DefaultSupervisorRegistry = NewSupervisorRegistry()

func Supervised(name string, policy RestartPolicy, run func(ctx context.Context) error) Worker {
    policy = policy.withDefaults()
    healthy := DefaultSupervisorRegistry.Register(name)

    return Worker{
        Name: name,
        Run: func(ctx context.Context, d Deps) error {
            failures := make([]time.Time, 0, policy.MaxConsecutivePanics+1)
            backoff := policy.BackoffInitial
            for {
                if err := ctx.Err(); err != nil {
                    return err
                }
                exit := runSafely(ctx, run, d)
                if exit.ctxDone {
                    return exit.err
                }
                if exit.panicked || exit.err != nil {
                    failures = trimFailures(append(failures, time.Now()), policy.BudgetWindow)
                    if len(failures) > policy.MaxConsecutivePanics {
                        healthy.Store(false)
                        d.Log.Error("supervisor: budget burn", "worker", name,
                            "panics_in_window", len(failures), "window", policy.BudgetWindow)
                        if policy.PanicAction == PanicActionFatalErrgroup && exit.err != nil {
                            return exit.err
                        }
                        return nil
                    }
                    if policy.PanicAction == PanicActionFatalErrgroup && exit.panicked {
                        healthy.Store(false)
                        return fmt.Errorf("supervisor %s panicked: %v", name, exit.panicVal)
                    }
                    sleepWithJitter(ctx, backoff, policy.JitterFrac)
                    backoff = nextBackoff(backoff, policy.BackoffMax)
                    continue
                }
                backoff = policy.BackoffInitial
                sleepWithJitter(ctx, policy.BackoffInitial, policy.JitterFrac)
            }
        },
    }
}

func OneShot(name string, run func(ctx context.Context) error) Worker {
    healthy := DefaultSupervisorRegistry.Register(name)
    return Worker{
        Name: name,
        Run: func(ctx context.Context, d Deps) error {
            exit := runSafely(ctx, run, d)
            if exit.ctxDone {
                return exit.err
            }
            if exit.panicked {
                healthy.Store(false)
                return fmt.Errorf("oneshot %s panicked: %v", name, exit.panicVal)
            }
            if exit.err != nil {
                healthy.Store(false)
            }
            return exit.err
        },
    }
}

type runExit struct {
    err      error
    panicked bool
    panicVal any
    ctxDone  bool
}

func runSafely(ctx context.Context, run func(context.Context) error, d Deps) (exit runExit) {
    defer func() {
        if r := recover(); r != nil {
            exit.panicked = true
            exit.panicVal = r
            if d.Log != nil {
                d.Log.Error("supervisor: panic recovered", "panic", fmt.Sprintf("%v", r),
                    "stack", string(debug.Stack()))
            }
        }
    }()
    err := run(ctx)
    if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
        exit.ctxDone = true
    }
    exit.err = err
    return
}

func trimFailures(in []time.Time, window time.Duration) []time.Time {
    cutoff := time.Now().Add(-window)
    out := in[:0]
    for _, t := range in {
        if t.After(cutoff) {
            out = append(out, t)
        }
    }
    return out
}

func nextBackoff(cur, max time.Duration) time.Duration {
    n := cur * 2
    if n > max {
        return max
    }
    return n
}

func sleepWithJitter(ctx context.Context, base time.Duration, jitter float64) {
    delta := time.Duration(float64(base) * jitter * (2*randFloat() - 1))
    d := base + delta
    if d <= 0 {
        d = base
    }
    t := time.NewTimer(d)
    defer t.Stop()
    select {
    case <-ctx.Done():
    case <-t.C:
    }
}

func randFloat() float64 {
    return float64(time.Now().UnixNano()%1_000_000) / 1_000_000.0
}
```

Imports to add to the file's import block: `errors`, `fmt`, `runtime/debug`, `sort`, `sync`, `sync/atomic`, `time`.

**【源码验证 Source verification】**
- Read `shared/runtime/supervisor.go` (lines 1-31): file currently exports only `RunWorkers`. Confirmed — no `Supervised` symbol exists, so adding it is purely additive.
- Read `shared/runtime/supervisor_test.go` (lines 1-47): existing tests use `Worker{Name, Run}`; the new API must preserve this `Worker` shape so existing tests stay green. Confirmed.
- Read `shared/runtime/module.go` (lines 24-27): `Worker` is `{Name string; Run func(ctx context.Context, d Deps) error}`. Confirmed.
- ADR — chose **wrap-as-`Worker`** over a brand-new `Worker2` type: the existing `Module.Workers() []Worker` contract is consumed by `binaries/*/main.go` and changing it cascades into 21 modules. Wrap is additive.

**【如何验证 VERIFY】**
1. `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime && go vet ./...` — expect no output (success).
2. `go test ./shared/runtime/ -run TestSupervised -race -count=10 -v` — expects:
   - `TestSupervised_PanicRestartsWithinBudget` PASS
   - `TestSupervised_BudgetBurnFlipsHealthy` PASS
   - `TestSupervised_CtxCancelExitsHealthy` PASS
   - `TestOneShot_PanicFlipsHealthy` PASS
   - All under `-race` with no DATA RACE warnings, 10 iterations each.
3. `golangci-lint run ./shared/runtime/...` — zero warnings.
4. Existing test must stay green: `go test ./shared/runtime/ -run TestRunWorkers -race -count=5` — PASS.

---

### TASK T2 — Rewrite `binaries/web/main.go` to use `Supervised`; split HTTP from worker errgroups

**【是什么 What it IS】**
- Edit `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/web/main.go`.
- Replace the three bare `go func()` blocks at lines 67-72 (HTTP listener), 74-78 (cache invalidator), 79-83 (module workers) with two `errgroup.Group`s (HTTP / workers) and `Supervised` wrappers.
- Strict shutdown order: `srv.Shutdown` → cancel worker ctx → `wg.Wait` → `pool.Close` (the original code does `defer pool.Close()` and `defer rdb.Close()` at lines 36, 42, which fires **before** workers drain — that is S2; this task fixes it).

**【不是什么 What it is NOT】**
- ❌ Not `binaries/all-in-one/main.go` (T3).
- ❌ Not module-level worker definitions (each module's `Workers()` already returns `[]runtime.Worker`; T7 wires those through `Supervised`).
- ❌ Not the `/readyz` change (T6).
- ❌ Not the cache invalidator's internal logic (the only change here is **who calls it**, not what it does).

**【为什么 Why this bug occurs】**
- v2 REV-FINAL-V10 + v1 finding S2: `binaries/web/main.go` lines 67/74/79 are three bare `go func` blocks; line 36 `defer pgPool.Close()` and line 42 `defer rdb.Close()` fire when `main` returns — but main returns after `<-ctx.Done()` on line 85, before any of the three goroutines have drained. The pool gets closed under workers' feet → in-flight queries return "conn closed" → ack-before-side-effect leak (S2).
- Panic in any of those goroutines crashes the process (Panic-site 5 — cache invalidator).

**【做什么 DO】**
1. Read `binaries/web/main.go` (90 lines).
2. Rewrite the body of `main()` so the three bare goroutines become:
   - HTTP listener: stays as a single `go func` inside its **own** errgroup `httpG, httpCtx := errgroup.WithContext(ctx)`. HTTP errors do not cancel worker ctx.
   - Cache invalidator: registered via `runtime.Supervised("cache.invalidator", recipe, ...)` and included in the worker errgroup.
   - Module workers: each `[]runtime.Worker` element is wrapped with `runtime.Supervised(w.Name, recipe, w.Run)` and run in a single worker errgroup.
3. Shutdown sequence (replacing the current 88-89 block):
   ```
   <-ctx.Done()
   shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
   defer cancel()
   _ = srv.Shutdown(shutdownCtx)   // 1. stop accepting new HTTP
   workerCancel()                  // 2. signal workers to stop
   _ = workerG.Wait()              // 3. wait for workers to drain
   _ = httpG.Wait()                // 4. listener returns ErrServerClosed
   // pool.Close / rdb.Close fire via deferred stack last
   ```
4. Move `defer pgPool.Close()` and `defer rdb.Close()` so they fire **after** the wait sequence — option A: replace the `defer`s with explicit calls at the end of `main`. Option B: leave `defer`s but put the wait logic in a non-deferred block; deferred close still fires last because deferreds are LIFO.
   - Chosen: **Option A** (explicit Close calls) because the executor must verify Close ordering against the supervisor in a single place. ADR below.

**【不做什么 DON'T】**
- ❌ Don't put HTTP listener and workers in the same errgroup — that is exactly S3 (worker error cancels HTTP ctx → in-flight requests die).
- ❌ Don't remove the `errors.Is(err, http.ErrServerClosed)` filter — `Shutdown` triggers this and it is the clean-exit signal.
- ❌ Don't add a `RestartPolicy` argument for the HTTP listener — listener is OneShot (lifecycle is bound to `srv.ListenAndServe` semantics). Use `runtime.OneShot("http.listener", ...)`.
- ❌ Don't add `Sleep` between Shutdown and workerCancel — that masks ordering bugs.
- ❌ Don't change the `signal.NotifyContext` block (lines 29-30); rely on it as the trigger.

**【怎么做 HOW (literal)】**

```go
// /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/web/main.go
package main

import (
    "context"
    "errors"
    "net/http"
    "os"
    "os/signal"
    "syscall"
    "time"

    "golang.org/x/sync/errgroup"

    "github.com/cnxx-tech/ailinkmax-go/binaries/web/app"
    cachebiz "github.com/cnxx-tech/ailinkmax-go/modules/cache/business"
    "github.com/cnxx-tech/ailinkmax-go/shared/contracts"
    pcache "github.com/cnxx-tech/ailinkmax-go/shared/platform/cache"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/config"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/log"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/store"
    "github.com/cnxx-tech/ailinkmax-go/shared/runtime"
)

func main() {
    cfg, err := config.Load(os.Getenv("ALM_CONFIG_FILE"))
    if err != nil {
        panic(err)
    }
    logger := log.New(cfg.String("log.level"))

    ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
    defer stop()

    pgPool, err := store.NewPG(ctx, store.PgConfig{DSN: cfg.String("db.url"), Multiplier: 4})
    if err != nil {
        panic(err)
    }
    rdb, err := store.NewRedis(cfg.String("redis.url"))
    if err != nil {
        pgPool.Close()
        panic(err)
    }
    sharedCache := pcache.New(rdb)

    deps := runtime.Deps{
        Cfg: cfg, Log: logger, DB: pgPool, Redis: rdb,
        Reg: contracts.NewRegistry(),
    }
    modules := app.WebModules()
    for _, m := range modules {
        m.Provide(deps, deps.Reg)
    }
    var workers []runtime.Worker
    for _, m := range modules {
        workers = append(workers, m.Workers()...)
    }

    r := runtime.BuildRouter(deps, modules)
    runtime.ServeStatic(r, cfg.String("frontend.dist"))

    addr := cfg.String("http.addr")
    if addr == "" {
        addr = ":8080"
    }
    srv := runtime.NewHTTPServer(addr, r)

    // HTTP errgroup — isolated from workers so worker errors never cancel listener ctx (S3).
    httpG, _ := errgroup.WithContext(ctx)
    httpG.Go(func() error {
        logger.Info("web listening", "addr", addr)
        if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
            return err
        }
        return nil
    })

    // Worker errgroup — separate ctx so HTTP shutdown is the trigger.
    workerCtx, workerCancel := context.WithCancel(context.Background())
    workerG, workerErrCtx := errgroup.WithContext(workerCtx)
    _ = workerErrCtx

    invalidator := cachebiz.NewInvalidator(rdb, sharedCache, logger)
    invalidatorWorker := runtime.Supervised("cache.invalidator",
        runtime.RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 500 * time.Millisecond, BackoffMax: 30 * time.Second},
        invalidator.Run)
    workerG.Go(func() error { return invalidatorWorker.Run(workerCtx, deps) })

    for _, w := range workers {
        wrapped := runtime.Supervised(w.Name,
            runtime.RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 200 * time.Millisecond, BackoffMax: 10 * time.Second},
            func(ctx context.Context) error { return w.Run(ctx, deps) })
        workerG.Go(func() error { return wrapped.Run(workerCtx, deps) })
    }

    <-ctx.Done()
    shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
    defer cancel()

    _ = srv.Shutdown(shutdownCtx)
    workerCancel()
    if err := workerG.Wait(); err != nil && !errors.Is(err, context.Canceled) {
        logger.Error("workers exited with error", "err", err)
    }
    if err := httpG.Wait(); err != nil {
        logger.Error("http exited with error", "err", err)
    }

    rdb.Close()
    pgPool.Close()
    logger.Info("web stopped")
}
```

Notes:
- The three original `go func` blocks (originally at lines 67, 74, 79) are eliminated. `errgroup.Go` is allowed because the closure is gated through `Supervised(...)` — the lint analyzer (T5) special-cases `errgroup.Group.Go` when its argument is a call to `Supervised(...).Run` or `OneShot(...).Run`.
- The `Worker.Run` signature is `func(ctx, Deps)`; `Supervised` produces a `Worker` whose `Run` accepts `(ctx, Deps)`. The wrapped worker still uses `workerCtx` (not the original `ctx`) so HTTP shutdown alone doesn't cancel workers.
- `pgPool.Close()` and `rdb.Close()` now fire **after** `workerG.Wait()` returns — fixing S2.

**【不做什么 DON'T】**
- ❌ Don't keep `defer pgPool.Close()` and `defer rdb.Close()` near lines 36/42 — they fire too early.
- ❌ Don't use `g.Go(invalidator.Run)` directly bare — it bypasses `Supervised`.

**【源码验证 Source verification】**
- Confirmed `binaries/web/main.go` Read: lines 67, 74, 79 are three bare `go func`; lines 36, 42 are `defer pgPool.Close()` / `defer rdb.Close()`. v2 REV-FINAL-V10 wording is **correct** for this file.
- Confirmed `shared/runtime/host.go:30` `BuildRouter` mounts middleware; `/readyz` is at line 46 and returns 200 always — T6 changes this.
- ADR — chose to **drop** the `defer ... .Close()` rather than reorder via LIFO: explicit Close after `workerG.Wait()` is one fewer cognitive hop and matches the v2 requirement that close ordering is single-sourced.

**【如何验证 VERIFY】**
1. `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go build ./binaries/web/` — expect exit 0.
2. New integration test `binaries/web/main_shutdown_test.go` (chaos): boot main inline with stubbed deps; SIGTERM; assert order = (a) `srv.Shutdown` returned (b) `workerG.Wait` returned (c) only **then** `pgPool.Close` was observed. Implement via a fake `pgxpool.Pool` wrapper that records calls.
3. `go test ./binaries/web/ -run TestShutdownOrder -race -count=5 -v` — PASS.
4. `go vet ./binaries/web/...` — zero output.

---

### TASK T3 — Split `binaries/all-in-one/main.go` errgroup into HTTP/worker pair (S3)

**【是什么 What it IS】**
- Edit `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/all-in-one/main.go`.
- Replace the single `errgroup.WithContext(ctx)` at line 74 with two errgroups: one for HTTP listener + `srv.Shutdown`, one for workers + cache invalidator.
- Apply the same `Supervised` wrapping as T2.

**【不是什么 What it is NOT】**
- ❌ Not a change to module-level workers (T7).
- ❌ Not a change to scheduler/archive internals (those modules' workers already get wrapped through `runtime.RunWorkers`; T7 wires `Supervised`).

**【为什么 Why this bug occurs】**
- v1 S3 + v2 REV-FINAL-V10: `binaries/all-in-one/main.go:74-89` runs HTTP listener (line 75-81), `RunWorkers` (line 82), cache invalidator (line 83), and the shutdown goroutine (line 84-89) all in **one** errgroup. If `RunWorkers` returns a non-nil error, errgroup cancels `gctx` → HTTP listener (which observes ctx via Server.BaseContext) → all in-flight HTTP requests die. That is the textbook S3 connascence.

**【做什么 DO】**
1. Read `binaries/all-in-one/main.go` (96 lines).
2. Replace the single errgroup with the same split as T2: `httpG`, `workerG`; workers run in their own ctx so a worker `return err` does not cancel HTTP.
3. Strict shutdown order matches T2.

**【不做什么 DON'T】**
- ❌ Don't share a context between HTTP and workers — even if you "promise" workers won't error.
- ❌ Don't combine cache invalidator into the HTTP errgroup — it must be in workerG so it lives across HTTP-restart-window only if it has its own restart, which `Supervised` provides.

**【怎么做 HOW (literal)】**

```go
// /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/all-in-one/main.go
package main

import (
    "context"
    "errors"
    "net/http"
    "os"
    "os/signal"
    "syscall"
    "time"

    "golang.org/x/sync/errgroup"

    "github.com/cnxx-tech/ailinkmax-go/binaries/web/app"
    "github.com/cnxx-tech/ailinkmax-go/modules/archive"
    cachebiz "github.com/cnxx-tech/ailinkmax-go/modules/cache/business"
    "github.com/cnxx-tech/ailinkmax-go/modules/scheduler"
    "github.com/cnxx-tech/ailinkmax-go/shared/contracts"
    pcache "github.com/cnxx-tech/ailinkmax-go/shared/platform/cache"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/config"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/log"
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/store"
    "github.com/cnxx-tech/ailinkmax-go/shared/runtime"
)

func main() {
    cfg, err := config.Load(os.Getenv("ALM_CONFIG_FILE"))
    if err != nil {
        panic(err)
    }
    logger := log.New(cfg.String("log.level"))

    ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
    defer stop()

    pgPool, err := store.NewPG(ctx, store.PgConfig{DSN: cfg.String("db.url"), Multiplier: 6})
    if err != nil {
        panic(err)
    }
    rdb, err := store.NewRedis(cfg.String("redis.url"))
    if err != nil {
        pgPool.Close()
        panic(err)
    }
    sharedCache := pcache.New(rdb)

    modules := append(app.WebModules(),
        archive.New(), scheduler.New(),
    )

    deps := runtime.Deps{
        Cfg: cfg, Log: logger, DB: pgPool, Redis: rdb,
        Reg: contracts.NewRegistry(),
    }
    for _, m := range modules {
        m.Provide(deps, deps.Reg)
    }

    var workers []runtime.Worker
    for _, m := range modules {
        workers = append(workers, m.Workers()...)
    }

    r := runtime.BuildRouter(deps, modules)
    runtime.ServeStatic(r, cfg.String("frontend.dist"))
    addr := cfg.String("http.addr")
    if addr == "" {
        addr = ":8080"
    }
    srv := newHTTPServer(addr, r)

    httpG, _ := errgroup.WithContext(ctx)
    httpG.Go(func() error {
        logger.Info("web listening", "addr", addr)
        if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
            return err
        }
        return nil
    })

    workerCtx, workerCancel := context.WithCancel(context.Background())
    workerG, _ := errgroup.WithContext(workerCtx)

    invalidator := cachebiz.NewInvalidator(rdb, sharedCache, logger)
    invWorker := runtime.Supervised("cache.invalidator",
        runtime.RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 500 * time.Millisecond, BackoffMax: 30 * time.Second},
        invalidator.Run)
    workerG.Go(func() error { return invWorker.Run(workerCtx, deps) })

    for _, w := range workers {
        w := w
        wrapped := runtime.Supervised(w.Name,
            runtime.RestartPolicy{MaxConsecutivePanics: 5, BackoffInitial: 200 * time.Millisecond, BackoffMax: 10 * time.Second},
            func(ctx context.Context) error { return w.Run(ctx, deps) })
        workerG.Go(func() error { return wrapped.Run(workerCtx, deps) })
    }

    <-ctx.Done()
    shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
    defer cancel()
    _ = srv.Shutdown(shutdownCtx)
    workerCancel()
    if err := workerG.Wait(); err != nil && !errors.Is(err, context.Canceled) {
        logger.Error("workers exited with error", "err", err)
    }
    if err := httpG.Wait(); err != nil {
        logger.Error("http exited with error", "err", err)
    }

    rdb.Close()
    pgPool.Close()
    logger.Info("all-in-one stopped")
}
```

**【源码验证】**
- Confirmed `binaries/all-in-one/main.go:74-89`: single `errgroup.WithContext(ctx)`, all four goroutines share `gctx` → S3 confirmed.
- `newHTTPServer` lives in `binaries/all-in-one/server.go`; signature unchanged.
- ADR — chose to leave the `archive.New(), scheduler.New()` append on the modules list rather than register them as `runtime.Supervised` here, because their `Workers()` already returns `[]runtime.Worker` and the `for _, w := range workers` loop wraps them uniformly. Avoids two registration paths.

**【如何验证 VERIFY】**
1. `go build ./binaries/all-in-one/` — exit 0.
2. New chaos test `binaries/all-in-one/main_isolation_test.go`: simulate a worker panic via injected fake worker; assert HTTP listener stays up (test HTTP `/livez` returns 200 during and after worker panic).
3. `go test ./binaries/all-in-one/ -run TestHttpIsolatedFromWorkerPanic -race -count=5` — PASS.

---

### TASK T4 — Document per-worker `RestartPolicy` recipes in `shared/runtime/policies.go`

**【是什么 What it IS】**
- Create new file `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/policies.go`.
- Export named policy constructors (`PolicyBilling()`, `PolicyCacheInvalidator()`, etc.) so callsites stop hard-coding.

**【不是什么 What it is NOT】**
- ❌ Not a config-file driven system — these are code constants. Operator-overridable via env happens in a later WS (WS-10).

**【为什么 Why this exists】**
- T2/T3/T7 will hard-code recipes in N call sites. v2 REV-FINAL-V8 wants a **single source of truth** per recipe so changing a backoff is one edit.
- Documents intent: future executor sees that `PolicyBilling()` has `MaxConsecutivePanics=10` not `5` because billing tolerance is wider.

**【做什么 DO】**
1. Create file with constructors listed in §1.3 (literal table).
2. Add a `policies_test.go` ensuring each constructor returns the documented fields.

**【不做什么 DON'T】**
- ❌ Don't introduce a config struct now — keep it dead simple, function-per-recipe.

**【怎么做 HOW (literal)】**

```go
// /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/policies.go
package runtime

import "time"

func PolicyBillingSettler() RestartPolicy {
    return RestartPolicy{
        MaxConsecutivePanics: 10,
        BackoffInitial:       200 * time.Millisecond,
        BackoffMax:           10 * time.Second,
        JitterFrac:           0.3,
        BudgetWindow:         120 * time.Second,
    }
}

func PolicyDrainFleetAcks() RestartPolicy {
    return RestartPolicy{
        MaxConsecutivePanics: 5,
        BackoffInitial:       100 * time.Millisecond,
        BackoffMax:           5 * time.Second,
        BudgetWindow:         60 * time.Second,
    }
}

func PolicyCacheInvalidator() RestartPolicy {
    return RestartPolicy{
        MaxConsecutivePanics: 5,
        BackoffInitial:       500 * time.Millisecond,
        BackoffMax:           30 * time.Second,
        BudgetWindow:         300 * time.Second,
    }
}

func PolicyScheduler() RestartPolicy {
    return RestartPolicy{
        MaxConsecutivePanics: 5,
        BackoffInitial:       500 * time.Millisecond,
        BackoffMax:           30 * time.Second,
        BudgetWindow:         300 * time.Second,
    }
}

func PolicyArchiveDrainer() RestartPolicy {
    return RestartPolicy{
        MaxConsecutivePanics: 5,
        BackoffInitial:       200 * time.Millisecond,
        BackoffMax:           10 * time.Second,
        BudgetWindow:         60 * time.Second,
    }
}

func PolicyDefault() RestartPolicy { return RestartPolicy{} } // all zeros → withDefaults() applies
```

T2/T3 then use `runtime.PolicyCacheInvalidator()` and `runtime.PolicyDefault()` instead of inline structs.

**【源码验证】**
- ADR alt-A: use a single map `var Policies = map[string]RestartPolicy{...}` — rejected because it admits typo lookups that compile but mismatch at runtime.
- ADR alt-B: load from `config/policies.toml` — rejected because it adds a config plumbing dependency for one-line constants.

**【如何验证 VERIFY】**
1. `go test ./shared/runtime/ -run TestPolicy -count=1` — PASS asserting each constructor returns documented values.
2. `golangci-lint run ./shared/runtime/` — zero warnings.

---

### TASK T5 — Implement Go AST analyzer `scripts/check-goroutines/`

**【是什么 What it IS】**
- New directory `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/scripts/check-goroutines/` containing a `go/analysis` analyzer named `goroutinecheck` plus a `main.go` driver and a `golangci-lint` plugin manifest.
- The analyzer walks AST `*ast.GoStmt` nodes; for each, climbs to the called function expression. Flags any `go <call>` whose `<call>` is **not** a call to `runtime.Supervised(...).Run(...)`, `runtime.OneShot(...).Run(...)`, `errgroup.Group.Go` where the argument is a literal whose body itself only calls `Supervised(...).Run` or `OneShot(...).Run`, or an entry on the allowlist.
- Scopes the lint to `modules/...` and `binaries/...` packages; `shared/...` is excluded (`Supervised` implementation lives there and uses `go` internally).

**【不是什么 What it is NOT】**
- ❌ Not a regex (v2 REV-FINAL-V9 explicitly bans regex; sonnet `a482ea02ec22ebe61` showed regex false-positive on comments).
- ❌ Not a Rust lint (T11).
- ❌ Not wired into PR review tooling (`make check` integration is in WS-12).

**【为什么 Why this bug occurs】**
- Without an AST analyzer, future PRs reintroduce bare `go func` → invariant rot → S3/S4/Panic-site-5 regress within weeks.

**【做什么 DO】**
1. Create `scripts/check-goroutines/analyzer.go` with `var Analyzer = &analysis.Analyzer{...}`.
2. Implement `run(pass *analysis.Pass)`:
   - For each file: `ast.Inspect`, match `*ast.GoStmt`.
   - For each `GoStmt`, examine `.Call.Fun`:
     - If `*ast.SelectorExpr` with Sel.Name `"Run"` and the receiver type is `runtime.Worker`, OK.
     - If `*ast.FuncLit`, recurse into body — must contain exactly one statement that is `return runtime.Supervised(...).Run(ctx, d)` or `return runtime.OneShot(...).Run(ctx, d)`.
     - Else, `pass.Reportf(stmt.Pos(), "bare go func; use runtime.Supervised or runtime.OneShot")`.
   - Special-case `*ast.CallExpr` whose Fun is `errgroup.Group.Go`: descend into the literal argument and apply the same rule.
3. Allowlist file `scripts/check-goroutines/allowlist.txt`: one `pkg.func` per line; the analyzer reads it from `pass.Pkg.Path()` resolution.
4. CLI driver `scripts/check-goroutines/cmd/main.go` so it can run standalone via `go run ./scripts/check-goroutines/cmd ./modules/... ./binaries/...`.
5. Add `golangci-lint` plugin scaffolding in `.golangci.yml` (a follow-up — for now the standalone driver is the gate).

**【不做什么 DON'T】**
- ❌ Don't lint `*_test.go` files — exempt via `pass.Pkg.Name()` suffix or `_test.go` filename check.
- ❌ Don't lint `shared/runtime/...` — that's where the supervisor lives.
- ❌ Don't try to match on string text — match on `types.Info` resolved identifiers.
- ❌ Don't be silent on errgroup.Go in `binaries/`: T2/T3 use `g.Go(func() error { return wrapped.Run(...) })`, which **is** the allowed form; the analyzer must accept that exact shape and reject `g.Go(func() error { /* arbitrary body */ })`.

**【怎么做 HOW (literal)】**

```go
// scripts/check-goroutines/analyzer.go
package goroutinecheck

import (
    "go/ast"
    "strings"

    "golang.org/x/tools/go/analysis"
    "golang.org/x/tools/go/analysis/passes/inspect"
    "golang.org/x/tools/go/ast/inspector"
)

var Analyzer = &analysis.Analyzer{
    Name:     "goroutinecheck",
    Doc:      "flag bare `go func` and `errgroup.Group.Go` not wrapped in runtime.Supervised/OneShot",
    Requires: []*analysis.Analyzer{inspect.Analyzer},
    Run:      run,
}

func run(pass *analysis.Pass) (any, error) {
    pkg := pass.Pkg.Path()
    if !strings.Contains(pkg, "/modules/") && !strings.Contains(pkg, "/binaries/") {
        return nil, nil
    }
    insp := pass.ResultOf[inspect.Analyzer].(*inspector.Inspector)
    nodeFilter := []ast.Node{(*ast.GoStmt)(nil), (*ast.CallExpr)(nil)}
    insp.Preorder(nodeFilter, func(n ast.Node) {
        switch x := n.(type) {
        case *ast.GoStmt:
            checkCall(pass, x.Call, "bare go statement")
        case *ast.CallExpr:
            if isErrgroupGo(pass, x) {
                if len(x.Args) == 1 {
                    if lit, ok := x.Args[0].(*ast.FuncLit); ok {
                        if !literalIsSupervisedRun(pass, lit) {
                            pass.Reportf(x.Pos(),
                                "errgroup.Group.Go must call runtime.Supervised(...).Run or runtime.OneShot(...).Run; bare closure forbidden")
                        }
                    }
                }
            }
        }
    })
    return nil, nil
}

func checkCall(pass *analysis.Pass, c *ast.CallExpr, kind string) {
    if isSupervisedRun(pass, c) || isOneShotRun(pass, c) {
        return
    }
    if lit, ok := c.Fun.(*ast.FuncLit); ok {
        if literalIsSupervisedRun(pass, lit) {
            return
        }
    }
    pass.Reportf(c.Pos(), "%s; use runtime.Supervised(name, policy, fn).Run(...) or runtime.OneShot(name, fn).Run(...)", kind)
}

// helpers (isSupervisedRun, isOneShotRun, isErrgroupGo, literalIsSupervisedRun) — implement by walking
// the SelectorExpr/Ident chain and resolving via pass.TypesInfo.Uses.
```

Helpers above are sketched; the executor must implement them by inspecting `pass.TypesInfo.Uses[ident].Pkg().Path()` to verify identity rather than string match.

**【源码验证】**
- `go/analysis` is part of `golang.org/x/tools` — already an indirect dep via golangci-lint. Verify via `go list -m golang.org/x/tools`.
- Pattern `binaries/web/main.go` (after T2 lands) must lint clean: lint analyzer must accept the exact `g.Go(func() error { return wrapped.Run(workerCtx, deps) })` shape used by T2/T3.

**【如何验证 VERIFY】**
1. Unit tests `scripts/check-goroutines/analyzer_test.go` using `analysistest.Run`. Fixtures:
   - `testdata/src/baretest/bad.go`: contains `go func() { foo() }()` → expects `// want "bare go statement; use runtime.Supervised"`.
   - `testdata/src/wrappedtest/good.go`: contains `g.Go(func() error { return w.Run(ctx, deps) })` where `w` is `runtime.Supervised(...)` → no diagnostic.
2. Run: `go test ./scripts/check-goroutines/ -count=1` — PASS.
3. Run analyzer over the whole project as a smoke test: `go run ./scripts/check-goroutines/cmd ./modules/... ./binaries/...` — must report **zero** findings after T2, T3, T7 land (otherwise T2/T3/T7 missed a site).

---

### TASK T6 — Wire `SupervisorRegistry` into `/readyz`

**【是什么 What it IS】**
- Edit `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/host.go` line 46.
- Replace `r.Get("/readyz", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusOK) })` with a handler that consults `DefaultSupervisorRegistry.AllHealthy()` and (in a follow-up WS-10 task) the DB/Redis pool pings.

**【不是什么 What it is NOT】**
- ❌ Not the Redis/PG ping integration (that is WS-10 S1). The `/readyz` change here only **adds** the supervisor check; the deps check is a TODO comment for WS-10.
- ❌ Not `/livez` (livez stays 200-always — liveness, not readiness).

**【为什么 Why this bug occurs】**
- v2 REV-FINAL-V11: `Supervised` flips a `healthy *atomic.Bool` when budget burns; without `/readyz` aggregation, the orchestrator (k8s/LB) keeps routing requests to a Pod whose billing settler is dead.

**【做什么 DO】**
1. Edit `host.go:46`.
2. The new handler returns:
   - 200 + JSON `{"ok": true, "unhealthy": []}` when `DefaultSupervisorRegistry.AllHealthy()`.
   - 503 + JSON `{"ok": false, "unhealthy": [...]}` otherwise.

**【不做什么 DON'T】**
- ❌ Don't make the handler block on a slow check (Redis/PG ping belongs to WS-10).
- ❌ Don't add per-call logging — the LB hits this every few seconds.
- ❌ Don't change `/livez` behavior.

**【怎么做 HOW (literal)】**

```go
// shared/runtime/host.go (replacing the line 46 handler)
r.Get("/readyz", func(w http.ResponseWriter, _ *http.Request) {
    unhealthy := DefaultSupervisorRegistry.Unhealthy()
    w.Header().Set("Content-Type", "application/json")
    if len(unhealthy) == 0 {
        w.WriteHeader(http.StatusOK)
        _, _ = w.Write([]byte(`{"ok":true,"unhealthy":[]}`))
        return
    }
    w.WriteHeader(http.StatusServiceUnavailable)
    body, _ := json.Marshal(map[string]any{"ok": false, "unhealthy": unhealthy})
    _, _ = w.Write(body)
})
```

Add `encoding/json` to the imports of `shared/runtime/host.go`.

**【源码验证】**
- Confirmed `shared/runtime/host.go:46` returns 200 always; no existing readiness aggregation. v2 REV-FINAL-V11 wording matches source.

**【如何验证 VERIFY】**
1. New test `shared/runtime/host_readyz_test.go`:
   - Setup: `DefaultSupervisorRegistry = NewSupervisorRegistry()`; register `"x"` and flip flag → false; assert `/readyz` returns 503 and body contains `"x"`.
   - Healthy case → 200, empty unhealthy list.
2. `go test ./shared/runtime/ -run TestReadyz -race -count=5` — PASS.
3. `tests/contract` route test (`tests/contract/...`) must not regress (`/readyz` route still exists).

---

### TASK T7 — Migrate `billing.Settler.drainFleetAcks`, `billing.ShardFleet.Run` shards, scheduler, archive drainer to `Supervised`

**【是什么 What it IS】**
- Edit:
  - `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/billing/business/settler.go:100` — `go func() { _ = s.drainFleetAcks(ctx) }()` → `Supervised`.
  - `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/billing/business/user_shard.go:111-117` — per-shard `go func(sh *shard)` → `Supervised` per shard (one Registry entry per `billing.shard.<n>`).
  - `modules/scheduler/business/*.go` — every bare `go func` in scheduler workers (H1). [Read first; T7 author writes the literal patches.]
  - `modules/archive/business/*.go` — drainer and sink (H2). [Read first.]

**【不是什么 What it is NOT】**
- ❌ Not the cache invalidator (T2 handles it).
- ❌ Not `binaries/all-in-one/main.go` (T3).
- ❌ Not module worker registration changes — modules continue to expose `Workers() []runtime.Worker`; this task wraps the **internal** spawned goroutines.

**【为什么】**
- H1, H2, Panic-site 1, Panic-site 2: these are the four named places where panic kills a worker or the process. T7 closes them via `Supervised`.

**【做什么 DO】**
1. `modules/billing/business/settler.go:100`: replace the bare goroutine with a `Supervised` worker started in `Run`. Choice: rather than spawn inline, return both workers from `Settler.Workers()` (new method) so the binary wraps them uniformly. **However**, settler.Run already runs in a `runtime.Worker`; the inner goroutine is one level deeper.
   - **Inline form** (chosen): wrap the inner goroutine via `Supervised("billing.drain_fleet_acks", runtime.PolicyDrainFleetAcks(), s.drainFleetAcks).Run(ctx, runtime.Deps{Log: s.log})`. The `Deps` only needs `Log` here.
   - **Restructure form** (rejected): hoisting to `Workers()` breaks the current contract where the settler internally decides whether to start the fleet drain based on `s.handler.UsesFleet()`.
2. `modules/billing/business/user_shard.go:109-120`: today `ShardFleet.Run` `for _, sh := range f.shards { wg.Add(1); go func(sh) { ... }() }`. Replace each per-shard `go` with:
   ```go
   sw := runtime.Supervised("billing.shard."+strconv.Itoa(sh.id),
       runtime.PolicyBillingSettler(), sh.Run)
   wg.Add(1)
   go func() { defer wg.Done(); _ = sw.Run(ctx, runtime.Deps{Log: f.log}) }()
   ```
   The outer `go` is allowed because its body is exactly the canonical `Supervised(...).Run(ctx, d)` form the analyzer accepts. (Add `f.log *slog.Logger` to ShardFleet — currently it has none, derive from caller in T7 or pass via `NewShardFleet`.)
3. Repeat for scheduler workers (modules/scheduler/business/...) — Read those files first to enumerate `go` keyword sites.
4. Repeat for archive drainer/sink (modules/archive/business/...) — same.

**【不做什么 DON'T】**
- ❌ Don't change behavior of `drainFleetAcks` itself — just wrap.
- ❌ Don't change how `ShardFleet.Submit` hashes — only the supervisor wrap.
- ❌ Don't register two shards under the same name (each shard uses a unique numeric suffix).
- ❌ Don't add `Supervised` around `s.handler.HandleStream` — that's per-message work, not a long-running goroutine.

**【怎么做 HOW (literal — settler.go:100 snippet)】**

```go
// modules/billing/business/settler.go (around line 99-101)
if s.handler.UsesFleet() {
    drain := runtime.Supervised("billing.drain_fleet_acks", runtime.PolicyDrainFleetAcks(), s.drainFleetAcks)
    go func() { _ = drain.Run(ctx, runtime.Deps{Log: s.log}) }()
}
```

(The outer `go func() { _ = drain.Run(...) }()` is the exact form the analyzer T5 accepts.)

```go
// modules/billing/business/user_shard.go (replace lines 109-120 body)
func (f *ShardFleet) Run(ctx context.Context) error {
    var wg sync.WaitGroup
    for i, sh := range f.shards {
        sh := sh
        i := i
        sw := runtime.Supervised(
            "billing.shard."+strconv.Itoa(i),
            runtime.PolicyBillingSettler(),
            sh.Run,
        )
        wg.Add(1)
        go func() { defer wg.Done(); _ = sw.Run(ctx, runtime.Deps{Log: f.log}) }()
    }
    wg.Wait()
    return ctx.Err()
}
```

Add `f.log *slog.Logger` field to `ShardFleet` (line 54 area) and accept it via `NewShardFleet`; update call sites in `modules/billing/module.go:372` and `settler_test.go:117,163` to pass loggers (use `slog.Default()` in tests).

For scheduler/archive: executor lists their bare `go` sites by running:
```bash
grep -rn "go func" /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/scheduler /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/archive --include='*.go'
```
Then applies the same pattern.

**【源码验证】**
- Confirmed `settler.go:100` literal: `go func() { _ = s.drainFleetAcks(ctx) }()` — matches v1 Panic-site 2.
- Confirmed `user_shard.go:111-117`: per-shard `go func(sh *shard) { defer wg.Done(); _ = sh.Run(ctx) }(sh)` — matches Panic-site 1.
- `ShardFleet` struct (line 54) has fields `{n, shards}` — no logger. Adding one is additive; verify call sites.
- ADR: chose to keep `wg.Wait()` semantics so `ShardFleet.Run` still returns when shards exit. Alternative — replace `wg` with `errgroup` — rejected because shard exits are expected on ctx cancel and we don't want one shard's exit to cancel siblings (each shard's `Supervised` envelope already handles its own restart).

**【如何验证 VERIFY】**
1. `go test ./modules/billing/... -race -count=5` — existing tests stay green (settler_test, user_shard_test).
2. New test `modules/billing/business/settler_supervisor_test.go`: inject a panicking `drainFleetAcks` (stub via interface); assert Settler.Run does not return; Prom counter `gw_supervisor_restart_total{worker="billing.drain_fleet_acks"}` increments.
3. Run T5 analyzer: `go run ./scripts/check-goroutines/cmd ./modules/billing/...` — zero findings.
4. Run T5 analyzer over scheduler/archive — zero findings (proves T7 covered every site).

---

### TASK T8 — Add `supervised_spawn` / `one_shot_spawn` + `SupervisorRegistry` to `crates/gw-runtime/src/supervisor.rs`

**【是什么 What it IS】**
- Extend the existing `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-runtime/src/supervisor.rs` (verified at line 51, `Supervisor` struct with `RestartPolicy` already implemented).
- Add three new public items:
  - `pub fn supervised_spawn<F, Fut>(name, policy, token, factory) -> JoinHandle<()>` — thin wrapper over `Supervisor::new(name, policy, token).spawn(factory)` that also registers with `SupervisorRegistry`.
  - `pub fn one_shot_spawn<Fut>(name, token, fut) -> JoinHandle<Result<(), anyhow::Error>>` — single execution; panic-catch via `catch_unwind`; flips `unhealthy` on Err or panic; **no** restart.
  - `pub static SUPERVISOR_REGISTRY: Lazy<SupervisorRegistry>` (or initialized in `LlmGateway::new`) — holds `Arc<AtomicBool>` per task name.
- Make `gw_store::managed_loop::Supervisor::spawn` (already at `crates/gw-store/src/managed_loop.rs:33`) register its `Fatal`-on-exit signal with `SUPERVISOR_REGISTRY` via a new constructor `ManagedLoop::spawn_with_registry`.

**【不是什么 What it is NOT】**
- ❌ Not a new crate (REV-FINAL-W11 audit confirmed `Supervisor` already exists — extend, do not duplicate).
- ❌ Not a rewrite of the existing `Supervisor::spawn` — that stays.
- ❌ Not the migration of the 3 spawn sites (T9 does that).

**【为什么】**
- v2 REV-FINAL-W11 audit: existing `Supervisor` covers (a) panic via `catch_unwind`, (b) RestartPolicy backoff, (c) ShutdownToken cancellation, (d) unhealthy flag. The 4 audit asks of W11 are met **except** the sibling `one_shot_spawn` and the process-wide registry that `/readyz` can read.

**【做什么 DO】**
1. In `crates/gw-runtime/src/supervisor.rs`, add the registry:
   ```rust
   pub struct SupervisorRegistry {
       flags: dashmap::DashMap<&'static str, Arc<AtomicBool>>,
   }
   impl SupervisorRegistry {
       pub fn new() -> Self { Self { flags: DashMap::new() } }
       pub fn register(&self, name: &'static str) -> Arc<AtomicBool> {
           let b = Arc::new(AtomicBool::new(true));
           if self.flags.insert(name, b.clone()).is_some() {
               panic!("supervisor: duplicate task name {name}");
           }
           b
       }
       pub fn all_healthy(&self) -> bool {
           self.flags.iter().all(|kv| kv.value().load(Ordering::SeqCst))
       }
       pub fn unhealthy(&self) -> Vec<&'static str> {
           let mut v: Vec<&'static str> = self.flags.iter()
               .filter(|kv| !kv.value().load(Ordering::SeqCst))
               .map(|kv| *kv.key())
               .collect();
           v.sort_unstable();
           v
       }
   }
   pub static SUPERVISOR_REGISTRY: once_cell::sync::Lazy<SupervisorRegistry> =
       once_cell::sync::Lazy::new(SupervisorRegistry::new);
   ```
2. Add `supervised_spawn`:
   ```rust
   pub fn supervised_spawn<F, Fut>(
       name: &'static str,
       policy: RestartPolicy,
       token: ShutdownToken,
       factory: F,
   ) -> JoinHandle<()>
   where
       F: Fn(ShutdownToken) -> Fut + Send + Sync + 'static,
       Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
   {
       let sup = Supervisor::new(name, policy, token);
       let flag = SUPERVISOR_REGISTRY.register(name);
       // Tee Supervisor's existing unhealthy_flag into the registry by wrapping
       // factory so that we observe the registry flag via the existing unhealthy
       // store. The existing supervisor sets its own internal flag; we mirror.
       let sup_flag = sup.unhealthy_flag();
       let handle = sup.spawn(factory);
       tokio::spawn(async move {
           // poll the sup_flag and mirror to registry flag
           loop {
               if sup_flag.load(Ordering::SeqCst) {
                   flag.store(false, Ordering::SeqCst);
                   return;
               }
               tokio::time::sleep(std::time::Duration::from_secs(1)).await;
               if handle.is_finished() { return; }
           }
       });
       handle
   }
   ```
3. Add `one_shot_spawn`:
   ```rust
   pub fn one_shot_spawn<Fut>(
       name: &'static str,
       token: ShutdownToken,
       fut: Fut,
   ) -> JoinHandle<Result<(), anyhow::Error>>
   where
       Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
   {
       let flag = SUPERVISOR_REGISTRY.register(name);
       tokio::spawn(async move {
           let r = AssertUnwindSafe(async move {
               tokio::select! {
                   v = fut => v,
                   _ = token.cancelled() => Ok(()),
               }
           })
           .catch_unwind()
           .await;
           match r {
               Ok(Ok(())) => Ok(()),
               Ok(Err(e)) => { flag.store(false, Ordering::SeqCst); Err(e) }
               Err(_panic) => {
                   flag.store(false, Ordering::SeqCst);
                   Err(anyhow::anyhow!("one_shot task {name} panicked"))
               }
           }
       })
   }
   ```
4. In `crates/gw-store/src/managed_loop.rs`, add a `spawn_with_registry` constructor that, on `Outcome::Fatal`, flips the registry flag for the loop's `name()`.
5. Update `crates/gw-gateway/src/health.rs:93` `/readyz` to call `gw_runtime::supervisor::SUPERVISOR_REGISTRY.all_healthy()` and return 503 with JSON `{"unhealthy":[...]}` when not.

**【不做什么 DON'T】**
- ❌ Don't rename the existing `Supervisor`/`RestartPolicy` types — Rust call sites depend on them.
- ❌ Don't put `SUPERVISOR_REGISTRY` in `gw-store` — it belongs in `gw-runtime` per `docs/architecture.md` layering (gw-runtime is the lower layer).
- ❌ Don't duplicate the catch_unwind in `supervised_spawn` — the existing `Supervisor::spawn` already does it.
- ❌ Don't make `supervised_spawn` async — it returns a JoinHandle.

**【怎么做 HOW】** — see snippets in DO. Place in `crates/gw-runtime/src/supervisor.rs` below the existing `Supervisor` impl (after line 199).

Imports to add (top of file):
```rust
use dashmap::DashMap;
use once_cell::sync::Lazy;
```
Cargo.toml of `gw-runtime`: ensure `dashmap`, `once_cell` are dependencies (DashMap is already widely used in this workspace).

**【源码验证】**
- Read `crates/gw-runtime/src/supervisor.rs` 1-199: confirmed `Supervisor::new(name, policy, token)`, `unhealthy_flag()`, `spawn()` with `catch_unwind`, RestartPolicy fields all present.
- Read `crates/gw-store/src/managed_loop.rs` 1-135: confirmed `Supervisor::spawn(Arc<L>)` with `Outcome::Fatal` exit path.
- Read `crates/gw-gateway/src/health.rs:93`: confirmed `tokio::spawn(handle_connection(...))` for the HTTP health server itself; `/readyz` handler lives in same file (executor confirms the handler line by reading the file fully when applying patch).
- ADR — chose to **mirror** the existing `Supervisor::unhealthy_flag()` into a registry-owned `Arc<AtomicBool>` (rather than have `Supervisor::new` directly take a registry handle) because changing the existing constructor signature breaks all current call sites. Mirror via a tiny watchdog tokio task. Alternative — add a `Supervisor::new_registered(name, policy, token, registry)` constructor and migrate sites — rejected as scope creep.

**【如何验证】**
1. `cargo build -p gw-runtime` — exit 0.
2. New test `crates/gw-runtime/src/supervisor.rs` `#[cfg(test)] mod registry_tests`:
   - register a task; flip via `supervised_spawn` with a factory that returns Err 6 times (exceeding default budget); assert `SUPERVISOR_REGISTRY.unhealthy()` contains the task name.
   - `one_shot_spawn` with panicking future; assert returned `Err` and registry sees task unhealthy.
3. `cargo test -p gw-runtime supervisor:: -- --nocapture` — PASS.
4. `bash scripts/check-invariants.sh` — zero violations (existing invariants unchanged).

---

### TASK T9 — Migrate `DetailedArchive::start` (R16), `QuotaStateProbeRunner::start` (R17), `spawn_heartbeat_task` (R18) to `supervised_spawn`

**【是什么 What it IS】**
- Edit three Rust files:
  - `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-ledger/src/detailed_archive.rs:134` — replace `tokio::spawn(worker(rx, redis, cfg))` (N workers) with `supervised_spawn("detailed_archive_worker_<n>", PolicyArchive(), token, |t| async move { worker(rx, redis, cfg, t).await })`.
  - `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-store/src/quota_state.rs:141` — replace `tokio::spawn(async move { loop { run_once; sleep; } })` with `supervised_spawn("quota_state_probe", PolicyProbe(), token, ...)`.
  - `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-ledger/src/instance_id.rs:100` — replace `tokio::spawn(async move { ... heartbeat loop ... })` with `supervised_spawn("instance_heartbeat", PolicyHeartbeat(), token, ...)`.
- For each, define a `Policy*()` constructor — keep them in `gw-runtime/src/supervisor.rs` or a new `gw-runtime/src/policies.rs`.

**【不是什么 What it is NOT】**
- ❌ Not the broader Tunnel/billing spawn audit (WS-2 owns those).
- ❌ Not `crates/gw-gateway/src/lib.rs:507` (`drain_handle = tokio::spawn(drain_fut)`) — that is a OneShot during shutdown and belongs in `one_shot_spawn("gateway.drain", ...)` (small bonus task, see step 4 below).

**【为什么】**
- R16: `DetailedArchive` workers spawned bare; panic in `worker(...)` (decode error, serde panic on malformed input) kills the worker → channel writes start failing → `try_send` increments drop metric, but **silently** until the operator notices `gw_detailed_log_drop_total`.
- R17: `QuotaStateProbeRunner` periodic loop; panic → quota states stay stale → wrong availability decisions.
- R18: `spawn_heartbeat_task`; panic → no SETEX → after 600s TTL the W4 reconciler sees the instance as dead → false residual claim → potential double-execution.

**【做什么 DO】**
1. Add `Policy*()` recipes:
   ```rust
   // crates/gw-runtime/src/supervisor.rs (or new policies.rs)
   pub fn policy_archive() -> RestartPolicy {
       RestartPolicy {
           max_restarts: 10,
           window: Duration::from_secs(60),
           backoff_initial: Duration::from_millis(100),
           backoff_max: Duration::from_secs(5),
           backoff_multiplier: 2.0,
           backoff_jitter: 0.2,
       }
   }
   pub fn policy_probe() -> RestartPolicy {
       RestartPolicy {
           max_restarts: 5,
           window: Duration::from_secs(300),
           backoff_initial: Duration::from_secs(1),
           backoff_max: Duration::from_secs(30),
           backoff_multiplier: 2.0,
           backoff_jitter: 0.2,
       }
   }
   pub fn policy_heartbeat() -> RestartPolicy {
       RestartPolicy {
           max_restarts: 10,
           window: Duration::from_secs(120),
           backoff_initial: Duration::from_secs(1),
           backoff_max: Duration::from_secs(10),
           backoff_multiplier: 2.0,
           backoff_jitter: 0.2,
       }
   }
   ```
2. **R16** — edit `crates/gw-ledger/src/detailed_archive.rs:126-137`:
   ```rust
   pub fn start(redis: RedisPool, cfg: DetailedArchiveCfg, token: ShutdownToken) -> Self {
       let (tx, rx) = mpsc::channel::<CapturedSurface>(cfg.channel_capacity);
       let workers = cfg.workers.max(1);
       let shared_rx = std::sync::Arc::new(tokio::sync::Mutex::new(rx));
       for i in 0..workers {
           let rx = shared_rx.clone();
           let cfg = cfg.clone();
           let redis = redis.clone();
           let name = Box::leak(format!("detailed_archive_worker_{i}").into_boxed_str());
           supervised_spawn(name, policy_archive(), token.child(), move |t| {
               let rx = rx.clone();
               let cfg = cfg.clone();
               let redis = redis.clone();
               async move {
                   worker(rx, redis, cfg, t).await;
                   Ok(())
               }
           });
       }
       Self { tx }
   }
   ```
   `worker(...)` must accept a `ShutdownToken` and return `Result<(), anyhow::Error>` (executor refactors the inner `async fn worker` accordingly; current signature is `fn worker(rx, redis, cfg) -> ()`).
3. **R17** — edit `crates/gw-store/src/quota_state.rs:136-148`:
   ```rust
   impl QuotaStateProbeRunner {
       pub fn start(self, token: ShutdownToken) {
           if !self.cfg.enabled { return; }
           supervised_spawn("quota_state_probe", policy_probe(), token, move |t| {
               let runner = self.clone(); // make QuotaStateProbeRunner Clone or wrap in Arc
               let cfg = self.cfg.clone();
               async move {
                   let interval = Duration::from_secs(cfg.interval_secs.max(1));
                   loop {
                       tokio::select! {
                           _ = t.cancelled() => return Ok(()),
                           _ = tokio::time::sleep(interval) => {}
                       }
                       runner.run_once().await;
                   }
               }
           });
       }
   }
   ```
   (`self` cannot be `move`'d into a `Fn` factory because the factory is called multiple times on restart; either make `QuotaStateProbeRunner: Clone + 'static` — Arc the store/queue/client — or capture the constituent Arcs and rebuild a probe inline. ADR: Arc-ify.)
4. **R18** — edit `crates/gw-ledger/src/instance_id.rs:99-110`:
   ```rust
   pub fn spawn_heartbeat_task(pool: RedisPool, instance_id: String, token: ShutdownToken) {
       supervised_spawn("instance_heartbeat", policy_heartbeat(), token, move |t| {
           let pool = pool.clone();
           let instance_id = instance_id.clone();
           async move {
               let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
               loop {
                   tokio::select! {
                       _ = t.cancelled() => return Ok(()),
                       _ = tick.tick() => {}
                   }
                   if let Err(e) = renew_heartbeat(&pool, &instance_id).await {
                       tracing::warn!(error = ?e, instance_id = %instance_id, "instance heartbeat renewal failed");
                       // do not bubble — heartbeat misses are tolerable up to TTL/3; supervisor budget catches sustained failures
                   }
               }
           }
       });
   }
   ```
5. Caller updates: `gw-gateway/src/lib.rs:815` (`QuotaStateProbeRunner::start`) now takes a `ShutdownToken`; pass `wired.shutdown.child()` (or equivalent token field — executor reads `LlmGateway` shutdown plumbing to find the canonical token).
6. (Bonus) `gw-gateway/src/lib.rs:507` `let drain_handle = tokio::spawn(drain_fut)` → `one_shot_spawn("gateway.drain", token, drain_fut)` to register it in the readyz map.

**【不做什么 DON'T】**
- ❌ Don't change `DetailedArchive::try_send` behavior — only the spawn surface changes.
- ❌ Don't rename `spawn_heartbeat_task` — it's referenced from `gw-gateway/src/lib.rs` callers.
- ❌ Don't move `policy_*()` to a per-crate constants file — `gw-runtime` is the single home.
- ❌ Don't drop the existing `cargo test -p gw-ledger -- detailed_archive` expectations — the test currently expects `start(redis, cfg)`; bump signature there.

**【源码验证】**
- Confirmed `detailed_archive.rs:134`: `tokio::spawn(worker(rx, redis, cfg))` — matches R16.
- Confirmed `quota_state.rs:141`: `tokio::spawn(async move { let interval = ...; loop { self.run_once().await; tokio::time::sleep(interval).await; } })` — matches R17.
- Confirmed `instance_id.rs:100`: `tokio::spawn(async move { let mut tick = ...; loop { tick.tick().await; if let Err(e) = renew_heartbeat(...) ... } })` — matches R18.
- ADR — Arc-ify `QuotaStateProbeRunner` rather than rebuild from parts: it already holds `Arc<EntityStore>`, `Arc<QuotaStateUpdateQueue>`, plus owned `reqwest::Client` (cheap to clone) and a `QuotaStateProbeCfg` (small Clone). Add `#[derive(Clone)]` is sufficient.

**【如何验证】**
1. `cargo build -p gw-ledger -p gw-store -p gw-gateway` — exit 0.
2. New test `crates/gw-ledger/tests/detailed_archive_supervisor.rs`:
   - inject a `CapturedSurface` shaped to make `worker` panic (e.g. exceed an internal cap with a known panic point); assert `DetailedArchive::try_send` keeps working after restart (supervisor must have restarted the worker).
3. New test `crates/gw-ledger/tests/instance_heartbeat_supervisor.rs`:
   - simulate `renew_heartbeat` panic (via a fake `RedisPool` that panics on the third call); assert task is restarted up to budget then registry shows `instance_heartbeat` unhealthy.
4. `cargo test -p gw-ledger -p gw-store -- --nocapture` — PASS.
5. `bash scripts/check-invariants.sh` — zero violations.

---

### TASK T10 — Wire `gw-store::managed_loop::Supervisor::spawn` to register with `SUPERVISOR_REGISTRY`

**【是什么 What it IS】**
- Edit `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-store/src/managed_loop.rs:33-86`.
- Add a new sibling `spawn_with_registry(l: Arc<L>) -> JoinHandle<()>` that calls `SUPERVISOR_REGISTRY.register(l.name())` and flips the flag on the `Outcome::Fatal` arm before `break`.
- Keep the existing `spawn` for backward-compat; mark it `#[deprecated]` with a note pointing to `spawn_with_registry`.

**【不是什么 What it is NOT】**
- ❌ Not a rewrite of `ManagedLoop` semantics.
- ❌ Not changing the tick/ensure/Outcome enum.

**【为什么】**
- Without registry-aware spawn, `/readyz` cannot see fatal ManagedLoops. There are several in the codebase (e.g. counter store loops in `gw-store`).

**【做什么 DO】**
1. Add `spawn_with_registry`:
   ```rust
   impl Supervisor {
       pub fn spawn_with_registry<L: ManagedLoop>(l: Arc<L>) -> JoinHandle<()> {
           let name = l.name();
           let flag = gw_runtime::supervisor::SUPERVISOR_REGISTRY.register(name);
           tokio::spawn(async move {
               if let Err(e) = l.ensure().await {
                   /* same observe block */
               }
               loop {
                   match l.tick().await {
                       Outcome::Continue => continue,
                       Outcome::Sleep(d) => tokio::time::sleep(d).await,
                       Outcome::Recover(reason) => {
                           /* observe + ensure */
                           let _ = l.ensure().await;
                       }
                       Outcome::Fatal(e) => {
                           /* observe */
                           flag.store(false, std::sync::atomic::Ordering::SeqCst);
                           break;
                       }
                   }
               }
           })
       }
   }
   ```
2. Migrate every `Supervisor::spawn` callsite to `spawn_with_registry`. Find them with:
   ```bash
   grep -rn "managed_loop::Supervisor::spawn\b\|Supervisor::spawn(" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates --include="*.rs"
   ```

**【不做什么 DON'T】**
- ❌ Don't delete the old `spawn` in the same PR — staged deprecation lets call sites migrate.
- ❌ Don't register the loop name from inside `tokio::spawn` — register before, so duplicate-name panics fire at bootstrap.

**【怎么做】** — see DO; code identical to existing `spawn` plus the two extra lines.

**【源码验证】**
- Confirmed `managed_loop.rs:33-86` shape. `SUPERVISOR_REGISTRY` import from `gw_runtime::supervisor` requires `gw-store/Cargo.toml` to depend on `gw-runtime` — verify it already does via `grep gw-runtime crates/gw-store/Cargo.toml`. If not, add it (no circular per `docs/architecture.md` — gw-store sits above gw-runtime).

**【如何验证】**
1. `cargo build -p gw-store` — exit 0.
2. Existing test `supervisor_calls_ensure_on_recover` stays green.
3. New test `managed_loop::tests::fatal_flips_registry`: run a `MockLoop` that returns `Fatal` on tick #1; assert `SUPERVISOR_REGISTRY.all_healthy()` returns false after the join.

---

### TASK T11 — Implement Rust `syn`-based lint `scripts/check-tokio-spawn.rs`

**【是什么 What it IS】**
- New file `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/scripts/check-tokio-spawn/Cargo.toml` + `src/main.rs`.
- A standalone binary that walks every `.rs` file under `crates/`, parses with `syn::parse_file`, visits `syn::ExprCall` whose path is `tokio::spawn`, and emits a finding **unless** the call is one of:
  - Inside a `#[cfg(test)]` item.
  - Inside `crates/gw-runtime/src/supervisor.rs` (the implementation).
  - On an allowlist file `scripts/check-tokio-spawn/allowlist.txt`.
- Drive from `bash scripts/check-invariants.sh` so it joins the existing invariant gate.

**【不是什么 What it is NOT】**
- ❌ Not regex (REV-FINAL-V9 forbids).
- ❌ Not a clippy lint (heavier; ship clippy lint later if needed).
- ❌ Not a clippy macro lint — visitor-style is enough.

**【为什么】**
- Same regression-prevention rationale as T5.

**【做什么 DO】**
1. `Cargo.toml` deps: `syn = { version = "2", features = ["full","visit"] }`, `walkdir`, `anyhow`.
2. Visitor implementation:
   ```rust
   use syn::visit::Visit;
   struct SpawnVisitor { findings: Vec<(String, usize)>, current_file: String, in_test: bool }
   impl<'ast> Visit<'ast> for SpawnVisitor {
       fn visit_item(&mut self, i: &'ast syn::Item) {
           let was = self.in_test;
           let has_test = matches!(i, syn::Item::Mod(m) if m.attrs.iter().any(|a| a.path().is_ident("cfg") && format!("{:?}", a.meta).contains("test")))
               || matches!(i, syn::Item::Fn(f) if f.attrs.iter().any(|a| a.path().is_ident("cfg") && format!("{:?}", a.meta).contains("test")));
           if has_test { self.in_test = true; }
           syn::visit::visit_item(self, i);
           self.in_test = was;
       }
       fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
           if self.in_test { return syn::visit::visit_expr_call(self, e); }
           if let syn::Expr::Path(p) = &*e.func {
               let s = p.path.segments.iter().map(|s| s.ident.to_string()).collect::<Vec<_>>().join("::");
               if s == "tokio::spawn" || s == "::tokio::spawn" {
                   self.findings.push((self.current_file.clone(), e.func.span().start().line));
               }
           }
           syn::visit::visit_expr_call(self, e);
       }
   }
   ```
3. CLI: print `file:line: tokio::spawn outside supervisor (use supervised_spawn or one_shot_spawn)`; exit code 1 if any findings outside allowlist.
4. Allowlist `allowlist.txt`:
   - `crates/gw-runtime/src/supervisor.rs`
   - `crates/gw-store/src/managed_loop.rs`
   - other lines added as needed during execution (each entry needs a 1-line justification comment).

**【不做什么 DON'T】**
- ❌ Don't lint test code.
- ❌ Don't lint `tests/` directories.
- ❌ Don't try to resolve aliases (`use tokio::spawn as ts; ts(...)`); add an additional check for any expr-call whose ident is `spawn` and whose `use` path resolves to tokio (advanced — can defer with a TODO).

**【怎么做】** — see DO.

**【源码验证】**
- Pre-T9 baseline: running this lint over the current tree should report sites at:
  - `crates/gw-ledger/src/detailed_archive.rs:134`
  - `crates/gw-ledger/src/detailed_archive.rs:317`
  - `crates/gw-ledger/src/instance_id.rs:100`
  - `crates/gw-store/src/quota_state.rs:100` (intentional fire-and-forget — needs `one_shot_spawn`)
  - `crates/gw-store/src/quota_state.rs:141`
  - `crates/gw-gateway/src/lib.rs:507`, `1560`
  - `crates/gw-gateway/src/health.rs:93`
  - `crates/gw-gateway/src/control_plane/state_writer.rs:84`
  - Plus older tunnel/codec spawns owned by WS-2.
- Post-T9: lint must report only WS-2-owned sites (allowlist them as `// WS-2 owns this migration; see master plan §4 WS-2` until WS-2 lands).

**【如何验证】**
1. `cargo run --bin check-tokio-spawn -- crates/` — after T9 + allowlist additions, exit 0.
2. Insert a deliberate bare `tokio::spawn(async { })` into a temp file; assert lint reports it.
3. Wire into `scripts/check-invariants.sh` — add `bash -c "cargo run --quiet --manifest-path scripts/check-tokio-spawn/Cargo.toml -- crates/"` and propagate exit code.

---

### TASK T12 — Update `/readyz` in `gw-gateway/src/health.rs` to consume `SUPERVISOR_REGISTRY`

**【是什么 What it IS】**
- Edit `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-gateway/src/health.rs` `/readyz` handler.
- Return 503 + JSON body listing unhealthy tasks when `SUPERVISOR_REGISTRY.all_healthy()` is false.

**【不是什么 What it is NOT】**
- ❌ Not the Redis/PG ping check — that's WS-10.
- ❌ Not `/livez` (stays liveness only).

**【为什么】**
- v2 REV-FINAL-V11 (Rust mirror): /readyz must reflect supervisor health so k8s can stop routing.

**【做什么 DO】**
1. Read `crates/gw-gateway/src/health.rs` to locate the `/readyz` handler (likely a `match req.uri().path()` switch in `handle_connection`).
2. In the readyz branch:
   ```rust
   let unhealthy = gw_runtime::supervisor::SUPERVISOR_REGISTRY.unhealthy();
   if unhealthy.is_empty() {
       respond_json(200, &serde_json::json!({"ok": true, "unhealthy": []}))
   } else {
       respond_json(503, &serde_json::json!({"ok": false, "unhealthy": unhealthy}))
   }
   ```

**【不做什么 DON'T】**
- ❌ Don't block on Redis ping inside this handler — WS-10 wires that with a 500 ms timeout separately.

**【怎么做】** — see DO snippet; executor adapts to the actual `respond_*` helpers in `health.rs`.

**【源码验证】**
- `crates/gw-gateway/src/health.rs:93` `tokio::spawn(handle_connection(...))` confirms the health server file location. Executor opens the file to find the actual readyz match.

**【如何验证】**
1. `cargo build -p gw-gateway` — exit 0.
2. New test `crates/gw-gateway/tests/readyz_supervisor.rs`:
   - Boot a minimal health server; spawn a task that exits via `one_shot_spawn` with Err; curl `/readyz` → expect 503 with task name in body.
3. `cargo test -p gw-gateway readyz` — PASS.

---

### TASK T13 — Add Prometheus counters & alert rules for supervisor restarts and registry unhealthy

**【是什么 What it IS】**
- Add metric `gw_supervisor_restart_total{worker,reason="panic|error"}` (Go) and reuse the existing Rust `gw_supervisor_restart_total{task,reason}` (already in `gw-runtime/src/supervisor.rs:126`).
- Add gauge `gw_supervisor_unhealthy{worker}` set to 1 when registry flag flips false.
- Add alert rule `SupervisorUnhealthy` (`gw_supervisor_unhealthy > 0 for 1m`) in `deploy/prometheus/alerts.yml` (or the project equivalent).

**【不是什么 What it is NOT】**
- ❌ Not a dashboard build (WS-10 owns Grafana panels).

**【为什么】**
- v1 §10 / v2 REV-009: every supervisor flip must be observable; otherwise the only signal is `/readyz` polling, which loses worker-level granularity.

**【做什么 DO】**
1. In Go `shared/runtime/supervisor.go` `runSafely` panic branch, after logging: `metrics.SupervisorRestartTotal.WithLabelValues(name, "panic").Inc()`. Define the metric in `shared/platform/metrics/` if it doesn't already exist there; otherwise inline create via `prometheus.NewCounterVec` and register at init.
2. On `Supervised` budget-burn flipping healthy=false, set `metrics.SupervisorUnhealthy.WithLabelValues(name).Set(1)`.
3. Rust side: `Supervisor::spawn` already emits `gw_supervisor_restart_total` (line 126). For the new `supervised_spawn` mirror, ensure same metric label set `task=name`. For `SUPERVISOR_REGISTRY.unhealthy()`, expose a periodic refresh of `gw_supervisor_unhealthy{task=...}` — easiest via a small `tokio::spawn` registered with `supervised_spawn("supervisor.unhealthy_publisher", ...)`.
4. Add alert rules per above.

**【不做什么 DON'T】**
- ❌ Don't add high-cardinality labels (no `instance_id`, no `request_id`).
- ❌ Don't emit a metric per tick — only on transitions.

**【源码验证】**
- `crates/gw-runtime/src/supervisor.rs:126`: confirmed `metrics::counter!("gw_supervisor_restart_total", "task" => name, "reason" => outcome).increment(1);` exists.
- Go `shared/platform/metrics/` exists (used by `BuildRouter`); executor confirms registry path.

**【如何验证】**
1. Run a settler with injected panic in tests; assert Prom counter `gw_supervisor_restart_total{worker="billing.settler",reason="panic"}` increments.
2. `curl /metrics | grep gw_supervisor` — both counter and gauge appear.
3. `promtool check rules deploy/prometheus/alerts.yml` — exit 0.

---

## 3. DoD (覆盖 v2 REV-005 阈值与工具要求)

- Unit tests:
  - `go test ./shared/runtime/ -race -count=10` PASS (T1, T4, T6).
  - `cargo test -p gw-runtime -p gw-store -p gw-ledger -- --nocapture` PASS (T8, T9, T10).
- Lint:
  - `go run ./scripts/check-goroutines/cmd ./modules/... ./binaries/...` exit 0.
  - `cargo run --bin check-tokio-spawn -- crates/` exit 0.
- Chaos tests (WS-13 harness, scenario `panic-flood`):
  - Inject `panic!("test")` into random Go module worker every 30s for 1h via fault-injection middleware; expected: process uptime == 1h; `gw_supervisor_restart_total` increments; `/readyz` momentarily 503 then 200 as budget resets; final exit code 0.
  - Inject `panic!` into `DetailedArchive::worker` similarly; expected: `gw_detailed_log_drop_total` stays bounded; supervisor restarts within budget; `/readyz` reports unhealthy iff budget burns.
  - Send SIGKILL to `binaries/web` at random instants × 100; expect (a) zero pgPool-close-before-worker-drain events in logs (T2 fix) — measured by adding a counter `gw_shutdown_close_before_drain_total` in the rdb/pgPool close path; (b) all in-flight HTTP requests finish or return 503 cleanly, never connection-reset.
- HTTP isolation:
  - `binaries/all-in-one`: inject `return errors.New("boom")` from a worker; `curl /livez` returns 200 throughout (T3 fix).
- Metric & alert:
  - `gw_supervisor_restart_total > 0 within 5m` after the chaos run; `SupervisorUnhealthy` fires within 60s of a budget-burn.
- Mirror & doc:
  - `cmp /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-4-panic-supervisor.md /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-4-panic-supervisor.md` reports identical except line 1 (header).
- Cross-dep sanity:
  - `bash scripts/check-mod-deps.sh` zero violation.
  - `bash scripts/check-invariants.sh` zero violation.

---

## 4. Open INVESTIGATE items absorbed from v2 §99

- **INVESTIGATE-W11** (v2 REV-FINAL-W11 audit asks): T8 (this plan) verifies items (a)-(d) by reading `crates/gw-runtime/src/supervisor.rs` lines 1-199 and confirms: (a) panic via `catch_unwind` (line 98-100), (b) RestartPolicy backoff (line 86-87, 162-169), (c) ShutdownToken via `token.is_cancelled` (line 89, 105, 154), (d) `one_shot_spawn` sibling is **absent** — added in T8. **Verdict**: extend existing crate. No new crate.
- **INVESTIGATE-T9-cancellation** (new): `spawn_heartbeat_task` (R18) callers — `crates/gw-gateway/src/lib.rs` (search for `spawn_heartbeat_task`) currently does not pass a token. T9 introduces the token parameter; the executor needs to verify the caller has a token in scope (`wired.shutdown.child()` typical). If not, gate that change behind an explicit fallthrough rather than silently using `ShutdownToken::default()`.

---

## 5. Cross-WS coordination

- **WS-2** owns the broader migration sweep (settler/sink/drainer/tunnel). WS-4 lands the API (T1, T8) at D1; WS-2 sweeps the rest by D5. The per-worker recipes table (§1.3) is the contract.
- **WS-10** consumes `/readyz` registry surface for the DB/Redis ping aggregation (S1).
- **WS-12** integrates T5 / T11 lints into `make check`.
- **WS-13** provides `panic-flood` chaos scenario for the DoD chaos tests.

---

## 6. Verification command summary (cheat-sheet)

```bash
# Go side
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
go test ./shared/runtime/ -race -count=10
go test ./binaries/web/ -run TestShutdownOrder -race -count=5
go test ./binaries/all-in-one/ -run TestHttpIsolatedFromWorkerPanic -race -count=5
go test ./modules/billing/... -race -count=5
go run ./scripts/check-goroutines/cmd ./modules/... ./binaries/...
golangci-lint run ./shared/runtime/... ./binaries/... ./modules/...

# Rust side
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo build -p gw-runtime -p gw-store -p gw-ledger -p gw-gateway
cargo test -p gw-runtime -p gw-store -p gw-ledger -p gw-gateway -- --nocapture
cargo run --bin check-tokio-spawn -- crates/
bash scripts/check-invariants.sh
```

— end of WS-4 sub-plan —
