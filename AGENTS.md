# AGENTS.md

This file is the entry point for any agent working in this repo. It describes **what** the project does, where to find detailed reference docs, and the operational rules that must hold. Architecture and request-lifecycle detail live in [`docs/`](docs/) and must be kept in sync with the code (see §Sync rule below).

## Sync rule

When you make a change that crosses any of these boundaries, update the matching doc **in the same commit**:

| If you change… | Update |
|----------------|--------|
| Crate layout, layering invariants, traits, configuration tables, persistence namespaces, background-task graph | [`docs/architecture.md`](docs/architecture.md) |
| Pingora filters (order, behaviour, `ReqCtx` fields), ledger / detailed-log emission, error-handler fanout | [`docs/workflow.md`](docs/workflow.md) |
| `scripts/check-invariants.sh` lints (activated, retired, or renamed) | [`docs/architecture.md`](docs/architecture.md) §Invariants |
| A behaviour that looks like a bug but is deliberate (fail-closed, generic error message, pair-header requirement, etc.) | [`docs/architecture.md`](docs/architecture.md) §Deliberate behaviours |
| A subsystem's as-built behaviour (codec, routing, key-health, ratelimit, auth, billing, vendor, task, tunnel) | The matching domain spec under [`docs/spec/`](docs/spec/) (`01`–`09`) |
| Deferred gap → implemented, or a new gap discovered | [`docs/ROADMAP.md`](docs/ROADMAP.md) — remove the implemented item, or add the new one with a priority tag |
| Cross-process touchpoint with Go (Redis stream payload, `counter:*` key, PgSQL entity payload JSONB schema, encrypted column shape, KEK rotation) | [`docs/go-contracts/`](docs/go-contracts/main.md) — `main.md` + the relevant `parts/0{1,2,3}-*.md`. Adding a new touchpoint requires a new part or extending an existing one. |
| The agent entry-point file you are reading (`CLAUDE.md` **or** `AGENTS.md`) | The other one. [`CLAUDE.md`](CLAUDE.md) and [`AGENTS.md`](AGENTS.md) are byte-identical except the H1 title; any edit to either **must** be mirrored to the other in the **same commit**. |

PRs that touch code in these areas without updating the index are incomplete.

> **`CLAUDE.md` ⇄ `AGENTS.md` are kept in lockstep.** They share one body so that any agent — whichever entry-point file it reads — sees the same rules. The only intended difference is the first-line title. Edit both together, every time; a divergence between them is a bug.

## What this gateway does

A Rust LLM gateway on **Pingora 0.8**. It accepts OpenAI- / Anthropic- / Gemini-shaped client traffic, normalises to an in-memory IR, authenticates, merges 5-tier permissions, enforces RPM / quota / hard caps, picks an upstream vendor key via one of 8 strategies, streams the response back through the matching client codec, and records usage to a Redis stream. **Cost computation is not performed in Rust** — the Go billing service consumes `usage:records`. The `no_cost_micro` and `no_billing_crate` lints enforce this.

Capability surface highlights:

