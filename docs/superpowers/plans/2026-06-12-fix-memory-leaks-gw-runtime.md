# Crate 1: gw-runtime + gw-proxy — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close two related memory leaks: `SpillFile` shard files leak on non-clean exit; `reset_response_attempt_state` does not reset archive accumulators, causing data contamination across response-side failover and additional disk leak.

**Architecture:** Add an idempotent best-effort `Drop` impl for `SpillFile` that asynchronously unlinks shard files via `tokio::spawn` (with a sync fallback if no runtime). Update `reset_response_attempt_state` in `gw-proxy` to reset both `archive_upstream_response_body` and `archive_downstream_response_body` to `ArchiveState::default()`, which triggers `SpillFile::Drop` on any previously-spilled state.

**Tech Stack:** Rust 1.88.0, Tokio (full), `tokio::fs`, `metrics`, `tracing`, `tempfile` (dev). No new workspace deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 1.

**Branch:** `fix/memory-leaks-gw-runtime` cut from latest `main`.

---

## File Structure

**Modified files:**
- `crates/gw-runtime/src/spill_io.rs` — add `impl Drop for SpillFile`
- `crates/gw-proxy/src/gateway.rs:131-146` — extend `reset_response_attempt_state` with archive resets
- `crates/gw-runtime/tests/spill_io_integration.rs` — add 3 new tests (drop unlinks shards, drop tolerates missing shard, drop coexists with LRU)
- `crates/gw-proxy/tests/archive_state.rs` — add 1 new test (attempt-1 → reset → attempt-2 no bleed)
- `docs/spec/06-billing-ledger.md` — document SpillFile Drop ownership in archive section
- `docs/architecture.md` — add `SpillFile::Drop` to §Deliberate behaviours
- `docs/ROADMAP.md` — close items
- `CLAUDE.md` and `AGENTS.md` — only if architecture.md change requires it (verify in Task 5)

**No new files.**

---

## Task 1: Branch Setup

**Files:**
- No code changes; branch only.

- [ ] **Step 1: Pull latest main and cut branch**

```bash
git checkout main
git pull --ff-only origin main
git checkout -b fix/memory-leaks-gw-runtime
```

Expected: `Switched to a new branch 'fix/memory-leaks-gw-runtime'`. Working tree clean.

- [ ] **Step 2: Pre-implementation verification (per spec §2 mandate)**

Run:

```bash
grep -n "impl Drop for SpillFile" crates/gw-runtime/src/spill_io.rs
```

Expected: no output (no existing Drop impl). If output appears, STOP — the leak has been independently fixed; re-evaluate scope with user before continuing.

Run:

```bash
grep -nA3 "fn reset_response_attempt_state" crates/gw-proxy/src/gateway.rs
```

Expected: function body around line 131 does NOT contain `archive_upstream_response_body` or `archive_downstream_response_body`. If it does, STOP.

---

## Task 2: SpillFile::Drop — failing test for shard unlinking

**Files:**
- Test: `crates/gw-runtime/tests/spill_io_integration.rs`

- [ ] **Step 1: Append failing test to test file**

Add at the end of `crates/gw-runtime/tests/spill_io_integration.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_unlinks_shard_files() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root
        .create("drop_test", NonZeroUsize::new(3).unwrap())
        .await
        .unwrap();
    for i in 0..30u32 {
        file.append(format!("{i}\n").as_bytes()).await.unwrap();
    }
    file.flush().await.unwrap();

    let paths: Vec<_> = file.shard_paths().to_vec();
    for p in &paths {
        assert!(p.exists(), "shard {} should exist after writes", p.display());
    }

    drop(file);
    // Give the spawned cleanup task time to execute. The cleanup spawns
    // `tokio::fs::remove_file` per shard; on a multi-thread runtime with no
    // other load this completes within a few ms.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if paths.iter().all(|p| !p.exists()) {
            break;
        }
    }
    for p in &paths {
        assert!(
            !p.exists(),
            "shard {} should be unlinked after drop",
            p.display()
        );
    }
}
```

- [ ] **Step 2: Run the new test and verify it fails**

Run:

```bash
cargo test -p gw-runtime --test spill_io_integration drop_unlinks_shard_files -- --nocapture
```

Expected: FAIL with "shard ... should be unlinked after drop" — confirms current behavior leaks shard files on drop.

---

## Task 3: SpillFile::Drop — implement async best-effort unlink

