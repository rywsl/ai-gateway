# Crate 6: gw-capability — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Eliminate the `Box::leak` per unknown capability string in `gw-capability/src/resolver.rs:119`. Replace with `Arc<str>` + Moka intern table so distinct tag strings share a single `Arc` and the keyspace is bounded.

**Architecture:** Change `Capability::Tagged(&'static str)` → `Capability::Tagged(Arc<str>)`. The enum loses `Copy` (becomes `Clone`). All `Capability` callsites that depend on `Copy` get a `.clone()`. A global Moka `Cache<Box<str>, Arc<str>>` (cap 10_000, no TTI — Arcs in `Tagged` would dangle if evicted from intern table; instead, max_capacity bounds the intern table itself) interns tag strings.

**Tech Stack:** Rust 1.88.0, `moka::sync::Cache` (workspace dep), no new deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 6.

**Branch:** `fix/memory-leaks-gw-capability` cut from local `main` (includes Crate 5 merge `7537fba`).

**Test budget**: ≤ 6 new tests (small crate, plan specifies 4).

---

## Pre-Implementation Findings

| Item | Status |
|------|--------|
| `Box::leak` in `parse_capability` | ✅ confirmed at `resolver.rs:119` |
| `Capability` enum currently `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]` | ✅ confirmed at `types.rs:3` |
| `Capability::Tagged(&'static str)` | ✅ confirmed at `types.rs:13` |
| `Capability::Tagged` actual callsites outside gw-capability | None — only `gw-capability/src/{resolver,filter}.rs` |
| `Capability` (any variant) callsites outside gw-capability | Only `gw-proxy/tests/capability_filter.rs` (8 callsites, all by-ref or variant-construction) |
| `*c` (Copy deref) callsites | 1 — `resolver.rs:63` in `r.add(*c, ...)` |
| `Vec<Capability>` / `SmallVec<[Capability; N]>` | A handful — SmallVec/Vec work fine with Clone, no Copy needed |

**Verdict:** removing `Copy` is contained. Main edit surface: `types.rs` (1 line on derive), `resolver.rs` (the `Box::leak` line + 1 `*c` dereference + by-value `c` in `add` callsites), and minor adjustments in `filter.rs`. Tests in `gw-proxy/tests/capability_filter.rs` use `&Capability::X` or pass variants by value — both Clone-friendly.

---

## File Structure

**Modified:**
- `crates/gw-capability/src/types.rs` — `Capability` enum: `Tagged(&'static str)` → `Tagged(Arc<str>)`; remove `Copy` from derives
- `crates/gw-capability/src/resolver.rs` — module-level intern cache + `parse_capability` returns `Capability` constructed from interned `Arc<str>`; `*c` → `c.clone()` in line 63
- `crates/gw-capability/src/filter.rs` — adapt any `Capability::Tagged(s)` matches (verify by grep — likely just pattern matching, no Copy dependency)
- `crates/gw-capability/Cargo.toml` — add `moka = { workspace = true }` if missing
- Any caller that compiles-broken from removing Copy: add `.clone()` (mechanical)
- `docs/spec/<capability-file>.md` — document new Tagged semantic + Moka intern bound
- `docs/architecture.md` §Deliberate behaviours — entry on bounded interning
- `docs/ROADMAP.md` — close items

**Test files:**
- `crates/gw-capability/src/resolver.rs` mod tests — 3 new tests (intern reuse, distinct strings get distinct Arcs, intern table bounded by capacity)
- `crates/gw-capability/src/types.rs` — 1 new test (Capability::Tagged Eq + Hash on Arc<str>)

**Total: 4 new tests.** Budget 6. Margin 2.

---

## Task 1: Branch Setup + Verification

- [ ] **Step 1: Cut branch**

```bash
git checkout main
git status                # expect: ahead-of-origin, clean
git log --oneline -3      # expect: 7537fba (Crate 5 merge) at top
git checkout -b fix/memory-leaks-gw-capability
```

- [ ] **Step 2: Verify scope hasn't moved**

