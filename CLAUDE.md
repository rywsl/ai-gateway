# CLAUDE.md

This file is the entry point for any agent working in this repo. It describes **what** the project does, where to find detailed reference docs, and the operational rules that must hold. Architecture and request-lifecycle detail live in [`docs/`](docs/) and must be kept in sync with the code (see §Sync rule below).

## Sync rule

When you make a change that crosses any of these boundaries, update the matching doc **in the same commit**:

| If you change… | Update |
|----------------|--------|
| Crate layout, layering invariants, traits, configuration tables, persistence namespaces, background-task graph | [`docs/architecture.md`](docs/architecture.md) |
| Pingora filters (order, behaviour, `ReqCtx` fields), ledger / detailed-log emission, error-handler fanout | [`docs/workflow.md`](docs/workflow.md) |
| `scripts/check-invariants.sh` lints (activated, retired, or renamed) | [`docs/architecture.md`](docs/architecture.md) §Invariants |
| A behaviour that looks like a bug but is deliberate (fail-closed, generic error message, pair-header requirement, etc.) | [`docs/architecture.md`](docs/architecture.md) §Deliberate behaviours |
| Deferred gap → implemented | Remove from [`docs/architecture.md`](docs/architecture.md) §Known gaps; if it was tracked in a spec or plan under `docs/superpowers/`, mark it landed there too |
| Cross-process touchpoint with Go (Redis stream payload, `counter:*` key, PgSQL entity payload JSONB schema, encrypted column shape, KEK rotation) | [`docs/go-contracts/`](docs/go-contracts/main.md) — `main.md` + the relevant `parts/0{1,2,3}-*.md`. Adding a new touchpoint requires a new part or extending an existing one. |

PRs that touch code in these areas without updating the index are incomplete.

## What this gateway does

A Rust LLM gateway on **Pingora 0.8**. It accepts OpenAI- / Anthropic- / Gemini-shaped client traffic, normalises to an in-memory IR, authenticates, merges 5-tier permissions, enforces RPM / quota / hard caps, picks an upstream vendor key via one of 8 strategies, streams the response back through the matching client codec, and records usage to a Redis stream. **Cost computation is not performed in Rust** — the Go billing service consumes `usage:records`. The `no_cost_micro` and `no_billing_crate` lints enforce this.

Capability surface highlights:

- **Wire formats**: OpenAI Chat + Responses, Anthropic Messages, Gemini Generate. Same-protocol passthrough is zero-copy; cross-protocol request AND response re-render are wired (G8 closed in `50cd9e7`) — both directions emit the `gw_codec_cross_render_total{from,to}` metric.
- **Auth**: `(x-api-key, x-api-key-id)` pair, `Authorization: Bearer <JWT>` (HS256/RS256, `iss`/`aud`/`nbf`/leeway on the management-token surface), optional HMAC signing with ±300 s replay window.
- **Permissions**: 5-tier merge (global → user-group → user → sub-account → api-key); model-permission regex with a 100 ms per-call ReDoS budget + auto-disable.
- **Routing**: 8 strategies (`FixedOrder`, `RoundRobin`, `LoadBalance`, `WeightedRr`, `CostFirst`, `LatencyFirst`, `CacheAffinity`, `Sticky`). Multi-dim per-(key, format) health, sliding-window cooldown, per-key in-flight cap.
- **Capability**: model → required capabilities → matching keys via `gw-capability::CapabilityResolver` (resolver injection into `LlmGateway` is the wave-7 follow-up T6-A).
- **Tunnel**: WebSocket transport crate `gw-tunnel-client` exists (frame codec, WS transport, DoH resolver, target-filter whitelist), but is **not yet wired into the data path** as of 2026-05-22 — `--features tunnel` only re-exports the crate. Implementation tracked in [`docs/superpowers/specs/2026-05-22-tunnel-t-design.md`](docs/superpowers/specs/2026-05-22-tunnel-t-design.md).
- **Observability**: `usage:records` (billing), `usage:detailed` (verbose, three-level redaction, XADD MAXLEN), Prometheus `/metrics`, `/livez` + `/readyz`, admin `/healthz/*` (token-gated).

For the dependency DAG, traits, persistence namespaces, KEK rotation, and the full list of invariants & deliberate behaviours, read [`docs/architecture.md`](docs/architecture.md). For the request lifecycle (bootstrap, every filter phase, error fanout, streaming, cache invalidation), read [`docs/workflow.md`](docs/workflow.md).

## Source layout

