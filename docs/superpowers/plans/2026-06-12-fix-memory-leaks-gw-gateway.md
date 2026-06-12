# Crate 7: gw-gateway — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Cancel the orphan tunnel probe loop on shutdown, and call `RedisPool::quit()` during drain.

**Architecture:** `TunnelStack::start_probe_loop` gains a `ShutdownToken` parameter and a `tokio::select!` cancellation arm. Bootstrap wraps the spawn in `gw_runtime::Supervisor` and registers the JoinHandle with `ShutdownCoordinator`, matching the pattern from prior crates. The Redis pool gets a drain-time `pool.quit()` hook registered with the ShutdownCoordinator.

**Tech Stack:** Rust 1.88.0, `gw-runtime::Supervisor` (existing), fred 9.4 (existing). No new deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 7.

**Branch:** `fix/memory-leaks-gw-gateway` cut from local `main` (includes Crate 6 merge `b8b7b34`).

**Test budget**: ≤ 4 new tests (small crate).

---

## Pre-Implementation Findings (scope-correction)

| Item | Spec said | Empirical reality |
|------|-----------|-------------------|
| Tunnel `_probe_handle` | `lib.rs:1369` permanent task | ✅ confirmed at `lib.rs:1389`; `start_probe_loop` at `gw-proxy/src/tunnel_stack.rs:95` is bare `tokio::spawn(loop)` with no cancellation. Real fix. |
| Redis pool `quit()` | drain-time hook needed | ✅ `RedisPool::quit()` exists in fred 9.4 (`fred-9.4.0/src/clients/pool.rs:681`); just needs wiring |
| Pingora upstream pool | "no explicit `Opt` set" | ❌ **scope reduction**: timeouts ARE explicitly set in `crates/gw-proxy/src/timeouts.rs:30-36` (connection_timeout, read_timeout, total_connection_timeout). `Server::new(None)` refers to CLI args (daemon mode flags), NOT the conn pool. The conn pool lives on `HttpPeer.options` and is already configured. **This item is removed from scope** — already done. |

Net effect: 2 real fixes (probe cancellation + Redis quit) instead of 3.

---

## File Structure

**Modified:**
- `crates/gw-proxy/src/tunnel_stack.rs` — `start_probe_loop` signature gains `shutdown: ShutdownToken`; loop adds cancellation arm
- `crates/gw-gateway/src/lib.rs` — replace `let _probe_handle = stack.start_probe_loop(...)` with Supervisor-wrapped spawn + `shutdown.register(handle)`; add Redis pool quit hook on drain
- `crates/gw-store/src/redis_client.rs` — may need to expose a `quit()` shim (or callers can call `redis.pool.quit().await` directly)
- `docs/spec/09-tunnel.md` — document probe loop lifecycle
- `docs/architecture.md` §Bootstrap / §Shutdown — note probe + Redis quit are now drain-aware
- `docs/ROADMAP.md` — close items

**Test files:**
- `crates/gw-proxy/src/tunnel_stack.rs` mod tests — 1 new test (probe loop respects shutdown token)
- `crates/gw-gateway/src/lib.rs` `#[cfg(test)] mod` — 1 new test (Redis quit invoked on drain) IF feasible; otherwise rely on integration

**Total: 1-2 new tests** (small surface area). Budget 4.

**No new workspace deps.**

---

## Task 1: Branch Setup + Verification

- [ ] **Step 1: Cut branch**

```bash
git checkout main
git status                        # expect: ahead-of-origin, clean
git log --oneline -3              # expect: b8b7b34 (Crate 6 merge) at top
git checkout -b fix/memory-leaks-gw-gateway
```

- [ ] **Step 2: Pre-impl verification**

```bash
grep -n "_probe_handle\|start_probe_loop" crates/gw-gateway/src/lib.rs
```
Expected: line 1389 still uses `let _probe_handle = stack.start_probe_loop(Duration::from_secs(5));`

```bash
grep -n "pub fn start_probe_loop" crates/gw-proxy/src/tunnel_stack.rs
```
Expected: line 95, signature is `pub fn start_probe_loop(self: &Arc<Self>, interval: Duration) -> JoinHandle<()>` (no ShutdownToken yet).

