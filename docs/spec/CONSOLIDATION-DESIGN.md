# Spec Consolidation — Design (as-built standard + roadmap)

- **Date**: 2026-06-08
- **Status**: Approved (brainstorming) — implementation plan pending
- **Author**: collaborative (Claude + user)
- **Type**: one-time documentation refactor / migration design
- **Why this file lives in `docs/spec/`**: the migration deletes `docs/superpowers/specs/`, so the design that performs that deletion cannot live there. This file is the provenance record for how the `docs/spec/` standard came to be; it may be removed once the migration lands, or kept as history.

## 1. Context

A 7-domain spec-vs-code audit (2026-06-08) found the design corpus describes a substantially more complete system than exists. Two failure modes dominate:

1. **Spec finalized but not implemented** — whole subsystems are scaffold or absent (the entire `gw-adapter` vendor layer, K-B/K-C/K-D, kiro codec, task billing, R-D/R-F, RL-Core, several C-* billing pieces), yet their specs are marked "integrated"/"landed".
2. **Spec ⇄ spec / spec ⇄ code conflicts** — e.g. `v2-integration` forbids the `gw-billing` crate while it exists and is depended on by four crates; `daily_quota_tokens` ownership disagreement; retired `usage_source` still referenced in a contract; two `issues/2026-05-29-*` docs that self-contradict; CLAUDE.md count drift (15→21 crates, 3→10 migrations).

Root cause: the project went through an architecture turn (adapter-driven → `impersonate` + Go-delegated) and a v2 cleanup; **code followed, the early dated specs did not**, and nothing marks them superseded. CI compounds it — `.gitlab-ci.yml` only builds, so `check-invariants.sh` never runs and the doc⇄code drift is unchecked.

## 2. Decisions (locked during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| D1 | What the new standard describes | **As-built** (mirror of code) — authoritative; unbuilt work goes to a separate roadmap |
| D2 | Output shape | **A set of domain specs**, sliced by capability family, each anchored to its backing crate(s) |
| D3 | Disposition of old corpus | **Hard-delete** the 51 dated specs + 24 dated plans; git preserves history |
| D4 | Relationship to `architecture.md` / `workflow.md` | **Coexist** — they stay as cross-system overview + request lifecycle; domain specs own per-subsystem detail; CLAUDE.md sync rule is updated |
| D5 | Roadmap depth | **Actionable design points** per unbuilt item (interface/key-constants/integration接点), recoverable without digging git |
| D6 | Partition scheme | **Capability families** (S/R/K/RL/C/A/T + tunnel), recast as-built |

## 3. Target structure

```
docs/
  architecture.md     KEEP — cross-system overview: layering, trait index, dependency DAG,
                      persistence namespaces + KEK, invariants, deliberate behaviours;
                      gains an index section pointing into spec/
  workflow.md         KEEP — bootstrap + Pingora filter chain + error fanout (request lifecycle)
  spec/               NEW — the as-built domain standard (authoritative)
    README.md           index: family → crate → status table
    01-protocol-codec.md        gw-codec, gw-codec-macros
    02-routing.md               gw-routing, gw-capability
    03-key-health.md            gw-health
    04-rate-limiting.md         gw-auth::ratelimit, ip_*
    05-auth-permissions.md      gw-auth (jwt/apikey/merge/mgmt) + gw-proxy auth + HMAC signature
    06-billing-ledger.md        gw-ledger, gw-billing
    07-vendor-integration.md    gw-adapter, gw-egress-impersonate, gw-core::impersonate, OAuth path
    08-async-tasks.md           gw-task, gw-task-runtime, gw-task-worker
    09-tunnel.md                gw-tunnel-client (feature-gated)
  ROADMAP.md          NEW — every unbuilt / divergent item with actionable design points
  go-contracts/       KEEP as-is — Rust↔Go contracts; cross-referenced from domain specs
  issues/             KEEP — operational gotchas; stale / self-contradicting ones pruned
DELETE: docs/superpowers/specs/ (51 files) + docs/superpowers/plans/ (24 files)  — git keeps history
```

