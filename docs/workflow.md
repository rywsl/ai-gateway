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
                                       4. AdaptiveRpmManager + tick_decay loop
                                       5. EntityStore::new(redis, pg, loader)
                                          with vendor_key → adaptive_rpm.evict hook
                                       6. KeyHealthStore, InFlightStore,
                                          RateLimitGuard, CounterStore
                                       7. Ledger::start(redis, channel_capacity,
                                                         batch_size, flush_interval)
                                       8. StreamConsumer::start(redis,
                                             "events:entity" + "events:counter")
                                       9. health::run_health_server(8080)
                                      10. metrics_exporter::install(9090)
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

### 1.1 Shutdown sequence

`crates/gw-runtime/src/shutdown.rs::ShutdownCoordinator` drives a two-phase drain on SIGTERM / SIGINT. Phases:

| Phase | Name | Duration | Behaviour |
|-------|------|----------|-----------|
| 0 | Running | indefinite | Normal operation. `/readyz` → 200, `/livez` → 200. |
| 1 | AcceptStop | `phase1_accept_stop_ms` (default 5 s) | New connections are no longer accepted. Pingora's listener is stopped. Existing in-flight requests continue. `/readyz` → 503, `/livez` → 200. |
| 2 | TaskDrain | `phase2_task_drain_ms` (default 25 s) | Supervisor tasks receive `ShutdownToken::cancel()`. The billing flusher and DLQ drainer flush remaining records. `BillingBacklog` is serialized to `backlog_shutdown_<ts>.jsonl` in `spill_io.root_dir`. `/readyz` → 503, `/livez` → 200. |
| — | Stopped | hard_deadline_ms (default 35 s) | Process exits. If `BillingBacklog` dump fails, exit code is 2 (data-at-risk). If dump succeeds, exit code is 0. |

Key design decisions:

- **`/livez` stays 200 during drain** so k8s does not restart the Pod and discard the in-memory `BillingBacklog`. Only `/readyz` signals unready.
- **Double SIGTERM** triggers `abort_all` — immediate process termination without graceful drain (emergency path).
- **BacklogRecovery on bootstrap**: on the next start, `BacklogRecovery` reads `backlog_shutdown_*.jsonl` files from `spill_io.root_dir` and re-enqueues records into `usage:records`. Corrupt files are quarantined to `<spill_root>/corrupt/`.

## 2. Pingora filter chain

`crates/gw-proxy/src/gateway.rs::LlmGateway` is the `ProxyHttp` impl. Each phase delegates to a module under `crates/gw-proxy/src/filters/`. Short-circuits return `Err(short_circuit(status))` whose root cause is `Custom("gw_short_circuit")` so `suppress_error_log` can mute Pingora's own ERROR line.

| # | Phase | Module | Role |
|---|-------|--------|------|
| 0 | `new_ctx` | `gateway.rs` | Initialise `ReqCtx` (request_id, trace_id, start time, atomic counters). |
| 1 | `early_request_filter` | `filters::early` | BackpressureGate admit check, global IP filter, global stream cap, protocol detect, debug trigger. |
| 2 | `request_filter` | `filters::request` | Auth, IR parse, 5-tier permission merge, balance/circuit, RPM, hard caps, freeze. |
| 3 | `upstream_peer` | `filters::upstream` | Candidate filter → strategy pick → `HttpPeer`. |
| 4 | `upstream_request_filter` | `filters::upstream` | Codec rewrite → `ctx.body.set_outbound(...)`; vendor auth header injection. |
| 5 | `request_body_filter` | `gateway.rs` | Write `ctx.body.outbound_or_raw()` upstream at EOS; drop pre-EOS chunks. |
| 6 | `response_filter` | `gateway.rs` | Capture upstream status; trigger response-side failover for retryable status codes (configurable via `[error.classification].retryable_status_codes`) before body bytes start and while `[error.classification].failover_total_budget_ms` has not expired; classify upstream errors (status ≥ 400) via injected `Arc<dyn ErrorClassifier>` (`ConfigurableClassifier` from config) and store `ctx.error_outcome`; activate error passthrough when `[error.passthrough].enabled = true`. |
| 7 | `response_body_filter` | `filters::response` | Mark body-started, incremental SSE parse, usage accumulate, TTFT capture. With `proxy.archive.storage = "memory"` the Pingora sync hook runs the CPU-only memory archive path directly; `block_in_place` is reserved for explicit `disk_spill` archive mode or reading already-spilled error-passthrough bytes. Sensitive-filter transit surrogates are reversed before downstream bytes are forwarded and accumulated for archive emission. When `ctx.error_passthrough_active` is set and the accumulated body is valid JSON (non-HTML), the final chunk is replaced with `render_error_to_client` output — a structured protocol-specific error envelope. |
| 8 | `logging` | `filters::logging` | Release freeze (C-γ §ADR-Cγ-2), record key-health outcome, persist quota snapshots when present, emit `UsageRecord` + `DetailedArchive`, fanout to `ErrorHandler`. |
| — | `fail_to_connect` | `gateway.rs` | G10 retry hook (between `upstream_peer` and `upstream_request_filter`). Non-TLS connect failure → push failed `KeyId` onto `ctx.retry_blacklist`, drop in-flight guard, bump `ctx.retry_count`, `e.set_retry(true)` until `MAX_CONNECT_RETRIES` (=2); Pingora then re-enters `upstream_peer` which filters the blacklist via `get_candidate_keys`. TLS errors stamp `GW_UPSTREAM_TLS_ERROR` on `ctx.upstream_error_code` and propagate (config defect, not transient); exhausted non-TLS retries stamp `GW_UPSTREAM_CONNECT_FAILED`. Metric `gw_retry_attempt_total{outcome=recovered\|exhausted\|tls_blocked}`. |
| — | `suppress_error_log` | `gateway.rs` | Mute Pingora's ERROR line for `gw_short_circuit` rejects. |

### 2.1 `early_request_filter`

`crates/gw-proxy/src/filters/early.rs`:

0. **BackpressureGate admit check** — before any other work, `BackpressureGate::admit()` is consulted. If the `SystemHealthMonitor` reports any force-majeure hook at unhealthy (memory / FD / disk / Redis-OOM at 99% threshold, with hysteresis), `admit()` returns `Admit::Reject` and the filter short-circuits with 503 + `Retry-After: 2` + `GW_BACKPRESSURE_REJECTED`. The 503 body is deliberately opaque (no hint of which hook tripped). `Admit::Degraded` lets the request through with a `gw_backpressure_degraded_total` tick; `Admit::Ok` is the normal path.
1. **Unified IP filter (Global scope)** — `client_ip_from_session()` reads either L4 peer or first XFF entry depending on `[security].trust_xff`, then `check_ip(ip, None, None, ...)` applies deny-only blocklist → allowlist (when `[ip_filter].allowlist_mode = "enforce"`) → per-IP RPM. This is the first reject gate in the filter chain: global-scope deny / allowlist / IP-RPM failures short-circuit before auth, while User / ApiKey scope is intentionally checked later once auth resolves. Metrics emitted from this path include `gw_ip_blocklist_hit_total`, `gw_ip_allowlist_check_total`, `gw_ip_ratelimit_check_total`, and the snapshot gauges `gw_ip_blocklist_entries` / `gw_ip_allowlist_entries`.
2. **Global concurrent-stream cap** (`GLOBAL_STREAM_CAP = 100_000`). Reads `ratelimit.total_streams` atomic. Over → `GW_QUOTA_CONCURRENT_EXCEEDED` 429.
3. **Protocol detection** first checks `detect_cli(path, headers)` for CLI variants, stashing the fine-grained `ApiShape` on `ctx.client_shape` while keeping `ctx.client_codec` on the base protocol codec for error envelopes. Non-CLI requests fall back to `AnyCodec::detect(path, headers, [])` and store the standard shape. No match → `GW_REQUEST_PROTOCOL_UNKNOWN` 400.
4. **Debug trigger** — `X-Gw-Debug: 1` upgrades `ctx.debug` so later filters can opt-in to body capture (subject to debug sampling rate).