```bash
grep -n "RedisPool::quit\|pool.quit" crates/gw-store/src/redis_client.rs crates/gw-gateway/src/lib.rs
```
Expected: empty (no current shutdown call).

If any of these fail, STOP and surface.

---

## Task 2: TunnelStack probe loop — accept ShutdownToken

**Files:** `crates/gw-proxy/src/tunnel_stack.rs`

- [ ] **Step 1: Failing test**

Append to `mod tests` in `tunnel_stack.rs` (or create one if absent). First check the existing test scaffolding:

```bash
grep -n "#\[cfg(test)\]\|#\[test\]\|#\[tokio::test\]" crates/gw-proxy/src/tunnel_stack.rs | head -10
```

If a test module exists, append:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_loop_respects_shutdown_token() {
        let stack = Arc::new(TunnelStack::default());
        let coord = gw_runtime::ShutdownCoordinator::default();
        let token = coord.token();
        let handle = stack.start_probe_loop(std::time::Duration::from_secs(60), token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "probe loop did not exit within 5s of shutdown");
    }
```

If `TunnelStack::default()` doesn't exist, use whatever constructor is available (read existing tests for the pattern). If the test module doesn't exist, create it:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    // (insert the test above)
}
```

- [ ] **Step 2: Verify compile failure**

```bash
cargo test -p gw-proxy --lib tunnel_stack::tests::probe_loop_respects_shutdown_token 2>&1 | tail -10
```

Expected: `start_probe_loop` signature mismatch (current signature has no `ShutdownToken`).

- [ ] **Step 3: Modify `start_probe_loop` signature**

Replace the function body at `tunnel_stack.rs:95-120`:

```rust
    /// Periodically sample transport state per node and emit gauge
    /// metrics. The returned `JoinHandle` exits cleanly when the
    /// `ShutdownToken` is cancelled — register the handle with
    /// `ShutdownCoordinator` so drain awaits its exit.
    pub fn start_probe_loop(
        self: &Arc<Self>,
        interval: Duration,
        shutdown: gw_runtime::ShutdownToken,
    ) -> JoinHandle<()> {
        let stack = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!(
                            target: "gw_proxy::tunnel_stack",
                            "probe loop shutdown requested"
                        );
                        return;
                    }
                    _ = ticker.tick() => {
                        let nodes = stack.nodes.load_full();
                        for node in nodes.iter() {
                            let current = stack.transports.get(&node.id).and_then(|t| t.current());
                            let present = current.as_ref().is_some_and(|t| t.is_alive());
                            metrics::gauge!(
                                "gw_tunnel_transport_present",
                                "node_id" => node.id.clone()
                            )
                            .set(if present { 1.0 } else { 0.0 });
                            if let Some(age) = current.as_ref().map(|t| t.last_pong_age().as_secs_f64()) {
                                metrics::gauge!(
                                    "gw_tunnel_last_pong_age_seconds",
                                    "node_id" => node.id.clone()
                                )
                                .set(age);
                            }
                        }
                    }
                }
            }
        })
    }
```

Add `use gw_runtime::ShutdownToken;` at the top of the file if not already imported.

Check whether `gw_runtime` is in `gw-proxy/Cargo.toml`:

```bash
grep "^gw-runtime\|gw_runtime" crates/gw-proxy/Cargo.toml
```

If missing, add under `[dependencies]`: `gw-runtime = { path = "../gw-runtime" }`. Check what other path-deps look like in that Cargo.toml and follow the same form.

- [ ] **Step 4: Update bootstrap callsite**

At `crates/gw-gateway/src/lib.rs:1389` — replace:

```rust
let _probe_handle = stack.start_probe_loop(Duration::from_secs(5));
```

with:

```rust
let probe_handle = stack.start_probe_loop(Duration::from_secs(5), shutdown_token.clone());
shutdown.register(probe_handle);
```

Verify `shutdown_token` and `shutdown` are in scope at that location:

```bash
grep -nB2 -A2 "_probe_handle\|stack.start_probe_loop" crates/gw-gateway/src/lib.rs
```

Look at the surrounding context — these names should be in scope (the same names are used throughout the bootstrap).

