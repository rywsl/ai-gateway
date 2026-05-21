# Architecture — ailinkmax-gateway

> **Sync rule**: if you change the crate layout, layering invariants, traits, configuration tables, persistence namespaces, or background-task graph, update this file in the same commit. Code is the source of truth; this document is the index.
>
> Companion: [`workflow.md`](workflow.md) — what each filter does, per phase.

## 1. What this gateway is

A Rust LLM gateway built on **Pingora 0.8**. It terminates OpenAI / Anthropic / Gemini-shaped client traffic, normalises through an in-memory IR, authenticates, merges 5-tier permissions, enforces rate-limit / quota / hard-caps, picks an upstream vendor key via one of 8 strategies, streams the response back through the matching client codec, and emits a usage record per request to a Redis stream. Cost computation is **not** performed in Rust — the Go billing service consumes `usage:records`.

## 2. Workspace layout

Three layers with strict one-way dependency (enforced by `scripts/check-invariants.sh`):

```
┌────────── Execution ──────────┐  gw-gateway (bin), gw-proxy
│   may depend on ↓ ↓ ↓         │
├────────── Business ───────────┤  gw-adapter, gw-health, gw-routing,
│   may depend on ↓             │  gw-capability, gw-tunnel-client
├────────── Supporting ─────────┤  gw-core, gw-codec, gw-codec-macros,
│   type / mechanism only        │  gw-store, gw-auth, gw-ledger
└───────────────────────────────┘
```

**Forbidden edges**: Supporting → Business/Execution. Execution → Execution.

### 2.1 Crate responsibilities

| Crate | Layer | Responsibility |
|-------|-------|----------------|
| `gw-core` | Supporting | ID newtypes, entity structs, `RequestIR` / `ResponseIR` / `StreamEventIR`, `ErrorIR`, `ErrorClass`, `DefaultClassifier`, stable error-code dictionary. No I/O. |
| `gw-codec-macros` | Supporting | Proc-macros driving codec boilerplate (Phase 2 `wire_convert_matrix!` scaffolding). |
| `gw-codec` | Supporting | `ProtocolCodec` trait + OpenAI / Anthropic / Gemini codecs, same-protocol passthrough fast path, cross-protocol IR re-render, SSE accumulator, gzip/brotli compression, stream-policy truth table. |
| `gw-store` | Supporting | Redis + Postgres clients, `EntityStore` (9 Moka caches with cold-load chain), `KekCache` state machine, `CounterStore`, deduction state, `StreamConsumer`. |
| `gw-auth` | Supporting | `JwtValidator` (iss/aud/nbf/leeway), API-key verify, ADR-023 5-tier permission merge, governor RPM, ADR-024 hard caps, HMAC signature verify, IP allow/blocklist, model-permission regex w/ ReDoS budget, management-path detect. |
| `gw-ledger` | Supporting | `FreezeGuard` RAII (in-process `frozen_micro`), mpsc-backed billing flusher, `DetailedLogSink` (Redis Streams w/ MAXLEN), three-level redaction. |
| `gw-adapter` | Business | `UpstreamAdapter` trait (vendor-specific outbound rewrite hooks), `UpstreamErrorAnalyzer` trait, `ErrorOutcome`. |
| `gw-health` | Business | `KeyHealthStore` — multi-dim (KeyId, ApiFormat) circuit/cooldown state machine, sliding window. |
| `gw-routing` | Business | Candidate filtering, `InFlightStore`, 8 `Strategy` impls. Takes `&KeyHealthStore` in `pick`. |
| `gw-capability` | Business | Model-capability resolution: `Capability` enum, `RequiredCapabilities`, `ModelMapper`, `CapabilityResolver`, `AdapterCapabilityHook`, `CapabilityFilter`, `PricingReader`. |
| `gw-tunnel-client` | Business | Tunnel transport to remote nodes: frame codec (magic `0xAE21`, 8 msg types), WebSocket transport w/ heartbeat / reconnect, DoH resolver, target-filter whitelist. Opt-in via `gw-gateway` feature `tunnel`. |
| `gw-proxy` | Execution | Pingora `ProxyHttp` impl. Filter chain, `ReqCtx`, error fanout, timeouts, body-verify hook, jemalloc sampling. |
| `gw-gateway` | Execution | Binary entry. Figment config, observability, KMS init, bootstrap OS thread + tokio runtime, health HTTP server. |

