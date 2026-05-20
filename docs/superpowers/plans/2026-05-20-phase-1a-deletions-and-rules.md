# Phase 1a — Crate Layering Rules + Invariant Lint Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the documentation + CI lint harness that lock in the v2 integration invariants, so every later Phase-1 sub-plan (1b/1c/1d) and beyond can be guarded by automated greps.

**Architecture:** This sub-plan is **doc + tooling only — no Rust source deletions**. The destructive code deletions named in the integration spec §2 表 A (`retry.rs`, `breaker.rs`, `cost_micro`, `FreezeGuard::settle`, `markup_product`) are *live and load-bearing* at current HEAD — deleting them now breaks the build because their replacements (gw-health, failover.rs, UsageRecord) have not landed. Those deletions therefore travel with their replacements in sub-plans 1b / 1c / Phase 4. What 1a *can* safely deliver: (1) the `CLAUDE.md` layering rules that govern those later changes, and (2) a `scripts/check-invariants.sh` lint harness that asserts the invariants which are **already true at HEAD**, with the not-yet-applicable lints staged as commented "activate in Phase N" blocks so reviewers see the full target set.

**Tech Stack:** Bash (lint script), Markdown (CLAUDE.md / plan cleanup), `cargo metadata` + `jq` for dependency-edge checks, `git grep` for source pattern checks.

---

## Context for the implementer (read first)

You have **zero prior context** on this repo. Key facts you need:

- This is a Rust workspace (`Cargo.toml` at root, crates under `crates/`). Current crates: `gw-core`, `gw-codec-macros`, `gw-codec`, `gw-store`, `gw-auth`, `gw-routing`, `gw-ledger`, `gw-proxy`, `gw-gateway`.
- The authoritative design this plan serves is `docs/superpowers/specs/2026-05-20-v2-integration-design.md` (henceforth "the integration spec"). Its §3 lists 8 cross-crate invariants (C-1…C-8) and §5.2 lists 14 lint rules. **You do not need to implement the invariants here** — only the doc + the subset of lints that pass today.
- "支撑层 / 业务层 / 执行体层" = supporting-layer / business-layer / execution-layer crates. The integration spec §3 C-1 defines them:
  - Supporting: `gw-core`, `gw-codec`, `gw-codec-macros`, `gw-store`, `gw-routing`, `gw-auth`, `gw-ledger`
  - Business: `gw-adapter`, `gw-health`, `gw-task` (none exist yet)
  - Execution: `gw-proxy`, `gw-task-runtime` (only `gw-proxy` exists)
- **Do not** attempt to delete `crates/gw-routing/src/retry.rs`, `crates/gw-routing/src/breaker.rs`, the `cost_micro` field, `FreezeGuard::settle`, or `markup_product` in this sub-plan. They are referenced by `gw-proxy`, `gw-gateway`, strategy modules, and several tests. Deleting them breaks `cargo build`.

### Lint regex caveat (important — already discovered)

The integration spec §5.2 writes the layering lint as `git grep -nE 'task::|billing::|...'`. That naive regex **false-positives** at HEAD: `billing::` matches `crate::billing::BillingRecord` (legit module in supporting crate `gw-ledger`, renamed away only in 1b) and `task::` matches `tokio::task::JoinHandle`. So this plan implements `crate_layering` as a **dependency-edge check** via `cargo metadata` (supporting crates must not depend on business crates) rather than a string grep. This is more robust and false-positive-free.

---

## File Structure

**Modify:**
- `CLAUDE.md` — append a "Crate layering rules (v2 integration)" section after the existing "Things that are intentional, not bugs" section. ~40 lines added. Single responsibility: durable rules an implementer must follow.
- `docs/superpowers/plans/2026-05-19-business-split-refactor.md` — delete the duplicated stale v1 residual block (lines ~178–344, the second copy of "## File Structure" onward that still references `gw-billing` and `FreezeGuard::settle`). Single responsibility: remove the internal contradiction the integration spec §2 Bm6 flagged.

**Create:**
- `scripts/check-invariants.sh` — executable bash lint harness. Asserts the HEAD-green invariants now; carries the deferred lints as commented `# PENDING (Phase N)` blocks. Single responsibility: one place CI and later plans call to enforce §3 / §5.2.

