# AILinkMax-Gateway

`ailinkmax-gateway` is a Rust LLM gateway built on Pingora. It terminates
OpenAI-, Anthropic-, and Gemini-shaped client traffic, normalises requests
through an in-memory IR, applies authentication, permission merging,
rate-limit and quota enforcement, picks an upstream vendor key via one of
eight selection strategies, streams the response back through the matching
client codec, and emits a single billing record per request to a Redis
stream.

The implementation follows the design spec under
`docs/superpowers/plans/2026-05-18-llm-gateway-implementation.md`. v1 is
feature-complete for the documented happy path and the failure matrix
called out in §5; gaps that remain are recorded in the
[Status](#status--v1-limitations) section below.

## Workspace map

| Crate | Responsibility |
|-------|----------------|
| `gw-core` | ID newtypes, entity structs, IR types (`RequestIR` / `ResponseIR` / `StreamEventIR`), `ErrorIR`, stable error code dictionary. |
| `gw-codec-macros` | Proc-macro helpers driving boilerplate inside `gw-codec`. |
| `gw-codec` | OpenAI / Anthropic / Gemini protocol codecs, `ProtocolCodec` trait, same-protocol passthrough fast path, cross-protocol IR re-render. |
| `gw-store` | Redis + Postgres clients, `EntityStore` write-through cache, entity loader, KMS-backed `KekCache`, deduction state, counter store. |
| `gw-auth` | JWT / API-key verify, ADR-023 five-tier permission merge, governor-based RPM rate limiting, ADR-024 single-request hard caps, HMAC signature verify, IP allow / blocklist. |
| `gw-routing` | Candidate filtering, breaker store, eight selection strategies (`fixed_order`, `round_robin`, `load_balance`, `weighted_rr`, `cost_first`, `latency_first`, `cache_affinity`, `sticky`), retry config + budget. |
| `gw-ledger` | `FreezeGuard` RAII for the three-tier deduction state (ADR-004), mpsc-backed billing flusher with Redis pipeline, detailed-log sink trait. |
| `gw-proxy` | Pingora `ProxyHttp` impl: request / upstream / response / logging filter chain, error emission, timeouts, per-request jemalloc sampling. |
| `gw-gateway` | Binary entry point. Owns config (figment), observability bootstrap (tracing + Prometheus), KMS init, runtime bootstrap thread that hosts background tasks while Pingora's `run_forever` lives on the main thread. |

## Quick start

```bash
# Build the binary in release mode.
cargo build --release -p gw-gateway

# Minimum env vars (override config/default.toml as needed):
#   - GW_KMS__KEK_HEX:  64 hex chars (32-byte KEK) for the dev KMS path.
#                      Must contain at least one a-f character to dodge a
#                      figment integer-coercion edge case.
#   - GW_REDIS__URL:   override the Redis URL (default redis://localhost:6379).
#   - GW_PGSQL__URL:   override the Postgres URL.
GW_KMS__KEK_HEX=$(printf '%064s' deadbeef | tr ' ' '0') \
GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
  ./target/release/gw-gateway
```

The binary opens three TCP listeners by default (`config/default.toml`):

| Port | Purpose |
|------|---------|
| 8000 | Business HTTP listener (proxy entrypoint). |
| 9090 | Prometheus `/metrics` exporter. |
| 8080 | Health probes — `/livez` (always 200), `/readyz` (200 once bootstrap completes). |

See `docs/dev-setup.md` for a per-machine setup walk-through, including the
Docker requirements for `--ignored` tests.

## Test matrix

```bash
# Unit + in-process integration (no Docker required). Runs in ~10s.
cargo test --workspace

# Adds testcontainers-backed integration (Postgres + Redis). Requires a
# running Docker daemon.
cargo test --workspace -- --ignored

# Criterion microbenchmarks (slow; only runs the bench targets).
cargo bench --workspace
```

Lints:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Benches currently shipped:

- `gw-codec/benches/passthrough.rs` — same-protocol passthrough (AC-2.3-1).
- `gw-codec/benches/cross_protocol.rs` — cross-protocol IR re-render (AC-2.3-2).
- `gw-auth/benches/merge_permissions.rs` — five-tier merge with 10 user groups (AC-023-5).
- `gw-routing/benches/strategy_pick.rs` — all 8 strategies over an 8-key slice.

The bench acceptance criteria from the spec are: P50 ≤ 200 µs on codec
passthrough, P99 ≤ 5 µs on merge_permissions, P99 ≤ 100 µs on every
strategy pick. Concrete numbers on developer-class hardware are captured
under `target/criterion/` after `cargo bench`; deviations from the spec
are tracked as TODO items in the relevant crate.

## Status / v1 limitations

What v1 ships:

- All 8 routing strategies with breaker integration.
- Same-protocol passthrough with `Cow::Borrowed` zero-copy fast path.
- Cross-protocol *request* IR re-render (OpenAI ↔ Anthropic ↔ Gemini).
- ADR-023 five-tier permission merge, ADR-024 hard caps, ADR-004 freeze RAII.
- Single-event SSE streaming surface (sufficient for the v1 codec contracts).
- Billing record emission via mpsc-backed flusher to a Redis stream.
- KMS dev mode (env-supplied KEK); production AWS KMS integration is a
  follow-up.

Known gaps tracked for v2:

- Cross-protocol response body re-render is not yet wired into the
  proxy response filter; v1 routes 2xx through the streaming path.
- HMAC signature verification is opt-in via header; mandatory enforcement
  per policy ships in a follow-up.
- Multi-event SSE buffering / coalescing is single-event today.
- The full Pingora + wiremock + reqwest e2e harness is scaffolded in
  `crates/gw-gateway/tests/e2e_full_flow.rs` but marked `#[ignore]`; we
  ship per-filter integration tests in `e2e_failure_modes.rs` that run
  without Docker.

## Layout

```
crates/
  gw-core/         IR + entity + IDs + error codes
  gw-codec-macros/ proc-macros for codec boilerplate
  gw-codec/        OpenAI / Anthropic / Gemini wire codecs
  gw-store/        Redis + Postgres + KMS + caches
  gw-auth/         auth + permissions + rate limit + hard caps
  gw-routing/      strategies + breaker + retry + candidates
  gw-ledger/       freeze RAII + billing flusher
  gw-proxy/        Pingora ProxyHttp filter chain
  gw-gateway/      binary entrypoint + bootstrap thread
config/default.toml  default config (override via env / config/local.toml)
docs/
  superpowers/     design specs and implementation plan
  dev-setup.md     per-machine dev setup
```

## License

Apache-2.0. See `LICENSE` (TBD) or `[workspace.package]` in `Cargo.toml`.