### 2.2 Dependency DAG (path deps only)

```
gw-core ◄── everything

Supporting:
  gw-codec-macros  ◄── gw-codec
  gw-codec         ◄── gw-proxy, gw-adapter
  gw-store         ◄── gw-auth, gw-routing, gw-ledger, gw-capability,
                       gw-tunnel-client, gw-proxy, gw-gateway
  gw-auth          ◄── gw-capability, gw-proxy, gw-gateway
  gw-ledger        ◄── gw-proxy, gw-gateway

Business:
  gw-health        ◄── gw-adapter, gw-routing, gw-proxy, gw-gateway
  gw-adapter       ◄── (wired into gw-proxy in Phase 3)
  gw-routing       ◄── gw-proxy, gw-gateway
  gw-capability    ◄── (resolver injection pending — wave-7 T6-A)
  gw-tunnel-client ◄── gw-gateway (feature="tunnel")

Execution:
  gw-proxy ◄── gw-gateway (binary)
```

## 3. Public traits — extension surface

| Trait | Crate | Purpose | Impls |
|-------|-------|---------|-------|
| `ProtocolCodec` | gw-codec | Wire ⇄ IR per protocol | `OpenAiCodec`, `AnthropicCodec`, `GeminiCodec`, `PassthroughCodec`, `ErrorPassthroughCodec`, `AnyCodec` (dispatcher) |
| `ErrorClassifier` | gw-core | HTTP status + body → `ClassifierOutcome` | `DefaultClassifier` |
| `UpstreamErrorAnalyzer` | gw-adapter | Vendor-specific error shape → `ErrorOutcome` | `DefaultErrorAnalyzer` (Phase 3 adds vendor analyzers) |
| `UpstreamAdapter` | gw-adapter | 4 outbound rewrite hooks + transport profile | `NoopAdapter` |
| `Strategy` | gw-routing | Pick one `VendorKey` from candidates | `FixedOrder`, `RoundRobin`, `LoadBalance`, `WeightedRr`, `CostFirst`, `LatencyFirst`, `CacheAffinity`, `Sticky` (8 total, `StrategyAny` enum) |
| `FreezeGuard` | gw-ledger | RAII freeze on `frozen_micro` | `RequestFreeze`, `TaskFreeze` |
| `DetailedLogSink` | gw-ledger | Verbose log output | `RedisStreamSink` (XADD MAXLEN) |
| `ResolveBackend` | gw-capability | Model / key capability lookup | (injected at boot) |
| `JwtBlacklist` | gw-auth | Token revocation check | `RedisJwtBlacklist` (with `FailMode`) |
| `HeaderView` | gw-adapter | Borrow-friendly header access | (per-adapter) |
| `ContentFilter` / `TokenEstimator` / `Redactor` | gw-core | Pluggable scrubbing / sizing | `Noop*` |

## 4. Configuration

Figment layering (low → high): `config/default.toml` → `config/local.toml` (gitignored) → `GW_*` env vars (`__` is nesting separator).

### 4.1 TOML tables

| Table | Purpose |
|-------|---------|
| `[server]` | `business_port` (8000), `metrics_port` (9090), `health_port` (8080) |
| `[redis]` | URL, pool size |
| `[pgsql]` | URL, max connections |
| `[ratelimit]` | Workspace `global_rpm` ceiling (per-user lives in DB) |
| `[ledger]` | `channel_capacity`, `batch_size`, `flush_interval_ms` for the billing flusher |
| `[kms]` | KEK identifier; secret is env-only |
| `[debug]` | Sampling rate + body-capture toggles for detailed logs |
| `[security]` | `trust_xff` (default `false`) |

Secrets must come from env, never the TOML.

### 4.2 Env-var caveat

`GW_KMS__KEK_HEX` **must contain at least one a-f character**. Figment tries integer coercion first; an all-digit 64-char hex round-trips as `i64` and fails the hex parser downstream. Use `python3 -c 'import secrets; print(secrets.token_hex(32))'`.

### 4.3 KEK rotation state machine

`crates/gw-store/src/encrypt.rs::KekCache`:

```
Healthy(K_old)
    ↓ enter_dual_read(K_new)        // K_old + K_new both resident, K_new active
DualRead { reencrypting: false }
    ↓ mark_reencrypting()
DualRead { reencrypting: true }     // re-encryption job running
    ↓ drop_old_kek(K_old)
Healthy(K_new)
```

