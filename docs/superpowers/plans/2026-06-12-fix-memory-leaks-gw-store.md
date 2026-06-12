# Crate 2: gw-store + gw-ledger — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `DeductGc` resilient (Supervisor + ShutdownCoordinator), and add a defensive expired-Open sweep for `USER_CIRCUIT` folded into the same GC tick.

**Architecture:** Replace `DeductGc::start_default()` (bare `tokio::spawn`, no panic recovery, no shutdown integration) with `DeductGc::spawn(shutdown: ShutdownToken)` that wraps the sweep loop in `gw_runtime::Supervisor` — providing catch_unwind + exponential-backoff restart. The same loop also calls a new `gw_store::deduction::sweep_user_circuit()` per tick which evicts only `UserCircuit::Open { until, .. }` entries past `until + STALE_GRACE` (5 min default). `UserCircuit::Closed` entries are NOT touched.

**Tech Stack:** Rust 1.88.0, `gw-runtime::Supervisor` (already in workspace), `dashmap`, `tokio`, `tracing`, `metrics`. No new workspace deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 2.

**Branch:** `fix/memory-leaks-gw-store` cut from latest local `main` (which now includes Crate 1's merge commit `97ba794`).

---

## Scope decision recorded

`USER_CIRCUIT` is currently dead code in the workspace — full-tree grep `USER_CIRCUIT.insert` / `USER_CIRCUIT.entry` returns ZERO writers (only one reader at `crates/gw-proxy/src/filters/request.rs:1175`). The map is therefore not actively leaking. Per user decision 2026-06-12, we still add a defensive sweep for forward compatibility: when future writers land (per the design intent stated in `request.rs:1170-1174`), the eviction is already in place. The sweep is scoped narrowly to expired `Open` entries — `Closed` semantics need a separate design conversation if/when writers arrive.

---

## File Structure

**Modified files:**
- `crates/gw-store/src/deduction.rs` — add `STALE_GRACE` constant, add `pub fn sweep_user_circuit() -> u64`
- `crates/gw-ledger/src/deduct_gc.rs` — replace `start()` / `start_default()` with `spawn(shutdown: ShutdownToken) -> JoinHandle<()>` using `Supervisor`; fold `sweep_user_circuit()` into the tick loop
- `crates/gw-gateway/src/lib.rs:1057` — replace `let _deduct_gc_handle = gw_ledger::DeductGc::start_default();` with `let deduct_gc_handle = gw_ledger::DeductGc::spawn(shutdown_token.clone()); shutdown.register(deduct_gc_handle);`
- `docs/spec/06-billing-ledger.md` — document DeductGc supervisor wrap + USER_CIRCUIT defensive sweep
- `docs/architecture.md` — add to §Deliberate behaviours (DeductGc self-restart; USER_CIRCUIT Closed not swept)
- `docs/ROADMAP.md` — close items

**Test files:**
- `crates/gw-store/src/deduction.rs` — new `#[cfg(test)] mod tests` for `sweep_user_circuit`
- `crates/gw-ledger/src/deduct_gc.rs` — new tests for shutdown-token cancellation; existing 3 sweep tests unchanged

**No new files.** No new workspace deps.

---

## Task 1: Branch Setup + Verification

**Files:**
- No code changes; branch + grep only.

- [ ] **Step 1: Confirm on latest local main and cut branch**

```bash
git checkout main
git status   # expect: ahead of origin/main, working tree clean
git log --oneline -3   # expect 97ba794 as latest (Crate 1 merge)
git checkout -b fix/memory-leaks-gw-store
```

Expected: `Switched to a new branch 'fix/memory-leaks-gw-store'`.

- [ ] **Step 2: Pre-implementation verification**

Confirm DeductGc still uses bare `tokio::spawn`:

```bash
grep -nA8 "impl DeductGc" crates/gw-ledger/src/deduct_gc.rs | head -20
```

Expected: `start()` returns `tokio::spawn(async move { ... })` without Supervisor. If you see `Supervisor::new` instead, STOP — the work has been done independently, surface to orchestrator.

Confirm `USER_CIRCUIT` has no writers:

```bash
grep -rn "USER_CIRCUIT\.insert\|USER_CIRCUIT\.entry" crates/
```

Expected: empty output. If output appears, STOP — the dead-code premise is wrong and the design needs re-evaluation.

Confirm gw-gateway call site:

```bash
grep -n "DeductGc::start_default\|_deduct_gc_handle" crates/gw-gateway/src/lib.rs
```

Expected: line ~1057 still has `let _deduct_gc_handle = gw_ledger::DeductGc::start_default();`

---

## Task 2: USER_CIRCUIT sweep — failing test for expired Open eviction

**Files:**
- Test: `crates/gw-store/src/deduction.rs` (new `#[cfg(test)] mod tests` at end of file)

- [ ] **Step 1: Append a new test module at the end of `deduction.rs`**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // USER_CIRCUIT is a global static; tests that assert counts must serialize.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn cleanup(prefix: &str) {
        let keys: Vec<_> = USER_CIRCUIT
            .iter()
            .filter(|e| e.key().as_ref().starts_with(prefix))
            .map(|e| e.key().clone())
            .collect();
        for k in keys {
            USER_CIRCUIT.remove(&k);
        }
    }

    #[test]
    fn sweep_user_circuit_evicts_expired_open() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_a_";
        cleanup(prefix);

        // Insert one Open entry whose `until` is far in the past (> STALE_GRACE).
        // The grace adds 5 min on top of `until` — pass `until = now - 10 min`.
        let key = UserId::from(format!("{prefix}expired"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() - Duration::from_secs(600),
                reason: CircuitReason::BalanceZero,
            },
        );
        assert!(USER_CIRCUIT.contains_key(&key));

        let removed = sweep_user_circuit();

        assert_eq!(removed, 1);
        assert!(!USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }
}
```

- [ ] **Step 2: Run the new test and verify it fails to compile**

```bash
cargo test -p gw-store --lib tests::sweep_user_circuit_evicts_expired_open 2>&1 | tail -20
```

Expected: compile error — `sweep_user_circuit` is not yet defined. This confirms the failing-test-first discipline.

---

## Task 3: USER_CIRCUIT sweep — additional failing tests

**Files:**
- Test: `crates/gw-store/src/deduction.rs` (same `mod tests`)

- [ ] **Step 1: Append three more tests**

Append after the first test, INSIDE the same `mod tests`:

```rust
    #[test]
    fn sweep_user_circuit_keeps_active_open() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_b_";
        cleanup(prefix);

        // Open with `until` in the future — must NOT be evicted.
        let key = UserId::from(format!("{prefix}active"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() + Duration::from_secs(60),
                reason: CircuitReason::BalanceZero,
            },
        );

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_keeps_recently_expired_open_within_grace() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_c_";
        cleanup(prefix);

        // Open with `until` slightly in the past but within STALE_GRACE (5 min).
        // until = now - 60s; grace = 5 min → still inside grace window.
        let key = UserId::from(format!("{prefix}grace"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() - Duration::from_secs(60),
                reason: CircuitReason::BalanceZero,
            },
        );

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_keeps_closed_unconditionally() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_d_";
        cleanup(prefix);

        // Closed entries are not touched. (Closed has no timestamp; without
        // writers in the codebase we cannot judge staleness — explicit policy.)
        let key = UserId::from(format!("{prefix}closed"));
        USER_CIRCUIT.insert(key.clone(), UserCircuit::Closed);

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_on_empty_map_is_noop() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_e_";
        cleanup(prefix);

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        cleanup(prefix);
    }
