# Dev setup

This guide covers everything required to build, test, and run
`ailinkmax-gateway` on a developer workstation. CI mirrors this setup; the
only divergence is that CI provisions Postgres + Redis services explicitly
instead of leaning on testcontainers.

## Toolchain

- Rust **1.86.0** is pinned via `rust-toolchain.toml`. `rustup` will pick
  it up automatically when you first build inside this workspace.
- Components: `rustfmt`, `clippy`. Both are listed in the toolchain file
  so `rustup component list --installed` should reflect them after the
  initial fetch.

```bash
rustup show           # verify the active toolchain is 1.86.0
rustup component add rustfmt clippy  # only if missing
```

Rationale for the 1.86 pin: Pingora 0.8 transitively depends on
`getrandom 0.4` (via `cf-rustracing`), whose `wasip3` target manifests
require `edition2024`. The minimum rustc that recognises that edition is
1.85; we pin 1.86 to match the toolchain installed in CI.

## Builds

```bash
# Workspace check (fast, no codegen).
cargo check --workspace

# Optimised binary.
cargo build --release -p gw-gateway

# Smoke run: requires `GW_KMS__KEK_HEX` set to 64 hex chars.
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
  ./target/release/gw-gateway
```

### KMS dev mode

`GW_KMS__KEK_HEX` must be exactly 64 hexadecimal characters (32 bytes).
It is parsed by `gw-gateway::kms::init` and validated at startup.

> **Edge case to know:** figment's env-var deserializer first tries to
> coerce values to integers. A string composed entirely of decimal digits
> (e.g. `0123…`) would round-trip as `i64`, then fail the hex parser.
> Always include at least one `a–f` character. The `python3 secrets`
> one-liner above does this naturally; if you copy a manual value, add
> a hex letter such as `deadbeef…`.

Production AWS KMS integration replaces this dev path; the env var is
intended for local + CI use only.

## Storage (Redis + Postgres)

The integration tests in `crates/gw-store/tests/` use [testcontainers] to spin
ephemeral containers per test run. They are guarded with `#[ignore]` so
that the default `cargo test` invocation passes on machines without
Docker.

Run them explicitly:

```bash
cargo test --workspace -- --ignored
```

Requirements:

- A reachable Docker daemon. Linux: any working Docker / Podman socket.
  macOS: Docker Desktop, Colima, or OrbStack — all expose `unix:///var/run/docker.sock`.
- Sufficient disk / RAM for the Postgres 16 and Redis 7 images
  testcontainers-modules pulls on first run.

### Running without containers

If you'd rather point at a long-lived Redis / Postgres on your host:

```bash
docker run --rm -p 6379:6379 redis:7-alpine &
docker run --rm -p 5432:5432 \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=gateway \
  postgres:16-alpine &

# Apply the gw-store migration once.
psql postgres://postgres:postgres@localhost:5432/gateway \
  -f crates/gw-store/migrations/0001_initial.sql

GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
  cargo run -p gw-gateway --release
```

## sqlx offline cache

v1 uses runtime-checked sqlx (`sqlx::query` / `sqlx::raw_sql`) exclusively,
so there is no `.sqlx/` directory and no `cargo sqlx prepare` step. When
we adopt the compile-time-checked `sqlx::query!` macros in a future
phase, this section will be updated with the workflow:

```bash
# Future: regenerate .sqlx/ after schema changes.
cargo install sqlx-cli --version 0.8 --no-default-features --features postgres,rustls
DATABASE_URL=postgres://postgres:postgres@localhost:5432/gateway \
  cargo sqlx prepare --workspace
```

## Tests

```bash
# 1. Default — unit + in-process integration. No Docker needed. ~10s.
cargo test --workspace

# 2. With Docker available — adds the ignored testcontainers tests.
cargo test --workspace -- --ignored

# 3. Single crate / single test, useful while iterating.
cargo test -p gw-auth --test merge
```

Failure-mode coverage lives in
`crates/gw-gateway/tests/e2e_failure_modes.rs` and exercises the public
APIs of `gw-auth`, `gw-codec`, and `gw-ledger` directly so the cases run
without Docker.

## Benchmarks

```bash
# Compile every bench (catches signature drift without running).
cargo bench --workspace --no-run

# Full run with default criterion config (~minutes per bench).
cargo bench --workspace

# Quick smoke run for one bench (~6 seconds).
cargo bench -p gw-codec --bench passthrough -- \
  --warm-up-time 1 --measurement-time 2 --sample-size 10
```

Bench output (HTML + JSON) is written under `target/criterion/`. The
plotters backend renders inline PNGs when `gnuplot` is not on `$PATH`.

## Lints / formatting

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Both are part of the CI gate.

## Observability

`gw-gateway` boots with structured `tracing` JSON output to stdout and
exposes a Prometheus scrape endpoint on `:9090/metrics`. The health
probes on `:8080` map to `/livez` (always 200 once the runtime is up)
and `/readyz` (flips to 200 only once the bootstrap thread finishes
wiring KMS / Redis / Postgres and the Pingora service has been added).

To follow logs while running:

```bash
RUST_LOG=gw_gateway=debug,gw_proxy=debug \
  ./target/release/gw-gateway 2>&1 | jq .
```

## Common pitfalls

- **`GW_KMS__KEK_HEX` rejected as non-hex.** See the KMS edge case
  above — include at least one a–f letter.
- **Docker tests time out on macOS.** Docker Desktop's first-run image
  pulls can exceed the default test timeout. Pre-pull the images once
  with `docker pull postgres:16-alpine redis:7-alpine`.
- **`cargo bench` rebuilds the whole workspace.** Bench profiles inherit
  from `release` with debug info, so the first run after `cargo build
  --release` should be cheap; switching back to debug after will recompile.

[testcontainers]: https://docs.rs/testcontainers/0.23