```bash
grep -n "Box::leak" crates/gw-capability/src/resolver.rs
```
Expected: line 119.

```bash
grep -n "Tagged" crates/gw-capability/src/types.rs
```
Expected: `Tagged(&'static str),` at line 13.

```bash
grep -n "Clone, Copy" crates/gw-capability/src/types.rs
```
Expected: line 3 derives include `Copy`.

```bash
grep -n "^moka" crates/gw-capability/Cargo.toml
```
If empty: dep needs adding (Task 2 Step 1).

---

## Task 2: Add Moka dep + intern table

**Files:** `crates/gw-capability/src/resolver.rs`, `crates/gw-capability/Cargo.toml`

- [ ] **Step 1: Add moka dep if missing**

If Task 1 Step 2 showed no moka in Cargo.toml, add under `[dependencies]`:
```toml
moka = { workspace = true }
```

- [ ] **Step 2: Add the intern table inside `resolver.rs`**

Near the top of `resolver.rs` (after the existing `use` block):

```rust
use moka::sync::Cache;
use std::sync::Arc;
use std::sync::LazyLock;

/// Bounded intern table for `Capability::Tagged` strings. Maps a tag
/// string to a shared `Arc<str>` so distinct requests with the same
/// tag share storage. Capacity-bounded (10_000); does NOT use TTI
/// because the resulting `Arc<str>` is embedded in `Capability::Tagged`
/// values held by callers — TTI eviction would not free those, only
/// orphan the intern slot. Max-capacity LRU eviction is acceptable:
/// in steady state an evicted tag is re-interned to a new (but
/// semantically equivalent) `Arc`. Equality / hash on `Capability`
/// uses `Arc<str>`'s `str` comparison, so re-interned Arcs match.
const CAPABILITY_INTERN_CAPACITY: u64 = 10_000;

static CAPABILITY_INTERN: LazyLock<Cache<Box<str>, Arc<str>>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(CAPABILITY_INTERN_CAPACITY)
        .build()
});

/// Intern a tag string. Returns a shared `Arc<str>` if the cache had
/// it; otherwise creates a fresh `Arc<str>` and stores it. Even when
/// evicted (capacity overflow) and re-interned, two `Capability::Tagged`
/// values with the same str content compare equal via `Arc<str>` deref.
fn intern_tag(s: &str) -> Arc<str> {
    let key: Box<str> = s.into();
    CAPABILITY_INTERN.get_with(key, || Arc::<str>::from(s))
}
```

- [ ] **Step 3: Rewrite the `Box::leak` line**

At `resolver.rs:119`, replace:

```rust
other => Capability::Tagged(Box::leak(other.to_string().into_boxed_str())),
```

with:

```rust
other => Capability::Tagged(intern_tag(other)),
```

(File won't compile yet because `Capability::Tagged` still takes `&'static str` — fixed in Task 3.)

---

## Task 3: Capability enum: Tagged(Arc<str>), remove Copy

**Files:** `crates/gw-capability/src/types.rs`, `crates/gw-capability/src/resolver.rs`, `crates/gw-capability/src/filter.rs`, possibly callers

- [ ] **Step 1: Failing tests (types-level Eq/Hash semantics)**

Append to or create `#[cfg(test)] mod tests` at the bottom of `types.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn tagged_arc_strings_equal_when_content_matches() {
        // Two Tagged values constructed from distinct Arcs but with
        // the same content must compare equal — this is what makes
        // intern eviction observably benign.
        let a = Capability::Tagged(Arc::<str>::from("custom-foo"));
        let b = Capability::Tagged(Arc::<str>::from("custom-foo"));
        assert_eq!(a, b);

        let c = Capability::Tagged(Arc::<str>::from("custom-bar"));
        assert_ne!(a, c);
    }

    #[test]
    fn tagged_hash_consistent_with_eq() {
        use std::collections::HashSet;
        let mut set: HashSet<Capability> = HashSet::new();
        set.insert(Capability::Tagged(Arc::<str>::from("custom-foo")));
        assert!(set.contains(&Capability::Tagged(Arc::<str>::from("custom-foo"))));
    }
}
```

- [ ] **Step 2: Verify the tests fail to compile**

```bash
cargo test -p gw-capability --lib types::tests 2>&1 | tail -10
```

Expected: error — `Capability::Tagged` currently takes `&'static str`, can't construct with `Arc<str>`.