```

- [ ] **Step 2: Run all 5 tests and verify they fail to compile**

```bash
cargo test -p gw-store --lib tests::sweep_user_circuit 2>&1 | tail -10
```

Expected: compile errors — `sweep_user_circuit` still not defined.

---

## Task 4: USER_CIRCUIT sweep — implementation

**Files:**
- Modify: `crates/gw-store/src/deduction.rs`

- [ ] **Step 1: Add STALE_GRACE constant and sweep function**

Add the constant near the top of the file (after the `use` block, before `pub struct DeductionState`):

```rust
/// Grace window after `Open.until` before a USER_CIRCUIT entry becomes
/// eligible for eviction. Allows brief jitter between the circuit's
/// `until` and an upcoming consumer revalidation.
pub const USER_CIRCUIT_STALE_GRACE: std::time::Duration = std::time::Duration::from_secs(300);
```

Add the sweep function at the bottom of the file (after the `pub static USER_CIRCUIT: ...` line, before the `#[cfg(test)] mod tests`):

```rust
/// Evict `USER_CIRCUIT` entries whose `Open.until` has passed by more than
/// [`USER_CIRCUIT_STALE_GRACE`]. `Closed` entries are NOT evicted — without
/// a creation timestamp on `Closed` we cannot judge staleness, and the
/// current codebase has no `USER_CIRCUIT.insert(.., Closed)` writers.
///
/// Returns the count of evicted entries. Called per tick by [`DeductGc`].
pub fn sweep_user_circuit() -> u64 {
    let now = std::time::Instant::now();
    let mut removed: u64 = 0;
    USER_CIRCUIT.retain(|_uid, circuit| match *circuit {
        UserCircuit::Open { until, .. } => {
            if now > until + USER_CIRCUIT_STALE_GRACE {
                removed += 1;
                false
            } else {
                true
            }
        }
        UserCircuit::Closed => true,
    });
    if removed > 0 {
        metrics::counter!("gw_user_circuit_sweep_removed_total").increment(removed);
    }
    removed
}
```

