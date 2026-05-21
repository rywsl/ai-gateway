# AILinkMax-Gateway

A Rust LLM gateway built on Pingora 0.8. Terminates OpenAI-, Anthropic-, and Gemini-shaped client traffic, normalises to an in-memory IR, applies auth + permission merge + RPM/quota/hard-cap enforcement, picks one of 8 upstream routing strategies, streams the response back through the matching client codec, and records usage to a Redis stream. Cost computation lives in the Go billing service that consumes `usage:records`.

## Reading order

- **First time in the repo** → [`CLAUDE.md`](CLAUDE.md) — what the gateway does, source layout, build pitfalls, where the docs live.
- **Architecture, traits, persistence, invariants, deliberate behaviours, known gaps** → [`docs/architecture.md`](docs/architecture.md).
- **Request lifecycle (bootstrap, every filter phase, error fanout, streaming, cache invalidation)** → [`docs/workflow.md`](docs/workflow.md).
- **Per-machine dev setup (Postgres, Redis, Docker)** → [`docs/dev-setup.md`](docs/dev-setup.md).
- **Authoritative design** → `docs/superpowers/specs/`. **Implementation plans + status** → `docs/superpowers/plans/`. **Operational gotchas** → `docs/issues/`.

## Quick start

```bash
# Build the binary in release mode.
cargo build --release -p gw-gateway

# Required env vars (override config/default.toml as needed).
# GW_KMS__KEK_HEX must contain at least one a-f character — figment's env
# decoder integer-coerces all-digit hex strings and the hex parser then fails.
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
  ./target/release/gw-gateway
```

Listeners (`config/default.toml`):

| Port | Purpose |
|------|---------|
| 8000 | Business HTTP listener (LLM proxy entrypoint). |
| 9090 | Prometheus `/metrics` exporter. |
| 8080 | Health — `/livez` (always 200), `/readyz` (200 once bootstrap completes), `/healthz/*` (admin, token-gated). |

Postgres migrations under `crates/gw-store/migrations/*.sql` are **not auto-applied**; run them with `psql -f` against your dev database. See [`docs/dev-setup.md`](docs/dev-setup.md).

## Tests & lints

```bash
# Unit + in-process integration (no Docker required). ~10s.
cargo test --workspace

# Adds testcontainers-backed Postgres + Redis tests. Requires Docker.
cargo test --workspace -- --ignored

# Lints (CI gates).
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Architectural invariants (layering, retired symbols).
bash scripts/check-invariants.sh
```

The full Pingora + wiremock + reqwest harness is `crates/gw-gateway/tests/e2e_full_flow.rs` (`#[ignore]`d); per-filter coverage that runs without Docker lives in `e2e_failure_modes.rs`.

## Benches

```bash
cargo bench --workspace --no-run                                # catch signature drift fast
cargo bench -p gw-codec --bench passthrough -- --warm-up-time 1 --measurement-time 2 --sample-size 10
```

Shipped benches:

- `gw-codec/benches/passthrough.rs` — same-protocol passthrough (AC-2.3-1).
- `gw-codec/benches/cross_protocol.rs` — cross-protocol IR re-render (AC-2.3-2).
- `gw-auth/benches/merge_permissions.rs` — 5-tier merge w/ 10 user groups (AC-023-5).
- `gw-routing/benches/strategy_pick.rs` — all 8 strategies over an 8-key slice.

Spec acceptance targets: P50 ≤ 200 µs on codec passthrough, P99 ≤ 5 µs on `merge_permissions`, P99 ≤ 100 µs on every strategy pick. Numbers land under `target/criterion/` after `cargo bench`.

## Layout

```
crates/                 13-crate Rust workspace (see docs/architecture.md §Workspace layout)
config/default.toml     figment base layer; per-machine override at config/local.toml
docs/
  architecture.md       crates, traits, persistence, invariants, deliberate behaviours
  workflow.md           startup + Pingora filter chain + error fanout
  dev-setup.md          dev environment setup
  superpowers/          design specs (specs/) + implementation plans (plans/)
  issues/               operational gotchas
scripts/check-invariants.sh   layering / namespace / retired-symbol lints
```

## License

Apache-2.0. See `LICENSE` (TBD) or `[workspace.package]` in `Cargo.toml`.