Invariants: max 2 KEKs concurrent; never drop only KEK; cannot drop active KEK; cannot enter dual-read twice.

## 5. Persistence

### 5.1 PostgreSQL

Migrations live at `crates/gw-store/migrations/`. **Not auto-applied** — run `psql -f` on a host Postgres for dev.

| File | Tables |
|------|--------|
| `0001_initial.sql` | `users`, `user_groups`, `api_keys`, `vendors`, `vendor_keys`, `model_groups`, `pricings`, `jwks`, `ip_blocks`. All share `(id TEXT PK, payload JSONB, version BIGINT, updated_at TIMESTAMPTZ, deleted_at TIMESTAMPTZ NULL)`. |
| `0002_gemini_file_mapping.sql` | `gemini_file_mapping(sha256_hex, user_id) PK → file_uri, mime, size, expire_unix_s`. |
| `0003_model_mapping.sql` | `model_mapping(user_alias, protocol) PK → default_upstream_model, vendor_key_overrides JSONB, availability, pricing_strategy_ref`. |
| `0004_capabilities.sql` | New `models` table (canonical entity shape) for `Model.capabilities`; `vendor_keys` gains `forced_capabilities JSONB` + `exclusive_capability_match BOOLEAN`. Capability strings parsed by `gw-capability` at resolve time (see [`docs/superpowers/specs/2026-05-21-known-gaps-design.md`](superpowers/specs/2026-05-21-known-gaps-design.md) ADR-G13-1). |

### 5.2 Redis namespaces

| Namespace | Purpose | Writer | Reader |
|-----------|---------|--------|--------|
| `cache:{ns}:{id}` | Entity cache L1 | `gw-store::loader` (write-back), control plane | `EntityStore::*_or_load` |
| `counter:{scope}:{id}:{field}` | Runtime counters (`balance_micro`, RPM, TPM) | Go billing service, `CounterStore::incrby` | `request_filter` balance check, RPM guard |
| `events:entity` | Cache-invalidation stream | Control plane | `StreamConsumer` → `EntityStore::invalidate` |
| `events:counter` | Counter event echo (Phase 5) | (deferred) | `StreamConsumer` (observability only in v1) |
| `usage:records` | Billing records | `Ledger` flusher (XADD batched) | Go billing service |
| `usage:detailed` | Verbose request/response log | `RedisStreamSink` (XADD MAXLEN) | Optional Go consumer |
| (in-process) | Sticky assignment | `Sticky` strategy | `Sticky` strategy. G12 (T2-C) moved bindings to an in-process `moka::sync::Cache` (`time_to_live = 30 min`, `max_capacity = 100_000`, LRU eviction on overflow); no Redis namespace is used. |
| `jwt:blacklist:*` | JWT revocation (T1-C) | Control plane | `RedisJwtBlacklist` (with `FailMode::ClosedOnError` / `OpenOnError`) |
| `lock:*`, `key_health:*`, `circuit:*`, `dedup:*` | Reserved for Phase 5+ | — | — |

## 6. Lifecycle — bootstrap shape

Pingora's `Server::run_forever` is `-> !` and manages its own per-service runtimes, but we need long-lived background tasks (Ledger flusher, StreamConsumer, health server) on a runtime that outlives Pingora. The pattern in `crates/gw-gateway/src/lib.rs`:

```
main thread                              bootstrap thread (detached)
─────────────                            ─────────────────────────
load config                              build multi-thread tokio rt
spawn bootstrap thread  ────────────►    KMS init
                                         Redis connect
                                         Postgres connect
                                         EntityStore + Loader
                                         KeyHealthStore, InFlightStore,
                                          RateLimitGuard, CounterStore
                                         Ledger::start         (spawned task)
                                         StreamConsumer::start (spawned task)
                                         health::run_server   (spawned task)
                                         Prometheus exporter   (spawned task)
                  ◄─── Wired bundle ───  pending::<()>().await    // park, keep tasks alive
build pingora::Server
add LlmGateway service
health::READY ← true
server.run_forever()    // never returns
```

`Ledger::try_send` is synchronous (`tokio mpsc::Sender::try_send`) so Pingora worker threads enqueue records without owning the bootstrap runtime handle.

### 6.1 Listeners