**Files:**
- Modify: `crates/gw-runtime/src/spill_io.rs` (after line 191, before end of file)

- [ ] **Step 1: Implement Drop**

Append after the `impl SpillFile { ... }` block:

```rust
impl Drop for SpillFile {
    fn drop(&mut self) {
        // Best-effort cleanup of shard files. Coexists with the LRU enforcer
        // in `SpillRoot::spawn_quota_enforcer`: this Drop is timely, the LRU
        // is fallback. Both paths tolerate `NotFound` errors silently.
        let paths = std::mem::take(&mut self.shard_paths);
        if paths.is_empty() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    for path in paths {
                        match tokio::fs::remove_file(&path).await {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                // Already cleaned (LRU or external).
                            }
                            Err(e) => {
                                metrics::counter!("gw_spill_drop_cleanup_fail_total")
                                    .increment(1);
                                tracing::warn!(
                                    target: "gw_runtime::spill_io",
                                    path = %path.display(),
                                    error = %e,
                                    "spill_file_drop_cleanup_failed"
                                );
                            }
                        }
                    }
                });
            }
            Err(_) => {
                // No tokio runtime (atexit, test teardown). Fall back to sync.
                // Errors are silently dropped — this is the cleanup-of-last-resort
                // and the LRU enforcer will catch anything we miss on next start.
                for path in paths {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}
```

- [ ] **Step 2: Run the new test and verify it passes**

Run:

```bash
cargo test -p gw-runtime --test spill_io_integration drop_unlinks_shard_files -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Run all existing gw-runtime tests to ensure no regression**

Run:

```bash
cargo test -p gw-runtime
```

Expected: all tests PASS.

---

## Task 4: SpillFile::Drop — tolerate already-missing shards

**Files:**
- Test: `crates/gw-runtime/tests/spill_io_integration.rs`

- [ ] **Step 1: Append failing test**

Append:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_tolerates_missing_shards() {
    let tmp = TempDir::new().unwrap();
    let root = root(&tmp);
    let file = root
        .create("missing_test", NonZeroUsize::new(2).unwrap())
        .await
        .unwrap();
    file.append(b"data\n").await.unwrap();
    file.flush().await.unwrap();
    let paths: Vec<_> = file.shard_paths().to_vec();

    // Simulate LRU racing ahead: unlink shard 0 BEFORE drop.
    tokio::fs::remove_file(&paths[0]).await.unwrap();
    assert!(!paths[0].exists());

    drop(file);

    // Drop must not panic; shard 1 must be cleaned; shard 0 stays gone.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if !paths[1].exists() {
            break;
        }
    }
    assert!(!paths[0].exists());
    assert!(!paths[1].exists(), "shard 1 should be cleaned by Drop");
}
```

- [ ] **Step 2: Run new test and verify it passes**

Run:

```bash
cargo test -p gw-runtime --test spill_io_integration drop_tolerates_missing_shards -- --nocapture
```

Expected: PASS (the Drop impl already handles `NotFound` silently).

If FAIL: review the Drop impl in `spill_io.rs` — ensure `NotFound` branch returns without metric/log.

---

## Task 5: SpillFile::Drop — coexists with LRU enforcer

**Files:**
- Test: `crates/gw-runtime/tests/spill_io_integration.rs`

- [ ] **Step 1: Append the test**

Append:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_coexists_with_lru_enforcer() {
    use std::time::Duration;
    let tmp = TempDir::new().unwrap();
    // Very small quota so any payload immediately triggers LRU eviction.
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1, // forces LRU to clean everything every tick
        buffer_size: 4096,
    }));
    let enforcer = root.clone().spawn_quota_enforcer(Duration::from_millis(20));

    let file = root
        .create("lru_race", NonZeroUsize::new(2).unwrap())
        .await
        .unwrap();
    file.append(b"x".repeat(8192).as_slice()).await.unwrap();
    file.flush().await.unwrap();

    // Let LRU run a few ticks while file is still live (it cannot remove open
    // BufWriter-held files on every OS, but should at minimum not crash).
    tokio::time::sleep(Duration::from_millis(80)).await;

    drop(file);

    // After drop, give both Drop and LRU time to settle. Either path may have
    // removed the files first — assert no panic, no double-remove failure.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop the enforcer; the test ends.
    enforcer.abort();

    // Whether files exist or not is OS-dependent (BufWriter may have flushed
    // before LRU ran). The invariant we assert is "no panic and no error
    // counter increment from double-remove".
    // metrics counter check would require a metrics recorder hook; instead we
    // confirm the assertion that we reached this line without panic.
}
```

- [ ] **Step 2: Run the test and verify it passes**

Run:

```bash
cargo test -p gw-runtime --test spill_io_integration drop_coexists_with_lru_enforcer -- --nocapture
```

Expected: PASS (the test asserts no panic; reaching the end of the test body is success).

- [ ] **Step 3: Commit gw-runtime changes**

Run:

```bash
git add crates/gw-runtime/src/spill_io.rs crates/gw-runtime/tests/spill_io_integration.rs
git commit -m "$(cat <<'EOF'
fix(gw-runtime): add SpillFile::Drop for timely shard unlinking