Add `metrics` to the `use` block at the top of the file? Check if `metrics` is already a dependency of `gw-store`:

```bash
grep "^metrics" crates/gw-store/Cargo.toml
```

If `metrics` is NOT in `gw-store`'s `[dependencies]`, add it:

```toml
metrics = { workspace = true }
```

- [ ] **Step 2: Run the new tests and verify they pass**

```bash
cargo test -p gw-store --lib tests::sweep_user_circuit 2>&1 | tail -20
```

Expected: 5/5 PASS.

- [ ] **Step 3: Run all gw-store tests for regression**

```bash
cargo test -p gw-store
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-store/src/deduction.rs crates/gw-store/Cargo.toml
git commit -m "$(cat <<'EOF'
fix(gw-store): add sweep_user_circuit for expired Open entries

Audit 2026-06-12 flagged USER_CIRCUIT as having no eviction. The map
is currently dead code (no writers in the workspace), but the design
intent at request.rs:1170-1174 anticipates the ledger flusher writing
Open entries. Adds a defensive sweep that evicts Open past
`until + STALE_GRACE` (5 min). Closed unconditionally kept — without a
creation timestamp on Closed we cannot judge its staleness.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 5: DeductGc — failing test for shutdown-token cancellation

**Files:**
- Test: `crates/gw-ledger/src/deduct_gc.rs` (existing `mod tests`)

- [ ] **Step 1: Append a new test**

Append inside the existing `mod tests`:

```rust
    /// New API: `DeductGc::spawn(token)` returns a JoinHandle whose task
    /// exits when the shutdown token is cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        use gw_runtime::ShutdownCoordinator;
        use std::time::Duration;

        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = DeductGc::spawn(token);

        // Cancel immediately; the task should exit promptly.
        coord.shutdown();

        // The Supervisor wraps the loop; on cancellation it should return
        // within a short window. 5s is generous.
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "DeductGc task did not exit within 5s of shutdown"
        );
    }
```

Add to the test imports at the top of `mod tests` (or use full paths inline):

```rust
    // existing imports kept; gw_runtime is a new dep below