- [ ] **Step 3: Modify the enum**

Replace lines 3-14 of `crates/gw-capability/src/types.rs`:

```rust
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    Vision,
    Audio,
    Video,
    Thinking,
    ToolUse,
    StreamingOnly,
    OpenAiResponsesShape,
    VertexAiServiceAccount,
    Tagged(Arc<str>),
}
```

Note: `Copy` removed. `PartialEq` / `Eq` / `Hash` on `Arc<str>` delegate to the underlying `str`, so two `Tagged(Arc::<str>::from("x"))` instances compare equal even across Arc re-creation — which is critical for the intern-eviction case.

- [ ] **Step 4: Fix `resolver.rs` callers**

Find the `*c` dereference at `resolver.rs:63`:

```bash
grep -n "\*c\b" crates/gw-capability/src/resolver.rs
```

Replace `*c` with `c.clone()` at that line. (The full context:)

```rust
for c in self.upgrade_path.iter() {
    r.add(c.clone(), CapabilityOrigin::RetryUpgrade);
}
```

Build to find more Copy-dependent sites:

```bash
cargo build -p gw-capability 2>&1 | grep -E "error\[" | head -20
```

For each error referencing a moved `Capability` or "expected `Capability::Tagged(...)` pattern" issue, add `.clone()` or use by-reference. Common patterns to expect:

- `if c == Capability::Vision` — still works (no Copy needed for comparing variants).
- `set.contains(&c)` — works.
- `r.add(c, origin)` where `c: Capability` is held by reference iterator — change to `r.add(c.clone(), origin)`.
- `match c { Capability::Tagged(s) => ... }` — `s` was `&'static str`, now `&Arc<str>`. Adjust uses of `s` to `s.as_ref()` or `&**s` if `&str` is needed.

- [ ] **Step 5: Fix `filter.rs`**

```bash
grep -n "Capability::Tagged" crates/gw-capability/src/filter.rs
```

For each match arm or constructor, mirror the resolver fix: dereference via `&**s` for `&str`, or `.clone()` for ownership.

- [ ] **Step 6: Workspace-wide build to catch external impacts**

```bash
cargo build --workspace 2>&1 | grep -E "error\[" | head -30
```

For each error in any crate:
- If it's a `*c` dereference in test code, change to `c.clone()`.
- If it's a `c` being used by-value after a borrow, restructure to clone earlier.
- If it's a pattern match on `Capability::Tagged(s)` where `s` was used as `&str`, change uses of `s` to `s.as_ref()` or `&**s`.

Iterate until `cargo build --workspace` is clean.

- [ ] **Step 7: Run all tests**

```bash
cargo test -p gw-capability
cargo test -p gw-proxy --test capability_filter
```

Expected: PASS.

- [ ] **Step 8: Commit (single commit for types change + all callers)**