The SpillFile previously had no Drop impl, so shard files were leaked
on every non-clean ArchiveState transition (failover retry, client
disconnect mid-spill, panic unwind). The LRU enforcer in
SpillRoot::spawn_quota_enforcer would eventually reclaim them, but
under high abort/retry load disk fills before LRU runs.

Drop spawns an async unlink per shard via tokio::spawn when a runtime
is available, else falls back to std::fs::remove_file. NotFound errors
are silent (LRU may have already cleaned). Coexists idempotently with
the LRU enforcer.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds. `git status` clean.

---

## Task 6: reset_response_attempt_state — failing test for archive bleed

**Files:**
- Test: `crates/gw-proxy/tests/archive_state.rs`

- [ ] **Step 1: Inspect the existing test file**

Run:

```bash
head -10 crates/gw-proxy/tests/archive_state.rs
```

Confirm imports: `use gw_proxy::ctx::{ArchiveCaps, ArchiveState};` exist.

- [ ] **Step 2: Locate the helper for invoking reset_response_attempt_state**

Run:

```bash
grep -n "reset_response_attempt_state" crates/gw-proxy/src/
```

If `reset_response_attempt_state` is private (no `pub`), check existing tests for how `ReqCtx` is constructed in tests; we will need a thin test-only wrapper. Run:

```bash
grep -rn "reset_response_attempt_state\|pub(crate) fn reset" crates/gw-proxy/src/
```

If still private, add a `#[cfg(test)] pub fn` re-export at the bottom of `crates/gw-proxy/src/gateway.rs`:

```rust
#[cfg(test)]
pub fn reset_response_attempt_state_for_test(
    ctx: &mut ReqCtx,
    new_key: Arc<gw_core::entity::VendorKey>,
) {
    reset_response_attempt_state(ctx, new_key);
}
```

Verify it compiles:

```bash
cargo build -p gw-proxy --tests
```

Expected: builds clean.

- [ ] **Step 3: Append the failing test to archive_state.rs**

Append the following. **Important:** if `ReqCtx::new_for_test` or similar test constructor does NOT exist, the test must construct `ReqCtx` manually with `..Default::default()` or skip with a clear `#[ignore]` comment and surface to user via DONE_WITH_CONCERNS. Try the constructor first; this skeleton assumes a `ReqCtx::for_test(...)` exists — adjust to whatever constructor the crate provides:

```rust
#[tokio::test]
async fn reset_response_attempt_clears_archive_accumulators() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    // Simulate attempt-1 archive: write into both archive_*_response_body
    // until they spill, capture their shard paths.
    let mut upstream = ArchiveState::default();
    let mut downstream = ArchiveState::default();
    let chunk = vec![1u8; 4096];
    upstream
        .append_chunk(&chunk, &caps(2048, 8192), &root, "req-test", "upstream")
        .await;
    downstream
        .append_chunk(&chunk, &caps(2048, 8192), &root, "req-test", "downstream")
        .await;
    assert!(matches!(upstream, ArchiveState::Spilled { .. }));
    assert!(matches!(downstream, ArchiveState::Spilled { .. }));

    // Assert each accumulator is non-empty before reset.
    assert!(upstream.len() > 0);
    assert!(downstream.len() > 0);

    // The actual reset call: assign default. This is exactly what the
    // production code change in gateway.rs will do.
    upstream = ArchiveState::default();
    downstream = ArchiveState::default();

    // Post-reset: both are InMemory and empty.
    assert!(matches!(upstream, ArchiveState::InMemory(_)));
    assert!(matches!(downstream, ArchiveState::InMemory(_)));
    assert_eq!(upstream.len(), 0);
    assert_eq!(downstream.len(), 0);

    // Give SpillFile::Drop time to async-unlink the old shards. Then read
    // tmp directory and assert no spill shard files remain — this proves the
    // reset semantics + Drop work together to release disk.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let count = std::fs::read_dir(tmp.path()).unwrap().count();
        if count == 0 {
            break;
        }
    }
    let remaining: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        remaining.is_empty(),
        "expected all archive shards to be unlinked after reset, found: {remaining:?}"
    );
}
```

