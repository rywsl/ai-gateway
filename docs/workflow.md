# Workflow — request lifecycle

> **Sync rule**: if you add, reorder, or remove a Pingora filter, change `ReqCtx` fields, alter the ledger / detailed-log emission, or rewire the error-handler fanout, update this file in the same commit.
>
> Companion: [`architecture.md`](architecture.md) — crates, traits, persistence, lifecycle.

## 1. Startup sequence

`crates/gw-gateway/src/main.rs` installs the jemalloc allocator and calls `gw_gateway::run()`. The runtime split is documented in `crates/gw-gateway/src/lib.rs`:

```
main thread                          bootstrap thread (gw-bootstrap, detached)
─────────────                        ──────────────────────────────────────────
load_config()                        rt = tokio::runtime::Builder::new_multi_thread()
spawn bootstrap thread  ──────────►  rt.block_on(async {
                                       1. kms::init_kek_cache(cfg.kms)
                                       2. RedisCtx::connect(cfg.redis)
                                       3. PgSQL::connect(cfg.pgsql)
                                       4. EntityStore::new(redis, pg, loader)
                                       5. KeyHealthStore, InFlightStore,
                                          RateLimitGuard, CounterStore
                                       6. Ledger::start(redis, channel_capacity,
                                                        batch_size, flush_interval)
                                       7. StreamConsumer::start(redis,
                                            "events:entity" + "events:counter")
                                       8. health::run_health_server(8080)
                                       9. metrics_exporter::install(9090)
                                     });
                                     // Send Wired bundle via sync_channel
                                     pending::<()>().await   // park forever
recv Wired
build pingora::Server
add LlmGateway service on cfg.server.business_port (8000)
health::READY.store(true)
server.run_forever()   // -> !
```

Rationale: `run_forever` consumes the `Server` and never returns. Long-lived tasks (Ledger flusher, StreamConsumer, health HTTP) need a runtime that outlives that call, so they run on the bootstrap runtime. `Ledger::try_send` is synchronous (`tokio mpsc::Sender::try_send`) — Pingora worker threads enqueue records without holding a runtime handle.

## 2. Pingora filter chain

`crates/gw-proxy/src/gateway.rs::LlmGateway` is the `ProxyHttp` impl. Each phase delegates to a module under `crates/gw-proxy/src/filters/`. Short-circuits return `Err(short_circuit(status))` whose root cause is `Custom("gw_short_circuit")` so `suppress_error_log` can mute Pingora's own ERROR line.

| # | Phase | Module | Role |
|---|-------|--------|------|
| 0 | `new_ctx` | `gateway.rs` | Initialise `ReqCtx` (request_id, trace_id, start time, atomic counters). |
| 1 | `early_request_filter` | `filters::early` | Global IP blocklist, global stream cap, protocol detect, debug trigger. |
| 2 | `request_filter` | `filters::request` | Auth, IR parse, 5-tier permission merge, balance/circuit, RPM, hard caps, freeze. |
| 3 | `upstream_peer` | `filters::upstream` | Candidate filter → strategy pick → `HttpPeer`. |
| 4 | `upstream_request_filter` | `filters::upstream` | Codec rewrite → `ctx.cached_body`; vendor auth header injection. |
| 5 | `request_body_filter` | `gateway.rs` | Swap `cached_body` in at EOS; drop pre-EOS chunks. |
| 6 | `response_filter` | `gateway.rs` | Capture upstream status. |
| 7 | `response_body_filter` | `filters::response` | Incremental SSE parse, usage accumulate, TTFT capture. |
| 8 | `logging` | `filters::logging` | Release freeze (C-γ §ADR-Cγ-2), record key-health outcome, emit `UsageRecord` + `DetailedLog`, fanout to `ErrorHandler`. |
| — | `suppress_error_log` | `gateway.rs` | Mute Pingora's ERROR line for `gw_short_circuit` rejects. |

### 2.1 `early_request_filter`

`crates/gw-proxy/src/filters/early.rs`:

1. **IP blocklist (Global scope)** — `client_ip_from_session()` reads either L4 peer or first XFF entry depending on `[security].trust_xff`. Global-scope hit short-circuits; User / ApiKey scope is checked later once auth resolves.
2. **Global concurrent-stream cap** (`GLOBAL_STREAM_CAP = 100_000`). Reads `ratelimit.total_streams` atomic. Over → `GW_QUOTA_CONCURRENT_EXCEEDED` 429.
3. **Protocol detection** via `AnyCodec::detect(path, headers, [])` — branch-free, no-alloc. Stash on `ctx.client_codec`. No match → `GW_REQUEST_PROTOCOL_UNKNOWN` 400.
4. **Debug trigger** — `X-Gw-Debug: 1` upgrades `ctx.debug` so later filters can opt-in to body capture (subject to debug sampling rate).