- **Wire formats**: OpenAI Chat + Responses, Anthropic Messages, Gemini Generate. Same-protocol passthrough is zero-copy; cross-protocol request AND response re-render are wired (request side by codec-s4 v1; response side both stream and non-stream by S-codec v2, landed 2026-05-24). Cross-render activity is surfaced via `gw_codec_cross_render_total{from,to}` (legacy/stream), `gw_codec_cross_render_nonstream_total{from,to}` (v2 non-stream), and `gw_codec_cross_render_lossy_total{from,to,field}` (vendor-specific drops).
- **Auth**: `(x-api-key, x-api-key-id)` pair, `Authorization: Bearer <JWT>` (HS256/RS256, `iss`/`aud`/`nbf`/leeway on the management-token surface), optional HMAC signing with ±300 s replay window.
- **Permissions**: 5-tier merge (global → user-group → user → sub-account → api-key); model-permission regex with a 100 ms per-call ReDoS budget + auto-disable.
- **Routing**: 8 strategies (`FixedOrder`, `RoundRobin`, `LoadBalance`, `WeightedRr`, `CostFirst`, `LatencyFirst`, `CacheAffinity`, `Sticky`). Multi-dim per-(key, format) health, sliding-window cooldown, per-key in-flight cap.
- **Capability**: model → required capabilities → matching keys via `gw-capability::CapabilityResolver`.
- **Tunnel**: optional WebSocket transport to remote proxy nodes (`--features tunnel`). When `cfg.tunnel.enabled = true`, `pick_peer` consults `TunnelStack::resolve` per-request; otherwise the byte-identical Direct path runs. `VendorKey.tunnel_preference.require_tunnel = true` makes a missing node a hard `GW_TUNNEL_NO_NODE` rather than a silent Direct fallback. Boot-time only — flipping `enabled` requires restart. See [`docs/spec/09-tunnel.md`](docs/spec/09-tunnel.md) and `[tunnel]` in `config/default.toml`.
- **Egress impersonation**: optional TLS-fingerprint + browser-header impersonation for vendor keys whose `egress_mode = Impersonate { profile }`. Routes the upstream connection through an in-process loopback (UDS by default) running on `wreq` + BoringSSL, which presents a Chrome-class JA3/JA4 fingerprint, HTTP/2 SETTINGS, and a per-identity static-header set (Claude Code / OpenAI Codex / Gemini CLI). Direct vendors are byte-identical to before (no rustls regression). Off by default at the workspace level (`[egress].disabled = true` is the kill-switch); enable per-vendor by setting `egress_mode`. See [`docs/spec/07-vendor-integration.md`](docs/spec/07-vendor-integration.md) and `[egress]` in `config/default.toml`.
- **Credit lease (hard overdraft protection)**: optional `[protection]` layer above the legacy soft `frozen_micro` defense. When enabled and Go pushes a user into protected mode (`events:counter ns=protected_lease`), Rust holds a per-Pod scalar credit pool and hard-rejects requests with `GW_QUOTA_LEASE_INSUFFICIENT` / `GW_QUOTA_LEASE_EXPIRED` (fail-closed on TTL). Go's outstanding ledger guarantees `Σ outstanding ≤ balance × (1 + overcommit_ratio)`. See [`docs/spec/06-billing-ledger.md`](docs/spec/06-billing-ledger.md) and [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](docs/go-contracts/contract-change/2026-05-29-credit-lease.md).
- **Soak / chaos harness**: a 50,000-req / 60-s end-to-end soak (3 protocols × stream/non-stream × `off|light|heavy` upstream chaos) driven from `tests/e2e/client/soak/`. Hard invariants cover process liveness, strict-equality account consistency (`usage:records + dlq == fake_upstream_hits_total` at drain steady state), resource gauge release, error-code catalog compliance, and chaos causal reconciliation. Reports are AI-consumable JSON+JSONL at `tests/e2e/client/.soak_report/`. See [`docs/architecture.md`](docs/architecture.md). Gateway code is not touched — soak only consumes existing surfaces (§C1).
- **Observability**: `usage:records` (billing), `usage:detailed` (wire-fidelity archive — 5 per-surface XADD per request, tiered compression raw/lz4/zstd, no redaction, drop-on-outage; Go drainer mandatory), Prometheus `/metrics`, `/livez` + `/readyz`, admin `/healthz/*` (token-gated). Billing robustness (lifecycle state machine, InflightLedger crash recovery, T0 emit-lag watchdog, per-attempt UsageRecord) per [`docs/spec/06-billing-ledger.md`](docs/spec/06-billing-ledger.md).

For the dependency DAG, traits, persistence namespaces, KEK rotation, and the full list of invariants & deliberate behaviours, read [`docs/architecture.md`](docs/architecture.md). For the request lifecycle (bootstrap, every filter phase, error fanout, streaming, cache invalidation), read [`docs/workflow.md`](docs/workflow.md).

## Source layout

```
crates/                  Rust workspace (21 crates; see docs/architecture.md §Workspace layout)
config/default.toml      figment base layer; per-machine override at config/local.toml (gitignored)
docs/
  architecture.md        Layered architecture, crates, traits, persistence, invariants
  workflow.md            Bootstrap + Pingora filter chain + error fanout
  dev-setup.md           Per-machine dev setup (Docker, Postgres, Redis)
  go-contracts/          Rust ↔ Go cross-process contracts (main.md + parts/01-07)
  spec/                  As-built domain standard (01-protocol-codec … 09-tunnel) — authoritative
  ROADMAP.md             Unbuilt + divergent items, priority-tagged (runtime-risk > capability-gap > doc-divergence)
  issues/                Operational gotchas (e.g. fred + redis:7 XREADGROUP parse warn,
                         egress-impersonate known gaps)
scripts/check-invariants.sh   Layering / namespace / retired-symbol lints
```

## Code Search

Use `semble search` to find code by describing what it does or naming a symbol/identifier, instead of grep:

​```bash
semble search "authentication flow" ./my-project
semble search "save_pretrained" ./my-project
semble search "save model to disk" ./my-project --top-k 10
​```

If you anticipate doing more than one search, use `semble index` to create an index.

​```bash
semble index ./my-project -o my_index
​```