**Test (verification, not unit tests — this is a tooling/doc plan):**
- Running `bash scripts/check-invariants.sh` exits 0 at the end of this plan.
- `cargo build --workspace` still passes (no source touched).

---

## Task 1: Add Crate Layering Rules to CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (append after the "## v1 gaps to be aware of when extending" section, which is currently the last section)

- [ ] **Step 1: Append the rules section**

Open `CLAUDE.md`. After the final line (the "v1 gaps" paragraph ending in `e2e_failure_modes.rs`), append exactly:

```markdown

## Crate layering rules (v2 integration)

Authoritative source: `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §3 C-1. Enforced by `scripts/check-invariants.sh`.

Three layers, strict one-way dependency:

- **Supporting layer** (`gw-core`, `gw-codec`, `gw-codec-macros`, `gw-store`, `gw-routing`, `gw-auth`, `gw-ledger`): provide generic mechanism only (IR/schema, Redis client, JWT, FreezeGuard trait, routing primitives). MUST NOT depend on any business or execution crate. MUST NOT introduce business namespaces (`mod task`, `mod billing`, vendor-specific `mod claude_code`, etc.). `gw-store` carries only the generic Entity / Counter / Append-log / Lock / Counter-Event protocols — no `task::*` / `billing::*` namespaces.
- **Business layer** (`gw-adapter`, `gw-health`, `gw-task`): one crate per business concern. May depend on any supporting crate. Business-to-business dependencies must be registered explicitly in a spec.
- **Execution layer** (`gw-proxy`, `gw-task-runtime`): runtime / server / binary entry points. May depend on supporting + business crates. **Execution crates MUST NOT depend on each other** — `gw-proxy ↔ gw-task-runtime` is forbidden in both directions.

**Billing boundary:** the Rust gateway only *records* usage (`UsageRecord` → `usage:records` stream). The usage→cost formula and markup-chain application live entirely in Go. No Rust code path may compute cost. The `gw-billing` crate is permanently retired and must not reappear.

**Hard deployment guarantee** (worker spec ADR-T16): gateway config/args/code carry no worker host/port; worker config/args/code carry no gateway host/port. The only coupling surface is a shared Redis namespace (+ KMS KEK, optional shared PgSQL). Any N-gateway × M-worker topology must work with zero peer-awareness.
```

- [ ] **Step 2: Verify the section is present and well-formed**

Run:
```bash
grep -n "## Crate layering rules (v2 integration)" CLAUDE.md
grep -c "Execution crates MUST NOT depend on each other" CLAUDE.md
```
Expected: first prints one line number; second prints `1`.

- [ ] **Step 3: Verify build still green (no source touched)**

Run: `cargo build --workspace`
Expected: finishes without error (CLAUDE.md is not compiled).

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(claude-md): add v2 crate layering rules + billing boundary + deploy guarantee"
```

---

## Task 2: Create the invariant lint harness

**Files:**
- Create: `scripts/check-invariants.sh`

- [ ] **Step 1: Write the lint script**

Create `scripts/check-invariants.sh` with exactly this content:

```bash
#!/usr/bin/env bash
# Invariant lint harness for the v2 integration.
# Authoritative spec: docs/superpowers/specs/2026-05-20-v2-integration-design.md §3 / §5.2.
#
# Each check below is one §5.2 lint. Checks that are GREEN at the current
# phase run unconditionally. Checks whose target code has not landed yet are
# kept as PENDING blocks (commented), labelled with the phase that activates
# them, so the full target set stays visible to reviewers.
#
# Usage: bash scripts/check-invariants.sh
# Exit 0 = all active invariants hold; non-zero = a violation was found.

set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
note() { printf '  \033[33m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32mok\033[0m   %s\n' "$*"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; fail=1; }

# ---------------------------------------------------------------------------
# crate_layering (§5.2) — dependency-edge form (robust; no string false-positives)
# Supporting crates MUST NOT depend on business/execution crates.
# Execution crates MUST NOT depend on each other.
# ---------------------------------------------------------------------------
echo "[crate_layering] supporting-layer must not depend on business/execution crates"
SUPPORTING="gw-core gw-codec gw-codec-macros gw-store gw-routing gw-auth gw-ledger"
FORBIDDEN_FOR_SUPPORTING="gw-adapter gw-health gw-task gw-task-runtime gw-proxy gw-billing gw-gateway gw-task-worker"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  meta="$(cargo metadata --no-deps --format-version 1 2>/dev/null)"
  for crate in $SUPPORTING; do
    deps="$(echo "$meta" | jq -r --arg c "$crate" \
      '.packages[] | select(.name==$c) | .dependencies[].name')"
    for forb in $FORBIDDEN_FOR_SUPPORTING; do
      if echo "$deps" | grep -qx "$forb"; then
        bad "$crate depends on $forb (supporting → business/execution forbidden)"
      fi
    done
  done
  # Execution crates must not depend on each other.
  proxy_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-proxy") | .dependencies[].name')"
  if echo "$proxy_deps" | grep -qx "gw-task-runtime"; then
    bad "gw-proxy depends on gw-task-runtime (execution ↔ execution forbidden)"
  fi
  [ "$fail" -eq 0 ] && ok "no forbidden supporting/execution edges"
else
  note "skipped: cargo + jq required"
fi

# ---------------------------------------------------------------------------
# no_billing_crate (§5.2) — gw-billing must never exist as a crate.
# ---------------------------------------------------------------------------
echo "[no_billing_crate] gw-billing must not be a workspace member or dependency"
if grep -rn 'gw-billing' Cargo.toml crates/*/Cargo.toml 2>/dev/null; then
  bad "gw-billing referenced in a Cargo.toml"
else
  ok "no gw-billing crate reference"
fi

# ---------------------------------------------------------------------------
# no_proxy_worker_dep (§5.2) — gw-proxy and gw-task-runtime must not interdepend.
# (gw-task-runtime does not exist yet; this trivially holds but we assert it so
#  the check is live the moment that crate appears.)
# ---------------------------------------------------------------------------
echo "[no_proxy_worker_dep] gw-proxy must not depend on gw-task-runtime"
if grep -rn 'gw-task-runtime' crates/gw-proxy/Cargo.toml 2>/dev/null; then
  bad "gw-proxy/Cargo.toml references gw-task-runtime"
else
  ok "gw-proxy does not depend on gw-task-runtime"
fi

# ---------------------------------------------------------------------------
# no_worker_addr_config / no_gateway_addr_config (§5.2, C-8)
# gateway config/code must not name a worker address, and vice versa.
# gw-task-worker does not exist yet, so only the gateway side is checkable now.
# ---------------------------------------------------------------------------
echo "[no_worker_addr_config] gateway must not hardcode a worker host/port"
if grep -rnE 'worker[_-]?host|worker[_-]?port|TASK_WORKER_(URL|HOST)' config/ crates/gw-gateway 2>/dev/null; then
  bad "gateway config/code names a worker address"
else
  ok "gateway carries no worker address"
fi

# ===========================================================================
# PENDING lints — target code has not landed yet. Uncomment in the named phase.
# ===========================================================================
#
# # no_legacy_retry (Phase 4: R-E failover replaces retry.rs)
# [ -f crates/gw-routing/src/retry.rs ] && bad "retry.rs still present"
#
# # no_legacy_breaker (Phase 1c: gw-health replaces breaker.rs)
# grep -rnE '\bBreakerState\b|\bBreakerStore\b' crates/ && bad "breaker types remain outside gw-health"
#
# # no_cost_micro (Phase 1b: UsageRecord drops cost_micro)
# grep -rnE '\bcost_micro\b' crates/ && bad "cost_micro field remains"
#
# # no_billing_records_stream (Phase 1b: stream renamed usage:records)
# grep -rn '"billing:records"' crates/ && bad "billing:records stream name remains"
#
# # no_freezeguard_settle (Phase 1b: FreezeGuard::release replaces settle)
# grep -rnE '\.settle\(.*actual|FreezeGuard::settle\(' crates/ && bad "FreezeGuard::settle remains"
#
# # no_markup_product_scalar (Phase 5: MarkupChainSnapshot replaces markup_product)
# grep -rnE '\bmarkup_product\b' crates/ && bad "markup_product scalar remains"
#
# # no_patch_body (Phase 1c: adapter trait v2 has no patch_body / BodyPatch)
# grep -rnE '\bfn patch_body\b|\bBodyPatch::' crates/ && bad "patch_body / BodyPatch remains"
#
# # no_ir_runtime_struct (Phase 1d: codec S1 v2 schema is compile-time only)
# grep -rnE 'IrRequest::new|IrResponse::new|IrRequest::default|IrResponse::default' crates/ && bad "IR runtime instantiation remains"
#
# # deploy_e2e_matrix (Phase 6/7: ADR-T16 acceptance tests)
# for t in 1g_0w 0g_1w n_m gateway_kill worker_kill; do
#   grep -rq "e2e_deploy_matrix_$t" crates/ || bad "missing deploy matrix test: $t"
# done

if [ "$fail" -ne 0 ]; then
  echo "INVARIANT CHECK FAILED"
  exit 1
fi
echo "all active invariants hold"
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/check-invariants.sh`
Expected: no output.