```bash
git add -u
git add crates/gw-capability/src/resolver.rs crates/gw-capability/src/types.rs crates/gw-capability/Cargo.toml
git commit -m "$(cat <<'EOF'
fix(gw-capability): intern Tagged capability strings via Moka

The previous `Box::leak(other.to_string().into_boxed_str())` at
resolver.rs:119 produced a permanent allocation per distinct unknown
capability string. Attacker-controllable input (arbitrary header
strings) made this an unbounded memory leak.

Changes:
- `Capability::Tagged(&'static str)` → `Capability::Tagged(Arc<str>)`.
- `Capability` enum loses `Copy` (now `Clone`); callers updated.
- New `CAPABILITY_INTERN: Cache<Box<str>, Arc<str>>` (max_capacity
  10_000) interns tag strings; `parse_capability` for an unknown tag
  returns a shared `Arc<str>`. Equality / hash on `Capability` use
  the underlying `str` content, so capacity-evicted-and-re-interned
  Arcs compare equal to live ones — eviction is observably benign.

Public-API impact: `Capability` is no longer `Copy`. All callers
(internal + 1 test file in gw-proxy) updated to use `.clone()` /
by-reference where the Copy semantic was previously implicit.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Intern-table behavior tests

**Files:** `crates/gw-capability/src/resolver.rs`

- [ ] **Step 1: Failing tests**

Append to the existing `mod tests` (or create one) in `resolver.rs`:

```rust
#[cfg(test)]
mod intern_tests {
    use super::*;

    #[test]
    fn parse_capability_returns_known_variant_for_known_string() {
        assert_eq!(parse_capability("vision"), Capability::Vision);
        assert_eq!(parse_capability("thinking"), Capability::Thinking);
    }

    #[test]
    fn parse_capability_interns_unknown_string() {
        let a = parse_capability("custom-foo");
        let b = parse_capability("custom-foo");
        // Both should equal each other and contain the same string.
        assert_eq!(a, b);
        match (&a, &b) {
            (Capability::Tagged(arc_a), Capability::Tagged(arc_b)) => {
                // Critical: the SAME Arc is returned (pointer equality on
                // the underlying str), proving the intern table is shared.
                assert!(
                    Arc::ptr_eq(arc_a, arc_b),
                    "Tagged Arcs for identical strings must be the same Arc"
                );
            }
            _ => panic!("expected Tagged variants"),
        }
    }

    #[test]
    fn intern_table_capacity_bounded_under_attack() {
        // Push past capacity. Equality still holds via Arc<str> content
        // comparison even after LRU eviction re-interns.
        for i in 0..(CAPABILITY_INTERN_CAPACITY as usize + 100) {
            let _ = parse_capability(&format!("attack-tag-{i}"));
        }
        CAPABILITY_INTERN.run_pending_tasks();
        assert!(
            CAPABILITY_INTERN.entry_count() <= CAPABILITY_INTERN_CAPACITY,
            "intern table must cap at CAPABILITY_INTERN_CAPACITY, got {}",
            CAPABILITY_INTERN.entry_count()
        );
    }
}
```

- [ ] **Step 2: Run new tests**

```bash
cargo test -p gw-capability --lib resolver::intern_tests
```

Expected: 3/3 PASS.

- [ ] **Step 3: Run full gw-capability test suite**

```bash
cargo test -p gw-capability
```

Expected: all PASS.

- [ ] **Step 4: Commit tests**

```bash
git add crates/gw-capability/src/resolver.rs
git commit -m "$(cat <<'EOF'
test(gw-capability): intern behaviour + cap-bounded under attack

Three new tests:
- parse_capability returns known variant for known string (regression
  guard for the lookup table).
- parse_capability with identical unknown string returns Arc::ptr_eq
  Arcs — proves the intern table is sharing.
- Pushing > CAPABILITY_INTERN_CAPACITY distinct strings caps the
  intern table at that limit — the attacker-controllable leak path
  identified by the audit is now bounded.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Docs sync

**Files:** `docs/spec/<capability-file>.md`, `docs/architecture.md`, `docs/ROADMAP.md`

- [ ] **Step 1: Locate the capability spec file**

```bash
ls docs/spec/ | grep -i capab
```

If a dedicated file exists (`02-capability.md` per the original spec, or whatever it actually is), use that. If no dedicated file, add a paragraph to the closest matching domain doc (likely the routing or vendor file) — surface a DONE_WITH_CONCERNS note if you can't find an obvious home.

- [ ] **Step 2: Update the capability spec (or closest doc)**

Add:

> **`Capability::Tagged` is `Arc<str>`, interned.** Unknown capability strings (header-driven, model-driven) flow through `parse_capability` which interns the string in a global `Moka::sync::Cache<Box<str>, Arc<str>>` (max_capacity 10_000). The previous `Box::leak` per distinct unknown string is replaced — the intern table is the upper bound on capability-string memory.
>
> The `Capability` enum is `Clone` (not `Copy`); callers pass by reference or clone. Equality and hashing delegate to the underlying `str`, so two `Tagged` values with the same content compare equal even if the intern table evicted and re-interned the string between observations.

- [ ] **Step 3: Update `docs/architecture.md` §Deliberate behaviours**

Append:

> **`Capability::Tagged` strings are bounded-interned.** Up to `CAPABILITY_INTERN_CAPACITY = 10_000` distinct unknown capability strings share a global `Arc<str>` via a Moka cache. LRU eviction at capacity is benign because `Capability::PartialEq` / `Hash` compare by `str` content — an evicted tag re-interned later compares equal to a live `Tagged` value with the same content. The previous `Box::leak`-per-unknown-string path is replaced.

- [ ] **Step 4: Update `docs/ROADMAP.md`**

Append (or remove matching open item, whichever is appropriate):

> - 2026-06-12 — Closed: `Capability::Tagged` `Box::leak` replaced with Moka intern table (cap 10_000); `Capability` enum API: `Copy` → `Clone`. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 6.

- [ ] **Step 5: CLAUDE.md ⇄ AGENTS.md check**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

- [ ] **Step 6: Commit docs**

```bash
git add docs/spec docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync spec/architecture/roadmap with Capability intern + Clone

Documents the Capability::Tagged Arc<str> + Moka intern bound, the
Copy→Clone public API change, and the equality semantic that makes
LRU eviction observably benign.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Completion gate (orchestrator-run, SEQUENTIAL)

- [ ] `cargo test -p gw-capability` PASS
- [ ] `cargo test -p gw-proxy --test capability_filter` PASS
- [ ] `cargo test -p gw-gateway` PASS
- [ ] `cargo fmt --all -- --check` (fix with `cargo fmt --all` + chore commit if dirty)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` (sequential)
- [ ] `bash scripts/check-invariants.sh` (sequential)

---

## Task 7: Merge to local main (orchestrator)

```bash
git checkout main
git merge --no-ff fix/memory-leaks-gw-capability -m "Merge Crate 6: Capability intern via Moka"
```

Then move to Crate 7 planning.

---

## Self-Review

**1. Spec coverage** — single Crate 6 item (`Box::leak` in resolver.rs:119) → Tasks 2+3 fix it; intern bound proven in Task 4. ✅

**2. Test count** — 4 new tests planned, budget 6. Margin 2.

**3. Placeholder scan** — `<capability-file>` in Task 5 — implementer resolves via Task 5 Step 1.

**4. Type consistency** —
- `Capability::Tagged(Arc<str>)` uniform across `types.rs`, `resolver.rs`, `filter.rs`.
- `CAPABILITY_INTERN_CAPACITY: u64 = 10_000` referenced by `LazyLock` builder + the cap-bound test.
- `intern_tag(s: &str) -> Arc<str>` is the single interning entry point.

---

## Notes for the Implementer

- This is mechanical: one type change (`Tagged(&'static str)` → `Tagged(Arc<str>)`), one derive change (`Copy` removed), one Box::leak → intern_tag call, and `.clone()` mechanical updates everywhere the compiler errors.
- Capacity is the SOLE bound — there is intentionally NO `time_to_idle`. Arcs in `Tagged` values held by callers would outlive an evicted intern slot; the LRU-evicted-then-re-interned scenario is the design intent, not a bug.
- `Arc<str>` Eq/Hash delegates to `str` — this is what makes intern-eviction-and-rebuild observably benign. The test in Task 3 Step 1 (`tagged_arc_strings_equal_when_content_matches`) is the most important regression guard.
- BLOCKED triggers: `cargo build --workspace` errors that require touching a crate's public API in a way that breaks external consumers (very unlikely — Capability is internal).
- DONE_WITH_CONCERNS triggers: docs file location ambiguous; an unexpectedly large fan-out of `.clone()` changes in `gw-proxy` test code.
