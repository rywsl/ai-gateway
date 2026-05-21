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
| `no_cost_in_dlq` | The G11 DLQ wrapper must not introduce `cost_micro` into `gw-ledger/src/flusher.rs` (cost lives in Go). |
| `no_global_hmac_force` | HMAC signing is per-api-key; a workspace-global force flag (ADR-G7-2 v2 follow-up) must not appear. |
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
| Daily-quota UTC midnight race | The pre-check in `request_filter` reads `counter:user:{eid}:daily_tokens:{day_N}`, but the post-increment in `logging` may run after the request crosses the UTC midnight boundary and land on day N+1's counter. Affects < 0.001 % of traffic; daily quota is best-effort accounting, not security. | `crates/gw-proxy/src/filters/request.rs` daily-quota pre-check, `crates/gw-proxy/src/filters/logging.rs` post-increment |
| Capability field-name / type asymmetry (`ApiKeyEntry.force_capabilities: SmallVec<[String;4]>` vs `VendorKey.forced_capabilities: Vec<String>`) | G13 (T3-C) reads forced caps from two different entities: the resolver pulls *required* caps from `ApiKeyEntry.force_capabilities` + `ApiKeyEntry.match_mode_exclusive` (caller-side requirement), while the candidate filter reads *advertised* caps from `VendorKey.forced_capabilities` + `VendorKey.exclusive_capability_match` (channel-side capability). The two were introduced by different waves (T4-B vs T0-C) and the naming/collection-type drift is left as-is rather than risk a churny rename; reconciliation is a follow-up. | `crates/gw-capability/src/backend.rs`, `crates/gw-routing/src/candidates.rs` |

## 9. Known gaps (closed by `feature/known-gaps-closure`)

The 13 gaps G1-G13 catalogued in [`docs/superpowers/specs/2026-05-21-known-gaps-design.md`](superpowers/specs/2026-05-21-known-gaps-design.md) all landed across Waves 0-4 of [`docs/superpowers/plans/2026-05-21-known-gaps.md`](superpowers/plans/2026-05-21-known-gaps.md). One-line summary:

| Gap | Surface | Wave/task | Commit | What landed |
|-----|---------|-----------|--------|-------------|
| G1 | `ApiKeyEntry.expires_at` enforcement | T1-A | `79aadd7` | Expired key → `401 GW_AUTH_INVALID_APIKEY`. |
| G2 | `ApiKeyEntry.ip_allowlist` per-key | T1-B | `f52724a` | Per-key allowlist re-check after auth resolves scope IDs. |
| G3 | `UserEntry.daily_quota_tokens` | T2-A | `aff1c6b` | Pre-check `counter:user:{eid}:daily_tokens:{YYYY-MM-DD}` (UTC); `403 GW_QUOTA_DAILY_TOKENS_EXCEEDED`; `quota=0` unlimited; Redis fails open; 48 h-TTL post-`INCRBY` in `logging`. |
| G4 | `Vendor.timeout_overrides` precedence | T1-C | `ec87db0` | `apply_timeout_precedence`: default < `Vendor.timeout_overrides` < `Pricing` (most-specific wins). |
| G5 | `Pricing.model_markup` → `MarkupChainSnapshot` | T2-B | `9f4407c` | `request_filter` pre-loads `Pricing`, `merge_permissions` fills `model_markup` (default 1.0); emitted verbatim, Go multiplies. |
| G6 | Hybrid JWT validator (JWKS + iss/aud/nbf) | T1-E | `059acba`, `e08f9f0` | RS256-via-JWKS-kid / HS256 hybrid on client-bearer path; per-cause 401s incl. `GW_AUTH_JWT_EXPIRED`. |
| G7 | HMAC body-verify at request_filter EOS | T1-D | `c727d06`, `2a7cbf6` | `verify_body` at step 7b; 4 reject codes; fail-closed (unverifiable body cannot reach codec parse). |
| G8 | Cross-protocol streaming response re-render | T4-A | `50cd9e7` | `response_body_filter` re-renders each `StreamEventIR` through client codec into `ctx.pending_client_body`; same-protocol stays zero-copy. |
| G9 | `parse_stream_chunk` → `SmallVec<[StreamEventIR; 1]>` | T3-A | `9f3fde5` | Trait widened from `Option`; one-event case inline; Gemini combined final event fans out `[Delta, End]`. |
| G10 | `fail_to_connect` cross-key retry hook | T3-B | `16504a1` | Non-TLS connect failure blacklists `KeyId`, `set_retry(true)` up to `MAX_CONNECT_RETRIES=2`; TLS errors short-circuit `GW_UPSTREAM_TLS_ERROR`. |
| G11 | `usage:records:dlq` single-fallback stream | T2-D | `5d49ef1` | Primary XADD failure → `dlq_meta`-wrapped XADD to `usage:records:dlq` (`MAXLEN ~ 100_000`); DLQ failure → drop + `gateway_usage_records_lost_total`. |
| G12 | Sticky moka cache + shared `StrategySet` | T2-C | `8304097` | Bindings on `moka::sync::Cache` (TTL 30 min, cap 100_000); per-request `strategy_for` replaced by shared `LlmGateway.strategies.pick_for`. |
| G13 | `CapabilityResolver` injection (EntityStore backend) | T3-C | `5a1f9d0` | `EntityStoreResolveBackend` threaded `Wired → LlmGateway → request_filter`; candidate filter applies `candidate_matches`; `503 GW_ROUTING_NO_CAPABILITY_MATCH`. |

### 9.1 v2 follow-ups

- **G6**: dedicated `signature_secret` field on `ApiKeyEntry` (currently reuses `hash` as the HMAC secret — v1.5 compromise).
- **G7**: workspace-global `signature_required` toggle (ADR-G7-2; out of scope for v1.5). Stash the decompressed body so the codec parse step reuses it (the compressed-body parse gap — see §8 Deliberate behaviours).
- **G8**: direct wire-to-wire codegen (codec-s3 N²) replacing the IR round-trip; multi-event SSE buffering across Pingora chunk boundaries; cross-protocol *non-streaming* response body re-render.
- **G9**: stateful cross-chunk accumulators (thinking signature continuation, tool_call delta coalescing) → codec-s3 v2.
- **G10**: response-side (5xx/429) retry executor → `FailoverExecutor v2` (routing-r-e); same-key retry with backoff; per-`ModelGroup` retry config.
- **G11**: three-layer fallback (in-memory + on-disk file + periodic drainer + startup recovery) → billing-c-delta.
- **G3**: per-tenant timezone for the daily-quota day window (v1.5 is UTC-only).
- **G13**: reconcile `ApiKeyEntry.force_capabilities` (SmallVec, singular `force_`) vs `VendorKey.forced_capabilities` (Vec, plural) into one breaking rename.

The `e2e_full_flow.rs` Pingora+wiremock+reqwest harness is `#[ignore]`d; per-filter coverage that runs without Docker lives in `e2e_failure_modes.rs`. The docker-based black-box harness lands on `feature/e2e-blackbox` (see [`docs/superpowers/specs/2026-05-21-e2e-blackbox-design.md`](superpowers/specs/2026-05-21-e2e-blackbox-design.md)).