### 2.2 `request_filter`

`crates/gw-proxy/src/filters/request.rs` — the longest filter. Order matters.

1. **Management-path branch** — `is_management_path(&path)` (e.g. `/admin/*`) sets `ctx.mgmt = true`. v1 rejects with `501` until a real `Authorization: Mgmt <token>` validator lands (T6-A).
2. **Auth extraction** — two mutually exclusive paths:
   - JWT: `Authorization: Bearer <token>` → hybrid dispatch (G6, T1-E). When `AuthConfig.global` is deployed, `LlmGateway.jwt_validator` is `Some(_)` and `validate_sub(bearer)` runs: RS256 + `kid` resolves via the `EntityStoreJwksResolver` (falls back to `AuthConfig.static_pem_rs256` if the resolver returns `None` or the token has no `kid`); HS256 uses `AuthConfig.static_secret_hs256`. `iss`/`aud`/`nbf`/`exp`/leeway are enforced per `AuthConfig`. Per-cause 401: `GW_AUTH_JWT_INVALID_ISS`, `GW_AUTH_JWT_INVALID_AUD`, `GW_AUTH_JWT_NOT_YET_VALID`, `GW_AUTH_JWT_EXPIRED`; signature / format collapse to the legacy `GW_AUTH_INVALID_JWT`. When `AuthConfig.global` is absent, the validator stays `None` and the request falls back to legacy `verify_jwt(bearer, store)` (RS256 via JWKS by `kid`, `exp` only — no regression vs v1).
   - API-key: `x-api-key` + `x-api-key-id` (both required, paired). `verify_apikey` SHA-256s the secret and constant-time compares against `ApiKeyEntry.hash`. `apikey_or_load` resolves the entry to extract `user_id`.
   - Missing both → `GW_AUTH_INVALID_APIKEY` 401 (intentionally generic).
3. **Optional HMAC capture** — if the resolved entry sets `signature_required`, extract `x-signature` + `x-timestamp` into `ctx.signature_received` / `ctx.signature_timestamp`, stash the algo on `ctx.signature_algo`, and clone `ApiKeyEntry.hash` onto `ctx.signature_secret` (v1.5 reuses the api-key hash as the HMAC secret; a dedicated `signature_secret` entity field is a v2 follow-up). The actual verification fires in step 7b after the body is buffered.
4. **User entry** — `store.user_or_load(&user_id)` stashed on `ctx.user`.
5. **Content-Encoding parse** — detect gzip / br so a later signature-verify or audit step sees the right payload.
6. **IP blocklist re-check (User/ApiKey scope)** — `check_blocklist(ip, Some(&user_id), api_key_id, store)` now that scope IDs are resolved.
7. **Body read** — chunked `session.read_request_body()` with 16 MiB cap, frozen onto `ctx.cached_body`.
7b. **HMAC body-verify (G7, T1-D)** — if `ctx.signature_required`, build `BodyVerifyInput` (using `ctx.req_content_encoding` for the Content-Encoding parse, `ctx.signature_secret` for the secret, `ctx.signature_received`/`ctx.signature_timestamp` for the headers) and call `gw_proxy::body_verify::verify_body`. Outcomes from the 5-variant `gw_auth::signature::SignatureCheckResult`:
   - `NotRequired` / `Valid` → continue.
   - `Missing` → 401 `GW_AUTH_SIGNATURE_MISSING`.
   - `TimestampDrift` → 401 `GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`.
   - `Invalid` (HMAC mismatch OR body decompression failure) → 401 `GW_AUTH_SIGNATURE_INVALID`.

   Fail-closed: an unverifiable body cannot reach codec parse. `ctx.cached_body` is *not* replaced by the decompressed plaintext (codec parse paths are still expected to consume the on-wire body — see [`docs/architecture.md`](architecture.md) §Deliberate behaviours for the known compressed-body parse gap, deferred to a follow-up).