`gw-store` persistence (entity loading, encryption, KEK, namespaces, migrations) and the `metrics_catalog` observability surface stay in `architecture.md` — it already owns namespaces + KEK — rather than becoming thin standalone specs.

## 4. Family → crate → absorbed-corpus map

Each domain spec is written **from the code**; the third column records which deleted dated specs' still-true content it carries forward (for cross-reference during authoring only).

| Spec | Backing crate(s) | Absorbs (deleted specs) |
|------|------------------|-------------------------|
| 01-protocol-codec | gw-codec, gw-codec-macros | codec-s1..s4, v2-codec-cleanup, cli-variants (kiro/S5 → ROADMAP) |
| 02-routing | gw-routing, gw-capability | routing-r-ab/c/e/g (R-AB rewrite, R-D, R-F → ROADMAP), capability resolver |
| 03-key-health | gw-health | key-health-k-a (K-B/K-C/K-D → ROADMAP / 07) |
| 04-rate-limiting | gw-auth (ratelimit, ip) | ratelimit-rl-adaptive, rl-ip (RL-Core per-key gate → ROADMAP) |
| 05-auth-permissions | gw-auth (jwt/apikey/merge/mgmt), gw-proxy auth filter | llm-gateway-design (auth+perm), systemic-fixes (signature, mgmt token) |
| 06-billing-ledger | gw-ledger, gw-billing | billing-c-alpha, billing-robustness, billing-record-durability, credit-lease, detailed-log (C-beta/gamma/delta gaps → ROADMAP) |
| 07-vendor-integration | gw-adapter, gw-egress-impersonate, gw-core::impersonate | egress-impersonation; **as-built = NoopAdapter + auth_template injection + egress profiles + Go-delegated OAuth** (A-T0..T6, in-Rust OAuth refresh → ROADMAP) |
| 08-async-tasks | gw-task, gw-task-runtime, gw-task-worker | task-worker-design (settle/billing + submit→poll → ROADMAP) |
| 09-tunnel | gw-tunnel-client | tunnel-t (default-build wiring + require_tunnel hard-fail → ROADMAP) |

## 5. Per-spec template (the anti-ambiguity contract)

Every `spec/0N-*.md` uses one skeleton so any reader knows where each fact lives:

```
# 0N — <Family> (as-built)
- Family · Backing crate(s) · Source-of-truth files · Status (one line)

## Purpose            one paragraph: what this subsystem does

## As-built surface   AUTHORITATIVE. Only what exists in code:
                      - types / traits / enums (with crate/path.rs anchors)
                      - key constants & config keys
                      - control flow / state machine
                      - error codes emitted
                      - metrics emitted
## Deliberate behaviours   fail-closed / intentional-quirk points in this domain
## Cross-process touchpoints   links into go-contracts/
## Invariants & lints   the check-invariants.sh rules guarding this domain
## Not built → ROADMAP   pointer list only; no design detail inline
```

Rules that keep it unambiguous:
- **Every claim in "As-built surface" is anchored to a `crate/path.rs` symbol.** If it isn't in code, it doesn't go here — it goes to ROADMAP.
- **No status badges, no "Phase 3 will…", no `unimplemented` prose.** Present tense, built things only.
- **One fact, one home.** Cross-cutting facts (layering, persistence namespaces) live in `architecture.md` and are linked, not duplicated.

## 6. ROADMAP.md

One section per family. Each unbuilt or divergent item carries:

- **Item** — what it is
- **Intent** — why it was specced (1–2 lines)
- **Design points** — actionable: interface signatures, key constants, the integration接点 with current code (enough to start without archaeology)
- **Source** — `deleted spec <name>, git <hash>` for full original design
- **Priority** — from the audit (runtime-risk > capability-gap > doc-only)

Seed contents (from the audit):

**Unbuilt capabilities** — full `gw-adapter` vendor layer (A-T0..T6) · K-B vendor error analyzers · K-C in-Rust OAuth refresh (vs current Go-delegated) · K-D quota probe · kiro codec + adapter (S5/A-T7) · R-AB multi-score rewrite · R-D affinity manager · R-F stream probe · RL-Core per-key RPM gate + `RateLimitKind` · C-beta `EstimatePricing` entity · C-gamma frozen→Redis sync · C-delta DLQ three-tier "never drop".