You can then reuse this index later on:

​```bash
semble search "save_pretrained" --index my_index
​```

An index is not automatically updated, so if the code changes significantly, reindex. If you notice stale results while resolving searches to files, reindex.

Use `--content docs` to search documentation and prose, `--content config` for config files (yaml, toml, etc.), or `--content all` to search code, docs, and config:

​```bash
semble search "deployment guide" ./my-project --content docs
semble search "database host port" ./my-project --content config
semble search "authentication" ./my-project --content all
​```

Use `semble find-related` to discover code similar to a known location (pass `file_path` and `line` from a prior search result):

​```bash
semble find-related src/auth.py 42 ./my-project
​```

Like search, `find-related` also accepts an `--index` argument.

`path` defaults to the current directory when omitted; git URLs are accepted.

If `semble` is not on `$PATH`, use `uvx --from "semble[mcp]" semble` in its place.

### Workflow

1. Index the repo using `semble index -o cached_index`.
2. Start with `semble search` to find relevant chunks. Pass the index to achieve results faster.
3. Use `--content docs` for documentation, `--content config` for config files, or `--content all` for everything.
4. Inspect full files only when the returned chunk does not give enough context.
5. Optionally use `semble find-related` with a promising result's `file_path` and `line` to discover related implementations.
6. Use grep only when you need exhaustive literal matches or quick confirmation of an exact string.

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

# Soak / chaos harness. Long-running; not in default `make e2e`.
make e2e-soak        # full 833 rps × 60s × off + heavy (spec §12.1 verification gate)
make e2e-soak-smoke  # 100 rps × 10s smoke for driver/seed iteration
```

Running `make e2e-soak-smoke` before opening a PR is strongly encouraged for changes that touch request handling, codec, routing, or ledger code paths. The smoke run takes roughly 15 seconds with Docker up and catches accounting drift, resource leaks, and cross-protocol regressions that unit tests won't. It's recommended as a final local sanity check, not a CI gate.
If `make e2e-soak-smoke` passes clean, the full `make e2e-soak` run is unlikely to surprise you.

Smoke-run the binary:

```bash
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
  ./target/release/gw-gateway
```

Ports: `8000` business proxy, `9090` Prometheus `/metrics`, `8080` health (`/livez`, `/readyz`, `/healthz/*`).

## Toolchain & build pitfalls

- **Rust 1.88.0** is pinned in `rust-toolchain.toml`. The bump from 1.86 was driven by `wreq` (egress-impersonate transitive) which requires a newer compiler; Pingora 0.8 + `getrandom 0.4`'s `wasip3` target manifest is still satisfied.
- **`GW_KMS__KEK_HEX` must contain at least one a–f character.** Figment's env deserializer tries integer coercion first, so an all-digit 64-char hex string round-trips as `i64` and fails the hex parser. `python3 secrets.token_hex(32)` is fine; hand-rolled values must include a letter.
- **No sqlx offline cache.** v1 uses runtime-checked `sqlx::query` / `sqlx::raw_sql` exclusively — there is no `.sqlx/` directory and no `cargo sqlx prepare` step. When the codebase migrates to `query!` macros the workflow will be added in `docs/dev-setup.md`.
- Migrations live under `crates/gw-store/migrations/` (`0001_initial.sql`, `0002_gemini_file_mapping.sql`, `0003_model_mapping.sql`, `0004_capabilities.sql`, `0004_task_worker.sql` — note the duplicate `0004_` prefix, `0005_proxy_nodes.sql`, `0006_auth_configs.sql`, `0007_management_tokens.sql`, `0008_signature_secret.sql`, `0009_egress_impersonation.sql`) and are **not auto-applied**; for a host Postgres run them with `psql -f`.
- **boring-sys system deps.** The egress-impersonate stack pulls `wreq` → `boring-sys`, which bundles BoringSSL and runs bindgen at build time. The Docker builder installs `cmake`, `build-essential`, `pkg-config`, `perl`, `libssl-dev`, `libclang-dev` for this. A host build needs the same set (or the equivalent for your distro) on `$PATH`. Without `libclang-dev` the bindgen step fails; without `build-essential` (a C++ toolchain) the BoringSSL build fails.

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

Not the as-built design — those live under [`docs/spec/`](docs/spec/). Not the roadmap — [`docs/ROADMAP.md`](docs/ROADMAP.md) tracks unbuilt and divergent work. Not deep architecture — [`docs/architecture.md`](docs/architecture.md) and [`docs/workflow.md`](docs/workflow.md) are. Keep this file small; if you find yourself adding more than a paragraph here, the right home is probably `docs/`.
