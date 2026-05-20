# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`ailinkmax-gateway` is a Rust LLM gateway built on Pingora 0.8. It terminates OpenAI/Anthropic/Gemini-shaped client traffic, normalises through an in-memory IR, authenticates + merges permissions + enforces rate-limit/quota/hard-caps, picks an upstream vendor key via one of 8 strategies, streams the response back through the matching codec, and emits a billing record per request to a Redis stream.

The design spec is `docs/superpowers/specs/2026-05-18-llm-gateway-design.md`; the implementation plan + current status are tracked in `docs/superpowers/plans/`. Significant operational gotchas captured in `docs/issues/`.

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

Ports: 8000 business proxy, 9090 Prometheus `/metrics`, 8080 health (`/livez`, `/readyz`).

## Toolchain & build pitfalls

- **Rust 1.86.0** is pinned in `rust-toolchain.toml`. Do not bump without coordinating: Pingora 0.8 pulls `getrandom 0.4` (via `cf-rustracing`) whose `wasip3` target manifest needs `edition2024` (min rustc 1.85). 1.86 matches CI.
- **`GW_KMS__KEK_HEX` must contain at least one a–f character.** Figment's env deserializer tries integer coercion first, so an all-digit 64-char hex string round-trips as `i64` and fails the hex parser. `python3 secrets.token_hex(32)` is fine; hand-rolled values must include a letter.
- **No sqlx offline cache.** v1 uses runtime-checked `sqlx::query` / `sqlx::raw_sql` exclusively — there is no `.sqlx/` directory and no `cargo sqlx prepare` step. When the codebase migrates to `query!` macros the workflow will be added in `docs/dev-setup.md`.
- Migrations live at `crates/gw-store/migrations/0001_initial.sql` and are not auto-applied; for a host Postgres run them with `psql -f`.

## Configuration

Figment layers, low → high precedence: `config/default.toml` → `config/local.toml` (gitignored) → env vars `GW_*` with `__` as nesting separator (e.g. `GW_REDIS__URL`, `GW_PGSQL__URL`, `GW_KMS__KEK_HEX`, `GW_SERVER__BUSINESS_PORT`). Secrets must come from env, never the TOML.

## Architecture: bootstrap shape

Pingora's `Server::run_forever` is `-> !` and manages its own per-service tokio runtimes, but we need long-lived background tasks (Ledger flusher, health server, StreamConsumer) on a runtime that outlives Pingora. The pattern in `gw-gateway/src/lib.rs`:

1. A detached `gw-bootstrap` OS thread builds a multi-thread tokio runtime, does all async init (KMS → Redis → Postgres → `EntityStore` → `Ledger::start` → `StreamConsumer::start` → health HTTP server), and hands a `Wired` bundle back to `main` via `sync_channel`.
2. The bootstrap thread then parks its runtime on `pending::<()>()` so the spawned tasks keep running.
3. `main` constructs the Pingora `Server`, adds the `LlmGateway` service, flips `health::READY` to true, and calls `run_forever()` on the main thread.

`Ledger::try_send` is sync (`tokio mpsc::Sender::try_send`) so it is callable from Pingora's worker threads even though the receiver lives on the bootstrap runtime. Don't move ledger emission to an `async fn` that requires a handle to the right runtime — that's the whole point of the split.

## Architecture: filter chain

`gw-proxy::LlmGateway` is the `ProxyHttp` impl. Each phase delegates to one `filters::*` module:

| Phase                       | Module                  | Role |
|-----------------------------|-------------------------|------|
| `early_request_filter`      | `filters::early`        | IP blocklist, global stream cap, protocol detection, debug trigger |
| `request_filter`            | `filters::request`      | auth (`x-api-key` + `x-api-key-id` pair), IR parse, ADR-023 5-tier permission merge, governor RPM, ADR-024 hard caps, balance check, freeze-estimate, circuit check |
| `upstream_peer`             | `filters::upstream`     | candidate filtering → one of 8 strategies → `HttpPeer` |
| `upstream_request_filter`   | `filters::upstream`     | codec rewrite of outbound body into `ctx.cached_body`; auth header injection |
| `request_body_filter`       | `gateway.rs`            | swaps the cached body in at EOS (otherwise cross-protocol rewrites never reach the wire) |
| `response_filter`           | `gateway.rs`            | captures upstream status |
| `response_body_filter`      | `filters::response`     | incremental SSE parsing, usage accumulation |
| `logging`                   | `filters::logging`      | release freeze (C-γ §ADR-Cγ-2), record breaker outcome, emit `UsageRecord` + `DetailedLog` |
| `suppress_error_log`        | `gateway.rs`            | mutes Pingora's own ERROR line for intentional short-circuits (matches `HTTPStatus` outer + `Custom("gw_short_circuit")` root cause) |