- [ ] **Step 5: Build + run new test**

```bash
cargo build -p gw-proxy -p gw-gateway
cargo test -p gw-proxy --lib tunnel_stack::tests::probe_loop_respects_shutdown_token
```

Expected: PASS.

- [ ] **Step 6: Run gw-proxy tests for regression**

```bash
cargo test -p gw-proxy
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-proxy/src/tunnel_stack.rs crates/gw-gateway/src/lib.rs crates/gw-proxy/Cargo.toml
git commit -m "$(cat <<'EOF'
fix(gw-proxy,gw-gateway): cancel tunnel probe loop on shutdown

TunnelStack::start_probe_loop previously ran as a bare tokio::spawn
infinite loop; the returned JoinHandle was bound to _probe_handle and
discarded, leaving the task running for the life of the runtime
holding Arc<TunnelStack> and transitively Arc<DashMap<String,
Arc<ReconnectingTransport>>>.

Adds ShutdownToken parameter to start_probe_loop with a
tokio::select! cancellation arm. Bootstrap now registers the handle
with ShutdownCoordinator so drain awaits its exit.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Redis pool quit on shutdown drain

**Files:** `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: Locate the existing drain code**

```bash
grep -nB2 -A10 "coordinator.drain\|drain_fut\|phase2_task_drain" crates/gw-gateway/src/lib.rs | head -40
```

Find where the drain phases are orchestrated. The Redis pool quit should run AFTER all task spawns are aborted/drained (phase 2 in the existing model) but BEFORE the process exits.

- [ ] **Step 2: Add the quit hook**

The cleanest pattern (matching how other shutdown work hooks integrate): right after `coordinator.drain(...)` resolves, but before returning from main. Locate the structure around the `drain_fut` await.

Add:

```rust
// Phase 3: Redis pool quit (after all task-side drainers are done).
// Closes all connections in the fred pool gracefully.
if let Err(e) = redis.pool.quit().await {
    tracing::warn!(target: "gw_gateway", error = %e, "redis pool quit failed");
}
```

The exact placement depends on the existing drain orchestration shape. If the drain function consumes `redis` by value, hold a `Arc` clone before drain and quit on that. If it's already accessible after drain, simpler.

**If the wiring is non-obvious (e.g. `redis` was moved into a closure earlier)**, instead expose a "shutdown task" via the coordinator pattern that other crates use:

```rust
// Register a closure that runs during phase-2 drain.
let redis_for_quit = redis.pool.clone();
shutdown.register_shutdown_task(async move {
    if let Err(e) = redis_for_quit.quit().await {
        tracing::warn!(target: "gw_gateway", error = %e, "redis pool quit failed");
    }
});
```

CHECK: does `ShutdownCoordinator` have `register_shutdown_task` or similar? Run:

```bash
grep -n "register\|pub fn" crates/gw-runtime/src/shutdown.rs | head -20
```

If there's a register-async-fn pattern, use it. If only `register(JoinHandle)` exists, spawn a task that awaits `shutdown_token.cancelled()` then calls `pool.quit()`, and register THAT handle.

The latter form (always works):

```rust
let redis_for_quit = redis.pool.clone();
let token_for_quit = shutdown_token.clone();
let quit_handle = tokio::spawn(async move {
    token_for_quit.cancelled().await;
    if let Err(e) = redis_for_quit.quit().await {
        tracing::warn!(target: "gw_gateway", error = %e, "redis pool quit failed");
    }
});
shutdown.register(quit_handle);
```

This task awaits cancellation, runs the quit, then exits — registered with the coordinator so drain awaits it.

- [ ] **Step 3: Build + sanity test**

```bash
cargo build -p gw-gateway
cargo test -p gw-gateway
```

Expected: clean. No new test added for the Redis quit (production-only behavior; covered by integration if needed).

- [ ] **Step 4: Commit**