```

Check whether `gw-runtime` is in `gw-ledger`'s `[dev-dependencies]`:

```bash
grep "gw-runtime\|gw_runtime" crates/gw-ledger/Cargo.toml
```

If it appears under `[dependencies]` (likely, since `inflight_reaper` uses it), good. If not in `[dev-dependencies]`, no action needed since `[dependencies]` is also visible to tests.

- [ ] **Step 2: Run the new test and verify it fails to compile**

```bash
cargo test -p gw-ledger --lib deduct_gc::tests::spawn_respects_shutdown_token 2>&1 | tail -20
```

Expected: compile error — `DeductGc::spawn` does not exist yet.

---

## Task 6: DeductGc — Supervisor wrap + USER_CIRCUIT sweep fold-in

**Files:**
- Modify: `crates/gw-ledger/src/deduct_gc.rs`

- [ ] **Step 1: Replace the impl block**

Replace the existing `impl DeductGc { ... }` block (lines 20–60) entirely with:

```rust
impl DeductGc {
    /// Spawn the periodic DEDUCT + USER_CIRCUIT sweep supervised by
    /// `gw_runtime::Supervisor`. The Supervisor provides catch_unwind +
    /// exponential-backoff restart so a panic in `sweep()` does not cause
    /// the in-memory DEDUCT map to grow unbounded thereafter.
    ///
    /// Returns the `JoinHandle` so the caller can register it with the
    /// `ShutdownCoordinator`. The task runs until cancelled via
    /// `ShutdownToken` or until the restart budget is exhausted.
    pub fn spawn(shutdown: gw_runtime::ShutdownToken) -> tokio::task::JoinHandle<()> {
        gw_runtime::Supervisor::new(
            "deduct_gc",
            gw_runtime::RestartPolicy::default(),
            shutdown,
        )
        .spawn(move |tok| async move {
            let mut tick = tokio::time::interval(GC_INTERVAL);
            // Skip the immediate first tick — original `tokio::time::sleep`
            // pattern waited a full interval before the first sweep; match
            // that.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(target: "gw_ledger::deduct_gc", "shutdown requested");
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        let dedup_removed = Self::sweep();
                        let circuit_removed = gw_store::deduction::sweep_user_circuit();
                        if dedup_removed > 0 || circuit_removed > 0 {
                            debug!(dedup_removed, circuit_removed, "deduct GC + user-circuit sweep");
                        }
                    }
                }
            }
        })
    }

    /// Single synchronous sweep of DEDUCT. Returns the number of entries
    /// removed. Kept as a separate function so tests can exercise the
    /// sweep without spawning a Supervisor.
    pub fn sweep() -> u64 {
        let mut removed: u64 = 0;
        DEDUCT.retain(|_uid, state| {
            let base = state.base_micro.load(Ordering::Relaxed);
            let deducted = state.deducted_micro.load(Ordering::Relaxed);
            let frozen = state.frozen_micro.load(Ordering::Relaxed);
            if base == 0 && deducted == 0 && frozen == 0 {
                removed += 1;
                false
            } else {
                true
            }
        });
        if removed > 0 {
            metrics::counter!("gw_deduct_gc_removed_total").increment(removed);
        }
        removed
    }
}
```

Verify `gw-runtime` is in `gw-ledger`'s `[dependencies]`:

```bash
grep "^gw-runtime\|gw_runtime" crates/gw-ledger/Cargo.toml
```

Expected: present. (It's used by `inflight_reaper`.)

- [ ] **Step 2: Build to verify compilation**

```bash
cargo build -p gw-ledger
```

Expected: clean build.

- [ ] **Step 3: Run the cancellation test**

```bash
cargo test -p gw-ledger --lib deduct_gc::tests::spawn_respects_shutdown_token
```

Expected: PASS.

- [ ] **Step 4: Run all deduct_gc tests for regression**

```bash
cargo test -p gw-ledger --lib deduct_gc::tests
```

Expected: 4 tests PASS (3 existing + 1 new).

---

## Task 7: gw-gateway call-site update

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs:1057` (3-line area)

- [ ] **Step 1: Read the current call site with context**

```bash
sed -n '1050,1062p' crates/gw-gateway/src/lib.rs
```

You should see:

```rust
    // The JoinHandle is intentionally dropped: the spawned task lives for the
    // life of the bootstrap runtime (which is parked on pending()).
    let _deduct_gc_handle = gw_ledger::DeductGc::start_default();
```

- [ ] **Step 2: Replace lines 1055–1057 with the new wiring**

Replace the comment + line:

```rust
    // DeductGc is now Supervisor-wrapped (gw_runtime::Supervisor) and
    // registered with the ShutdownCoordinator so it survives panics and
    // exits cleanly on shutdown.
    let deduct_gc_handle = gw_ledger::DeductGc::spawn(shutdown_token.clone());
    shutdown.register(deduct_gc_handle);
```

The variables `shutdown_token` and `shutdown` must already be in scope at this line — verify via:

```bash
sed -n '1050,1070p' crates/gw-gateway/src/lib.rs | grep -E "shutdown_token|shutdown\."
```