| Port | Server | Module |
|------|--------|--------|
| 8000 | Pingora business proxy | `gw-proxy::LlmGateway` |
| 9090 | Prometheus `/metrics` | `gw-gateway::metrics_catalog` |
| 8080 | Health HTTP — `/livez`, `/readyz`, `/healthz/circuits`, `/healthz/key/{id}`, `/healthz/resilience` | `gw-gateway::health` |

The `/healthz/*` admin routes return `401` until management-token validation lands (T6-A).

## 7. Invariants (`scripts/check-invariants.sh`)

Active lints (all green at HEAD):

| Lint | Meaning |
|------|---------|
| `crate_layering` | Supporting must not depend on Business/Execution; gw-routing / gw-capability / gw-tunnel-client (Business) must not depend on Execution; gw-proxy ↮ gw-task-runtime. |
| `no_billing_crate` | `gw-billing` is permanently retired — must not reappear. |
| `no_proxy_worker_dep` | gw-proxy must not depend on gw-task-runtime. |
| `no_worker_addr_config` | Gateway config/code must not embed worker host/port (ADR-T16). |
| `no_cost_micro` | Rust must not carry `cost_micro` (cost computation lives in Go). |
| `no_billing_records_stream` | Use `usage:records`, not `billing:records`. |
| `no_freezeguard_settle` | `FreezeGuard::release` only — `settle()` must not appear. |
| `no_markup_product_scalar` | Carry `markup_chain` (array), not `markup_product` scalar. |
| `no_legacy_breaker` | `BreakerState` / `BreakerStore` retired (replaced by gw-health). |
| `no_legacy_retry` | `gw-routing/src/retry.rs` deleted in Phase 1c. |
| `no_patch_body` | Adapter trait v2 has no `patch_body` / `BodyPatch`. |

Pending lints (uncomment when target phase lands):

| Lint | Lands at |
|------|----------|
| `no_ir_runtime_struct` | Phase 2 — atomic with `gw-core::ir` → `gw-core::schema` rename and removal of public constructors / `Default` / `Serialize`+`Deserialize`. |
| `no_gateway_addr_config` | Phase 6 — symmetric to `no_worker_addr_config`. |
| `deploy_e2e_matrix` | Phase 6/7 — N-gateway × M-worker topology acceptance. |

## 8. Deliberate behaviours (not bugs)