```bash
git add crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
fix(gw-gateway): drain-time Redis pool quit

Spawns a small task at bootstrap that awaits the ShutdownToken
cancellation and calls RedisPool::quit() — closes the fred pool's
connections gracefully during drain instead of relying on process
exit. The JoinHandle is registered with ShutdownCoordinator so drain
awaits the quit completion.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Docs sync

**Files:** `docs/spec/09-tunnel.md`, `docs/architecture.md`, `docs/ROADMAP.md`

- [ ] **Step 1: Update `docs/spec/09-tunnel.md`**

Locate the section on TunnelStack initialization / probe loop:

```bash
grep -n "probe_loop\|start_probe\|TunnelStack" docs/spec/09-tunnel.md | head -5
```

Add a sentence (or append to the existing initialization paragraph):

> **Probe loop is shutdown-aware.** `start_probe_loop` takes a `ShutdownToken` and exits cleanly on cancellation. The bootstrap registers the returned `JoinHandle` with `ShutdownCoordinator` so drain awaits the probe loop's exit before terminating the runtime.

- [ ] **Step 2: Update `docs/architecture.md`**

Append to the §Shutdown section (or wherever shutdown is documented):

> **Redis pool drain.** The bootstrap registers a small `tokio::spawn` task that awaits `ShutdownToken::cancelled()` then calls `RedisPool::quit()`. Its JoinHandle is registered with `ShutdownCoordinator`, so drain awaits the quit's completion. Connections in the fred pool close gracefully instead of relying on process exit.

> **Tunnel probe loop drain.** `TunnelStack::start_probe_loop` accepts a `ShutdownToken`; its `tokio::select!` cancellation arm exits the loop cleanly. The bootstrap registers the JoinHandle with `ShutdownCoordinator`.

- [ ] **Step 3: Update `docs/ROADMAP.md`**

Append (or remove a matching open item):

> - 2026-06-12 — Closed: Tunnel probe loop shutdown-aware; Redis pool drain-time `quit()`. Pingora upstream pool item from the original audit was a scope error — pool config is already explicit via `HttpPeer.options` in `crates/gw-proxy/src/timeouts.rs`. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 7.

- [ ] **Step 4: CLAUDE.md ⇄ AGENTS.md byte-identical body**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

- [ ] **Step 5: Commit docs**

```bash
git add docs/spec/09-tunnel.md docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync spec/architecture/roadmap with tunnel probe + Redis quit

Documents the shutdown-aware probe loop, the drain-time Redis pool
quit, and records the scope correction for the Pingora upstream pool
item (already explicitly configured via HttpPeer.options).

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Completion gate (orchestrator, SEQUENTIAL)

- [ ] `cargo test -p gw-proxy` PASS
- [ ] `cargo test -p gw-gateway` PASS
- [ ] `cargo fmt --all -- --check` (fix + chore commit if dirty)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` (sequential)
- [ ] `bash scripts/check-invariants.sh` (sequential)

---

## Task 6: Merge to local main (orchestrator)

```bash
git checkout main
git merge --no-ff fix/memory-leaks-gw-gateway -m "Merge Crate 7: tunnel probe + Redis quit shutdown-aware (FINAL)"
```

---

## Self-Review

**1. Spec coverage** — Crate 7 items:
- Tunnel probe handle → Tasks 2 (real fix)
- Redis pool quit → Task 3 (real fix)
- Pingora upstream pool — **out of scope** per pre-impl finding; recorded in ROADMAP + task 4 docs

**2. Test count** — 1 new test planned. Budget 4. Crate 7 is the smallest fix.

**3. Placeholder scan** — Task 3 Step 2 has TWO alternative implementation paths because the bootstrap drain shape varies; implementer picks based on actual code structure.

**4. Type consistency** — `ShutdownToken` parameter added to `start_probe_loop`; `ShutdownCoordinator::register(JoinHandle)` for both fixes; no new types.

---

## Notes for the Implementer

- This is the smallest crate. ≤ 2 commits of code + 1 commit of docs.
- The Redis quit wiring in Task 3 Step 2 has two alternative shapes — pick the one that matches the existing bootstrap drain orchestration. Both are correct.
- `start_probe_loop` was always returning a `JoinHandle<()>` — the only API change is adding the `ShutdownToken` parameter. Callers that previously `let _ = ...` need to register the handle now.
- BLOCKED triggers: if `redis.pool` field is consumed/moved before bootstrap returns, Task 3 needs the spawned-task pattern (Alternative 2); otherwise the direct post-drain call works.
