# Tunnel default-compile decision (2026-06-15)

## Status

Accepted. Supersedes [`docs/design/2026-06-10-tunnel-artifact-release-decision.md`](./2026-06-10-tunnel-artifact-release-decision.md) (4 days prior).

## Context

The 2026-06-10 decision locked the policy: tunnel code stays out of `latest`
behind the `--features tunnel` flag, with a separate `latest-tunnel` image
built via a CI second job. The stated reason was that tunnel was not yet
production-validated.

Phase 1 of the Cnexus to Aether tunnel compatibility work changes the
underlying premise:

1. Tunnel is being productionized via the new `tests/tunnel-e2e/` Docker
   harness with three cnexus-protocol scenarios.
2. Tunnel becomes core integration infrastructure with the addition of the
   Aether wire protocol in Phase 2; Aether-compatible proxy nodes are existing
   production deployments.
3. The `cfg.tunnel.enabled = false` runtime gate remains, so the compile-time
   gate's emergency-off value is duplicated by config.

Keeping `--features tunnel` after the Aether work lands would mean:

- Every tunnel code change requires a `cfg(feature = "tunnel")` /
  `cfg(not(feature = "tunnel"))` matching pair, increasing change cost.
- CI continues to build two artifacts for what is a single runtime config
  switch.
- Reviewers must mentally evaluate every diff against both feature states.

## Decision

Remove the `--features tunnel` Cargo feature. Tunnel code compiles
unconditionally into the single `latest` image. The runtime gate
`[tunnel].enabled = false` (default) remains the operator's kill switch;
flipping it requires restart per spec.

The `latest-tunnel` image is retired. CI publishes one image.

## Consequences

- Binary size grows by the size of `gw-tunnel-client` plus the now-unconditional
  `tokio-rustls` and `webpki-roots` deps. Estimated impact is less than 1 MB per
  release.
- Compile time includes tunnel code in all gateway builds.
- `features.tunnel` in the control-plane contract is retained in the JSON shape
  and fixed at `true` for forward compatibility with existing Go consumers.
- Runtime-off behavior is unchanged: `cfg.tunnel.enabled = false` produces the
  Direct path, and this remains the supported kill switch.

## Reversal

To restore the split-artifact policy:

1. Re-introduce `[features] tunnel = [...]` in `gw-proxy` and `gw-gateway`
   `Cargo.toml` with `optional = true` on the gated deps.
2. Re-introduce `cfg(feature = "tunnel")` gates on the changed sites.
3. Re-introduce the CI `rust-build-tunnel` and `push-tunnel-image` jobs.
4. Restore `features.tunnel = false` in the control-plane contract.

The cost of reversal is roughly equal to the cost of this forward step. The
reason to do it would be: tunnel is found to cause a hard regression that cannot
be controlled via the runtime gate. Document the failure mode here if that
happens.

## References

- Spec: `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
- Prior decision: `docs/design/2026-06-10-tunnel-artifact-release-decision.md`