Short-circuits go through `error_emit::send_error`, which sends a vendor-flavoured JSON envelope before returning the marker `Error`.

## Workspace dependency direction

```
gw-core ◄── everything (IR, IDs, error codes)
gw-codec-macros ◄── gw-codec
gw-codec        ◄── gw-proxy
gw-store        ◄── gw-auth, gw-routing, gw-ledger, gw-proxy, gw-gateway
gw-auth         ◄── gw-proxy
gw-routing      ◄── gw-proxy
gw-ledger       ◄── gw-proxy
gw-proxy        ◄── gw-gateway (binary)
```

Treat `gw-core` as the type-only ground floor. Anything that pulls a runtime or I/O goes in `gw-store` or higher.

## Things that are intentional, not bugs

- **Non-stream completion requests are rejected with `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400.** Codec `parse_response` is `unimplemented!()` for every codec in v1, so non-stream would silently bill `cost_micro = 0`. v1.1 lifts this; until then, do not "fix" it by stubbing the parser.
- **Anthropic SDK incompatibility.** The gateway requires `(x-api-key, x-api-key-id)` together. Anthropic's public protocol uses `x-api-key` alone — porting clients must supply both. Missing either yields `GW_AUTH_INVALID_APIKEY` (deliberately generic).
- **`trust_xff` defaults to `false`.** With it off, the IP blocklist matches the L4 socket peer (which behind an L7 LB is the LB's IP, not the client). Only enable it behind a trusted L7 LB that strips client-supplied XFF headers, otherwise the blocklist is trivially bypassable.
- **Distinct 429 codes.** `GW_QUOTA_RPM_EXCEEDED` = gateway tripped per-user RPM; `GW_UPSTREAM_OVERLOADED` = upstream returned 429. Don't collapse them.

## v1 gaps to be aware of when extending

`expires_at`, `ip_allowlist`, `daily_quota_tokens`, `Vendor.timeout_overrides`, `Pricing.model_markup`, JWT `iss`/`aud`/`nbf` validation, mandatory HMAC, cross-protocol *response* body re-render, multi-event SSE buffering, retry executor on the proxy hot path, and DLQ for billing flush failures are all explicitly deferred — see the "Known gaps" list in `README.md`. The `Sticky` strategy leaks assignment-map entries (no TTL sweep) and `crates/gw-gateway/tests/e2e_full_flow.rs` is the full Pingora + wiremock + reqwest harness but is `#[ignore]`d; per-filter coverage that runs without Docker lives in `e2e_failure_modes.rs`.

## Crate layering rules (v2 integration)

Authoritative source: `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §3 C-1. Enforced by `scripts/check-invariants.sh`.

Three layers, strict one-way dependency:

- **Supporting layer** (`gw-core`, `gw-codec`, `gw-codec-macros`, `gw-store`, `gw-auth`, `gw-ledger`): provide generic mechanism only (IR/schema, Redis client, JWT, FreezeGuard trait, routing primitives). MUST NOT depend on any business or execution crate. MUST NOT introduce business namespaces (`mod task`, `mod billing`, vendor-specific `mod claude_code`, etc.). `gw-store` carries only the generic Entity / Counter / Append-log / Lock / Counter-Event protocols — no `task::*` / `billing::*` namespaces.
- **Business layer** (`gw-adapter`, `gw-health`, `gw-routing`, `gw-task`): one crate per business concern. May depend on any supporting crate. Business-to-business dependencies must be registered explicitly in a spec. `gw-routing` was reclassified from supporting to business in Phase 1c: its `Strategy::pick` signature takes `&KeyHealthStore` from `gw-health`; the crate carries `InFlightStore` + strategy impls only — the multi-dim health / circuit / cooldown state machine lives in `gw-health`.
- **Execution layer** (`gw-proxy`, `gw-task-runtime`): runtime / server / binary entry points. May depend on supporting + business crates. **Execution crates MUST NOT depend on each other** — `gw-proxy ↔ gw-task-runtime` is forbidden in both directions.

**Billing boundary:** the Rust gateway only *records* usage (`UsageRecord` → `usage:records` stream). The usage→cost formula and markup-chain application live entirely in Go. No Rust code path may compute cost. The `gw-billing` crate is permanently retired and must not reappear.

**Hard deployment guarantee** (worker spec ADR-T16): gateway config/args/code carry no worker host/port; worker config/args/code carry no gateway host/port. The only coupling surface is a shared Redis namespace (+ KMS KEK, optional shared PgSQL). Any N-gateway × M-worker topology must work with zero peer-awareness.