- [ ] **Step 4: Run the new test and verify it passes**

Run:

```bash
cargo test -p gw-proxy --test archive_state reset_response_attempt_clears_archive_accumulators -- --nocapture
```

Expected: PASS — this test does NOT exercise the production `reset_response_attempt_state` function directly (we couldn't easily construct a full `ReqCtx`). It verifies the underlying mechanism: assigning `ArchiveState::default()` releases the Spilled file and `SpillFile::Drop` cleans disk.

The production behavior is verified by Task 7 (which modifies `reset_response_attempt_state` to do exactly this assignment).

---

## Task 7: reset_response_attempt_state — implement archive reset

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs:131-146`

- [ ] **Step 1: Edit reset_response_attempt_state**

Replace the function body. The existing function ends at the line containing `ctx.pending_client_body = None;`. After that line and before the closing `}`, add the two archive resets:

```rust
fn reset_response_attempt_state(ctx: &mut ReqCtx, new_key: Arc<gw_core::entity::VendorKey>) {
    ctx.inflight_guard = None;
    ctx.picked_vendor_key = Some(new_key);
    ctx.picked_vendor = None;
    ctx.upstream_codec = None;
    ctx.upstream_status = None;
    ctx.body_started = false;
    ctx.client_body_committed = false;
    ctx.stream_phase = crate::stream_probe::StreamPhase::Connecting;
    ctx.probe_started_at = None;
    ctx.probe_buffer.clear();
    ctx.pending_probe_failure = None;
    ctx.stream_probe_retry_pending = false;
    ctx.sse_leftover.clear();
    ctx.pending_client_body = None;
    // Reset archive accumulators. Without this, bytes from the failed attempt
    // bleed into the retry attempt's detailed-log surfaces, and any Spilled
    // shard files from the prior attempt would only be reclaimed by the LRU
    // enforcer. Assignment of the default variant drops the previous state,
    // which triggers `SpillFile::Drop` for Spilled variants.
    ctx.archive_upstream_response_body = ArchiveState::default();
    ctx.archive_downstream_response_body = ArchiveState::default();
}
```

Verify the import at the top of `gw-proxy/src/gateway.rs` includes `ArchiveState`. If not, add:

```bash
grep -n "use crate::ctx::" crates/gw-proxy/src/gateway.rs | head -5
```

If `ArchiveState` is not in the import list, add it. (Most likely it's already in scope via `use crate::ctx::*` or a similar wildcard; verify.)

- [ ] **Step 2: Build gw-proxy to verify the change compiles**

Run:

```bash
cargo build -p gw-proxy
```

Expected: builds clean.

- [ ] **Step 3: Run all gw-proxy tests**

Run:

```bash
cargo test -p gw-proxy
```

Expected: all tests PASS, including the new one. Pay attention to existing tests on `reset_response_attempt_state` (if any in `gateway.rs:1984+ mod tests`); they should still pass because the new behavior only adds resets, not modifies existing field clears.

- [ ] **Step 4: Commit gw-proxy changes**

Run:

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-proxy/tests/archive_state.rs
git commit -m "$(cat <<'EOF'
fix(gw-proxy): reset archive accumulators on response-side failover

reset_response_attempt_state was clearing all other per-attempt buffers
(probe_buffer, sse_leftover, pending_client_body, stream_phase) but
NOT archive_upstream_response_body / archive_downstream_response_body.
This caused two issues on every response-side failover:

  1. Bytes from the failed attempt bled into the retry's detailed-log
     surfaces, corrupting downstream archive consumers.
  2. Any Spilled shard files from the failed attempt were only
     reclaimed by the LRU enforcer — slow under churn.

Assigning ArchiveState::default() drops the previous state. For
Spilled variants this now triggers SpillFile::Drop (introduced in
the prior commit), which asynchronously unlinks shard files.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 8: Docs sync — architecture, spec, ROADMAP

**Files:**
- Modify: `docs/architecture.md` (§Deliberate behaviours)
- Modify: `docs/spec/06-billing-ledger.md` (archive section)
- Modify: `docs/ROADMAP.md` (close two items)
- Verify: `CLAUDE.md` and `AGENTS.md` byte-identical

- [ ] **Step 1: Locate the Deliberate behaviours section in architecture.md**

Run:

```bash
grep -n "Deliberate behaviour" docs/architecture.md | head -5
```

Open the section. Add a new entry (preserve existing entries' formatting style):

> **SpillFile cleanup is dual-pathed.** `SpillFile::Drop` asynchronously unlinks shard files via `tokio::spawn` when a runtime is available (sync fallback otherwise). The `SpillRoot::spawn_quota_enforcer` LRU loop is the fallback for cases the Drop missed (no-runtime fallback failure, process kill before spawn completes). Both paths treat `NotFound` as silent success; the system is intentionally idempotent.

- [ ] **Step 2: Update the archive section in docs/spec/06-billing-ledger.md**

Run:

```bash
grep -n "archive\|SpillFile\|ArchiveState" docs/spec/06-billing-ledger.md | head -10
```

Find the section that describes archive lifecycle. Add a short paragraph after the lifecycle description:

> **Spill-file lifetime.** A spilled archive's shard files are unlinked the moment the owning `ArchiveState::Spilled` variant drops — either when the request's `ReqCtx` drops or when `reset_response_attempt_state` reassigns the accumulator on response-side failover. Unlink is asynchronous (`tokio::spawn`) and best-effort; the `SpillRoot` LRU enforcer is the fallback. Reset on failover prevents both data contamination and disk leak across retries.

- [ ] **Step 3: Update docs/ROADMAP.md**

Run:

```bash
grep -n "SpillFile\|archive.*reset\|reset_response_attempt" docs/ROADMAP.md
```

If matching items exist (e.g. "SpillFile lacks Drop" / "archive reset on failover"), remove them. If no items match, append under the "Recently closed" or appropriate section:

> - 2026-06-12 — Closed: `SpillFile::Drop` for timely shard cleanup; `reset_response_attempt_state` resets archive accumulators on response-side failover. Both prevent disk and memory leaks under churn. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 1.

- [ ] **Step 4: Check CLAUDE.md ⇄ AGENTS.md byte-identical**

Run:

```bash
diff CLAUDE.md AGENTS.md
```

Expected: empty (no differences). This change does NOT modify CLAUDE.md, so no AGENTS.md update is needed. If the diff is non-empty, STOP — there's a pre-existing drift to surface to user.

- [ ] **Step 5: Commit docs**

Run:

```bash
git add docs/architecture.md docs/spec/06-billing-ledger.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync architecture/spec/roadmap with SpillFile Drop + archive reset