**Runtime-risk divergences (bugs to fix)** — task settle leaks freeze + bills nothing · gateway task-freeze is in-process (breaks multi-gateway) · credit-lease `GW_QUOTA_LEASE_EXPIRED` ships 402 not 503 · `lease_grace_ms` configured but never applied · RL-Adaptive learns from non-RPM 429s · `require_tunnel=true` silently falls back to Direct in the default build · **`check-invariants.sh` never runs in CI**.

## 7. Conflict resolution (cleanup payoff)

Deleting the conflicting old specs means each new domain spec states exactly one truth:

| Conflict (audit) | Resolution in new standard |
|------------------|----------------------------|
| `gw-billing` "forbidden" (v2-integration) vs revived (4 deps) | 06 states it exists as the task-formula crate; contradiction dies with the deleted v2-integration |
| `daily_quota_tokens` Go-vs-Rust | 04/06 state it is Rust-side (as built: `request.rs:1133` + `GW_QUOTA_DAILY_TOKENS_EXCEEDED`) |
| `usage_source` retired but still in B1 contract | 06 documents `vendor_exposure_state` / `usage_provenance`; go-contracts note updated |
| `signature_required` "forbidden/open" vs shipped | 05 documents the global `[signature].required` toggle as built |
| `issues/2026-05-29-*` self-contradicting (Fixed table vs per-section "open") | pruned / status reconciled |
| A-T*/K-* "integrated" vs scaffold | 03/07 describe the real (Noop + Go-delegated) path; the rest → ROADMAP |

Items that are genuine **bugs** (not just stale docs) are NOT silently blessed — they go to ROADMAP §runtime-risk with a fix note.

## 8. Index-doc updates (sync-rule cost)

- **CLAUDE.md + AGENTS.md** (kept byte-identical per their own rule): rewrite "Source layout" and the "Sync rule" table to point at `docs/spec/`; delete the "v2 cleanup status" section (it references deleted sub-specs); fix stale counts (15→21 crates, 3→10 migrations).
- **architecture.md**: add a `docs/spec/` index section; keep persistence / invariants / deliberate-behaviours as the cross-system home.
- **Repoint every cross-reference** to `docs/superpowers/specs/...` and `docs/superpowers/plans/...` across the repo (docs, code comments, READMEs) to the new domain spec or ROADMAP. A grep gate (§10) enforces zero dangling references.

## 9. Deletion list

- `docs/superpowers/specs/` — all 51 `*.md` (git history retains them; ROADMAP carries forward the still-relevant design intent).
- `docs/superpowers/plans/` — all 24 `*.md` (implementation plans, historical).
- If `docs/superpowers/` is then empty, remove the directory.

## 10. Execution (phased — full plan via writing-plans)

1. **Scaffold** — create `docs/spec/` skeletons (README + 9 templated stubs) + empty `ROADMAP.md`.
2. **Author** — fill the 9 domain specs from code, **one agent per domain verifying against source** (the audit already established ground truth for divergent areas; the rest is verified while writing). Parallelizable.
3. **ROADMAP** — populate from the audit's two tables at "pick-it-up" granularity.
4. **Cutover** — delete the old corpus; repoint all references; update CLAUDE.md + AGENTS.md + architecture.md.
5. **Consistency pass (gates)** — (a) `grep -r 'superpowers/specs\|superpowers/plans' docs/ crates/` returns nothing; (b) every "As-built surface" claim resolves to a real symbol; (c) crate/migration counts in CLAUDE.md match `Cargo.toml` / `migrations/`; (d) README index lists exactly the 9 specs.

## 11. Out of scope

- **No code changes.** This refactor touches docs only. The runtime-risk bugs land in ROADMAP, not fixed here.
- **`go-contracts/` content** stays as-is (live Rust↔Go contract), only cross-referenced.
- **Wiring `check-invariants.sh` into CI** is recorded in ROADMAP (it is the highest-leverage fix) but is a separate change.
- **`docs/issues/` deep cleanup** beyond pruning the self-contradicting 2026-05-29 pair.

