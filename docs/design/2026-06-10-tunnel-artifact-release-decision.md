# Tunnel Artifact Release Decision

- **Date:** 2026-06-10
- **Status:** Accepted
- **Scope:** W8 follow-up release artifact policy for the Rust gateway tunnel feature

## Context

The master program F3 decision kept the default image non-tunnel while W8 delivered the HTTP-to-WS bridge, live tunnel coverage, and split CI artifacts. W8 is now implemented: CI builds the default non-tunnel artifact as `latest` and a tunnel-enabled artifact as `latest-tunnel`.

## Decision

Keep the split artifact policy.

- `latest` remains the default non-tunnel image built with Cargo defaults (`default = []`).
- `latest-tunnel` remains the opt-in image built with `--features tunnel`.
- Tunnel is not promoted into the default `latest` image.

## Rationale

The split preserves the non-tunnel rollout path for existing operators while making the tunnel build explicit for operators who choose it. Runtime tunnel use still requires `[tunnel].enabled = true`, so the release artifact and runtime config both remain opt-in.

## Consequences

`docs/ROADMAP.md` should treat default artifact policy as closed rather than a decision-gated residual. `docs/spec/09-tunnel.md` should continue documenting the as-built split artifact model without listing default artifact promotion as an open ROADMAP item.