```
crates/                  Rust workspace (13 crates; see docs/architecture.md §Workspace layout)
config/default.toml      figment base layer; per-machine override at config/local.toml (gitignored)
docs/
  architecture.md        Layered architecture, crates, traits, persistence, invariants
  workflow.md            Bootstrap + Pingora filter chain + error fanout
  dev-setup.md           Per-machine dev setup (Docker, Postgres, Redis)
  go-contracts/          Rust ↔ Go cross-process contracts (main.md + parts/01-03)
  superpowers/specs/     Authoritative design specs
  superpowers/plans/     Implementation plans + status
  issues/                Operational gotchas (e.g. fred + redis:7 XREADGROUP parse warn)
scripts/check-invariants.sh   Layering / namespace / retired-symbol lints
```

## Common commands

```bash
# Build (binary lives in target/release/gw-gateway).
cargo build --release -p gw-gateway

# Default tests — unit + in-process integration, no Docker (~10s).
cargo test --workspace

# Adds testcontainers-backed Postgres+Redis integration tests. Needs Docker.
cargo test --workspace -- --ignored

# Single test / crate while iterating.
cargo test -p gw-auth --test merge

# Lints (both are CI gates).
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Architectural invariants.
bash scripts/check-invariants.sh

# Benches (criterion). --no-run catches signature drift fast.
cargo bench --workspace --no-run
cargo bench -p gw-codec --bench passthrough -- --warm-up-time 1 --measurement-time 2 --sample-size 10
```

Smoke-run the binary:

```bash
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
  ./target/release/gw-gateway
```

Ports: `8000` business proxy, `9090` Prometheus `/metrics`, `8080` health (`/livez`, `/readyz`, `/healthz/*`).

## Toolchain & build pitfalls

- **Rust 1.86.0** is pinned in `rust-toolchain.toml`. Do not bump without coordinating: Pingora 0.8 pulls `getrandom 0.4` (via `cf-rustracing`) whose `wasip3` target manifest needs `edition2024` (min rustc 1.85). 1.86 matches CI.
- **`GW_KMS__KEK_HEX` must contain at least one a–f character.** Figment's env deserializer tries integer coercion first, so an all-digit 64-char hex string round-trips as `i64` and fails the hex parser. `python3 secrets.token_hex(32)` is fine; hand-rolled values must include a letter.
- **No sqlx offline cache.** v1 uses runtime-checked `sqlx::query` / `sqlx::raw_sql` exclusively — there is no `.sqlx/` directory and no `cargo sqlx prepare` step. When the codebase migrates to `query!` macros the workflow will be added in `docs/dev-setup.md`.
- Migrations live at `crates/gw-store/migrations/0001_initial.sql`, `0002_gemini_file_mapping.sql`, `0003_model_mapping.sql` and are **not auto-applied**; for a host Postgres run them with `psql -f`.

## Configuration

Figment layers, low → high precedence: `config/default.toml` → `config/local.toml` (gitignored) → env vars `GW_*` with `__` as nesting separator (e.g. `GW_REDIS__URL`, `GW_PGSQL__URL`, `GW_KMS__KEK_HEX`, `GW_SERVER__BUSINESS_PORT`). Secrets must come from env, never the TOML. Table-by-table reference lives in [`docs/architecture.md`](docs/architecture.md) §Configuration.

## Where to look first when…

- **Tracing a request** — start at [`docs/workflow.md`](docs/workflow.md) §Pingora filter chain.
- **Adding a vendor / codec / strategy** — [`docs/architecture.md`](docs/architecture.md) §Public traits.
- **Debugging a 401 / 402 / 429** — `crates/gw-core/src/error/codes.rs` for the code, [`docs/workflow.md`](docs/workflow.md) §request_filter for where it's emitted.
- **A "this looks like a bug" moment** — check [`docs/architecture.md`](docs/architecture.md) §Deliberate behaviours first. Many fail-closed behaviours are intentional.
- **CI invariant failure** — `scripts/check-invariants.sh` output names the rule; the meaning is in [`docs/architecture.md`](docs/architecture.md) §Invariants.
- **Operational incident** — look under `docs/issues/`. The `fred` 9.4 + Redis 7 XREADGROUP parse warning is captured there and is benign.

## What this file is not

Not the design spec — those live under `docs/superpowers/specs/`. Not the implementation status — `docs/superpowers/plans/` tracks that. Not deep architecture — [`docs/architecture.md`](docs/architecture.md) and [`docs/workflow.md`](docs/workflow.md) are. Keep this file small; if you find yourself adding more than a paragraph here, the right home is probably `docs/`.
