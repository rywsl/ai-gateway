# Roadmap Authority And Scope Confirmation

- **Date:** 2026-06-09
- **Status:** Confirmed by user
- **Scope:** Rust proxy superpowers ROADMAP/spec work only

## Context

`docs/plans/2026-06-08-roadmap-implementation-program.md` records fork decisions F1, F2, and F3 as resolved inputs for the remaining roadmap waves. Later as-built docs (`docs/spec/07-vendor-integration.md` and `docs/ROADMAP.md`) still described the outbound adapter and Rust OAuth refresh work as decision-gated residuals because those docs were written from current runtime behavior.

On 2026-06-09 the user confirmed the authority order:

1. F1 and F2 are governed by the master program plan.
2. All remaining Rust proxy ROADMAP/spec items are in scope and must be implemented.

## Confirmed Decisions

### F1: Adapter Boundary

`crates/gw-adapter` stays as the standalone per-vendor outbound rewrite boundary and will be wired incrementally. The proxy is the production consumer of the outbound adapter seam. The task worker does not call adapters; task submission should pre-render wire bodies before enqueuing work, and the worker ships those bodies verbatim.

Direct vendors must remain byte-identical through the incremental migration. Impersonate vendors continue to reuse the existing `gw-core::impersonate` and `gw-egress-impersonate` implementation. New heavy vendor transforms, including Kiro, Vertex, and Codex, land behind the adapter boundary.

### F2: OAuth Runtime Writer

Go remains the OAuth configuration writer. Rust becomes the sole runtime refresh writer. Runtime refresh must use a single-writer design: Rust refreshes access tokens, persists encrypted token updates including rotated refresh tokens, increments the vendor-key version, and broadcasts the resulting entity update. Go must not also refresh the same runtime tokens.

The K-C implementation plan must include cross-Pod single-flight or version-CAS, refresh-token rotation handling, and a Go-contract change that retires the previous Go runtime refresh path.

### F3: Tunnel Artifact

Tunnel remains a separate `--features tunnel` artifact until the HTTP-to-WS frame bridge and live round-trip coverage are implemented. The default image stays non-tunnel. The tunnel bridge and full e2e loop remain in scope, but changing the default artifact requires a separate release decision after those runtime pieces are built and verified.

## Current Execution Scope

All remaining `docs/ROADMAP.md` items are in scope. The next implementation program should split the work into independently reviewable waves and keep one fix or feature per commit.

Near-term independent capability gaps:

- R-D proxy wiring for API-key affinity.
- R-F stream probe before the first committed SSE event.
- Async task request rendering into `input_payload.ir_wire_body`.

Cross-process and larger runtime waves:

- Kiro protocol/adapter integration, including `ProtocolKind`/`ApiShape` and Go contract updates.
- CLI-extracted usage provenance, including `usage:records` provenance contract updates.
- Full outbound adapter registry and heavy vendor transforms.
- Rust-owned OAuth runtime refresh/write-back.
- Tunnel HTTP-to-WS frame bridge and full live round-trip e2e coverage.

## Worker Rule

Subagents may collect evidence and implement assigned slices, but architecture decisions come from the user-confirmed records above. If a worker finds a new boundary conflict, it must surface that as a decision point instead of choosing silently.