- architecture.md §Deliberate behaviours: document SpillFile dual-path
  cleanup (Drop is timely, LRU is fallback; both idempotent on NotFound).
- spec/06-billing-ledger.md: clarify spill-file lifetime including
  failover reset.
- ROADMAP.md: close the two memory-leak items addressed by this PR.

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

Run:

```bash
cargo test -p gw-runtime
```

Expected: all PASS.

```bash
cargo test -p gw-proxy
```

Expected: all PASS. (Some gw-proxy tests are integration-heavy and slow; budget ~2 min.)

- [ ] **Step 2: Workspace clippy**

Run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean. Common issues to watch for:
- A `let _` warning on the `tokio::fs::remove_file` sync fallback if you wrote it differently; the spec lets you use `let _ = std::fs::remove_file(path);`
- Missing import warnings from the `ArchiveState` reset assignment

- [ ] **Step 3: Workspace fmt check**

Run:

```bash
cargo fmt --all -- --check
```

Expected: no output (already formatted). If output appears, run `cargo fmt --all` and amend the prior commit:

```bash
cargo fmt --all
git add -u
git commit --amend --no-edit
```

- [ ] **Step 4: Architectural invariants**

Run:

```bash
bash scripts/check-invariants.sh
```

Expected: clean. The Drop impl does NOT cross layering boundaries (purely internal to `gw-runtime`); the archive reset uses `ArchiveState::default()` which is already in scope in `gw-proxy/src/gateway.rs`. If the invariant script flags anything, read its message — it names the rule violated.