8. **Parse to IR** — `codec.parse_request(&body_bytes)` produces `RequestIR` carrying model name, stream flag, estimated input tokens. Stash `ctx.model`, `ctx.is_stream`, `ctx.idempotent = !is_stream`.
9. **Non-stream guard** — `is_stream == false` → `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400. v1 codec `parse_response` is `unimplemented!()`; without it Rust would emit `cost_micro = 0`. Fail-closed until v1.1.
10. **5-tier permission merge** — `merge_permissions(AuthCtx, store, global_rpm)` produces a `MarkupChainSnapshot`. Tier order: global → user-group → user → sub-account → api-key; intersection on lists, min-positive on `rpm`/`tpm`, product on markup. Sets `forced_model_group` + `effective_user_id` (parent on sub-accounts).
11. **Model permission** — `merged.allowed_models.contains(&model_name)`; deny → `GW_REQUEST_MODEL_NOT_FOUND` 403. The full regex / ReDoS-bounded path uses `gw-auth::model_permissions::check_with_budget` (100 ms call budget, per-key disable after 10 timeouts).
12. **Balance check** — `counters.get("user", effective_user_id, "balance_micro")`. `<= 0` → `GW_QUOTA_BALANCE_ZERO` 402. Redis error fails open.
13. **User circuit** — `USER_CIRCUIT.get(&effective_user_id)`. `Open { until, .. }` and `now < until` → 402 short-circuit. The flusher (Phase 5) populates this when balance flips negative.
14. **Rate limit** — `RateLimitGuard::check(&merged)` (governor RPM). If streaming, `check_stream_slot()` returns a RAII ticket on `ctx.stream_slot`.
15. **Hard caps (ADR-024)** — `HardCapCfg { max_input_tokens: 1_000_000, max_cost_micro: 5_000_000_000 }`. `check_input_tokens` and `check_cost(freeze_estimate)` enforce single-request ceilings.
16. **Freeze** — `RequestFreeze::new(effective_user_id, freeze_estimate_micro)` atomically increments `DEDUCT[uid].frozen_micro`. Stashed on `ctx.freeze_guard` as `Box<dyn FreezeGuard>`.

### 2.3 `upstream_peer`

`crates/gw-proxy/src/filters/upstream.rs`:

1. **Pick model group** — first from `merged.forced_model_group`, else first allowed group; load `ModelGroup` (carries vendor key list + strategy).
2. **Resolve pricing** — `pricing_or_load(model)` cached on ctx (used by `CostFirst`, timeouts, and the eventual logging emit).
3. **Candidates** — `get_candidate_keys(group, merged, health, in_flight)` filters by vendor capability (does this key accept `client_codec.kind()`?), key health (not cooldown), per-key in-flight (`PER_KEY_INFLIGHT_CAP = 1024`), and permission channel allow-list. Empty → `GW_ROUTING_NO_AVAILABLE_CHANNEL` 503.
4. **Strategy dispatch** — `strategy_for(group.strategy)` returns one of the 8 strategies; `pick(candidates, &KeyHealthStore, &InFlightStore, &PickCtx)` selects one Arc-cloned `VendorKey`.
5. **Build `HttpPeer`** — load `Vendor`, parse `base_urls[0]` → host/port/scheme_tls. Apply `Pricing` timeouts (connect, read, total, stream_total). Stash `picked_vendor_key`, `picked_vendor`. Install `InFlightGuard` so the counter decrements on `ReqCtx` drop.

### 2.4 `upstream_request_filter`

Same module. The phase has two responsibilities:

1. **Body rewrite** — if `picked.accessible_endpoints.contains(&client_codec.kind())`, take the fast path `same_protocol_passthrough(client_codec, cached_body, &mutations)` which returns a `Cow<[u8]>` (zero-copy when `Mutations::NONE`). Otherwise pick the first accessible endpoint, `client_codec.parse_request` → IR → `upstream_codec.render_request`. Frozen result lands on `ctx.cached_body`.
2. **Header injection** — strip `authorization`, `x-api-key`, `x-api-key-id` from the outbound request; inject `vendor.auth_template.replace("{api_key}", picked.api_key.expose_secret())`; set `Host` from the vendor base URL; emit a W3C `traceparent`; rewrite `Content-Length`.

### 2.5 `request_body_filter` (in `gateway.rs`)

Pingora streams the request body. We discard every chunk **until** `end_of_stream == true`, then swap in `ctx.cached_body` and let the final write go through. This guarantees cross-protocol rewrites reach the wire intact, and lets streaming clients see the request body delivered in one shot.

### 2.6 `response_filter` & `response_body_filter`

- `response_filter` captures the upstream HTTP status on `ctx.upstream_status`.
- `response_body_filter` (`crates/gw-proxy/src/filters/response.rs`) appends each chunk to `ctx.sse_leftover`, scans for `\n\n` event boundaries, and for each complete event calls `codec.parse_stream_chunk(event)`:
  - `Usage` events accumulate `input_tokens`, `output_tokens`, `cache_read_input_tokens` on `ctx.usage_accum`.
  - `Start` / `Delta` events record `ctx.timing.ttft` (one-shot, first delta wins).

The same loop forwards bytes to the client (same-protocol passthrough is the v1 path; cross-protocol *response* re-render is deferred — see [`architecture.md`](architecture.md) §9). Per-chunk SSE conversion for the OpenAI Responses shape lives in `crates/gw-codec/src/openai/responses/{wire_stream_chunk,conversion}.rs`.

### 2.7 Streaming policy

`crates/gw-codec/src/stream_policy.rs` carries the truth table that picks between four bridging modes. v1 ships only `PassThroughStream`:

| Client stream | Upstream sync | Upstream stream | Decision |
|---------------|---------------|-----------------|----------|
| false | true  | — | `PassThroughSync` |
| false | false | true | `AccumulateUpstreamStream` |
| true  | — | true | `PassThroughStream` ← v1 |
| true  | true  | false | `SimulateStreamFromSync` |
| —     | false | false | `PassThroughSync` (caller error) |

`AccumulateUpstreamStream` and `SimulateStreamFromSync` are wired but unused on the proxy hot path until cross-protocol *response* re-render lands.

### 2.8 `logging`

`crates/gw-proxy/src/filters/logging.rs` is the universal finaliser:

1. **Status class** — derive `1xx`/`2xx`/`3xx`/`4xx`/`5xx`/`other`/`err`/`early` from `ctx.upstream_status` or from the pingora `Error` etype. Emit `gateway_requests_total{status_class}`.
2. **Release freeze** — `ctx.freeze_guard.release()`. The guard's `Drop` is the safety net for cancellation paths; the explicit release ensures idempotency and surfaces the canonical metric.
3. **Key-health record** — `KeyHealthStore::record(key_id, api_format, OutcomeKind)`. Drives cooldown / circuit state in `gw-health`.
4. **Build `UsageRecord`** — needs all four of `auth_user_id`, `effective_user_id`, `model`, `picked_vendor_key`. Carries timestamps, request/trace IDs, `client_protocol`, `upstream_protocol`, `status` (`Ok` / `Error`), `usage` (from the accumulator), `usage_source` (`Upstream` if `output_tokens > 0` else `Estimated`), `freeze_amount_micro`, and the `markup_chain_snapshot`. `ledger.try_send(rec)` is sync, non-blocking — drops on full queue and increments a drop metric.
5. **DetailedLog** — `crates/gw-ledger::log_sink::RedisStreamSink` XADDs to `usage:detailed` with `MAXLEN` trim. Body preview inclusion is gated by `ctx.debug` flags and the config sampling rate. Three-level `LogVerbosity` redaction sits in `crates/gw-ledger/src/detailed_stream.rs`.
6. **TPM metric** — `gateway_tpm_consumed_total{user_id}` += `input + output`.
7. **ErrorHandler fanout** — if `ctx.error_outcome` is set (populated when a filter classified an error), construct `ErrorHandler` and `fanout(&HandlerCtx, &ClassifierOutcome)` to its 6 listeners (in fixed order, no I/O):
   1. `KeyHealthListener` — apply `CooldownDirective`, increment consecutive-failure counter.
   2. `RateLimitFeedbackListener` — translate upstream 429 hints into local guard state.
   3. `OAuthListener` — handle `OAuthSignal::RefreshFailed`, `AccountBlocked`.
   4. `CacheInvalidateListener` — invalidate cache entries (e.g. model-mapping on 404).
   5. `MetricsListener` — emit per-class error counters.
   6. `LedgerListener` — emit an error record to the ledger for audit.

### 2.9 `suppress_error_log`

`gateway.rs` overrides Pingora's default error logger. We mute *only* errors whose outer is `ErrorType::HTTPStatus(_)` **and** root cause is `Custom("gw_short_circuit")`. Genuine failures (connect refused, timeout, panic, etc.) still produce the standard ERROR line.

## 3. Error orchestration

`crates/gw-core/src/error/`:

- **`ErrorClass`** — `AuthDenied`, `QuotaDenied`, `RateLimitedSelf`, `RateLimitedUpstream(RateLimitKind)`, `AccountBlocked(AccountBlockReason)`, `UpstreamTransient`, `ProtocolError`, `ThinkingSignatureMismatch`, `ClientAbort`, `Internal`.
- **`AccountBlockReason`** — `Status401AccountDeactivated`, `Status401Unauthorized`, `Status402PaymentRequired`, `Status403AccountSuspended`, `Status400OrganizationDisabled`, `OAuthRefreshFailed`, `AdminForce`.
- **`Retryability`** — `No`, `SameKey`, `CrossKey`, `CrossVendor`.
- **`ClassifierOutcome`** — `class`, `retryable`, optional `cooldown: CooldownDirective`, optional `rate_limit_hint: RateLimitHint`, optional `oauth_signal: OAuthSignal`, optional `passthrough_body: PassthroughBody`.

`DefaultClassifier` (`classify_default.rs`) maps HTTP status to `ErrorClass`. Vendor `UpstreamErrorAnalyzer` impls (`gw-adapter::analyzer`) layer on `enhanced_body`, vendor-specific `OAuthSignal`, and `CooldownDirective` (e.g. parse `Retry-After`).

`error_emit::send_error(session, codec, &ErrorIR)` renders the envelope through the **client's** codec (so OpenAI clients get OpenAI-shaped errors, etc.), sets `Content-Type: application/json`, propagates `X-Request-Id` and `Retry-After`, writes header + body in one shot. The caller returns `short_circuit(status)`.

## 4. Cache invalidation

`crates/gw-store/src/stream_consumer.rs` XREADGROUPs `events:entity` (block 200 ms, count 64) on a consumer group anchored at `$` so a fresh pod ignores backfill — the cold-load chain covers the gap. Each event carries `{ns, id}`; `apply()` dispatches to the right `EntityStore::invalidate_*`:

- `user`, `apikey`, `user_group`, `vendor`, `vendor_key`, `model_group`, `pricing`, `jwks`, `ipblock` → the corresponding Moka cache eviction.
- `gemini_file_mapping`, `model_mapping` → file-mapping / model-alias cache eviction (entity loaders pending — T6-E + A-T2).

`events:counter` is consumed and ACKed in v1 but its payload is observability-only until Phase 5 wires balance echo-back.

## 5. Tunnel transport (feature `tunnel`)

`crates/gw-tunnel-client/` is compiled when `gw-gateway` is built with `--features tunnel`. It is **not** on the v1 hot path — it's a peer-to-peer transport between the gateway and aether-hub remote nodes.

- **Frame** (`frame.rs`) — magic `0xAE 0x21`, version `1`. 14-byte header: `magic | version | msg_type | flags(2) | stream_id(4) | length(4) | payload`. Max payload 4 MiB. 8 msg types: `Open`, `Data`, `Close`, `Ping`, `Pong`, `Error`, `ControlAuth`, `ControlNodeInfo`.
- **Transport** (`transport.rs`) — `tokio-tungstenite` WebSocket with heartbeat + reconnect backoff.
- **DoH resolver** (`safe_dns.rs`) + target-filter whitelist (`target_filter.rs`) — block SSRF / split-horizon attacks against the tunnel exit.
- **Pool** (`pool.rs`) — `arc-swap` for atomic config reload.

## 6. Observability surfaces

| Surface | Sink | Source |
|---------|------|--------|
| `usage:records` Redis stream | `gw-ledger::flusher` (XADD batched, configurable batch / interval) | `filters::logging` builds `UsageRecord`, calls `ledger.try_send` |
| `usage:detailed` Redis stream | `gw-ledger::log_sink::RedisStreamSink` (XADD MAXLEN) | `filters::logging` builds `DetailedLogRecord` subject to debug sampling + `LogVerbosity` |
| Prometheus `/metrics` | `metrics-exporter-prometheus` | `gw-gateway::metrics_catalog` defines 19 metric families (e.g. `gw_codec_convert_total`, `gw_freeze_acquire/release_total`, `gw_redos_timeout_total`, `gw_kms_state`, `gw_oauth_refresh_total`, `gw_capability_resolve_total`, `gw_resilience_*`, `gw_detailed_log_oversized_total`) |
| Health `/livez` | always 200 | — |
| Health `/readyz` | 200 once `READY=true` (flipped after `Wired` handoff) | `gw-gateway::health` |
| Health `/healthz/circuits`, `/healthz/key/{id}`, `/healthz/resilience` | 401 in v1 (management-token validation lands T6-A) | `gw-gateway::health` |

`FreezeGuard::release` (in logging) is the *only* point where Rust mutates `frozen_micro` downward. Go (the billing service) writes `deducted_micro` / `balance_micro` based on `usage:records`. Rust never computes cost — enforced by the `no_cost_micro` invariant.