### 2.2 `request_filter`

`crates/gw-proxy/src/filters/request.rs` — the longest filter. Order matters.

0. **Task-shape detection** — when `cfg.task.enabled`, `detect_task_shape(method, path, body)` inspects the request for task endpoint patterns: `POST /v1/tasks` (Submit), `GET /v1/tasks/:id` (Load), `GET /v1/tasks/:id/watch` (Watch), `POST /v1/tasks/:id/cancel` (Cancel). A match sets `ctx.is_task = true` and `ctx.task_action` to the corresponding `TaskAction` variant. This runs before auth so the `is_task` flag is available to all subsequent steps. When `cfg.task.enabled = false` (the default), the detection is inert and no task routes are active.
1. **Management-path branch** — `is_management_path(&path)` (e.g. `/admin/*`) runs the mgmt verify chain (T3-A/B/C, B2):
   1. `parse_mgmt_authorization` extracts `Authorization: Mgmt mt_<id>.<secret>`; missing or malformed → 401 `GW_AUTH_MGMT_TOKEN_MISSING`.
   2. `split_mgmt_token_payload` separates the `mt_<id>` and `<secret>` halves; bad format → 401 `GW_AUTH_MGMT_TOKEN_INVALID`.
   3. `EntityStore::mgmt_token_or_load(token_id)` resolves the `ManagementTokenEntry` from Moka L1 → Redis L1 → PgSQL cold load. Miss → 401 `GW_AUTH_MGMT_TOKEN_INVALID` (same code as hash mismatch, by design — don't leak existence).
   4. `RedisJwtBlacklist::check_mgmt(token_id, FailClosed)` checks `mgmt_token:blacklist:<token_id>`; hit *or* Redis outage → 401 `GW_AUTH_MGMT_TOKEN_REVOKED`.
   5. `verify_mgmt_token_secret(&entry, secret, MgmtScope::AdminRead)` does the constant-time SHA-256 compare + status/expiry/scope checks. Outcomes map to `GW_AUTH_MGMT_TOKEN_{INVALID, EXPIRED, DISABLED, SCOPE_DENIED}` (the first three 401, scope 403). Verified tokens populate `ctx.mgmt_user_id` and `ctx.mgmt_scope`.
   6. A successful verify returns to Pingora's upstream phases without entering the LLM business path. `upstream_peer` routes to `[admin].upstream_url` (the Go control plane), and `upstream_request_filter` preserves the `Authorization` header while adding `X-Gateway-Mgmt-User-Id` and `X-Gateway-Mgmt-Scope`.
2. **Auth extraction** — two mutually exclusive paths:
   - JWT: `Authorization: Bearer <token>` → hybrid dispatch (G6, T1-E). When `AuthConfig.global` is deployed, `LlmGateway.jwt_validator` is `Some(_)` and `validate_sub(bearer)` runs: RS256 + `kid` resolves via the `EntityStoreJwksResolver` (falls back to `AuthConfig.static_pem_rs256` if the resolver returns `None` or the token has no `kid`); HS256 uses `AuthConfig.static_secret_hs256`. `iss`/`aud`/`nbf`/`exp`/leeway are enforced per `AuthConfig`. Per-cause 401: `GW_AUTH_JWT_INVALID_ISS`, `GW_AUTH_JWT_INVALID_AUD`, `GW_AUTH_JWT_NOT_YET_VALID`, `GW_AUTH_JWT_EXPIRED`; signature / format collapse to the legacy `GW_AUTH_INVALID_JWT`. When `AuthConfig.global` is absent, the validator stays `None` and the request falls back to legacy `verify_jwt(bearer, store)` (RS256 via JWKS by `kid`, `exp` only — no regression vs v1). **Revocation (audit 2026-05-30 §3.1):** `validate_sub`/`verify_jwt` also return the token's `jti`; if present, it is checked against the `jwt:blacklist:` namespace (FailClosed) → `GW_AUTH_JWT_REVOKED` 401. A token with no `jti` is non-revocable (the blacklist is `jti`-keyed) and passes.
   - API-key: `x-api-key` + `x-api-key-id` (both required, paired). `verify_apikey` SHA-256s the secret and constant-time compares against `ApiKeyEntry.hash`. `apikey_or_load` resolves the entry to extract `user_id`. **Status gate (audit 2026-05-30 §1.1):** `check_api_key_status` rejects `ApiKeyStatus::Disabled` → `GW_AUTH_KEY_DISABLED` 401 (before expiry), and after the user loads `check_user_status` rejects `UserStatus::Banned` → `GW_AUTH_USER_BANNED` 403. The loader filters only `deleted_at`, so a status-changed row loads normally; these gates make disable/ban enforceable in-band. `merge_permissions` also rejects a banned parent (sub-account owner).
   - Missing both → `GW_AUTH_INVALID_APIKEY` 401 (intentionally generic).
3. **Optional HMAC capture** — if the resolved entry sets `signature_required`, extract `x-signature` + `x-timestamp` into `ctx.signature_received` / `ctx.signature_timestamp`, stash the algo on `ctx.signature_algo`, and clone `ApiKeyEntry.signature_secret` onto `ctx.signature_secret`. The actual verification fires in step 7b after the body is buffered. **Fail-closed (audit 2026-05-30 §1.6):** if signing is required but the secret is empty/NULL, `verify_required` returns `SecretUnavailable` → `GW_AUTH_SIGNATURE_NOT_CONFIGURED` 500 (an empty HMAC key is forgeable; a server misconfig must not fall open).
4. **User entry** — `store.user_or_load(&user_id)` stashed on `ctx.user`.
5. **Content-Encoding parse** — detect gzip / br so a later signature-verify or audit step sees the right payload.
6. **Unified IP filter re-check (User/ApiKey scope)** — `check_ip(ip, Some(&user_id), api_key_id, ...)` now that scope IDs are resolved. This is the second and final IP-filter position in the chain: it catches scoped deny / allowlist rows after auth and reuses the same per-IP RPM guard, preserving the same order of blocklist → allowlist → per-IP RPM in both passes.
7. **Body read** — chunked `session.read_request_body()` with 16 MiB cap, frozen onto `ctx.body.raw()`.
7b. **HMAC body-verify (G7, T1-D)** — if `ctx.signature_required`, build `BodyVerifyInput` (using `ctx.req_content_encoding` for the Content-Encoding parse, `ctx.signature_secret` for the secret, `ctx.signature_received`/`ctx.signature_timestamp` for the headers) and call `gw_proxy::body_verify::verify_body`. Internally decompresses gzip/brotli before HMAC comparison. Outcomes from the 5-variant `gw_auth::signature::SignatureCheckResult`:
   - `NotRequired` / `Valid` → continue.
   - `Missing` → 401 `GW_AUTH_SIGNATURE_MISSING`.
   - `TimestampDrift` → 401 `GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`.
   - `Invalid` (HMAC mismatch OR body decompression failure) → 401 `GW_AUTH_SIGNATURE_INVALID`.

   Fail-closed: an unverifiable body cannot reach codec parse. HMAC verify calls `ctx.body.prefill_decoded(plaintext)` so the decompressed bytes are cached in `RequestBodyView` and reused by codec parse in step 8 without a second decompress pass.
8. **Request shape extraction** — standard requests call `codec.parse_request(&body_bytes)` to produce `RequestIR` carrying model name, stream flag, and estimated input tokens. CLI requests do not enter IR: `extract_cli_request_hints(ctx.client_shape, path, body)` shallow-reads model / stream from the raw body or path, leaves `ctx.body` unchanged, and uses a conservative prompt estimate. Both branches stash `ctx.model`, `ctx.is_stream`, `ctx.idempotent = !is_stream` before permission, quota, RPM, hard-cap, and freeze gates.

### Step 8.5 — Credit lease (protected mode)

When `[protection].enabled = true` and the user has a protected lease entry from
`events:counter ns=protected_lease` (`mode=protected`):

1. Resolve `max_output_tokens` (request `max_tokens` → `default_output_ceiling_tokens` from config).
2. Read the lease's `coef_table_version` and call `estimate_request_credits` to compute the
   reserved credits for this request (input + max_output, 2-dim).
3. `LeaseGuard::try_acquire`:
   - `Expired`, `Insufficient`, or `NoLease` (warmup/race) → route through the read-only `OverflowGate` balance fallback before any legacy soft-defense path.
   - Overflow balance available → admit the request without a `LeaseGuard`; later billing still emits normal usage.
   - Overflow balance insufficient → 402 `GW_QUOTA_BALANCE_INSUFFICIENT`.
   - Overflow Redis/PG fallback unavailable → 503 `GW_QUOTA_BALANCE_CACHE_UNAVAILABLE`.
   - `Ok(guard)` → store `lease_guard` on `ReqCtx` and **skip step 16** `RequestFreeze`.

If `estimate_request_credits` fails (no coefs for the lease version), the request is
fail-closed-rejected with `GW_QUOTA_COEF_TABLE_NOT_READY`.

Credit lease activation starts from an admitted normal billable Rust request. Cold funded users may pass the Redis counter gate through the read-only `OverflowGate` balance fallback, which trusts only active non-deleted wallet rows, then the active-registration worker marks the user for Go renewal. Balance/fallback rejections, task endpoints, Gemini Files endpoints, and runs with protection disabled do not register active users.

Spec: [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md).

9. **Cross-protocol non-stream** — cross-protocol non-stream requests now round-trip end-to-end: `upstream_peer` lets the request through, the upstream body is accumulated in `response_body_filter` (§2.6), `parse_response` produces the IR, and `CrossRender` re-renders into `ctx.pending_client_body` (`gw_codec_cross_render_nonstream_total{from,to}`). The pre-S-codec-v2 `503 GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` short-circuit is gone. Same-protocol non-stream stays zero-copy on the response side (body is forwarded as-is; only `parse_response` runs to populate `ctx.usage`).
10. **5-tier permission merge** — `merge_permissions(AuthCtx, store, global_rpm)` produces a `MarkupChainSnapshot`. Tier order: global → user-group → user → sub-account → api-key; intersection on lists, min-positive on `rpm`/`tpm`, product on markup. Sets `forced_model_group` + `effective_user_id` (parent on sub-accounts).
11. **Model permission** — `merged.allowed_models.contains(&model_name)`; deny → `GW_REQUEST_MODEL_NOT_FOUND` 403. When the resolved `ApiKeyEntry` carries a `model_whitelist_regex`, `check_model_whitelist_regex()` runs after the static list check: the pattern is compiled into a `ModelPermission` via `ModelPermission::new(pattern, PermissionEffect::Allow)`, then evaluated with `check_with_budget(model_name)` under a 100 ms per-call ReDoS budget (`BUDGET_MS=100`). Outcomes: `None` regex → skip (static list governs). Compile error → `GW_REQUEST_MODEL_NOT_FOUND` 403. Match → allow. No match → `GW_REQUEST_MODEL_NOT_FOUND` 403. Budget timeout → counted by `RedosTracker` (per-key `OnceLock` singleton); after `TIMEOUT_THRESHOLD=10` consecutive timeouts the key's regex is auto-disabled and all subsequent requests are denied with 403 (fail-closed, not silently skipped).
11b. **Capability resolve (G13, ADR-G13-2)** — when `LlmGateway.capability_resolver` is wired (always, in production bootstrap), `CapabilityResolver::resolve` collapses `ApiKeyEntry.force_capabilities` (key-forced) + `Model.capabilities` (model) + the comma-split `x-require-capability` header (header-require) into a single `RequiredCapabilities` set, with `MatchMode::Exclusive` when `ApiKeyEntry.match_mode_exclusive` is set. The result is stashed on `ctx.required_caps`. The backend (`EntityStoreResolveBackend`) reads all three inputs from the `EntityStore`, soft-failing to the non-restrictive default on a store miss; a resolver error is non-fatal (empty set → unconstrained route). Metric `gw_capability_resolve_total{match}`. Consumed in §2.3 `upstream_peer` candidate filtering.
12. **Balance check** — read `counter:user:<uid>:base_micro`, `deducted_micro`, and `frozen_micro`, then compute `available = base - deducted - frozen`. `<= 0` → `GW_QUOTA_BALANCE_INSUFFICIENT` 402.
12b. **Daily token quota (G3, ADR-G3-2)** — when `user.daily_quota_tokens > 0`, GET `counter:user:{effective_user_id}:daily_tokens:{YYYY-MM-DD}` (UTC) via `CounterStore::get_str_key`, then `check_daily_quota(consumed, estimated_tokens, quota)` rejects with `403 GW_QUOTA_DAILY_TOKENS_EXCEEDED` when `consumed + estimated_tokens > quota`. `estimated_tokens` is input plus the request/default output cap from `EstimatePricing` when available, never the micro-unit freeze estimate. `quota == 0` is unlimited and short-circuits before the Redis read. The post-increment of actual `input + output` tokens with a 48 h TTL lives in §2.8 `logging`.
13. **User circuit** — `USER_CIRCUIT.get(&effective_user_id)`. `Open { until, .. }` and `now < until` → 402 short-circuit. The flusher (Phase 5) populates this when balance flips negative.
14. **Rate limit** — `RateLimitGuard::check(&merged)` (governor RPM, per-user bucket). If streaming, `check_stream_slot()` returns a RAII ticket on `ctx.stream_slot`. The vendor key is not picked until `upstream_peer`, so per-vendor-key limits are applied there: `AdaptiveRpmManager::try_admit` diverts over-ceiling learned keys, then `RateLimitGuard::check_key(picked.key_id, picked.rpm_limit)` applies the hard local key RPM gate and can return `429 GW_QUOTA_RPM_KEY_EXCEEDED`.
15. **Hard caps (ADR-024)** — `HardCapCfg` loaded from `[auth]` config (`hard_cap_max_input_tokens`, default 1,000,000; `hard_cap_max_cost_micro`, default 5,000,000,000). `check_input_tokens` and `check_cost(freeze_estimate)` enforce single-request ceilings.
16. **Freeze** — `RequestFreeze::new(effective_user_id, freeze_estimate_micro)` atomically increments `DEDUCT[uid].frozen_micro` and enqueues scalar Redis `INCRBY counter:user:<uid>:frozen_micro`. Stashed on `ctx.freeze_guard` as `Box<dyn FreezeGuard>`.
17. **Task endpoint short-circuit** — when `ctx.is_task`, the request exits the normal proxy path and dispatches to `filters::task` based on `ctx.task_action`:
    - **Submit** — `gw_billing::calculate_freeze_amount(kind, cfg)` computes the freeze estimate. If `TaskContract::freeze_task(user_id, task_id, freeze, available_balance_micro)` reports insufficiency, return `402 GW_TASK_FREEZE_FAILED`; otherwise `TaskContract::submit(record)` writes `entity:task:<id>` + XADD `tasks:queue`. Submit rollback calls `release_task_freeze`. Return `201 {task_id, status, expires_at}`. Metric: `gw_task_gw_submit_total`, `gw_task_gw_freeze_total{result}`.
    - **Load** — `TaskContract::load(task_id)`. `None` returns `404 GW_TASK_NOT_FOUND`. Success returns the serialized `TaskRecord`.
    - **Watch** — `TaskContract::watch(task_id)`. Returns a single SSE `status` event with the current task state.
    - **Cancel** — `TaskContract::settle(task_id, Cancelled, actual_micro=0)`. Returns the settled `TaskRecord`.
    The short-circuit bypasses `upstream_peer`, `upstream_request_filter`, and all response filters. Task freeze is Redis-backed through `TaskContract`, not `ctx.freeze_guard`, so there is no logging-phase request-freeze release for task submits.
18. **Gemini Files short-circuit** — `POST /v1beta/files`, `GET /v1beta/files`, `GET /v1beta/files/{file_id}`, and `DELETE /v1beta/files/{file_id}` are detected before management routing but continue through normal business auth, scoped IP checks, permission merge, balance, RPM, hard caps, and freeze. After those gates, `filters::gemini_files` selects a Gemini-capable vendor key through the same `ModelGroup` + `get_candidate_keys` + `StrategySet` routing path using synthetic model `gemini-files`, then `HttpGeminiFilesGateway` proxies directly to Gemini Files (`/upload/v1beta/files` for upload, `/v1beta/files...` for list/get/delete). Upload forwards the captured multipart bytes without transforming file content; successful upload records `gemini_file_mapping.file_uri -> vendor_key_id`, get/delete look up that mapping to avoid cross-vendor file migration, delete removes the mapping only after upstream success, and missing mappings return a gateway 404. This path bypasses `upstream_peer`, `upstream_request_filter`, and response filters just like task endpoints.

### 2.3 `upstream_peer`

`crates/gw-proxy/src/filters/upstream.rs`:

1. **Pick model group** — first from `merged.forced_model_group`, else first allowed group; load `ModelGroup` (carries vendor key list + strategy).
2. **Resolve pricing** — `pricing_or_load(model)` cached on ctx (used by `CostFirst`, timeouts, and the eventual logging emit).
3. **Candidates** — `get_candidate_keys(group, merged, health, in_flight, .., ctx.required_caps.as_ref())` filters by vendor capability (does this key accept `client_codec.kind()`?), key health (not cooldown), per-key in-flight (`PER_KEY_INFLIGHT_CAP = 1024`), permission channel allow-list, the G10 retry blacklist, and — when `ctx.required_caps` is set (G13) — `gw_capability::candidate_matches(required, parse(VendorKey.forced_capabilities), VendorKey.exclusive_capability_match)`: a channel with no advertised caps is non-restrictive; an exclusive channel must match the requirement set exactly. CLI requests then retain only keys whose `accessible_endpoints` contain `ctx.client_shape.protocol_kind()`; an empty set returns `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` 503 and increments `gw_cli_cross_protocol_denied_total`. Other empty results → `GW_ROUTING_NO_AVAILABLE_CHANNEL` 503, unless a re-run of the filter *without* the capability constraint yields candidates — i.e. the capability filter is what emptied the set — in which case the distinct `GW_ROUTING_NO_CAPABILITY_MATCH` 503 is emitted. The first successful candidate list is cached on `ctx.picked_candidates` so response-side failover can walk the original list without rerunning the original health / cooldown query.
4. **Strategy dispatch** — `strategy_for(group.strategy)` returns one of the 8 strategies; `pick(candidates, &KeyHealthStore, &InFlightStore, &PickCtx)` selects one Arc-cloned `VendorKey`. `PickCtx.message_hash` is populated from a SHA-256 of `ctx.body.raw()` bytes (first 8 bytes as `u64` LE via `body_hash_u64()`); an empty body produces `None`. `CacheAffinity` uses `model_name || ":" || message_hash` to pin content-identical requests to the same key. During response-side failover, `FailoverExecutor` preselects `ctx.picked_vendor_key`; the retry pass through `pick_peer` uses that key directly and only rebuilds the peer/vendor/in-flight state.

If `LlmGateway::tunnel = Some(_)`, `TunnelStack::resolve(provider, key, pref)` runs before HttpPeer construction. `TunnelResolution::Direct` keeps the existing path. `TunnelResolution::Use(node_id)` replaces the peer with `TunnelStack::peer_for_node(node_id, host, port)`, whose socket lands on the in-process loopback bridge. The bridge opens an owned `ailinkmax-tunnel` stream to the selected node and sends the real upstream target as a `MsgType::Open` frame payload. `ctx.tunnel_resolution` is stamped with `"direct"` or the node_id and surfaces in detailed logs.

When `pref.require_tunnel = true` and resolution is `Direct`, `pick_peer` short-circuits with `GW_TUNNEL_NO_NODE` (no silent Direct fallback). `NoLive` and `TargetDenied` errors during `peer_for_node` map to `GW_TUNNEL_NO_LIVE` / `GW_TUNNEL_TARGET_DENIED` respectively; `NoLive` also pushes the failed key onto `ctx.retry_blacklist` for cross-key failover.

5. **Build `HttpPeer`** — load `Vendor`, parse `base_urls[0]` → host/port/scheme_tls. Apply `Pricing` timeouts (connect, read, total, stream_total). Stash `picked_vendor_key`, `picked_vendor`. Install `InFlightGuard` so the counter decrements on `ReqCtx` drop.

   **Impersonate branch (egress-impersonate, 2026-05-28).** When the resolved `Vendor.egress_mode = Impersonate { profile }` and `[egress].disabled = false`, the peer is *not* built against the upstream host/port directly. Instead, `pick_peer` reads `LlmGateway.egress.bind_addr()` and constructs a loopback `HttpPeer` (UDS via `HttpPeer::new_uds(path, false, "egress.local")` or TCP via `HttpPeer::new(addr, false, host)`). The real upstream `(host, port)` is stashed on `ctx.egress_upstream` and the profile on `ctx.egress_profile`; `upstream_request_filter` adds `X-Egress-Target` (`host:port`), `X-Egress-Profile` (profile id), and a per-identity static header set produced by `gw_core::impersonate::ensure_identity` from the snapshot stored on `VendorKey.identity_snapshot`. The loopback (`gw-egress-impersonate`) replays the request through `wreq` + BoringSSL, presenting the Chrome-class TLS/H2 fingerprint, and surfaces transport failures via `X-Egress-Error-Class` (mapped to `GW_UPSTREAM_CONNECT_TIMEOUT` / `_CONNECT_REFUSED` / `_READ_TIMEOUT` / `_H2_PROTOCOL` / `_BODY_TRUNCATED` / `_PROXY_ERROR` / `_PANIC` / `_UNKNOWN` by `gw_proxy::gateway::map_egress_class`). When `[egress].disabled = true`, Impersonate vendors are excluded from the candidate set in step 3 and the kill-switch metric `gw_egress_disabled_total` ticks; direct vendors remain byte-identical to the legacy path. Impersonate is mutually exclusive with `Tunnel::Use(_)`: a vendor returning a non-Direct tunnel resolution with `egress_mode = Impersonate { .. }` short-circuits with `GW_TUNNEL_IMPERSONATE_CONFLICT`.

### 2.4 `upstream_request_filter`

Same module. The phase has two responsibilities:

1. **Body rewrite** — CLI requests call the matching `CliCodec::passthrough_request`, preserve raw bytes via `ctx.body` view, set `ctx.upstream_shape` to the CLI shape, and never parse/render IR. Standard requests first apply vendor-key request mutations: `forced_params` plus model-name rewrite from `VendorKey.model_mapping` / `ModelMapping.default_upstream_model`. When the resolved upstream model is non-empty and differs from the client model, `rewrite_request` injects `{"model": "..."}` into `RequestIR.passthrough_overrides`, updates `ctx.model` to the upstream name for downstream logging/routing consistency, increments `gw_model_rewrite_total{from,to}`, and forces same-protocol passthrough to re-render instead of zero-copy. Next, per-endpoint **body rules** (`[body_rules]` config) are checked: if the request path matches a configured endpoint pattern, each configured key is tested for presence in the IR typed fields, the raw request body, and any existing `passthrough_overrides`; missing keys are injected with set-if-absent semantics, incrementing `gw_body_rules_applied_total{endpoint,key}` per injection. Otherwise, if `picked.accessible_endpoints.contains(&client_codec.kind())`, take the fast path `same_protocol_passthrough(client_codec, ctx.body.raw(), &mutations)` which returns a `Cow<[u8]>` (zero-copy when `Mutations::NONE`). If not same protocol, pick the first accessible endpoint, `client_codec.parse_request` → IR → `upstream_codec.render_request`. Result stored via `ctx.body.set_outbound(...)`. The response body is not post-processed to restore the client model name.
2. **Header injection** — strip `authorization`, `x-api-key`, `x-api-key-id` from the outbound request; inject `vendor.auth_template.replace("{api_key}", picked.api_key.expose_secret())`; set `Host` from the vendor base URL; emit a W3C `traceparent`; rewrite `Content-Length`.
3. **Egress loopback timing** — when `ctx.egress_profile.is_some()` (the impersonate path, §2.3), the phase stamps `ctx.egress_sent_at = Some(Instant::now())` at its end. `response_filter` consumes it once to emit `gw_egress_loopback_rtt_seconds`. Re-set per attempt, so retries measure the latest attempt.

### 2.5 `request_body_filter` (in `gateway.rs`)

Pingora streams the request body. We discard every chunk **until** `end_of_stream == true`, then write `ctx.body.outbound_or_raw()` and let the final write go through. This guarantees cross-protocol rewrites reach the wire intact, and lets streaming clients see the request body delivered in one shot.

### 2.6 `response_filter` & `response_body_filter`

- `response_filter` captures the upstream HTTP status on `ctx.upstream_status`. At the top of the phase, if `ctx.egress_sent_at` is set (impersonate path, §2.4), it records the label-free `gw_egress_loopback_rtt_seconds` histogram via `.take()` so it fires at most once per attempt — note this gateway-side measurement includes the real upstream latency, so the Task 6.2 microbench (inline mock upstream) remains the pure loopback-overhead SLO gate (p99 <200µs UDS / <500µs TCP). On 429, it feeds the picked key into `AdaptiveRpmManager::observe_429`, preferring a vendor-specific RPM header (`anthropic-ratelimit-requests-limit`, `x-ratelimit-remaining-requests`, then the generic `x-ratelimit-limit` — `rpm_limit_header`) and falling back to the configured key RPM; a vendor RPM header also ticks `gw_adaptive_rpm_rpm_detected_total` (audit 2026-05-30 §3.9). This path updates `gw_adaptive_rpm_observe_total`, `gw_adaptive_rpm_learned_value`, and `gw_adaptive_rpm_confidence`. Enforcement of the learned ceiling now happens in `pick_peer` via `AdaptiveRpmManager::try_admit` (NOT here / not in `request_filter`); `gw_adaptive_rpm_enforced_total` increments when `try_admit` denies a send. The background decay loop emits `gw_adaptive_rpm_probe_total` on probe expansions and `gw_adaptive_rpm_released_total` when confidence decay drops a previously enforced key below threshold. For 5xx / 429 while `ctx.body_started == false`, it calls `FailoverExecutor::next_candidate` over the cached `ctx.picked_candidates`, then — BEFORE mutating ctx — calls `billing_lifecycle::settle_prior_attempt_spawn` to bump `attempt_index`, emit the failed attempt's `UsageRecord`, and `mark_settled` it (audit 2026-05-30 §1.2; matches the connect-path), pushes the failed key into `ctx.retry_blacklist`, drops the current `InFlightGuard`, stamps the chosen next key on `ctx.picked_vendor_key`, and returns an upstream `HTTPStatus` error with `set_retry(true)`. Pingora then re-enters `upstream_peer` without rerunning auth, RPM, quota, or freeze deduction. Exhausted / non-retryable cases fall through to the classifier. After failover resolution (retry returned, or exhausted/not-retryable), for any final upstream status ≥ 400 the filter constructs a `ClassifyCtx` from the picked vendor key (including `has_oauth` from `VendorKey.oauth.is_some()`) and calls `DefaultClassifier::classify_upstream`. The resulting `ClassifierOutcome` is stored in `ctx.error_outcome` so the logging filter's `ErrorHandler` fanout (§2.8 item 7) fires. OAuth keys receiving a 401 produce `OAuthSignalKind::RefreshFailed` (reason_code 12, 300 s default cooldown); non-OAuth 401s produce `OAuthSignalKind::AccountBlocked` (reason_code 2, 3600 s). Metric: `gw_routing_failover_total{reason,strategy}`.
- `response_body_filter` (`crates/gw-proxy/src/filters/response.rs`) first sets `ctx.body_started = true` on the first non-empty body callback so later response-side failover is prohibited. It then checks `ctx.response_deadline` — if `Instant::now()` exceeds the deadline (derived from `Timeouts.stream_total` or `Timeouts.total` stashed in `upstream_peer`), the response is truncated with a `GW_RESPONSE_DEADLINE_EXCEEDED` error and the upstream read is cancelled. After the deadline check, two branches follow:
  - **CLI** — chunks pass through `CliCodec::passthrough_response_chunk` unchanged. Non-stream responses accumulate until EOS and run `CliUsageExtractor::extract_response_usage`; streaming responses scan SSE event frames and run `extract_stream_usage` / TTFT event matching. Response bytes are not IR-rendered.
  - **Non-stream (`!ctx.is_stream`, B1 / T2-B)** — each chunk is appended to `ctx.upstream_body_accum: Option<BytesMut>`. On `end_of_stream`, the buffer is taken and `upstream_codec.parse_response(&body)` is called to extract `UsageIR` into `ctx.usage`. A parse error (codec-emitted `GW_UPSTREAM_PROTOCOL_DECODE_ERROR`) increments `gw_codec_parse_response_failure_total{codec,upstream_status}` and leaves `ctx.usage` at zero; the logging filter (§2.8) then stamps `UsageSource::Estimated` and falls back to the freeze-time estimate. Same-protocol non-stream bytes are forwarded verbatim — accumulation only feeds the usage extractor, not the outbound body.
  - **Streaming** — each chunk is appended to `ctx.sse_leftover`, the loop scans for `\n\n` event boundaries, and for each complete event calls `codec.parse_stream_chunk(event)`:
    - `Usage` events accumulate `input_tokens`, `output_tokens`, `cache_read_input_tokens` on `ctx.usage_accum`.
    - `Start` / `Delta` events record `ctx.timing.ttft` (one-shot, first delta wins).

After parsing, the loop decides how to forward the chunk to the client. If `ctx.client_codec.kind() == ctx.upstream_codec.kind()` (or the client codec is unknown) the upstream bytes are forwarded verbatim — same-protocol streaming stays zero-copy. When the kinds differ (G8, T4-A) each `StreamEventIR` is re-rendered through the *client* codec into the `ctx.pending_client_body: Option<BytesMut>` accumulator: the buffer is taken out at the start of the framed event, every IR event appends into it (so Gemini's combined `[Delta, End]` fan-out renders both, not just the last), and at the end of the chunk `response_body_filter` `take()`s the accumulator, `freeze()`s it, and substitutes it for the outbound body. The `.take()` is per-chunk — each Pingora callback gets a fresh accumulator so old rendered bytes never leak forward. Counters: `gw_codec_cross_render_total{from,to}` per re-rendered event, `gw_codec_passthrough_total{kind}` per same-protocol event. Per-chunk SSE conversion for the OpenAI Responses shape lives in `crates/gw-codec/src/openai/responses/{wire_stream_chunk,conversion}.rs`.

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
2. **Release freeze** — `ctx.freeze_guard.release()`. The guard's `Drop` is the safety net for cancellation paths; the explicit release ensures idempotency and surfaces the canonical metric. When `ctx.lease_guard` is set (protected-mode requests), settle the lease (`LeaseGuard::settle_actual` with 6-dim actual usage; refunds if actual ≤ reserved) and release it.
3. **Key-health record** — `KeyHealthStore::record(key_id, api_format, OutcomeKind)`. Drives cooldown / circuit state in `gw-health`.
4. **Build `UsageRecord`** — needs all four of `auth_user_id`, `effective_user_id`, `model`, `picked_vendor_key`. Carries timestamps, request/trace IDs, `client_protocol`, `upstream_protocol`, `status` (`Ok` / `Error`), `usage` (from the accumulator), `attempt_index`, `vendor_exposure_state` (`Settled` for fully consumed upstream responses, `Terminated` for cut-short attempts), per-dimension `usage_provenance`, `freeze_amount_micro`, and the `markup_chain_snapshot`. `ledger.emit(LedgerItem { record, clear: Some(ClearKey{ req, attempt }) })` enqueues onto the must-deliver primary mpsc; if primary is Full or Closed, it first attempts non-blocking primary overflow enqueue, and only falls through to DLQ / emergency disk / `BillingBacklog` if overflow is also unavailable. When `ctx.picked_vendor_key.is_some()`, the filter also calls `inflight.mark_settled(..)` to HSET `ts_settled_ms` for the T0 watchdog. **No `inflight.clear` is called from the proxy** — the ledger DELs the inflight key after XADD-confirmed delivery on `usage:records` (see [`billing-record-durability`](spec/06-billing-ledger.md)).
5. **DetailedArchive** — `crates/gw-ledger::detailed_archive::DetailedArchive` flusher pool consumes a bounded mpsc; each filter that completes a surface (downstream request body read / upstream rewrite / upstream response EOS / downstream response EOS / `logging`) calls `archive.try_send(CapturedSurface{attempt_index, ...})` after the proxy archive boundary has run. Request and response bodies, plus archive-surface headers and request method/URL metadata, may be masked before the `usage:detailed` XADD when the sensitive-filter policy contains enabled `archive_mask` rules. Reversible transit maps are request-local memory only; they are never serialized into `usage:detailed` and never persisted to PG/Redis. The archive is byte-for-byte equal to the wire only when the global sensitive-filter policy has zero enabled `archive_mask` rules. Cross-link: `crates/gw-proxy/src/filters/sensitive_filter.rs` and `crates/gw-runtime/src/sensitive_filter.rs`. Workers pick an encoding (raw / lz4 / zstd) by uncompressed payload size and XADD to `usage:detailed`. Response-side failover flushes captured failed-attempt response surfaces before the retry reset drops response accumulators. No ad hoc body redaction, header denylist, preview truncation, or sampling is allowed. Drop-on-outage (channel full / oversize / serialize fail / XADD fail) only emits metrics; the request path is never blocked. Go drainer is the long-term archive sink (180 days).
6. **TPM metric + deferred deduction** — `gateway_tpm_consumed_total` (aggregate counter; the per-user `user_id` label was removed in D3 #17 to bound cardinality — per-user enforcement is owned by the limiter, not the metric) += `input + output`. When `merged.tpm_limit > 0`, `RateLimitGuard::check_n(tokens)` deducts the actual token count post-response (deferred, not pre-checked). A TPM breach only affects the *next* request — the current response is already sent.
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

`DefaultClassifier` (`classify_default.rs`) maps HTTP status to `ErrorClass`. `ConfigurableClassifier` wraps it, overriding retryability for statuses in the failover range (401/402/403, 429, and 500-599) based on `[error.classification].retryable_status_codes`. `response_filter` also stops cross-key failover once the request's total elapsed time exceeds `[error.classification].failover_total_budget_ms`, stamping `GW_ROUTING_FAILOVER_TIMEOUT`. At response-body EOS, `LlmGateway` calls vendor `UpstreamErrorAnalyzer` impls (`gw-adapter::analyzer`) with upstream headers and an 8 KiB body head; those analyzers layer on vendor-specific class/cooldown/OAuth signals and `Retry-After` handling. Separately, `response_filter` extracts quota snapshots from rate-limit headers and `logging` schedules `QuotaStateUpdateQueue` writes for the picked vendor key.

`error_emit::send_error(session, codec, &ErrorIR)` renders the envelope through the **client's** codec (so OpenAI clients get OpenAI-shaped errors, etc.), sets `Content-Type: application/json`, propagates `X-Request-Id` and `Retry-After`, writes header + body in one shot. The caller returns `short_circuit(status)`.

`error_passthrough::render_error_to_client(client_kind, &PassthroughBody, error_class)` produces a structured protocol-specific error envelope from the upstream error body. Activated when `[error.passthrough].enabled = true` — the `response_body_filter` replaces the final body chunk at EOS. HTML upstream bodies bypass passthrough (always wrapped as JSON). Every envelope (stream `event: error` frame and non-stream alike) carries a stable `GW_...` gateway code (P0 #16): OpenAI/Anthropic in `error.code`, Gemini/Kiro in an additive `gateway_code` field (their `code` stays the numeric/vendor value). The code is derived from any upstream-supplied `GW_...` code, else mapped from `upstream_status`/`error_class`; message allow/deny masking never touches it.

Error-message rewrite rules layer on top of passthrough rendering. The gateway
bootstraps `gw-store::error_message_rules::ErrorMessageRuleStore`, cold-loads
the Go-published `entity:error_message_rules:global` snapshot once, and
`response_body_filter` reads only the local `ArcSwap` snapshot. A matched rule
replaces the rendered client-visible `message` after the stable gateway code is
derived; it never changes `GW_*` code, protocol type/status, HTTP status, or SSE
wrapping. Rule matching uses the fixed dimension order
provider/model/endpoint/api-format/request-path/client-protocol/gateway-code/http-status
plus bounded upstream-message substring checks. If a reload or compile fails,
the previous snapshot remains active.

### 3.1 Connect-failure retry path (billing emit)

On `fail_to_connect` with retry budget remaining:

- `on_retry_sync` mutates ctx state and returns the partial attempt's `UsageRecord`.
- A `tokio::spawn` enqueues `mark_settled(Terminated)` and `ledger.emit(LedgerItem { record, clear: Some(..) })`. The spawn does **not** call `inflight.clear`; the flusher owns DEL after XADD on `usage:records`.
- If the spawn never runs (process termination before it is scheduled), the inflight key remains and the startup reconciler emits a `CrashFallback` for the failed attempt. See [`billing-record-durability`](spec/06-billing-ledger.md).

## 4. Cache invalidation

`crates/gw-store/src/stream_consumer.rs` XREADGROUPs `events:entity` (block 200 ms, count 64) on a consumer group anchored at `$` so a fresh pod ignores backfill — the cold-load chain covers the gap. Each event carries `{ns, id}`; `apply()` dispatches to the right `EntityStore::invalidate_*`:

Before dispatching to the per-`ns` invalidate, the consumer DELs the Redis L1 key `entity:{ns}:{id}` (G6) so multi-pod deployments do not retain stale entries up to the L1 TTL. The DEL failure path only warns (`tracing::warn!`), it does not block the Moka invalidate.

- `user`, `apikey`, `user_group`, `vendor`, `vendor_key`, `model_group`, `pricing`, `jwks`, `ipblock` → the corresponding Moka cache eviction. `vendor_key` invalidation also runs registered hooks; gateway bootstrap registers `adaptive_rpm.evict(key_id)` so learned RPM state is cleared when a key rotates or its configured RPM changes.
- `gemini_file_mapping`, `model_mapping` → file-mapping / model-alias cache eviction (entity loaders pending — T6-E + A-T2).
- `error_message_rules` with `id=global` → `ErrorMessageRuleStore::reload()`.
  This is a special global snapshot path: StreamConsumer does not invalidate a
  Moka entity; it spawns a reload, recompiles the rule tree, and atomically swaps
  the local snapshot only on success.

`events:counter` is consumed and ACKed; `gw-ledger::counter_handler` handles `user_balance` (`op=incrby`, `base_micro` / `deducted_micro`), `protected_lease`, `coef_table`, and `coef_table_commit`. Unknown namespaces remain observability-only.

## 5. Tunnel transport (feature `tunnel`)

`crates/gw-tunnel-client/` and `crates/gw-tunnel-server/` are compiled into the tunnel-enabled build path. They implement the first-party `ailinkmax-tunnel` protocol; aether/mock behavior is reference material only.

- **Frame** (`frame.rs`) — magic `0xAE 0x21`, version `1`. 14-byte header: `magic | version | msg_type | flags(2) | stream_id(4) | length(4) | payload`. Max payload 4 MiB. 8 msg types: `Open`, `Data`, `Close`, `Ping`, `Pong`, `Error`, `ControlAuth`, `ControlNodeInfo`.
- **Transport** (`transport.rs`) — `tokio-tungstenite` WebSocket with heartbeat, reconnect backoff, stream-id routing, `TunnelStream`, chunked `Data`, and `Close` / `Error` propagation.
- **Server** (`gw-tunnel-server`) — WebSocket acceptor with optional `ControlAuth`, `ControlNodeInfo`, `Open host:port`, target validation, TCP relay, and stream-cap enforcement.
- **Bridge** (`gw-proxy::tunnel_bridge`) — loopback HTTP/1 bridge that strips `X-Gateway-Tunnel-*` headers, opens tunnel streams, and returns upstream responses to Pingora.
- **DoH resolver** (`safe_dns.rs`) + target-filter whitelist (`target_filter.rs`) — block SSRF / split-horizon attacks against the tunnel exit.
- **Pool** (`pool.rs`) — `arc-swap` for atomic config reload.

## 6. Observability surfaces

| Surface | Sink | Source |
|---------|------|--------|
| `usage:records` Redis stream | `gw-ledger::flusher` (XADD batched, configurable batch / interval) | `filters::logging` builds `UsageRecord`, calls `ledger.try_send` |
| `usage:detailed` Redis stream | `gw-ledger::detailed_archive::DetailedArchive` (workers XADD per-surface encoding + `attempt_index`) | `filters::*` and `filters::logging` call `archive.try_send(CapturedSurface)` |
| Prometheus `/metrics` | `metrics-exporter-prometheus` | `gw-gateway::metrics_catalog` defines metric families across codec, freeze, auth, adaptive/IP rate-limit, capability, resilience, detailed-log, billing, routing, tunnel, quota, CLI, and task surfaces (e.g. `gw_adaptive_rpm_observe_total`, `gw_adaptive_rpm_confidence`, `gw_ip_ratelimit_check_total`, `gw_ip_blocklist_hit_total`, `gw_codec_cross_render_total`, `gw_billing_emit_total`, `gw_cli_cross_protocol_denied_total`, `gw_cli_usage_extract_total`). The catalog is the single source of truth for metric names and help text. |
| Health `/livez` | always 200 (even during drain — k8s must not restart the Pod) | — |
| Health `/readyz` | 200 once `READY=true` **and** no shutdown phase active **and** no BackpressureGate rejection **and** no Supervisor-unhealthy component; otherwise 503 | `gw-gateway::health` |
| Health `/healthz/circuits`, `/healthz/key/{id}`, `/healthz/resilience`, `/healthz/strategies` | Mgmt-verified (`Authorization: Mgmt mt_<id>.<secret>`, scope ≥ `AdminRead`, FailClosed blacklist). 200 returns the `snapshot_json` body; verify failures map to `GW_AUTH_MGMT_TOKEN_*`. `/healthz/strategies` returns per-(key, format) p50 TTFT for `LatencyFirst` debugging. | `gw-gateway::health` |

For synchronous requests, `FreezeGuard::release` (in logging) is the point where Rust mutates `frozen_micro` downward. Go (the billing service) writes request-path `deducted_micro` / `balance_micro` based on `usage:records`. The async task path is separate: `RedisTaskContract::settle` releases `task_frozen_micro`, increments scalar `deducted_micro` by task-domain `actual_micro`, and emits `task_usage_v1` on `usage:records`.

Error-message rule observability is registered in the same catalog:
`gw_error_message_rules_snapshot_version`,
`gw_error_message_rules_reload_total{result}`,
`gw_error_message_rule_match_total{rule_id}`,
`gw_error_message_rule_no_match_total`, and
`gw_error_message_rule_compile_error_total`.

### 6.1 Task metrics

Two metric families cover the task system. Gateway-side metrics (prefix `gw_task_gw_`) are emitted from `gw-proxy::filters::task` and registered in `gw-gateway::metrics_catalog`. Worker-side metrics (prefix `gw_task_`) are emitted from the loop modules in `gw-task-runtime` and registered by `gw-task-worker::task_metrics::describe_worker_metrics()`.

**Gateway-side (`:9090`):**

| Metric | Type | Labels | Emitted at |
|--------|------|--------|------------|
| `gw_task_gw_submit_total` | counter | — | Successful task enqueue in `request_filter` task submit path |
| `gw_task_gw_freeze_total` | counter | `result` (`ok`, `insufficient`) | Task freeze success or insufficient-balance rejection |
| `gw_task_gw_load_total` | counter | — | `GET /v1/tasks/:id` entry |
| `gw_task_gw_watch_total` | counter | — | `GET /v1/tasks/:id/watch` entry |
| `gw_task_gw_cancel_total` | counter | — | `POST /v1/tasks/:id/cancel` entry |

**Worker-side (`:9091`):**

| Metric | Type | Labels | Emitted at |
|--------|------|--------|------------|
| `gw_task_claimed_total` | counter | — | After successful claim + permit acquisition in `ExecLoop` |
| `gw_task_settled_total` | counter | `status` (`succeeded`, `failed`) | Each settle outcome |
| `gw_task_execution_duration_seconds` | histogram | — | Full upstream call + timeout window |
| `gw_task_lease_recovered_total` | counter | — | Per watchdog recovery in `WatchdogLoop` |
| `gw_task_heartbeat_total` | counter | — | Per successful lease renewal in `HeartbeatLoop` |
| `gw_task_cancel_received_total` | counter | — | Per cancel event processed in `CancelLoop` |

## 6.2 Error observability sink

All non-trivial error events route through `gw_core::observability::observe()`:

| Path | When | Source |
|------|------|--------|
| Short-circuit | `emit_envelope_and_bail` in any filter | `kind=short_circuit`, `stage=request` |
| Upstream connect failure | `fail_to_connect` (gateway.rs) | `kind=upstream_connect`, `stage=upstream` |
| Upstream proxy failure | `fail_to_proxy` (gateway.rs) | `kind=upstream_connect \| upstream_mid_stream \| upstream_protocol`, `stage=upstream` |
| Codec parse / render error | gw-codec internal + response.rs chunk handling | `kind=codec_parse \| codec_render`, `stage=response` |
| Background worker recover/fatal | `Supervisor` in `gw-store/managed_loop.rs` | `kind=background_worker`, `stage=background` |

`gw_errors_total{code,kind,stage}` counter populated on every observation. Mid-stream upstream failure deliberately does **not** inject a synthetic SSE error event (see `docs/architecture.md` §Deliberate behaviours).

## 7. Task lifecycle

Background tasks follow a split-process lifecycle: the gateway accepts and enqueues, Redis coordinates, and a separate `gw-task-worker` process executes and settles.

```
Client                    Gateway (gw-proxy)           Redis                    Worker (gw-task-worker)
  │                           │                          │                          │
  │ POST /v1/tasks            │                          │                          │
  ├──────────────────────────►│                          │                          │
  │                           │ auth, balance            │                          │
  │                           │ calculate_freeze_amount  │                          │
  │                           │ INCRBY task_frozen_micro │                          │
  │                           ├─────────────────────────►│                          │
  │                           │ HSET entity:task:<id>    │                          │
  │                           ├─────────────────────────►│                          │
  │                           │ XADD tasks:queue         │                          │
  │                           ├─────────────────────────►│                          │
  │ 201 {task_id, queued}     │                          │                          │
  │◄──────────────────────────┤                          │                          │
  │                           │                          │ XREADGROUP claim         │
  │                           │                          │◄─────────────────────────┤
  │                           │                          │ ZADD tasks:leases        │
  │                           │                          │◄─────────────────────────┤
  │                           │                          │ HSET status=running      │
  │                           │                          │◄─────────────────────────┤
  │                           │                          │                          │
  │                           │                          │   upstream HTTP call     │
  │                           │                          │   poll status/poll URL   │
  │                           │                          │   (reqwest, with timeout)│
  │                           │                          │                          │
  │                           │                          │ settle(succeeded/failed) │
  │                           │                          │◄─────────────────────────┤
  │                           │                          │ HSET status + output     │
  │                           │                          │ ZREM tasks:leases        │
  │                           │                          │ INCRBY task_frozen_micro │
  │                           │                          │ INCRBY deducted_micro    │
  │                           │                          │ XADD usage:records       │
  │                           │                          │                          │
  │ GET /v1/tasks/:id         │                          │                          │
  ├──────────────────────────►│                          │                          │
  │                           │ HGETALL entity:task:<id> │                          │
  │                           ├─────────────────────────►│                          │
  │ 200 {task record}         │                          │                          │
  │◄──────────────────────────┤                          │                          │
```

### 7.1 Worker internal loops

`gw-task-worker` runs four concurrent loops on a single tokio runtime:

| Loop | Module | Cadence | Purpose |
|------|--------|---------|---------|
| `ExecLoop` | `gw-task-runtime::exec_loop` | Continuous `XREADGROUP` block | Claim tasks from `tasks:queue`, acquire concurrency permit (global/vendor/model/key semaphores), execute upstream HTTP call via `reqwest`, poll generic JSON LRO status URLs with exponential backoff, settle as succeeded or failed. |
| `CancelLoop` | `gw-task-runtime::cancel_loop` | Continuous `XREADGROUP` block on `tasks:cancel` | Read cancel events, look up the task's in-flight `CancellationToken` in the shared `RunningTaskRegistry`, cancel the token, then settle as `Cancelled` with `actual_micro=0`. |
| `WatchdogLoop` | `gw-task-runtime::watchdog_loop` | Periodic (configurable interval) | Acquire `lock:tasks:watchdog` with `SET NX PX` before scanning; the leader uses `ZRANGEBYSCORE tasks:leases 0 <now_ms>` to find expired leases, then `TaskContract::recover_lease` per expired ID. Recovered tasks re-enter the queue. |
| `HeartbeatLoop` | `gw-task-runtime::heartbeat_loop` | Periodic (`loops.heartbeat_interval_ms`) | Renew `lease_until_ms` on all in-flight tasks via `TaskContract::update_running`. Also refreshes the worker registration hash `entity:worker:<uuid>.last_heartbeat_ms`. |

### 7.2 Task state machine

```
Pending ──► Queued ──► Running ──► Succeeded
                          │   └──► Failed
                          │
                          └──► Cancelling ──► Cancelled
                                    └──► Succeeded  (race: provider returns 2xx before cancel wins)
```

Transitions are validated by `gw-task::transition::validate_transition`. Invalid transitions return `Err` without mutating Redis state.
