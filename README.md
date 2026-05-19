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
- BreakerState in-flight RAII counter (`InFlightGuard`) wired through
  `pick_peer` so `LoadBalance` and the per-key cap
  (`PER_KEY_INFLIGHT_CAP = 1024`) see real traffic.
- Same-protocol passthrough with `Cow::Borrowed` zero-copy fast path.
- Cross-protocol *request* IR re-render (OpenAI ↔ Anthropic ↔ Gemini).
- ADR-023 five-tier permission merge, ADR-024 hard caps, ADR-004 freeze RAII.
- Balance enforcement: `request_filter` reads
  `counter:user:{effective_user_id}:balance_micro` from Redis and rejects
  with `GW_QUOTA_BALANCE_ZERO` 402 when the balance is ≤ 0. The
  `USER_CIRCUIT` in-process cache is the secondary short-circuit.
- Distinct error codes for "gateway tripped user RPM"
  (`GW_QUOTA_RPM_EXCEEDED`) versus "upstream returned 429"
  (`GW_UPSTREAM_OVERLOADED`).
- Optional XFF parsing: `[security] trust_xff` (default `false`). When
  enabled the client IP is read from the first entry of
  `X-Forwarded-For`; otherwise the L4 socket peer is used. MUST only be
  enabled behind a trusted L7 LB that strips client-supplied XFF.
- Single-event SSE streaming surface (sufficient for the v1 codec contracts).
- Billing record emission via mpsc-backed flusher to a Redis stream.
- KMS dev mode (env-supplied KEK); production AWS KMS integration is a
  follow-up.

Operational behaviour and v1 caveats:

- **Authentication**: requests with a valid `x-api-key` + `x-api-key-id` pair
  resolve through the Redis → PostgreSQL cold-load chain. The cache is filled
  on first access and re-validated either via `events:entity` stream
  invalidation (when a control plane XADDs an event with `ns=<entity>, id=<id>`)
  or by per-entity TTL (300s for hot entities, 1800s for vendor/pricing,
  3600s for JWKS).
- **Anthropic SDK clients**: Anthropic's public protocol authenticates with
  `x-api-key` alone. The v1 gateway requires `(x-api-key-id, x-api-key)` as a
  pair so a BYOK gateway can route to the right tenant key without server-side
  lookups by key prefix. Clients porting from Anthropic's SDK must supply
  both headers; missing one yields `GW_AUTH_INVALID_APIKEY` ("missing auth")
  rather than a more specific code.
- **Cross-pod cache invalidation**: in v1 single-pod deployments the cache is
  refreshed via per-entity TTL only. Multi-pod deployments use the
  `events:entity` Redis stream for invalidation, but fred 9.4 + redis:7 has a
  known parse-error path on empty XREADGROUP replies that surfaces as warn
  logs without breaking the request path. See
  `docs/issues/2026-05-19-streamconsumer-xreadgroup-parse-error.md`.

Known gaps tracked for v2 (and v1.1 where called out):

- **Non-stream completions are REJECTED** at `request_filter` with
  `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400. v1.1 will lift this once codec
  `parse_response` is implemented (currently `unimplemented!()` for every
  codec); shipping non-stream today would silently bill `cost_micro = 0`.
- `ApiKeyEntry.expires_at` not enforced — v2.
- `ApiKeyEntry.ip_allowlist` not enforced — v2.
- `UserEntry.daily_quota_tokens` not enforced — v2.
- `Vendor.timeout_overrides` not honoured (only `Pricing` timeouts) — v2.
- `Pricing.model_markup` not multiplied into the cost calculation — v2.
- `Sticky` strategy memory leak: no TTL sweep on the assignment map — v2.
- JWT validation is RS256 algorithm-pinned only; `iss` / `aud` / `nbf`
  not validated — v2.
- HMAC signature verification is opt-in via header; mandatory enforcement
  per policy ships in a follow-up.
- Cross-protocol response body re-render is not yet wired into the
  proxy response filter; v1 routes 2xx through the streaming path.
- Multi-event SSE buffering / coalescing is single-event today
  (Gemini-style combined final event not implemented).
- Per-chunk `serde_json::Value` allocation in SSE parsing — v2 perf.
- `sse_leftover: Vec<u8>` allocates per event drain — v2 perf.
- `ipblock_snapshot()` rebuilds per request — v2 perf.
- No retry executor wired (Pingora default retries off); `RetryConfig`
  exists but the executor isn't on the proxy hot path — v2.
- No DLQ for billing flush failures: a Redis flusher error drops the
  bounded mpsc batch on the floor — v2.
- Cross-protocol body data loss for Anthropic / Gemini wires that lack
  an `extra` IR field — v2.
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