Should show prior uses. (Both are established in the bootstrap function before this line; confirmed via the adjacent `inflight_reaper` registration on lines 1061–1069.)

- [ ] **Step 3: Build to verify compilation**

```bash
cargo build -p gw-gateway
```

Expected: clean build.

- [ ] **Step 4: Run gw-gateway tests for regression**

```bash
cargo test -p gw-gateway
```

Expected: all PASS.

- [ ] **Step 5: Commit (combined: DeductGc impl + call site)**

```bash
git add crates/gw-ledger/src/deduct_gc.rs crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
fix(gw-ledger,gw-gateway): supervise DeductGc and fold in USER_CIRCUIT sweep

The audit flagged that the DeductGc background task ran under a bare
tokio::spawn whose JoinHandle was discarded with `let _`. A panic inside
the sweep loop would silently kill the task; thereafter the DEDUCT
DashMap accumulates zero-state entries unbounded (a panic-degenerated
leak path).

Wraps the spawn in gw_runtime::Supervisor (catch_unwind +
exponential-backoff restart) and registers the JoinHandle with the
ShutdownCoordinator. The same per-tick body now also calls
gw_store::deduction::sweep_user_circuit() so USER_CIRCUIT eviction
runs on the same cadence — defensive prep for when future writers
land per the design intent at request.rs:1170-1174.

The standalone sweep() function is preserved so existing unit tests
that exercise DEDUCT sweep directly continue to work without
Supervisor scaffolding.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 8: Docs sync

**Files:**
- Modify: `docs/spec/06-billing-ledger.md`, `docs/architecture.md`, `docs/ROADMAP.md`
- Verify: `CLAUDE.md` ⇄ `AGENTS.md` byte-identical body

- [ ] **Step 1: Update `docs/spec/06-billing-ledger.md`**

Locate the DEDUCT GC section:

```bash
grep -n "DEDUCT\|DeductGc\|deduct_gc" docs/spec/06-billing-ledger.md | head -10
```

Add (or rewrite the existing paragraph as) the following near the DEDUCT description:

> **DeductGc lifecycle.** The periodic DEDUCT sweep is supervised by `gw_runtime::Supervisor` (catch_unwind + exponential-backoff restart) and registered with the `ShutdownCoordinator`. A panic inside the sweep no longer silently disables eviction. The same per-tick body also calls `gw_store::deduction::sweep_user_circuit()` to evict `UserCircuit::Open { until }` entries past `until + USER_CIRCUIT_STALE_GRACE` (5 min default). `UserCircuit::Closed` entries are NOT evicted — currently the workspace has no `USER_CIRCUIT.insert(_, Closed)` writers, and `Closed` carries no creation timestamp.

- [ ] **Step 2: Update `docs/architecture.md` §Deliberate behaviours**

Locate the §Deliberate behaviours section and add:

> **DeductGc is panic-tolerant via Supervisor.** A panic in the sweep loop is recovered by `gw_runtime::Supervisor` with exponential-backoff restart (default `max_restarts=5` in a 60s window). If the restart budget is exhausted the supervisor marks itself unhealthy and exits — `DEDUCT` then accumulates zero-state entries until next restart; this is a deliberately bounded failure mode rather than silent death.

> **USER_CIRCUIT Closed entries are never swept.** Without a creation timestamp on the `Closed` variant we cannot judge staleness. The eviction policy is intentionally narrow: only `Open { until, .. }` past `until + 5 min` grace is removed. Closed entries currently never appear in the codebase (no writers); if a writer is introduced, the eviction policy needs a follow-up design decision.

- [ ] **Step 3: Update `docs/ROADMAP.md`**

Locate items related to DeductGc / USER_CIRCUIT / DEDUCT in the ROADMAP. If matching open items exist, remove them. Otherwise append under the appropriate "Recently closed" section:

> - 2026-06-12 — Closed: DeductGc is Supervisor-wrapped (panic-tolerant) and registered with ShutdownCoordinator; defensive USER_CIRCUIT sweep evicts expired `Open` entries. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 2.

- [ ] **Step 4: Verify CLAUDE.md ⇄ AGENTS.md byte-identical body**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

Expected: `BYTE-IDENTICAL BODY` (only H1 title differs; that line is excluded by `tail -n +2`).

- [ ] **Step 5: Commit docs**

```bash
git add docs/spec/06-billing-ledger.md docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync architecture/spec/roadmap with DeductGc supervisor + USER_CIRCUIT sweep