- [ ] **Step 3: Run it and verify it exits 0 at HEAD**

Run: `bash scripts/check-invariants.sh; echo "exit=$?"`
Expected: prints `ok` lines for crate_layering / no_billing_crate / no_proxy_worker_dep / no_worker_addr_config, then `all active invariants hold`, then `exit=0`.

If `jq` is not installed it prints `skipped: cargo + jq required` for crate_layering and still exits 0 — that is acceptable (the other checks are pure grep). If you want the full check locally: `brew install jq`.

- [ ] **Step 4: Sanity-check the harness actually catches a violation**

Temporarily prove the harness has teeth (do NOT commit this):
```bash
echo 'gw-billing = { path = "../gw-billing" }' >> crates/gw-core/Cargo.toml
bash scripts/check-invariants.sh; echo "exit=$?"
```
Expected: a `FAIL` line mentioning `gw-billing` and `exit=1`.

Now revert the tamper:
```bash
git checkout crates/gw-core/Cargo.toml
bash scripts/check-invariants.sh; echo "exit=$?"
```
Expected: back to `exit=0`.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-invariants.sh
git commit -m "ci: add invariant lint harness for v2 integration (§3 / §5.2)"
```

---

## Task 3: Remove the stale v1 residual block in the business-split-refactor plan

**Files:**
- Modify: `docs/superpowers/plans/2026-05-19-business-split-refactor.md`

**Why:** Integration spec §2 Bm6. That plan was revised to v2 (gw-billing retired, `FreezeGuard::release`) at the top, but a stale **duplicate** of "## File Structure" onward (still describing `gw-billing`, `FreezeGuard::settle(actual)`, `unfreeze()`) survives lower in the file, contradicting the v2 head. Delete the duplicate so the plan reads consistently.

- [ ] **Step 1: Locate the duplicate boundary**

Run:
```bash
grep -n '^## File Structure' docs/superpowers/plans/2026-05-19-business-split-refactor.md
grep -n '^**Tech Stack**' docs/superpowers/plans/2026-05-19-business-split-refactor.md
```
Expected: two matches for `## File Structure` (the first ~line 27 is the v2-correct one; the second ~line 189 begins the stale duplicate). The second `**Tech Stack**` (~line 179) marks where the duplicated tail begins.

- [ ] **Step 2: Read the region to confirm the exact cut points**

Run: `sed -n '170,200p' docs/superpowers/plans/2026-05-19-business-split-refactor.md`
Confirm you see: the v2 plan's first copy ends with its "Follow-ups" / closing blockquote around line ~176, then a blank line, then a **second** `**Tech Stack**:` + `**Out of scope**` + a second `## File Structure` that mentions `gw-billing`. That second copy (from the stray `**Tech Stack**` line through end-of-file) is the stale residual.

- [ ] **Step 3: Delete the stale duplicate (from the second `**Tech Stack**` line to EOF)**

Identify the line number `N` of the SECOND `**Tech Stack**` occurrence (from Step 1). Then trim the file to keep only lines `1 .. N-1` (also dropping the trailing blank line before it):

```bash
# Replace 178 with (N-1) from your Step-1 output if it differs.
N=$(grep -n '^**Tech Stack**' docs/superpowers/plans/2026-05-19-business-split-refactor.md | sed -n '2p' | cut -d: -f1)
head -n "$((N-1))" docs/superpowers/plans/2026-05-19-business-split-refactor.md > /tmp/bsr.md
mv /tmp/bsr.md docs/superpowers/plans/2026-05-19-business-split-refactor.md
```

- [ ] **Step 4: Verify only one copy of each section remains, and no stale gw-billing / settle references**