| Behaviour | Reason | Source |
|-----------|--------|--------|
| Non-stream completions rejected with `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400 | `parse_response` is `unimplemented!()` in every v1 codec; without it Rust would silently bill `cost_micro = 0`. Lifts in v1.1. | `crates/gw-proxy/src/filters/request.rs` non-stream guard |
| `(x-api-key, x-api-key-id)` pair required | A BYOK gateway can't route by key prefix; the pair makes tenant routing explicit. Anthropic SDK clients must supply both. Missing either yields `GW_AUTH_INVALID_APIKEY` (deliberately generic). | `crates/gw-proxy/src/filters/request.rs` auth extraction |
| `trust_xff = false` by default | With it off, the IP blocklist matches the L4 socket peer. Enabling it behind an untrusted L7 hop lets clients forge XFF and bypass the blocklist. Fail-closed. | `config/default.toml [security]`, `crates/gw-proxy/src/filters/early.rs` |
| Distinct 429 codes | `GW_QUOTA_RPM_EXCEEDED` (gateway tripped per-user RPM) vs `GW_UPSTREAM_OVERLOADED` (upstream returned 429) — clients need to distinguish local vs upstream throttling. | `crates/gw-core/src/error/codes.rs` |
| Body filter swaps at EOS only | Cross-protocol rewrites happen in `upstream_request_filter` and must reach the wire in one shot. Pre-EOS chunks are dropped. | `crates/gw-proxy/src/gateway.rs::request_body_filter` |
| HMAC verify decompresses but the codec re-reads raw bytes | T1-D's `verify_body` decompresses internally for the HMAC compare; the plaintext is discarded and `ctx.cached_body` still holds the on-wire (possibly gzipped) bytes. The codecs `serde_json::from_slice(&body_bytes)` directly without a decode step, so a signed + compressed request verifies OK then fails codec parse. Tracked as a follow-up: stash the decompressed body so step 8 reuses it. | `crates/gw-proxy/src/filters/request.rs` step 7b → step 8; `crates/gw-codec/src/{openai,anthropic,gemini}/codec.rs::parse_request` |
| `gw-core::ir` not renamed to `::schema` yet | Phase 1d landed the *scaffolding* parts of S1 v2 without breaking the runtime IR path. The strict A10 form lands atomically in Phase 2 alongside the wire→wire matrix migration. | `crates/gw-core/src/ir.rs`, `no_ir_runtime_struct` lint stays PENDING. |
| Capability field-name / type asymmetry (`ApiKeyEntry.force_capabilities: SmallVec<[String;4]>` vs `VendorKey.forced_capabilities: Vec<String>`) | G13 (T3-C) reads forced caps from two different entities: the resolver pulls *required* caps from `ApiKeyEntry.force_capabilities` + `ApiKeyEntry.match_mode_exclusive` (caller-side requirement), while the candidate filter reads *advertised* caps from `VendorKey.forced_capabilities` + `VendorKey.exclusive_capability_match` (channel-side capability). The two were introduced by different waves (T4-B vs T0-C) and the naming/collection-type drift is left as-is rather than risk a churny rename; reconciliation is a follow-up. | `crates/gw-capability/src/backend.rs`, `crates/gw-routing/src/candidates.rs` |

## 9. Known gaps (deferred)

Closure of the deferred gaps is tracked in [`docs/superpowers/specs/2026-05-21-known-gaps-design.md`](superpowers/specs/2026-05-21-known-gaps-design.md) and [`docs/superpowers/plans/2026-05-21-known-gaps.md`](superpowers/plans/2026-05-21-known-gaps.md). Status (in-flight on `feature/known-gaps-closure`):

- **Landed**: `ApiKeyEntry.expires_at` enforcement (G1, T1-A); `ApiKeyEntry.ip_allowlist` per-key enforcement (G2, T1-B); HMAC body-verify wired at request_filter EOS (G7, T1-D); hybrid `JwtValidator` (JWKS rotation + `iss`/`aud`/`nbf`/leeway) on the client-bearer path (G6, T1-E); `Vendor.timeout_overrides` precedence (G4, T1-C) — `apply_timeout_precedence` in `upstream.rs` composes the ladder hardcoded default < `Vendor.timeout_overrides` < `Pricing` timeouts (Pricing wins as the most-specific per-model rung); `usage:records:dlq` single-fallback stream on flush failure (G11, T2-D) — on primary XADD failure (per-record queueing or batch RTT) the record is wrapped with `dlq_meta` and XADDed to `usage:records:dlq` (`MAXLEN ~ 100_000`); `gw_usage_dlq_enqueued_total{outcome}` tracks DLQ success/error; if DLQ itself fails the record is dropped and counted in `gateway_usage_records_lost_total`. The full three-layer fallback (in-memory + on-disk file + periodic drainer + startup recovery) remains deferred to billing-c-delta; `UserEntry.daily_quota_tokens` enforcement (G3, T2-A) — pre-check in `request_filter` step 7a reads `counter:user:{eid}:daily_tokens:{YYYY-MM-DD}` and rejects `403 GW_QUOTA_DAILY_TOKENS_EXCEEDED` when `consumed + freeze_estimate` would exceed quota (quota=0 means unlimited; Redis failure fails open consistent with the balance check); `logging` fires a best-effort 48 h-TTL `INCRBY` of input+output tokens on the same key. UTC day boundary; per-tenant timezone is a v2 follow-up; `Pricing.model_markup` flows into `MarkupChainSnapshot` (G5, T2-B) — `request_filter` pre-loads `Pricing` after IR parse and passes it into `merge_permissions`, which populates `MarkupChainSnapshot.model_markup` from `Pricing.model_markup` (defaulting to 1.0 when unset). The lazy `pricing_or_load` in `upstream_peer` becomes a no-op pickup of `ctx.pricing`; EntityStore is Moka-memoised so the happy path adds no extra Postgres round-trip. Rust still does not compute cost — the chain is emitted verbatim into `UsageRecord.markup_chain_snapshot` and Go multiplies (`no_cost_micro` and `no_markup_product_scalar` stay green).
- **Landed (cont.)**: Sticky moka cache + shared `StrategySet` (G12, T2-C) — `Sticky` bindings moved from an unbounded `DashMap` to `moka::sync::Cache` with `time_to_live = 30 min` and `max_capacity = 100_000` (LRU on overflow); the per-request `strategy_for(kind)` factory in `upstream.rs` was deleted in favour of `LlmGateway.strategies.pick_for(kind)`, so `Sticky` (and any future stateful strategy) is constructed once and shared. Metrics: `gw_sticky_bind_total`, `gw_sticky_hit_total`, `gw_sticky_evict_total`, `gw_sticky_bindings_active`. ADR-G12-1; `ProtocolCodec::parse_stream_chunk` returns `SmallVec<[StreamEventIR; 1]>` (G9, T3-A) — the trait signature widened from `Option<StreamEventIR>` so a single framed SSE event can decompose into multiple IR events. The common one-event case stays inline (no heap), and Gemini's "combined final event" (`text` + `finishReason` in one frame) now fans out into `[Delta, End]` cleanly — no more silent text loss, no framer-side synthesized End. The `response_body_filter` consumer iterates the `SmallVec` so TTFT / usage accumulators run per IR event, setting up T4-A (cross-protocol response render). Stateful cross-chunk accumulators (thinking signature continuation, tool_call delta coalescing) remain deferred to codec-s3 v2. ADR-G9-1; `fail_to_connect` cross-key retry hook (G10, T3-B) — `LlmGateway` now implements `ProxyHttp::fail_to_connect`: on TCP / handshake failure other than TLS the failing `KeyId` is pushed onto `ctx.retry_blacklist`, `ctx.retry_count` is incremented, the in-flight guard for the failed key is dropped, and `e.set_retry(true)` instructs Pingora to retry. While `retry_count < MAX_CONNECT_RETRIES` (= 2) the next `upstream_peer` call filters the blacklist via the extended `get_candidate_keys(.., retry_blacklist)` and picks a different key. TLS handshake / cert errors (`TLSWantX509Lookup`, `TLSHandshakeFailure`, `TLSHandshakeTimedout`, `InvalidCert`, `HandshakeError`) short-circuit immediately — they're a config defect, not a transient — and stamp `GW_UPSTREAM_TLS_ERROR` on `ctx.upstream_error_code`. Exhausted non-TLS retries stamp `GW_UPSTREAM_CONNECT_FAILED`. Metric `gw_retry_attempt_total{outcome=recovered|exhausted|tls_blocked}`. v1.5 scope is connect-only; response-side (5xx / 429) retry remains deferred to the routing-r-e `FailoverExecutor v2`. ADR-G10-1; `CapabilityResolver` injection into `LlmGateway` (G13, T3-C) — a production `gw_capability::EntityStoreResolveBackend` implements `ResolveBackend` over the shared `EntityStore`: `model_caps` reads `Model.capabilities`, `key_forced_caps` reads `ApiKeyEntry.force_capabilities`, `match_mode_exclusive` reads `ApiKeyEntry.match_mode_exclusive` (all soft-failing to the non-restrictive default on a store miss). Bootstrap constructs the resolver and threads it `Wired → LlmGateway → request_filter`. `request_filter` step 6b collapses (key-forced + model + `x-require-capability` header) into `RequiredCapabilities` and stashes it on `ctx.required_caps` (resolver error is non-fatal → empty set → unconstrained route). `upstream_peer` passes `ctx.required_caps.as_ref()` into the extended `get_candidate_keys(.., required_caps)`, which capability-filters each candidate `VendorKey` via `gw_capability::candidate_matches(required, parse(forced_capabilities), exclusive_capability_match)` — a channel with no advertised caps is non-restrictive; an exclusive channel must match the requirement set exactly. When the capability filter is what emptied the candidate set (a re-run without caps yields candidates) the proxy emits `503 GW_ROUTING_NO_CAPABILITY_MATCH`, distinct from `GW_ROUTING_NO_AVAILABLE_CHANNEL`. Metric `gw_capability_resolve_total{match}`. The T0-C `VendorKeyRow` placeholders in `loader.rs` are replaced with real JSONB reads of the two new fields. ADR-G13-1 / G13-2.
- **Deferred**: cross-protocol *response* body re-render, multi-event SSE buffering, and the response-side (5xx / 429) retry executor on the proxy hot path (deferred to `FailoverExecutor v2`).

The `e2e_full_flow.rs` Pingora+wiremock+reqwest harness is `#[ignore]`d; per-filter coverage that runs without Docker lives in `e2e_failure_modes.rs`. The docker-based black-box harness lands on `feature/e2e-blackbox` (see [`docs/superpowers/specs/2026-05-21-e2e-blackbox-design.md`](superpowers/specs/2026-05-21-e2e-blackbox-design.md)).