- spec/06-billing-ledger.md: document DeductGc supervisor wrap + USER_CIRCUIT
  defensive sweep semantics.
- architecture.md §Deliberate behaviours: DeductGc panic-tolerance bounded
  by RestartPolicy; USER_CIRCUIT Closed entries are intentionally not swept.
- ROADMAP.md: close the items addressed by this PR.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 9: Completion gate

**Files:**
- No code changes; verification only.

- [ ] **Step 1: Per-crate tests**

```bash
cargo test -p gw-store
```

Expected: all PASS (existing + 5 new USER_CIRCUIT tests).

```bash
cargo test -p gw-ledger
```

Expected: all PASS (3 existing deduct_gc tests + 1 new + others unchanged).

```bash
cargo test -p gw-gateway
```

Expected: all PASS.

- [ ] **Step 2: Workspace clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean. Watch for unused-import warnings if any helpers became unused.

- [ ] **Step 3: Workspace fmt check**

```bash
cargo fmt --all -- --check
```

Expected: no output. If output appears:

```bash
cargo fmt --all
git add -u
git commit -m "$(cat <<'EOF'
chore: cargo fmt --all

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 4: Architectural invariants**

```bash
bash scripts/check-invariants.sh
```

Expected: clean.

---

## Task 10: Merge to local main (orchestrator action)

**Files:**
- No code changes.

This task is executed by the orchestrator (not the implementer subagent). After all prior tasks pass, the orchestrator:

- [ ] Reviews the branch state
- [ ] Checks out `main`
- [ ] Runs `git merge --no-ff fix/memory-leaks-gw-store` with an appropriate merge commit message
- [ ] Does NOT push to origin (user reserves MR submission for the final batch)

Implementer does NOT execute this task. Stop after Task 9 passes.

---

## Self-Review

**1. Spec coverage** — every §2 Crate 2 item maps to a task:
- `DeductGc` Supervisor wrap + panic recovery → Tasks 5/6/7
- `DEDUCT` indirect (panic-tolerant via Supervisor) → covered by Task 6 implementation
- `USER_CIRCUIT` defensive sweep → Tasks 2/3/4 (tests + impl)
- gw-gateway call-site wiring → Task 7
- Docs sync → Task 8
- Completion gate → Task 9
- ✅ Complete.

**2. Placeholder scan** — none. All commands and code blocks are concrete.

**3. Type consistency** —
- `USER_CIRCUIT_STALE_GRACE: Duration` — declared once in deduction.rs, used in sweep_user_circuit
- `gw_runtime::Supervisor`, `gw_runtime::RestartPolicy`, `gw_runtime::ShutdownToken` — match existing usage in `inflight_reaper.rs`
- Metric `gw_user_circuit_sweep_removed_total` — new; follows existing `gw_deduct_gc_removed_total` pattern
- `DeductGc::spawn(shutdown: ShutdownToken)` — new public API; `start()` and `start_default()` are removed (only one caller, updated in Task 7)

**4. Operational note** — the standalone `DeductGc::sweep()` is intentionally retained as a public function so the existing 3 sweep unit tests work without Supervisor scaffolding. This is NOT backwards-compatibility cruft (the audit-pattern says delete unused code); `sweep()` is actively used by tests.

---

## Notes for the Implementer

- The 60s `GC_INTERVAL` constant is preserved. Per the design spec, downstream code may assume this cadence — do not change it.
- The first-tick skip (`tick.tick().await` before the loop) matches the original `tokio::time::sleep(interval).await` pattern in the old `start()`; preserves behavior on cold boot.
- BLOCKED triggers: Task 1 Step 2 finds Supervisor wrapping already in place; Task 1 Step 2 finds `USER_CIRCUIT.insert` writers (premise is wrong); Task 7 Step 2 cannot find `shutdown_token` / `shutdown` in scope at line 1057 area; the new cancellation test in Task 5 hangs (suggests Supervisor wiring bug — surface before committing).