Run:
```bash
grep -c '^## File Structure' docs/superpowers/plans/2026-05-19-business-split-refactor.md
grep -nE 'gw-billing|settle\(actual\)|fn unfreeze' docs/superpowers/plans/2026-05-19-business-split-refactor.md
```
Expected: first prints `1`. Second prints only lines that are in the v2-correct head explicitly marking them as **撤销/retired** (e.g. the `~~gw-billing~~` strikethrough lines and the v2 note that `settle(actual)` is replaced) — there must be NO occurrence inside a live "New:" / "File Structure" instruction block. If the grep shows live (non-strikethrough, non-"撤销") references, you cut too little; re-check N.

- [ ] **Step 5: Verify the file still ends cleanly**

Run: `tail -5 docs/superpowers/plans/2026-05-19-business-split-refactor.md`
Expected: ends with the v2 plan's closing blockquote (`> 本 plan 仅交付 crate 骨架与依赖图改造…`), not a half-cut sentence.

- [ ] **Step 6: Commit**

```bash
git add docs/superpowers/plans/2026-05-19-business-split-refactor.md
git commit -m "docs(plan): drop stale v1 residual from business-split-refactor (integration §2 Bm6)"
```

---

## Task 4: Final verification

- [ ] **Step 1: Lint harness green**

Run: `bash scripts/check-invariants.sh; echo "exit=$?"`
Expected: `all active invariants hold` + `exit=0`.

- [ ] **Step 2: Build untouched**

Run: `cargo build --workspace`
Expected: success (this plan changed no `.rs` files).

- [ ] **Step 3: CLAUDE.md rules + lint script + clean plan all present**

Run:
```bash
grep -q '## Crate layering rules (v2 integration)' CLAUDE.md && echo "rules ok"
test -x scripts/check-invariants.sh && echo "script ok"
test "$(grep -c '^## File Structure' docs/superpowers/plans/2026-05-19-business-split-refactor.md)" = "1" && echo "plan deduped ok"
```
Expected: `rules ok`, `script ok`, `plan deduped ok`.

- [ ] **Step 4: Confirm no source deletions slipped in**

Run: `git diff --stat main -- crates/ | grep -E '\.rs' || echo "no .rs changes"`
Expected: `no .rs changes` (1a is doc + tooling only).

---

## Self-Review notes (author)

- **Spec coverage:** 1a maps to integration spec §3 C-0/C-1/C-8 (CLAUDE.md rules), §5.2 lint harness (the HEAD-green subset + PENDING staging of the rest), and §2 Bm6 (business-split-refactor cleanup). The destructive deletions in §2 表 A are explicitly *out of scope here* and reassigned to 1b/1c/Phase 4 in the architecture note — this is a deliberate scope decision, not a gap.
- **Placeholder scan:** none — every step has concrete commands/content.
- **Type consistency:** N/A (no Rust types defined). Lint names match §5.2 exactly (`crate_layering`, `no_billing_crate`, `no_proxy_worker_dep`, `no_worker_addr_config`, plus the PENDING set).
- **Known deviation from spec text:** `crate_layering` is implemented as a `cargo metadata` dependency-edge check rather than the §5.2 string grep, because the grep false-positives on `tokio::task::` / `crate::billing::BillingRecord` at HEAD. This is a strict improvement; integration spec §5.2 row should be footnoted to match when 1a lands (follow-up).

---

## Follow-ups (register, not in 1a)

1. **1b** gw-ledger balance/freeze 切边: activates PENDING lints `no_cost_micro`, `no_billing_records_stream`, `no_freezeguard_settle`; defines `gw-core::usage::UsageSnapshot`.
2. **1c** gw-adapter + gw-health scaffolding: activates `no_legacy_breaker`, `no_patch_body`; splits `breaker::in_flight` → `gw-routing::in_flight`.
3. **Phase 4** R-E failover: activates `no_legacy_retry`.
4. **Phase 1d** codec S1 v2: activates `no_ir_runtime_struct`.
5. **Phase 6/7**: activates `deploy_e2e_matrix`; wire `scripts/check-invariants.sh` into CI as a required gate.
6. Footnote integration spec §5.2 `crate_layering` row to note the dependency-edge implementation.

---

> 1a delivers the rulebook and the harness, not the demolition. Every later sub-plan uncomments its matching PENDING lint as it lands the replacement — that is how the demolition stays build-green.