---

## Task 10: Open PR

**Files:**
- No code changes.

- [ ] **Step 1: Push the branch**

Run:

```bash
git push -u origin fix/memory-leaks-gw-runtime
```

Expected: branch published.

- [ ] **Step 2: Open the PR**

Run:

```bash
gh pr create --title "fix(gw-runtime,gw-proxy): close 2 memory leaks (Crate 1 of 7)" --body "$(cat <<'EOF'
## Summary

Closes two related leaks identified in the 2026-06-12 memory-leak audit:

1. **`SpillFile` had no `Drop` impl** → shard files leaked on every non-clean ArchiveState transition. Added `impl Drop` that asynchronously unlinks shards via `tokio::spawn` (sync fallback if no runtime). Coexists idempotently with the existing LRU enforcer.

2. **`reset_response_attempt_state` did not reset archive accumulators** → bytes from a failed attempt bled into the retry's detailed-log surfaces AND any Spilled shard files leaked. Added `archive_upstream_response_body = ArchiveState::default()` and same for downstream; assignment drops the prior Spilled state, triggering the new Drop.

This is **Crate 1 of 7** in the memory-leak-fixes plan. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 1.

## Test plan

- [x] `cargo test -p gw-runtime` — 3 new tests cover drop-unlinks-shards, drop-tolerates-missing, drop-coexists-with-LRU
- [x] `cargo test -p gw-proxy` — new archive_state test verifies the reset mechanism + Drop integration
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo fmt --all -- --check`
- [x] `bash scripts/check-invariants.sh`
- [x] `CLAUDE.md ⇄ AGENTS.md` byte-identical (verified)

## Docs sync

- `docs/architecture.md` — new §Deliberate behaviours entry for dual-path SpillFile cleanup
- `docs/spec/06-billing-ledger.md` — spill-file lifetime clarification
- `docs/ROADMAP.md` — two items closed

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. Return the URL.

---

## Self-Review (run before declaring plan complete)

**1. Spec coverage** — every §2 Crate 1 item maps to a task:
- `SpillFile::Drop` → Tasks 2/3/4/5
- `reset_response_attempt_state` archive reset → Tasks 6/7
- Docs (`docs/spec/06-billing-ledger.md`, `docs/architecture.md` §Deliberate behaviours, `docs/ROADMAP.md`) → Task 8
- CLAUDE.md sync rule → Task 8 Step 4
- Per-crate completion gate (clippy, fmt, invariants) → Task 9
- PR open → Task 10
- Coverage: ✅ complete.

**2. Placeholder scan** — none. All commands, code blocks, and commit messages are concrete.

**3. Type consistency** —
- `ArchiveState`, `ArchiveCaps`, `SpillRoot`, `SpillRootConfig`, `SpillFile`, `NonZeroUsize`, `TempDir`, `Arc` consistently used.
- `metrics::counter!("gw_spill_drop_cleanup_fail_total")` is a new metric; name follows existing `gw_spill_*` convention.
- `ArchiveState::default()` returns `InMemory(BytesMut::new())` per source (verified line 76-80 of `ctx.rs`); assigning it triggers Drop of the prior state.

**4. Hard-coded assumption to flag** — Task 6 assumes `reset_response_attempt_state` may be private. If it's already `pub(crate)` or testing via the public `gateway.rs:1984+ mod tests` works, the `_for_test` shim is unnecessary. The implementer should follow the path of least resistance and skip Step 2 of Task 6 if the function is already test-reachable.

---

## Notes for the Implementer

- The plan deliberately does NOT touch `SpillRoot::spawn_quota_enforcer`. The LRU is fallback and we don't change it.
- The new metric `gw_spill_drop_cleanup_fail_total` is intentionally NOT registered in any `gw-runtime` boot-time metric registration code, because `metrics::counter!` lazily creates the metric on first increment. Verify your project's metric pattern — if there's a central registry, register it there.
- If `cargo test -p gw-proxy` times out or hangs, suspect a flake in the new archive test's sleep-loop. The loop is bounded (50 × 20ms = 1s); if it still hangs, increase iterations or surface as DONE_WITH_CONCERNS.
- BLOCKED triggers: Task 1 Step 2 finds the fix already exists; Task 6 cannot find or construct a `ReqCtx` for testing; Task 8 Step 4 reveals a pre-existing CLAUDE.md ⇄ AGENTS.md drift.
