# Architecture — ailinkmax-gateway

> **Sync rule**: if you change the crate layout, layering invariants, traits, configuration tables, persistence namespaces, or background-task graph, update this file in the same commit. Code is the source of truth; this document is the index.
>
> Companion: [`workflow.md`](workflow.md) — what each filter does, per phase.

## 1. What this gateway is

A Rust LLM gateway built on **Pingora 0.8**. It terminates OpenAI / Anthropic / Gemini-shaped client traffic, normalises through an in-memory IR, authenticates, merges 5-tier permissions, enforces rate-limit / quota / hard-caps, picks an upstream vendor key via one of 8 strategies, streams the response back through the matching client codec, and emits a usage record per request to a Redis stream. Cost computation is **not** performed in Rust — the Go billing service consumes `usage:records`.

## 2. Workspace layout

Three layers with strict one-way dependency (enforced by `scripts/check-invariants.sh`):

```
┌────────── Execution ──────────┐  gw-gateway (bin), gw-task-worker (bin), gw-proxy
│   may depend on ↓ ↓ ↓         │
├────────── Business ───────────┤  gw-adapter, gw-health, gw-routing,
│   may depend on ↓             │  gw-capability, gw-tunnel-client, gw-billing,
│                               │  gw-task-runtime, gw-egress-impersonate
├────────── Supporting ─────────┤  gw-core, gw-codec, gw-codec-macros,
│   type / mechanism only        │  gw-store, gw-auth, gw-ledger, gw-task,
│                               │  gw-runtime
└───────────────────────────────┘
```

**Forbidden edges**: Supporting → Business/Execution. Execution → Execution.

### 2.1 Crate responsibilities

| Crate | Layer | Responsibility |
|-------|-------|----------------|
| `gw-core` | Supporting | ID newtypes, entity structs, `RequestIR` / `ResponseIR` / `StreamEventIR`, `ErrorIR`, `ErrorClass`, `DefaultClassifier`, stable error-code dictionary. No I/O. The IR carries full v2 response surface: `ResponseIR.{refusal, annotations, server_tool_blocks, redacted_thinking, service_tier}`, `MessageIR::Developer`, `FinishReasonIR::Refusal`, `UsageIR` with 14 optional detail fields (audio/prediction/cache-creation/thoughts/server-tool-requests/service-tier/inference-geo), `StreamEventIR::{RefusalDelta, CitationsDelta, ServerToolStart, ServerToolDelta, ThinkingDelta, ToolCallStart, ToolCallDelta}`, plus `ReasoningConfig`, `AudioConfig`, `AnnotationIR`, `ServerToolBlockIR`, `RedactedThinkingIR` structs. |
| `gw-codec-macros` | Supporting | Proc-macros driving codec boilerplate (Phase 2 `wire_convert_matrix!` scaffolding). |
| `gw-codec` | Supporting | `ProtocolCodec` trait + OpenAI / Anthropic / Gemini codecs, same-protocol passthrough fast path, cross-protocol IR re-render, SSE accumulator, gzip/brotli compression, stream-policy truth table. CLI variant codecs (`gw-codec::cli`) implement the narrower `CliCodec` trait for transparent body passthrough with usage extraction — see §3.1. |
| `gw-store` | Supporting | Redis + Postgres clients, `EntityStore` (Moka caches with cold-load chain), vendor-key invalidation hooks, `KekCache` state machine, `CounterStore`, deduction state, `StreamConsumer`. |
| `gw-auth` | Supporting | `JwtValidator` (iss/aud/nbf/leeway), API-key verify, ADR-023 5-tier permission merge, governor per-user RPM/TPM/concurrency plus per-vendor-key RPM and `ratelimit::adaptive::AdaptiveRpmManager`, ADR-024 hard caps, HMAC signature verify, `ip_filter` / allowlist / blocklist / per-IP RPM, model-permission regex w/ ReDoS budget, management-path detect. |
| `gw-task` | Supporting | Task contract crate: shared task DTOs, lifecycle/status enums, Redis task freeze/settle Lua, and Redis/Lua protocol shapes for background work without pulling in store/runtime I/O. |
| `gw-ledger` | Supporting | `FreezeGuard` RAII (in-process request `frozen_micro`; task freezes delegate to its `TaskFreezeRelease` trait), mpsc-backed billing flusher, `DetailedLogSink` (Redis Streams w/ MAXLEN), three-level redaction; `lease`/`coef`/`credit_estimator`/`counter_handler` modules (2026-05-29 credit lease). `EmergencyDlqWriter` async over `SpillFile`, `BillingBacklog` (unbounded VecDeque), `BacklogRecovery` on bootstrap. |
| `gw-runtime` | Supporting | Shared runtime primitives: `Supervisor` (bounded-restart policy with exponential backoff + jitter), `ShutdownCoordinator` (two-phase drain: AcceptStop → TaskDrain → Stopped), `SpillRoot`/`SpillFile` (sharded async append-only files with quota enforcement), `RegexCache` (Moka-backed compiled-regex cache), `SystemHealthMonitor` + `BackpressureGate` (4 force-majeure hooks: memory/FD/disk/Redis-OOM at 99%). Workspace bottom — may not depend on any other workspace crate. |
| `gw-task-runtime` | Business | Task runtime coordination used by the worker path: `RedisTaskFreezeRelease`, execution/cancel/watchdog/heartbeat loops, generic JSON LRO polling, and task-level settlement helpers. Must not depend on `gw-proxy`. |
| `gw-adapter` | Business | `UpstreamAdapter` trait (vendor-specific outbound rewrite hooks, not wired for outbound yet), live `UpstreamErrorAnalyzer` trait, `ErrorOutcome`. |
| `gw-health` | Business | `KeyHealthStore` — multi-dim (KeyId, ApiFormat) circuit/cooldown state machine, sliding window. |
| `gw-routing` | Business | Candidate filtering, `InFlightStore`, 8 `Strategy` impls. Takes `&KeyHealthStore` in `pick`. |
| `gw-capability` | Business | Model-capability resolution: `Capability` enum, `RequiredCapabilities`, `ModelMapper`, `CapabilityResolver`, `AdapterCapabilityHook`, `CapabilityFilter`, `PricingReader`. |
| `gw-tunnel-client` | Business | First-party tunnel client: `cnexus/` holds the Cnexus frame codec (magic `0xAE21`, 8 msg types), WebSocket transport w/ heartbeat / reconnect, stream-id routing, and `TunnelStream`; `core.rs` / `target.rs` expose the protocol-neutral `Transport` trait and `StreamTarget`; `resolver`, `safe_dns`, and `target_filter` stay protocol-agnostic at the crate root. Consumed by `gw-proxy::tunnel_stack::TunnelStack` and `gw-proxy::tunnel_bridge` in the default build. Active when `cfg.tunnel.enabled = true`; otherwise `pick_peer` runs the byte-identical Direct path. |
| `gw-tunnel-server` | Business | First-party `ailinkmax-tunnel` server and binary. Accepts WebSocket, optional `ControlAuth`, `ControlNodeInfo`, `Open host:port`, target validation, TCP relay, `Data` / `Close` / `Error`, and stream-cap enforcement. |
| `gw-billing` | Business | Task-level billing formulas: base micro amounts, vendor/task-kind multipliers, duration buckets. No per-token pricing, no `cost_micro` — the `no_cost_micro` invariant applies. Depends only on `gw-task` + `serde`. Supporting crates must not depend on it. |
| `gw-egress-impersonate` | Business | In-process loopback egress for vendor keys whose `egress_mode = Impersonate { profile }`. Wraps `wreq` 6 + BoringSSL into a per-profile `ClientPool` (LRU), accepts gateway requests over a UDS or TCP listener, replays the request with a Chrome-class TLS/H2/header fingerprint, and surfaces transport-level failure classes via `X-Egress-Error-Class` headers. `EgressHandle::start` returns synchronously and supervises the listener + supervisor tasks; see `crates/gw-egress-impersonate/`. Depends on `gw-core` (`ImpersonateProfile`); supporting crates must not depend on it. Direct vendors do not touch this crate. |
| `gw-proxy` | Execution | Pingora `ProxyHttp` impl. Filter chain, `ReqCtx`, error fanout, timeouts, body-verify hook, jemalloc sampling. |
| `gw-task-worker` | Execution | Task worker binary. Figment config (`config/task-default.toml` + `GW_TASK_*` env), Redis pool, `ExecLoop` + `CancelLoop` + leader-gated `WatchdogLoop` + `HeartbeatLoop`, worker registration (`entity:worker:<uuid>` HASH + heartbeat refresh), health HTTP (`/livez`, `/readyz` on port 8081), Prometheus `/metrics` on port 9091. No business HTTP endpoint, no `gw-proxy` dependency. |
| `gw-gateway` | Execution | Binary entry. Figment config, observability, KMS init, bootstrap OS thread + tokio runtime, health HTTP server. |

### 2.2 Dependency DAG (path deps only)

```
gw-core ◄── everything
gw-runtime ◄── everything (workspace bottom)

Supporting:
  gw-codec-macros  ◄── gw-codec
  gw-codec         ◄── gw-proxy, gw-adapter
  gw-store         ◄── gw-auth, gw-routing, gw-ledger, gw-capability,
                       gw-tunnel-client, gw-proxy, gw-gateway
  gw-auth          ◄── gw-capability, gw-proxy, gw-gateway
  gw-task          ◄── gw-task-runtime, gw-gateway
  gw-ledger        ◄── gw-proxy, gw-gateway
  gw-runtime       ◄── gw-ledger, gw-egress-impersonate, gw-task-worker,
                       gw-proxy, gw-gateway

Business:
  gw-health        ◄── gw-adapter, gw-routing, gw-proxy, gw-gateway
  gw-adapter       ◄── gw-proxy, gw-gateway (error analyzers only)
  gw-routing       ◄── gw-proxy, gw-gateway
  gw-capability    ◄── gw-proxy, gw-gateway
  gw-tunnel-client ◄── gw-gateway (default)
  gw-billing       ◄── gw-gateway
  gw-task-runtime  ◄── gw-task-worker

Execution:
  gw-proxy ◄── gw-gateway (binary)
  gw-task-worker (binary) ◄── gw-task, gw-task-runtime, gw-billing, gw-store
```

## 3. Public traits — extension surface

| Trait | Crate | Purpose | Impls |
|-------|-------|---------|-------|
| `ProtocolCodec` | gw-codec | Wire ⇄ IR per protocol | `OpenAiCodec`, `AnthropicCodec`, `GeminiCodec`, `PassthroughCodec`, `ErrorPassthroughCodec`, `AnyCodec` (dispatcher) |
| `ErrorClassifier` | gw-core | HTTP status + body → `ClassifierOutcome` | `DefaultClassifier` |
| `UpstreamErrorAnalyzer` | gw-adapter | Vendor-specific error shape → `ErrorOutcome` | Built-in codex/openai, anthropic/claude, gemini/google fallback analyzers |
| `UpstreamAdapter` | gw-adapter | 4 outbound rewrite hooks + transport profile | `NoopAdapter` |
| `Strategy` | gw-routing | Pick one `VendorKey` from candidates | `FixedOrder`, `RoundRobin`, `LoadBalance`, `WeightedRr`, `CostFirst`, `LatencyFirst`, `CacheAffinity`, `Sticky` (8 total, `StrategyAny` enum) |
| `FreezeGuard` | gw-ledger | RAII freeze on `frozen_micro` | `RequestFreeze`, `TaskFreeze` |
| `DetailedLogSink` | gw-ledger | Verbose log output | `RedisStreamSink` (XADD MAXLEN) |
| `ResolveBackend` | gw-capability | Model / key capability lookup | (injected at boot) |
| `JwtBlacklist` | gw-auth | Token revocation check | `RedisJwtBlacklist` (with `FailMode`) |
| `HeaderView` | gw-adapter | Borrow-friendly header access | (per-adapter) |
| `ContentFilter` / `TokenEstimator` | gw-core | Pluggable scrubbing / sizing | `Noop*` |
| `CliCodec` | gw-codec | CLI variant codec: shape identity, shallow body-hint extraction (model + stream), usage extractor access, TTFT event types. Narrower than `ProtocolCodec` — no parse/render (CLI body is passthrough). | `ClaudeCliCodec`, `OpenAiCliCodec`, `OpenAiCompactCodec`, `GeminiCliCodec` |
| `CliUsageExtractor` | gw-codec | Extract token usage from raw CLI responses (SSE events or non-stream JSON) without IR conversion. | `ClaudeCliUsageExtractor`, `OpenAiCliUsageExtractor` (shared by `openai:cli` + `openai:compact`), `GeminiCliUsageExtractor` |

### 3.1 Wire formats and `ApiShape` FORMAT_IDs

`gw-core::protocol::ApiShape` (8 variants) provides the fine-grained wire-format identity used in codec detection, `UsageRecord` emission, and candidate filtering. `ProtocolKind` (3 variants: `OpenAi`, `Anthropic`, `Gemini`) remains the coarse bucket for key-health, routing strategy, and latency tracking.

| `ApiShape` variant | FORMAT_ID string | Protocol Kind | CLI? | Detection signal |
|--------------------|------------------|---------------|------|------------------|
| `OpenAiChat` | `openai:chat` | `OpenAi` | no | `/v1/chat/completions` |
| `OpenAiResponses` | `openai:responses` | `OpenAi` | no | `/v1/responses` (standard SDK) |
| `AnthropicMessages` | `anthropic:messages` | `Anthropic` | no | `/v1/messages` + `x-api-key` present |
| `GeminiGenerate` | `gemini:generate` | `Gemini` | no | Gemini path pattern, no CLI header |
| `ClaudeCli` | `claude:cli` | `Anthropic` | yes | `/v1/messages` + `Authorization: Bearer` + no `x-api-key` |
| `OpenAiCli` | `openai:cli` | `OpenAi` | yes | `/v1/responses` (CLI detection) |
| `OpenAiCompact` | `openai:compact` | `OpenAi` | yes | `/v1/responses/compact` |
| `GeminiCli` | `gemini:cli` | `Gemini` | yes | Gemini path + `x-app: cli` or UA `gemini-cli` |

Standard shapes flow through `ProtocolCodec::parse_request → RequestIR → render_request`; CLI shapes bypass IR entirely (body passthrough) and use `CliCodec::extract_hints` for shallow model/stream extraction. See §8 "CLI transparent proxy" for the rationale and cross-protocol constraint.

## 4. Configuration

Figment layering (low → high): `config/default.toml` → `config/local.toml` (gitignored) → `GW_*` env vars (`__` is nesting separator).

### 4.1 TOML tables

| Table | Keys / Purpose | Notes |
|-------|----------------|-------|
| `[server]` | `business_port` (8000), `metrics_port` (9090), `health_port` (8080) | |
| `[admin]` | `upstream_url` (`http://localhost:3000`) | Verified `/admin/*` requests are reverse-proxied to this Go control-plane upstream. |
| `[control_plane]` | `enabled` (true), `runtime_push_interval_ms` (2000), `snapshot_ttl_ms` (10000), `command_group` (`group`), `command_consumer_name` (empty = instance id), `command_block_ms` (5000) | Rust Wave 1 control-plane runtime. Publishes capability/runtime snapshots under `gateway:instance:<id>:*` and consumes Go-owned delivery entities from `commands:gateway`. Validates push interval >= 1000 ms, snapshot TTL >= 2x interval, and command block between 1000 and 30000 ms. |
| `[redis]` | URL, pool size | |
| `[pgsql]` | URL, max connections | |
| `[ratelimit]` | Workspace `global_rpm` ceiling (per-user lives in DB) | |
| `[ip_filter]` | `allowlist_mode` (`Off`/`Enforce`, default `Off`) | Boot-time IP allowlist enforcement mode. |
| `[ip_ratelimit]` | `global_per_ip_rpm` (600), `max_entries` (100000), `sweep_interval_secs` (300) | In-memory per-IP rate-limit guard sizing and sweep cadence. |
| `[adaptive_rpm]` | `enforcement_threshold` (0.6), `confidence_decay_per_min` (0.02), `min_header_observations` (2), `min_no_header_observations` (3), `buffer_ratio` (0.85), `probe_after_min` (30), `probe_increment` (5), `min_rpm` (5), `max_rpm` (10000) | Adaptive RPM tuning knobs for confidence-based per-key throttling. |
| `[task]` | `enabled` (false), `queue_stream` (`tasks:queue`), `cancel_stream` (`tasks:cancel`) | Background task worker toggle plus Redis stream names for queue/cancel traffic. |
| `[ledger]` | `channel_capacity`, `batch_size`, `flush_interval_ms` for the billing flusher | |
| `[kms]` | active KEK identifier plus optional previous KEK identifier during rotation; secret material is env-only | `previous_key_id` / `previous_kek_hex` let a restarted gateway keep dual-read compatibility while old rows are re-encrypted. |
| `[debug]` | Sampling rate + body-capture toggles for detailed logs | |
| `[security]` | `trust_xff` (default `false`) | |
| `[routing]` | `sticky_ttl_secs` (1800) | Routing knobs loaded at boot. `sticky_ttl_secs` controls Sticky's conversation binding TTL. |
| `[routing.reservation]` | `enabled` (false), `interval_secs` (5), optional `instance_id`, `thresholds` (`HashMap<String,u64>`) | Local-read + async-aggregate reservation global view. Disabled by default, so no tracker or Redis task is allocated and the reservation gate always passes. When enabled, picks increment a local per-model atomic counter; a background task publishes model rates to `events:reservation` and consumes that stream into a local `DashMap`. Redis is never called from the request path. `thresholds` maps model glob-prefix patterns (`*`, exact names, or `prefix*`) to max RPM; if the local global rate for the request model exceeds the matched threshold, the pipeline gate stage excludes all candidates for that model before scoring. Models without a threshold pass and still receive any scoring-phase `reservation_pressure`. The view is explicitly 5-second eventual consistency in v1 (`interval_secs` controls the sampling/aggregation cadence). |
| `[routing.presets]` | Multi-dim scheduling presets keyed by preset name; each value has `health`, `latency`, `cost`, `inflight`, `error_rate`, `reservation` weights | Boot-time only. Built-ins are `health_first`, `latency_first`, `cost_first`, and `balanced`; deployments may add custom preset names and select them with `StrategyKind::MultiDim("<name>")` in `model_groups.payload.strategy`. Weights are normalised at construction; all-zero weights become uniform. `reservation` defaults to 0 and only affects scoring when `[routing.reservation].enabled = true` has populated `CandidateFeatures.reservation_pressure`. |
| `[tunnel]` | `enabled`, `doh`, `dns_cache_ttl_secs`, `target_whitelist`, `heartbeat_interval_secs`, `missed_pong_threshold`, `backoff_min_secs`, `backoff_max_secs`, `max_streams_per_conn` | Tunnel subsystem (ADR-T-5). Default `enabled = false`. Boot-time only. |
| `[egress]` | `disabled` (bool, default `false` — workspace kill-switch), `bind` (UDS path or TCP `host:port`, default UDS `/tmp/gw-egress.sock`), `max_sessions` (default 64), `idle_timeout_secs` (default 300), `turn_state_ttl_secs` (default 300) | Egress-impersonate subsystem (per `2026-05-28-egress-impersonation-design.md`). Boot-time only. When `disabled = true`, vendors with `egress_mode = Impersonate { .. }` are excluded from `pick_peer` (`gw_egress_disabled_total` ticks). Direct vendors are unaffected either way. |
| `[auth]` | `hard_cap_max_input_tokens` (u32, default 1,000,000), `hard_cap_max_cost_micro` (u64, default 5,000,000,000) | Per-request hard ceilings on estimated input tokens and estimated cost. Consumed by `gw-auth::hard_cap::{check_input_tokens, check_cost}` at `request_filter` step 15 (ADR-024). |
| `[protection]` | `enabled` (bool, default `false`), `overcommit_ratio` (f64), `default_output_ceiling_tokens` (u32), `lease_ttl_secs` (u64), `coef_table_ttl_secs` (u64) | Hard overdraft protection (credit lease layer). When enabled and Go pushes a user into protected mode (`events:counter ns=protected_lease`), Rust holds a per-Pod scalar credit pool and hard-rejects with `GW_QUOTA_LEASE_INSUFFICIENT` / `GW_QUOTA_LEASE_EXPIRED`. See [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md). |
| `[error.classification]` | `retryable_status_codes` (default `[401, 402, 403, 429, 500, 502, 503, 504]`), `failover_total_budget_ms` (default `30000`) | Config-driven retryable status set and total response-side failover wall-clock budget. Consumed by both `ConfigurableClassifier` and failover. Boot-time only. |
| `[error.passthrough]` | `enabled` (default `false`) | When `true`, upstream error bodies (≥ 400) are re-rendered through `render_error_to_client` into client-protocol envelopes. HTML bodies are always wrapped as JSON. Boot-time only. |
| `[body_rules]` | Per-endpoint flat key-value defaults. Sub-table key = endpoint path, value = `{ key = value, … }`. Set-if-absent: existing client fields are never overridden. | Boot-time only. Values injected into `ir.passthrough_overrides` in `upstream_request_filter`. |
| `[health.quota_probe]` | `enabled` (false), `interval_secs` (30), `probe_endpoint` (`/v1/models`) | Optional background probe for keys in cooldown without Retry-After. Sends low-cost GET to upstream; 2xx clears cooldown, non-2xx keeps it. No `usage:records` emitted. Boot-time only. |
| `[quota_state_probe]` | `enabled` (false), `interval_secs` (300), `probe_endpoint` (`/v1/models`), `max_pending` (1000) | Optional quota-remaining probe for cached vendor keys whose persisted quota state is exhausted and reset-eligible. Writes `vendor_keys.quota_*` through Rust's `QuotaStateUpdateQueue`. Boot-time only. |
| `[runtime.supervisor]` | `default_max_restarts` (5), `window_ms` (60000), `backoff_initial_ms` (100), `backoff_max_ms` (30000), `backoff_multiplier` (2.0), `backoff_jitter` (0.2) | Bounded-restart policy for supervised long-running tasks. All spawns through `Supervisor::spawn` inherit these defaults unless overridden per-task. |
| `[runtime.shutdown]` | `phase1_accept_stop_ms` (5000), `phase2_task_drain_ms` (25000), `hard_deadline_ms` (35000), `billing_drain_ms` (20000) | Two-phase shutdown: phase 1 stops accepting new connections; phase 2 drains in-flight billing tasks; hard deadline aborts remaining work. |
| `[runtime.spill_io]` | `root_dir` (`/data/cnexus/spill`), `max_total_bytes` (10737418240), `buffer_size` (65536), `dlq_shards` (4) | SpillFile root and quota for DLQ / archive overflow. `dlq_shards` sets shard count for emergency DLQ writes. |
| `[runtime.regex_cache]` | `capacity` (4096), `tti_secs` (3600) | Moka-backed compiled-regex cache. `tti_secs` controls time-to-idle eviction. Shared across all model-permission regex evaluations. |
| `[runtime.health]` | `evaluate_interval_ms` (1000), `hysteresis_healthy_ticks` (3) | System health monitor cadence and hysteresis. A hook must report unhealthy for `hysteresis_healthy_ticks` consecutive evaluations before the `BackpressureGate` activates. |
| `[proxy.archive]` | `soft_cap_bytes` (4194304), `hard_cap_bytes` (8388608), `spill_dir` (`/data/cnexus/spill/archive`) | Archive buffer caps. Between soft and hard cap, `append_chunk` returns `ArchiveState::Spilling` and excess bytes are written to `spill_dir`. Above hard cap, further appends are dropped. |

Secrets must come from env, never the TOML.

### 4.2 Env-var caveat

`GW_KMS__KEK_HEX` **must contain at least one a-f character**. Figment tries integer coercion first; an all-digit 64-char hex round-trips as `i64` and fails the hex parser downstream. Use `python3 -c 'import secrets; print(secrets.token_hex(32))'`. During a KEK rotation window, `GW_KMS__PREVIOUS_KEK_HEX` and `GW_KMS__PREVIOUS_KEY_ID` must be supplied together; the previous kid must differ from the active `GW_KMS__KEY_ID`.

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

Bootstrap supports the same two-KEK read window: when `previous_kek_hex` and `previous_key_id` are configured, the gateway loads the previous KEK first and then enters dual-read with `kms.key_id` as active. This protects restarts during a rotation. The `rotate-kek` control-plane command is still advertised disabled until a re-encryption worker and runtime command hook are implemented end to end.

## 5. Persistence

> Schema details that double as **cross-process contracts with the Go management plane** (per-entity payload JSONB shape, encrypted `api_key` column, `version BIGINT` write protocol, KEK rotation choreography, all Redis stream payloads) live in [`docs/go-contracts/`](go-contracts/main.md). This section keeps the Rust-side index only.

### 5.1 PostgreSQL

Migrations live at `crates/gw-store/migrations/`. **Not auto-applied** — run `psql -f` on a host Postgres for dev.

| File | Tables |
|------|--------|
| `0001_initial.sql` | `users`, `user_groups`, `api_keys`, `vendors`, `vendor_keys`, `model_groups`, `pricings`, `jwks`, `ip_blocks`. All share `(id TEXT PK, payload JSONB, version BIGINT, updated_at TIMESTAMPTZ, deleted_at TIMESTAMPTZ NULL)`. |
| `0002_gemini_file_mapping.sql` | `gemini_file_mapping(sha256_hex, user_id) PK → file_uri, mime, size, expire_unix_s`. |
| `0003_model_mapping.sql` | `model_mapping(user_alias, protocol) PK → default_upstream_model, vendor_key_overrides JSONB, availability, pricing_strategy_ref`. Dispatch rewrites request-body `model` via `passthrough_overrides` when the resolved upstream model is non-empty and differs; response bodies are left untouched. |
| `0004_capabilities.sql` | New `models` table (canonical entity shape) for `Model.capabilities`; `vendor_keys` gains `forced_capabilities JSONB` + `exclusive_capability_match BOOLEAN`. Capability strings parsed by `gw-capability` at resolve time (see [`docs/ROADMAP.md`](ROADMAP.md)). |
| `0004_task_worker.sql` | `task_records(task_id TEXT PK, user_id, api_key_id, status, kind, model, vendor_key_id, input_payload JSONB, output_payload JSONB, frozen_micro BIGINT, actual_micro BIGINT, provider_task_id, created_at, updated_at, settled_at TIMESTAMPTZ NULL)`. `worker_registrations(worker_id TEXT PK, hostname, status, start_time TIMESTAMPTZ, last_heartbeat TIMESTAMPTZ, metadata JSONB)`. Both additive, `IF NOT EXISTS`. |
| `0010_vendor_key_quota_state.sql` | `vendor_keys.quota_availability`, `quota_remaining`, `quota_limit`, `quota_reset_at`, `quota_observed_at`, `quota_source`. Rust is the runtime writer for quota snapshots; Go may read for admin/reporting. |

### 5.2 Redis namespaces

| Namespace | Purpose | Writer | Reader |
|-----------|---------|--------|--------|
| `entity:{ns}:{id}` | Entity cache L1 | `gw-store::loader` (write-back), control plane | `EntityStore::*_or_load` |
| `entity:ipblock:{id}` | IP-block entry cache. Redis hash, 300 s TTL, Moka L1. Preloaded at boot (100-row pages, fail-closed). | `gw-store::loader::load_ipblock` | `EntityStore::ipblock_or_load`, `ipblock_snapshot` (walked by `gw-auth::ip_filter`) |
| `counter:{scope}:{id}:{field}` | Runtime counters (`base_micro`, `deducted_micro`, `frozen_micro`, RPM, TPM) | Go billing service for base/deducted, Rust frozen-delta worker for frozen, `CounterStore::incrby` | `request_filter` available-balance check, RPM guard |
| `events:entity` | Cache-invalidation stream | Control plane | `StreamConsumer` → `EntityStore::invalidate` |
| `events:counter` | Counter event echo (Phase 5) | (deferred) | `StreamConsumer` (observability only in v1) |
| `gateway:instance:{id}:capability` | Gateway capability snapshot. JSON, overwrite key with `PX control_plane.snapshot_ttl_ms`. Includes legacy `api_shapes`, detailed `api_shape_capabilities`, visible disabled command reasons, top-level `kiro_cli`, and `readiness.kiro_cli`. | `gw-gateway::control_plane::StateWriter` | Go control plane |
| `gateway:instance:{id}:runtime` | Slow runtime snapshot. JSON, overwrite key with `PX control_plane.snapshot_ttl_ms`; no stream backlog. Includes `readiness.runtime_snapshot`, `readiness.command_consumer`, and `readiness.kiro_cli`. | `gw-gateway::control_plane::StateWriter` | Go control plane |
| `gateway:instance:{id}:cmd_ack` | Command delivery ACK hash. Field is `<delivery_id>`, value is JSON ACK payload. | `gw-gateway::control_plane::CommandConsumer` | Go control plane |
| `commands:gateway` | Go-owned command delivery notification stream. Entries carry only `command_id`, `delivery_id`, `target_instance_id`, `delivery_mode`, `issued_at_ms`; command body is `entity:gateway_command_delivery:{delivery_id}`. | Go control plane | `gw-gateway::control_plane::CommandConsumer` with per-instance group `group:{instance_id}` by default |
| `entity:gateway_command_delivery:{delivery_id}` | Go-owned per-instance delivery entity hash (`v`, `enc=json`, `d`). Rust reads only its targeted delivery, writes ACK, deletes entity, then XACKs. | Go control plane | `gw-gateway::control_plane::CommandConsumer` |
| `usage:records` | Billing records | `Ledger` flusher (XADD batched), `RedisTaskContract::settle` for `task_usage_v1` payloads | Go billing service |
| `usage:detailed` | Wire-fidelity per-surface archive. 5 entries per request (downstream/upstream × request/response + summary). `encoding` ∈ {raw,lz4,zstd}+json picked by size. Headers + bodies stored raw (no redaction). XADD MAXLEN bounds Redis memory; Go drainer is mandatory consumer for 180d retention. Drop-on-outage (no DLQ). See [`detailed-log`](spec/06-billing-ledger.md). | `DetailedFlusher` (async pool, per-surface XADD) | Go drainer (mandatory) |
| `entity:task:<id>` | Task record hash. Flat fields: `task_id`, `user_id`, `api_key_id`, `status`, `kind`, `model`, `vendor_key_id`, `input_payload` (JSON string), `output_payload`, `frozen_micro`, `actual_micro`, `provider_task_id`, lifecycle timestamps (`created_at_ms`, `updated_at_ms`, `settled_at_ms`, `lease_until_ms`). Written by `RedisTaskContract::submit`, updated by worker loops. | `gw-task` (`RedisTaskContract`), `gw-task-worker` | Gateway task endpoints (load/watch/cancel), worker exec/cancel/watchdog loops |
| `tasks:queue` | Redis Stream. Task submissions (`XADD`). Each entry carries `task_id`; workers consume via `XREADGROUP` with a shared consumer group. | Gateway `request_filter` (task submit path) | `gw-task-worker` `ExecLoop::claim` |
| `tasks:cancel` | Redis Stream. Cancellation fan-out (`XADD`). Each entry carries `task_id`; workers consume via `XREADGROUP` (`tasks-cancel-fanout` group). | Gateway cancel endpoint | `gw-task-worker` `CancelLoop` |
| `tasks:leases` | Sorted set. Score = `lease_until_ms`. Workers `ZADD` on claim and heartbeat renewal; `WatchdogLoop` scans expired entries via `ZRANGEBYSCORE` and recovers them. | `gw-task-worker` (claim, heartbeat, watchdog) | `gw-task-worker` `WatchdogLoop` |
| `counter:user:<uid>:task_frozen_micro` | Scalar task freeze counter. Submit `INCRBY`s by the estimated task freeze; settle/release decrements by the frozen amount. | Gateway task submit, `RedisTaskContract::settle` | Gateway task submit, Go billing/drain |
| `counter:user:<uid>:deducted_micro` | Scalar task actual deduction counter. Task settle increments by `actual_micro`. | `RedisTaskContract::settle` | Gateway available-balance check, Go billing/drain |
| `counter:user:<uid>:task_frozen_tasks` | Set of task ids with active task freezes. Submit `SADD`s; settle/release `SREM`s. | Gateway task submit, `RedisTaskContract::settle` | Operators / reconciliation |
| `lock:tasks:watchdog` | Redis leader lock (`SET NX PX`, default 10 s TTL) gating expired-lease scans. | `gw-task-worker` `WatchdogLoop` | `gw-task-worker` `WatchdogLoop` |
| `entity:worker:<uuid>` | Worker registration hash. Fields: `worker_id`, `start_time_ms`, `status`, `hostname`, `last_heartbeat_ms`. **Observability only** — not used for routing or service discovery. Written at worker startup, refreshed by worker heartbeat loop. | `gw-task-worker` | Operators (Redis inspection) |
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
                                         AdaptiveRpmManager + decay loop
                                         EntityStore + Loader (+ vendor_key evict hook)
                                         KeyHealthStore, InFlightStore,
                                          RateLimitGuard, CounterStore
                                         Ledger::start         (spawned task)
                                         StreamConsumer::start (spawned task)
                                         control_plane::start  (spawned tasks, if enabled)
                                         ip_blocks preload     (100-row pages, fail-closed)
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

**Gateway process (`gw-gateway`):**

| Port | Server | Module |
|------|--------|--------|
| 8000 | Pingora business proxy | `gw-proxy::LlmGateway` |
| 9090 | Prometheus `/metrics` | `gw-gateway::metrics_catalog` |
| 8080 | Health HTTP — `/livez`, `/readyz`, `/healthz/circuits`, `/healthz/key/{id}`, `/healthz/resilience`, `/healthz/strategies` | `gw-gateway::health` |

**Task worker process (`gw-task-worker`):**

| Port | Server | Module |
|------|--------|--------|
| 9091 | Prometheus `/metrics` | `gw-task-worker::task_metrics` |
| 8081 | Health HTTP — `/livez`, `/readyz` | `gw-task-worker` |

The `/healthz/{circuits,key/<id>,resilience,strategies}` admin routes verify
`Authorization: Mgmt mt_<id>.<secret>` against the same
`RedisJwtBlacklist::check_mgmt` and `EntityStore::mgmt_token_or_load`
chain the proxy `request_filter` uses (FailClosed). The required scope
is `MgmtScope::AdminRead`; finer AdminFull/AdminRead splits are enforced
by the Go control plane. On success the route returns the matching
`snapshot_json` body (`application/json`). `/healthz/strategies` returns
`[{key_id, format, p50_ttft_ms}]` per entry in the `LatencyTracker`'s
`by_key_format` DashMap, exposing per-(key, format) p50 TTFT for
`LatencyFirst` strategy debugging; cold start (no samples yet) returns
`[]`. `/livez` and `/readyz`
remain unauthenticated.

### 6.2 Task worker deployment model

The task system is deployable as **N gateway processes + M worker processes** with
**Redis-only coordination**. `gw-gateway` accepts client traffic, writes task
submissions/cancellations to Redis, and reads task state back from Redis.
`gw-task-worker` consumes the same Redis namespace and never exposes a business
HTTP API to the gateway.

| Topology | Supported | Why |
|----------|-----------|-----|
| 1 gateway × 1 worker | yes | Local/dev baseline. |
| 1 gateway × N workers | yes | Redis Stream consumer-group fan-out distributes claims automatically. |
| N gateways × 1 worker | yes | Multiple gateways can enqueue into the same `tasks:queue` stream. |
| N gateways × M workers | yes | Default production shape. |
| Cross-AZ / cross-network / cross-cluster | yes | Only shared requirement is reachability to the same Redis namespace (and compatible task schema version). |

Hard constraints from the task-worker ADRs:

- **No gateway ↔ worker direct communication.** No per-worker host/port, no HTTP
  reverse proxy, no service-discovery table.
- **Gateway/worker startup order is independent.** With `[task].enabled = false`
  by default in `gw-gateway`, the gateway still boots normally without any task
  worker present. When workers are absent, submitted tasks remain queued in
  Redis until a worker appears. Conversely, `gw-task-worker` can start first and
  idle against Redis until a gateway later enqueues work.
- **Horizontal scaling is peer-blind.** Adding a gateway only adds another Redis
  writer/reader. Adding a worker only adds another consumer-group member.
  Neither side requires reconfiguration of the other.
- **Rolling restarts are peer-blind.** Gateway restarts only affect client
  connections. Worker restarts rely on lease expiry/recovery inside Redis; the
  gateway does not track worker instance identity.

Runtime coupling is limited to these Redis surfaces:

- Streams: `tasks:queue`, `tasks:cancel`
- Entities: `entity:task:*`, `entity:worker:*`
- Counters/lease release keys: task freeze + lease bookkeeping

The worker's `entity:worker:*` registration is **observability only**. It is not
used for request routing or service discovery; task execution remains driven by
Redis queue/lease state alone.

### 6.3 Task error codes

Task endpoints emit four dedicated error codes from `gw-core::error::codes`:

| Code | HTTP | Trigger |
|------|------|---------|
| `GW_TASK_SUBMIT_FAILED` | 500 | `TaskContract::submit` Redis write failed (XADD or HSET). |
| `GW_TASK_NOT_FOUND` | 404 | `TaskContract::load` or `watch` returns `None` for the requested task ID. |
| `GW_TASK_FREEZE_FAILED` | 402 | Insufficient available balance to cover the task freeze estimate from `gw-billing::calculate_freeze_amount` after existing Redis task freezes are considered. Checked in `TaskContract::freeze_task` after the shared balance gate. |
| `GW_TASK_CANCELLED` | 200 | Informational: task was cancelled (cancel endpoint returns the settled `TaskRecord` with `status=Cancelled`). |

## 7. Invariants (`scripts/check-invariants.sh`)

Active lints (all green at HEAD):

| Lint | Meaning |
|------|---------|
| `crate_layering` | Supporting must not depend on Business/Execution; gw-routing / gw-capability / gw-tunnel-client / gw-task-runtime (Business) must not depend on Execution; gw-proxy ↮ gw-task-runtime. |
| `no_billing_crate` | `gw-billing` exists as a task-level billing formula crate (§2.1) but supporting crates (`gw-core`, `gw-codec*`, `gw-store`, `gw-auth`, `gw-ledger`) must not depend on it. No per-token cost computation in Rust. |
| `no_proxy_worker_dep` | gw-proxy must not depend on gw-task-runtime. |
| `no_task_runtime_imports_proxy` | gw-task-runtime must not depend on gw-proxy (ADR-T1 forbidden supporting → execution edge). |
| `no_worker_addr_config` | Gateway config/code must not embed worker host/port (ADR-T16). |
| `no_cost_micro` | Rust must not carry `cost_micro` (cost computation lives in Go). |
| `no_billing_cost_per_token` | gw-billing must not contain `cost_per_token` or `price_per_token`; task billing remains duration/base/multiplier only. |
| `no_cli_auth_bypass` | CLI codecs may bypass IR conversion only; `skip_auth` / `bypass_auth` markers must not appear under `gw-codec/src/cli/`. |
| `task_crates_in_arch_doc` | docs/architecture.md must list `gw-task`, `gw-task-runtime`, `gw-task-worker`, and `gw-billing` whenever task-worker boundaries are active. |
| `no_billing_records_stream` | Use `usage:records`, not `billing:records`. |
| `no_cost_in_dlq` | The G11 DLQ wrapper must not introduce `cost_micro` into `gw-ledger/src/flusher.rs` (cost lives in Go). |
| `no_global_hmac_force` | HMAC signing is per-api-key; a workspace-global force flag (ADR-G7-2 v2 follow-up) must not appear. |
| `no_freezeguard_settle` | `FreezeGuard::release` only — `settle()` must not appear. |
| `no_markup_product_scalar` | Carry `markup_chain` (array), not `markup_product` scalar. |
| `no_legacy_breaker` | `BreakerState` / `BreakerStore` retired (replaced by gw-health). |
| `no_legacy_retry` | `gw-routing/src/retry.rs` deleted in Phase 1c. |
| `no_patch_body` | Adapter trait v2 has no `patch_body` / `BodyPatch`. |
| `e2e_fake_no_gw_codec_dep` | E2E fake crates (`e2e-fake-upstream`, `e2e-fake-web`) must not depend on `gw-codec` / `gw-proxy` / `gw-gateway` / `gw-auth` / `gw-store` / `gw-ledger`. `gw-core` (for `ids::*`) is allowed. |
| `no_dead_tunnel_use_alias` | `pub use gw_tunnel_client` in gw-gateway and `TunnelStack` use in gw-proxy must coexist (both-zero or both-nonzero). |
| `tunnel_require_means_hard_fail` | Every `require_tunnel` use must be paired with `GW_TUNNEL_NO_NODE` within 8 lines below (no silent Direct fallback). |
| `no_double_rustls` | Cargo.lock holds exactly one `rustls` version (T4 removed the 0.21 fork). |
| `no_detailed_redaction` | scripts/check-invariants.sh | redact_*/REDACT_HEADER_* names forbidden in gw-ledger/gw-proxy |
| `no_detailed_truncation` | scripts/check-invariants.sh | PREVIEW_MAX_BYTES / MAX_DETAILED_LOG_BYTES forbidden in gw-ledger/gw-proxy |
| `wire_enum_lock` | `ProtocolKind` / `ApiShape` serde + `as_str` + `format_id` wire forms locked against drift. See `crates/gw-core/tests/wire_enum_lock.rs`. |
| `no_cached_body` | `ctx.cached_body` was removed; use `ctx.body` (`RequestBodyView`). Phase B of 2026-05-28-systemic-fixes. |
| `managed_loop_advisory` (warn) | Long-loop `tokio::spawn` in gw-store/gw-ledger should use `ManagedLoop` + `Supervisor::spawn` (see `crates/gw-store/src/managed_loop.rs`). Warn-only during migration. |
| `no_currency_in_lease` | Credit lease modules (`gw-ledger::lease`, `::coef`, `::credit_estimator`, `::counter_handler`) must hold only opaque credits; no currency / pricing references. See `2026-05-29-credit-lease-design.md` §2/§16. |
| `supervisor_universal` | Every long-running tokio task runs under `gw_runtime::Supervisor`. The `managed_loop_advisory` lint is subsumed; new spawns must use `Supervisor::spawn` from the start. |
| `backpressure_opaque_503` | The 503 returned by `BackpressureGate` must never reveal which hook tripped. The response body is the generic `GW_BACKPRESSURE_REJECTED` envelope; no hook-specific error code or header. |
| `backlog_unbounded_by_construction` | `BillingBacklog` is an unbounded `VecDeque` — pressure shows as `gw_billing_backlog_pending_count` rising, not as dropped envelopes. The BackpressureGate at `early_request_filter` is the only shed point; once a `UsageRecord` exists, it must be delivered. |

Pending lints (uncomment when target phase lands):

| Lint | Lands at |
|------|----------|
| `no_ir_runtime_struct` | Phase 2 — atomic with `gw-core::ir` → `gw-core::schema` rename and removal of public constructors / `Default` / `Serialize`+`Deserialize`. |
| `no_gateway_addr_config` | Phase 6 — symmetric to `no_worker_addr_config`. |
| `deploy_e2e_matrix` | Phase 6/7 — N-gateway × M-worker topology acceptance. |

## 8. Deliberate behaviours (not bugs)

| Behaviour | Reason | Source |
|-----------|--------|--------|
| `(x-api-key, x-api-key-id)` pair required | A BYOK gateway can't route by key prefix; the pair makes tenant routing explicit. Anthropic SDK clients must supply both. Missing either yields `GW_AUTH_INVALID_APIKEY` (deliberately generic). | `crates/gw-proxy/src/filters/request.rs` auth extraction |
| `trust_xff = false` by default | With it off, the IP blocklist matches the L4 socket peer. Enabling it behind an untrusted L7 hop lets clients forge XFF and bypass the blocklist. Fail-closed. | `config/default.toml [security]`, `crates/gw-proxy/src/filters/early.rs` |
| Distinct 429 codes | `GW_QUOTA_RPM_EXCEEDED` (gateway tripped per-user RPM) vs `GW_UPSTREAM_OVERLOADED` (upstream returned 429) — clients need to distinguish local vs upstream throttling. | `crates/gw-core/src/error/codes.rs` |
| Body filter swaps at EOS only | Cross-protocol rewrites happen in `upstream_request_filter` and must reach the wire in one shot. Pre-EOS chunks are dropped. | `crates/gw-proxy/src/gateway.rs::request_body_filter` |
| **`RequestBodyView` (`crates/gw-proxy/src/body/view.rs`) is the sole authority on request body bytes.** Three explicit states: `raw` (immutable on-wire), `decoded()` (`OnceCell`-cached lazy decompress, populated by HMAC verify via `prefill_decoded` so codec parse reuses), `outbound_or_raw()` (post-mutation outbound). `cached_body` field was deleted in Phase B of 2026-05-28-systemic-fixes. CI lint `no_cached_body` enforces. | Centralises decompress-once behaviour; eliminates the dual-semantic `cached_body` that required callers to know whether it held raw or decoded bytes. | `crates/gw-proxy/src/body/view.rs`, `crates/gw-proxy/src/filters/request.rs` step 7b → step 8, `scripts/check-invariants.sh` `no_cached_body` |
| **`ProtocolKind` maintains two wire forms by design.** `serde` snake_case (`OpenAi → "open_ai"`) for entity payload JSONB (PG `vendor_keys.payload` etc, Go writes). `as_str()` (`OpenAi → "openai"`) for events stream payload and `model_mapping.protocol` TEXT column. Both forms locked in `crates/gw-core/tests/wire_enum_lock.rs`. | The two consumers (JSONB persistence written by Go vs Redis stream / DB column written by Rust) predate each other; renaming either would require a coordinated Go + Rust + migration deploy. Locked rather than silently drifted. | `crates/gw-core/src/protocol.rs`, `crates/gw-core/tests/wire_enum_lock.rs`, `scripts/check-invariants.sh` `wire_enum_lock` |
| **Mid-stream upstream failure does not emit SSE error event.** `fail_to_proxy` (gateway.rs) observes via `gw_errors_total{kind=upstream_mid_stream}` and truncates the connection. Synthetic `data: {"error":...}` injection is intentionally avoided. | Each SDK's stream parser behaves differently on malformed frames; injecting a synthetic error event would either be silently ignored or cause a second parse error. Truncation is the most portable mid-stream signal. | `crates/gw-proxy/src/gateway.rs::fail_to_proxy`, `gw_errors_total{kind=upstream_mid_stream}`. Spec: 2026-05-28-systemic-fixes-design.md §3.4 |
| `gw-core::ir` not renamed to `::schema` yet | Phase 1d landed the *scaffolding* parts of S1 v2 without breaking the runtime IR path. The strict A10 form lands atomically in Phase 2 alongside the wire→wire matrix migration. | `crates/gw-core/src/ir.rs`, `no_ir_runtime_struct` lint stays PENDING. |
| Daily-quota UTC midnight race | The pre-check in `request_filter` reads `counter:user:{eid}:daily_tokens:{day_N}`, but the post-increment in `logging` may run after the request crosses the UTC midnight boundary and land on day N+1's counter. Affects < 0.001 % of traffic; daily quota is best-effort accounting, not security. | `crates/gw-proxy/src/filters/request.rs` daily-quota pre-check, `crates/gw-proxy/src/filters/logging.rs` post-increment |
| Capability field-name / type asymmetry (`ApiKeyEntry.force_capabilities: SmallVec<[String;4]>` vs `VendorKey.forced_capabilities: Vec<String>`) | G13 (T3-C) reads forced caps from two different entities: the resolver pulls *required* caps from `ApiKeyEntry.force_capabilities` + `ApiKeyEntry.match_mode_exclusive` (caller-side requirement), while the candidate filter reads *advertised* caps from `VendorKey.forced_capabilities` + `VendorKey.exclusive_capability_match` (channel-side capability). The two were introduced by different waves (T4-B vs T0-C) and the naming/collection-type drift is left as-is rather than risk a churny rename; reconciliation is a follow-up. | `crates/gw-capability/src/backend.rs`, `crates/gw-routing/src/candidates.rs` |
| **`require_tunnel = true` is fail-closed.** When a `VendorKey.tunnel_preference.require_tunnel = true` resolves to `Direct` (no node matches the provider/key), `pick_peer` emits `GW_TUNNEL_NO_NODE` rather than silently falling back to Direct. This is enforced by the `tunnel_require_means_hard_fail` invariant. | Fail-closed design: a key that demands a tunnel must not silently use a Direct path. | `crates/gw-proxy/src/filters/upstream.rs`, `scripts/check-invariants.sh` |
| **ReDoS auto-disable is fail-closed.** When `RedosTracker` auto-disables a key's regex after `TIMEOUT_THRESHOLD` (10) consecutive budget timeouts, subsequent requests are denied with `GW_REQUEST_MODEL_NOT_FOUND` 403. The regex is not silently skipped. | A silently-skipped regex would let any model through, defeating the whitelist. Deny is the only safe default. | `crates/gw-auth/src/model_permissions.rs`, `crates/gw-proxy/src/filters/request.rs` step 11 |
| **`bail!` macro wraps `emit_and_bail` + `unreachable!()`.** The `request_filter` defines a local `bail!` macro that calls `emit_and_bail(session, ctx, err, rid).await?` followed by `unreachable!()`. The `unreachable!()` is never reached because `emit_and_bail` always returns `Err`, which the `?` operator propagates. The macro centralizes this two-line pattern across 26+ call sites. | Without the macro, every error exit would duplicate `emit_and_bail(...).await?; unreachable!()`. The `unreachable!()` satisfies the type checker (the block must produce `!` or a value). | `crates/gw-proxy/src/filters/request.rs:183` |
| **Client disconnection does not cancel billing.** `ClientDelivery::Disconnected` only triggers upstream cancellation; it does not prevent `UsageRecord` emission. | The vendor has already charged; we are obligated to bill the customer. Decouples client delivery from billing lifecycle. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **Crash-recovery fallback `UsageRecord` underestimates output.** `CrashFallback` state emits `output_tokens=0`, `input_tokens=prompt_estimate`; **this is a known underestimate**. | Partial usage in-flight on crash exists only in RAM; Redis holds only prompt-estimate baseline. Production deployments must ensure MTBF >> single-request duration to keep this path rare. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **must-deliver (`usage:records`) vs drop-on-outage (`usage:detailed`) are strictly separated.** `usage:records` mpsc full → synchronous XADD bypass (may briefly block `logging` filter); `usage:detailed` mpsc full → drop immediately. Different strategies reflect different invariants (accounting must arrive; archival may be lost). | Coupling billing reliability to observability reliability would require observability to never fail, an unreasonable constraint. Strict separation preserves both subsystem invariants. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **Inflight key DEL is performed by the ledger, not the proxy.** The proxy never calls `inflight.clear` on the request hot path. The flusher DELs the key after a successful XADD on `usage:records`; the `sync_xadd` bypass paths (Full / Closed) follow the same rule. Crash-after-XADD-before-DEL produces a `CrashFallback` duplicate that Go dedups by `(request_id, attempt_index)` + provenance level. | Decouples "record durability" from "ledger key cleanup" — the proxy emits and forgets; the ledger owns the cleanup contract. Makes the recovery invariant ("record lost ⇒ inflight key remains") trivially derivable. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **T0 emit-lag watchdog does not self-heal.** When settled `UsageRecord` is not emitted within 60s, the watchdog increments `gw_billing_emit_lag_t0_total` and logs error but **does not auto-emit fallback**. | Self-healing would double-count with the eventual real emit; operator intervention to investigate root cause is preferable. Real account state is preserved in `InflightLedger` for 24h. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **Deduplication of `UsageRecord` is delegated to Go.** Rust may emit multiple records for the same `(request_id, attempt_index)` (crash-recovery fallback + true emit coexisting), without idempotency check. Go settler deduplicates by provenance priority. | Rust-side idempotency requires cross-process coordination, higher complexity. Go-side dedup with provenance cascade is simpler and works correctly regardless of Rust emit ordering. | [`docs/spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **HMAC hard cutover**: `ApiKeyEntry.signature_secret` is the sole HMAC secret field. The v1 `hash-as-HMAC` path is removed. No dual-key fallback — existing deployments must populate `signature_secret` before enabling HMAC. Breaking change per overview §1.3. | `hash` served double duty (credential verify + HMAC), creating a covert channel where changing the API-key secret also rotated the HMAC key. `signature_secret` decouples the two concerns. Hard cutover avoids the complexity and risk of a dual-key transition period. | `crates/gw-proxy/src/filters/request.rs` step 3, `crates/gw-auth/src/signature.rs`, S-proxy-core H-1 |
| **CLI transparent proxy (body passthrough, no IR conversion).** CLI variants (`claude:cli`, `openai:cli`, `openai:compact`, `gemini:cli`) bypass the `parse_request → RequestIR → render_request` and `parse_response → ResponseIR → render_response` codec paths. Request and response bodies are forwarded verbatim. The full filter chain (auth, permission merge, RPM, balance, freeze, billing) still executes; only the codec conversion is skipped. Model name and stream flag are extracted via a shallow JSON parse (`CliCodec::extract_hints`) or URL path inspection, not full IR deserialization. Token usage is extracted from raw SSE events or non-stream JSON by `CliUsageExtractor`. | CLI tools (Claude Code, OpenAI Codex, Gemini CLI) send vendor-proprietary fields (thinking signatures, cache TTL hints, tool definitions) that the IR cannot losslessly represent. Passthrough preserves wire fidelity. The zero-copy path is also faster. | `crates/gw-proxy/src/filters/request.rs` step 8 (CLI branch), `crates/gw-proxy/src/filters/upstream.rs` (CLI passthrough), `crates/gw-proxy/src/filters/response.rs` (CLI body filter). Spec: [`spec/01-protocol-codec.md`](spec/01-protocol-codec.md). |
| **Gemini CLI impersonation deliberately uses the Chrome 131 profile.** The shipped `GeminiCli` profile is Chrome-class rather than Node/undici-class. A `GeminiNodeUndici` profile is only added if Google starts blocking Chrome-class fingerprints for Gemini CLI traffic. | Empirically, Chrome-class fingerprints have the highest Google edge pass-rate; over-impersonation is the safer compatibility choice until production evidence says otherwise. | `crates/gw-core/src/impersonate/gemini_cli.rs`, `crates/gw-egress-impersonate` profile selection. Gap record: `docs/issues/egress-impersonate-known-gaps.md` EI-3. |
| **CLI cross-protocol requests are denied, not converted.** When a CLI request's `ApiShape` maps to a `ProtocolKind` that no available vendor key supports, the gateway returns `503 GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` rather than attempting cross-protocol body conversion. Metric: `gw_cli_cross_protocol_denied_total`. | CLI bodies are not IR-parsed, so cross-protocol re-render is impossible. Silent fallback to a different protocol would produce responses the CLI tool cannot parse. | `crates/gw-proxy/src/filters/upstream.rs` candidate filter, `crates/gw-core/src/error/codes.rs`. Spec: ADR-CLI-8. |
| **Gemini CLI impersonation uses a Chrome 131 profile.** The Gemini CLI in the wild is Node + undici, but the shipped `GeminiCli` egress profile intentionally impersonates Chrome 131. Add a `GeminiNodeUndici` profile only if Google starts blocking Chrome-class fingerprints for Gemini CLI traffic. | This is deliberate over-impersonation: Chrome-class TLS / HTTP-2 fingerprints have the highest observed Google edge pass-rate, and over-impersonating toward a browser is safer than under-impersonating toward a bot-like Node fingerprint. | `crates/gw-core/src/impersonate/gemini_cli.rs`, `docs/issues/egress-impersonate-known-gaps.md` gap #3 |
| **`ServerToolBlockIR` uses typed wrapper with opaque `extra: serde_json::Map`.** Server tool blocks (Anthropic `server_tool_use`, `server_tool_result`, `mcp_tool_use`, `mcp_tool_result`; OpenAI Responses server-tool output items; Gemini `executableCode` / `codeExecutionResult`) carry typed `kind`, `id`, `name`, `input: Box<RawValue>` plus an opaque `extra: Map<String, Value>` for vendor-specific keys. The `input` field is `RawValue`, not `Value`, because Anthropic `#[serde(tag = "type")]` internally-tagged enums cannot directly deserialize `Box<RawValue>` fields, so a small `with` adapter buffers through `Value` and reconstitutes `RawValue`. | Full `Value` passthrough would lose the ability to distinguish "no input" from "empty object". Typed wrapper + opaque `extra` preserves known fields for cross-render gating while forwarding vendor-proprietary keys losslessly on same-protocol round-trips. | `crates/gw-core/src/ir.rs:270-276`, `crates/gw-codec/src/anthropic/via.rs`, `crates/gw-codec/src/gemini/via.rs`, `crates/gw-codec/src/openai/responses/via.rs` |
| **Anthropic `#[serde(other)] Unknown` content block variant.** The `AnthropicContent` enum carries an `Unknown` variant via `#[serde(other)]`. When Anthropic adds new block types the gateway doesn't yet model, deserialization succeeds (no parse error), the block is skipped in IR conversion with a `tracing::warn!`, and same-protocol passthrough preserves it through the `#[serde(flatten)] extra` map. | Hard-failing on unknown block types would break forward compatibility when Anthropic ships new content types. Warn-and-skip matches the "fail open for unknown extensions, fail closed for security" principle. | `crates/gw-codec/src/anthropic/wire.rs` (`AnthropicContent` enum) |
| **Gemini `grounding_metadata` stored in `ResponseIR.metadata.extensions["grounding_metadata"]`.** Gemini's `groundingMetadata` response field is an opaque `serde_json::Value` passed through `MetadataIR.extensions` rather than a dedicated IR struct. Cross-render drops it when the target lacks `supports_grounding` (only Gemini has it). | The `groundingMetadata` shape is complex and Gemini-specific (search queries, search results, web segments, support chunks). Modelling it as a typed struct would couple the IR to a single vendor's evolving schema. Opaque passthrough is correct for same-protocol, and cross-render gating handles the rest. | `crates/gw-codec/src/gemini/via.rs`, `crates/gw-codec/src/cross_render.rs` |
| **OpenAI audio response stored in `metadata.extensions["openai_audio"]`.** When OpenAI returns an audio response object (`message.audio: { id, data, expires_at, transcript }`), it is projected into `MetadataIR.extensions["openai_audio"]` as a JSON object rather than a dedicated `ResponseIR` field. Cross-render drops it (and `UsageIR.audio_tokens_in/out`) when the target lacks `supports_audio`. | Audio responses are OpenAI-specific and carry binary-encoded audio data. A dedicated IR field would force all protocols to acknowledge a concept only one vendor supports. The extensions map keeps the IR protocol-neutral. | `crates/gw-codec/src/openai/via.rs`, `crates/gw-codec/src/cross_render.rs` |
| **Protected-mode lease fails closed on TTL expiry.** When Go push pipeline stops, all requests for protected users return `503 GW_QUOTA_LEASE_EXPIRED` rather than silently falling back to soft defense. `GW_QUOTA_LEASE_EXPIRED` is the operator signal that the lease is stale; `GW_QUOTA_LEASE_INSUFFICIENT` (402) is the per-request credit signal. | Silent fallback to soft defense on a stale lease would defeat the hard-cap guarantee. Fail-closed is the only safe default when the Go pipeline is stalled. | Spec [`spec/06-billing-ledger.md`](spec/06-billing-ledger.md) |
| **Admit-time status gates enforce `ApiKeyStatus::Disabled` / `UserStatus::Banned`.** A key/user flipped via `op=status_change` (row retained, `deleted_at` NULL) is rejected on the business path: `GW_AUTH_KEY_DISABLED` (401) / `GW_AUTH_USER_BANNED` (403). `merge_permissions` additionally rejects a banned PARENT (sub-account owner). | The loader filters only `deleted_at`, so a disabled/banned row loads normally; before the gate, disable/ban could not be enforced in-band. Audit 2026-05-30 §1.1/§3.2. | `crates/gw-proxy/src/filters/request.rs` (`check_api_key_status`/`check_user_status`), `crates/gw-auth/src/merge.rs` |
| **Client JWT revocation is enforce-when-present.** A client JWT carrying a `jti` in the `jwt:blacklist:` namespace is rejected (`GW_AUTH_JWT_REVOKED` 401, FailClosed on Redis outage). A token with NO `jti` passes — the blacklist is `jti`-keyed, so a `jti`-less token is inherently non-revocable. | Rejecting every `jti`-less token would mass-break IdPs that omit `jti`; checking only when present closes the revocable-token hole without that collateral breakage. Audit 2026-05-30 §3.1. | `crates/gw-proxy/src/filters/request.rs` JWT branch, `crates/gw-auth/src/jwt.rs` |
| **HMAC required-but-unconfigured fails closed (500).** When signing is required but `signature_secret` is empty/NULL, the request is rejected with `GW_AUTH_SIGNATURE_NOT_CONFIGURED` (500) rather than verified against an empty HMAC key. | HMAC accepts a zero-length key, so verifying against it is forgeable by any client. A server misconfig must fail closed, not silently fall open. Audit 2026-05-30 §1.6. | `crates/gw-auth/src/signature.rs` (`SignatureCheckResult::SecretUnavailable`), `crates/gw-proxy/src/filters/request.rs` |
| **Unknown `multi_dim` routing preset falls back to `fixed_order` (does not panic).** A model group referencing a preset absent from this pod's config routes via `fixed_order` with a warn-once + `gw_routing_unknown_preset_total{code="GW_ROUTING_UNKNOWN_PRESET"}`, instead of panicking the request task. The runtime path stays fail-operational; operator typos in a pod's OWN `[routing.presets]` (all-zero / negative / non-finite weights) are instead caught fail-closed at boot by `Config::validate`. `GW_ROUTING_UNKNOWN_PRESET` is a catalogued diagnostic code, not a returned client envelope. | Control-plane skew (preset in DB, not yet in a pod's config) is normal; a per-request `panic!`/5xx is a worse failure mode than a deterministic fallback. Fail-operational for availability; fail-closed at boot for the pod's own config. Audit 2026-05-30 §2.1 + functional-audit #11. | `crates/gw-routing/src/strategy/set.rs::pick_for`, `crates/gw-gateway/src/config.rs::Config::validate`, `crates/gw-core/src/error/codes.rs::GW_ROUTING_UNKNOWN_PRESET` |
| **Direct (Rustls) per-key HTTP proxy is fail-closed, not silently bypassed.** A Direct vendor key with `proxy`/`default_proxy` set returns `GW_UPSTREAM_PROXY_UNSUPPORTED` (502); proxied egress must use `egress_mode = Impersonate` (whose loopback honors `X-Egress-Proxy`). | pingora 0.8's `Proxy.next_hop` is UDS-only and cannot route an HTTP proxy URL; silently connecting direct would leak the gateway's egress IP and violate the operator's routing policy. Audit 2026-05-30 §3.4. | `crates/gw-proxy/src/filters/upstream.rs::pick_peer` |
| **Adaptive per-vendor-key RPM is enforced at `pick_peer`, not `request_filter`.** The learned ceiling is applied via `AdaptiveRpmManager::try_admit` when the key is picked; an over-ceiling key is diverted to another candidate, degrading to the pick only if all candidates are hot. The per-user RPM bucket stays in `request_filter`. | The vendor key is unknown during `request_filter`, so the former clamp there was dead code. Enforcing back-pressure where the key is known makes the learned limit real; degrading (not hard-rejecting) keeps the user-RPM + cooldown as the binding limits. Audit 2026-05-30 §1.8. | `crates/gw-auth/src/ratelimit/adaptive.rs`, `crates/gw-proxy/src/filters/upstream.rs` |
| **Cross-protocol re-render uses one health/codec format resolver.** `gw_core::protocol::resolve_upstream_format` is the single source of truth used by the routing health gate (read) and the codec/health record path (write), so a protocol-conversion key's circuit/cooldown is keyed to the bucket selection consults. | Deriving the upstream format independently in two places let cross-protocol keys be selected under the client bucket while cooldown/circuit landed in the upstream bucket — making the safety net inert. Audit 2026-05-30 §1.5/§3.10. | `crates/gw-core/src/protocol.rs`, `crates/gw-routing/src/pipeline/mod.rs`, `crates/gw-proxy/src/filters/{upstream,logging}.rs` |
| **`/livez` remains 200 during drain.** When `ShutdownCoordinator` enters the AcceptStop or TaskDrain phase, `/livez` still returns 200 while `/readyz` returns 503. This is deliberate so that k8s does not restart the Pod and lose the in-memory `BillingBacklog`. `/readyz` is the only endpoint k8s should consult for traffic routing. | A Pod restart during drain would discard unflushed `BillingBacklog` entries. Keeping `/livez` at 200 prevents the kubelet from killing the process before billing data is fully drained. | `crates/gw-runtime/src/shutdown.rs`, `crates/gw-gateway/src/health.rs` |
| **503 under backpressure carries `Retry-After: 2`.** When `BackpressureGate` rejects a request (memory / FD / disk / Redis-OOM at 99%), the response includes `Retry-After: 2`. The 2-second window is deliberately short: the gateway wants clients to retry quickly so existing in-flight work can drain the resource pressure. | Longer retry delays would hold pressure longer; short delays give the system the best chance to self-heal once the force-majeure condition clears. | `crates/gw-runtime/src/health/mod.rs`, `crates/gw-proxy/src/filters/early.rs` |
| **SpillFile cleanup is dual-pathed.** `SpillFile::Drop` synchronously unlinks shard files via `std::fs::remove_file` (Drop is sync; async spawn would race with same-`request_id` retries). The `SpillRoot::spawn_quota_enforcer` LRU loop is the fallback for shards Drop missed (process kill before Drop runs, OS-level issues). Both paths treat `NotFound` as silent success; the system is intentionally idempotent. | Timely Drop prevents disk leak under churn; LRU is safety net for cases Drop cannot run. Dual path with idempotent `NotFound` handling avoids double-remove failures. | `crates/gw-runtime/src/spill_io.rs`, `crates/gw-proxy/src/gateway.rs::reset_response_attempt_state` |
| **DeductGc is panic-tolerant via Supervisor.** A panic in the sweep loop is recovered by `gw_runtime::Supervisor` with exponential-backoff restart (default `max_restarts=5` in a 60s window). If the restart budget is exhausted the supervisor marks itself unhealthy and exits — `DEDUCT` then accumulates zero-state entries until next restart; this is a deliberately bounded failure mode rather than silent death. | Before the Supervisor wrap, a panic in the sweep loop silently killed the spawned task, causing unbounded DEDUCT growth. The Supervisor makes the failure noisy (metric + unhealthy flag) and self-recovering within the restart budget. | `crates/gw-ledger/src/deduct_gc.rs`, `crates/gw-runtime/src/supervisor.rs` |
| **USER_CIRCUIT Closed entries are never swept.** Without a creation timestamp on the `Closed` variant we cannot judge staleness. The eviction policy is intentionally narrow: only `Open { until, .. }` past `until + 5 min` grace is removed. Closed entries currently never appear in the codebase (no writers); if a writer is introduced, the eviction policy needs a follow-up design decision. | Sweeping Closed entries without a timestamp would either evict all of them (over-aggressive) or none of them (no effect). The narrow policy avoids both pitfalls while still providing forward-compatible eviction for the Open variant that future writers will produce. | `crates/gw-store/src/deduction.rs::sweep_user_circuit`, `crates/gw-store/src/deduction.rs::USER_CIRCUIT_STALE_GRACE` |
| **LEASE_POOL: Unprotected idle eviction.** Entries whose `mode == Unprotected` and whose `last_seen_at_ms` is older than `LEASE_POOL_IDLE_TTL_MS` (30 min) are evicted by `ledger_sweep`. Protected entries are never evicted. In-flight `LeaseGuard` holds an `Arc<LeaseState>` so eviction is safe with respect to already-acquired guards (the Arc refcount keeps the state alive until the guard drops). | Without eviction, users who leave protected mode and never return accumulate stale Unprotected entries indefinitely. The 30-min TTL is conservative; a user who re-enters protected mode gets a fresh entry via `upsert_protected`. | `crates/gw-ledger/src/lease.rs::sweep_unprotected`, `crates/gw-ledger/src/ledger_sweep.rs` |
| **COEF_STAGING orphan eviction is version-based.** Entries whose `version` is strictly less than `active_version - COEF_STAGING_SAFETY_MARGIN` (100) are evicted by `ledger_sweep`. Wall-clock TTL was rejected (the codebase has a documented workaround for system-clock failures in `lease.rs::now_ms`). The single caller of `coef::lookup` is `credit_estimator` — at admission and settle. The SAFETY_MARGIN window exceeds the number of coef commits expected during a single request's settle, so an in-flight `LeaseGuard`'s captured `coef_version` is preserved. | Orphan staging versions (entries pushed by `apply_coef_entry` whose commit never arrived) would accumulate without bound. The version-based threshold ensures in-flight Go pushes are preserved (SAFETY_MARGIN exceeds max expected push depth) while catching truly abandoned versions. | `crates/gw-ledger/src/coef.rs::sweep_orphans`, `crates/gw-ledger/src/ledger_sweep.rs` |
| **LeaseGuard::Drop intentionally does NOT refund credits in this work.** A panic between `try_acquire` and `settle_actual` leaves the in-process `LeaseState.credits` counter lower than the true available value. Go's event-driven `protected_lease` pushes overwrite this counter periodically, bounding the false-rejection window. If operational evidence later shows the window is too long, a follow-up adds a Drop refund mirroring `RequestFreeze::Drop`'s pattern. (Recorded 2026-06-12 per user decision; see scope-correction in `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 3.) | A Drop refund would need to handle partial-settle states correctly and risk over-refunding if `settle_actual` has already been called. The Go reconciliation loop bounds the damage; adding Drop logic is deferred until there is evidence it is needed. | `crates/gw-ledger/src/lease.rs::LeaseGuard::Drop`, `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` |
| **Rate limiters are TTI-bounded, not persistent.** RPM/TPM sliding-window state lives in `moka::sync::Cache` with `time_to_idle` (1 hour default). A user that exhausts RPM and idles past TTI gets a fresh limiter on return. Note: `per_user_streams` (live concurrency counter) and `per_key_overrides` (admin policy) are NOT TTI-bounded — they use DashMap with RAII / persistent semantics respectively. | Unbounded DashMaps would accumulate entries for every user/key that ever made a request; Moka's capacity + TTI bounds the memory while the TTI boundary trade-off is acceptable (idle users getting fresh windows is less harmful than OOM from unbounded growth). The two exceptions are deliberate: live concurrency counters cannot tolerate TTI eviction (would bypass the cap), and admin policy must persist until explicit reset. | `crates/gw-auth/src/ratelimit/{mod,adaptive}.rs`, `crates/gw-auth/src/model_permissions.rs::RedosTracker` |
| **Routing sweep evicts on staleness, not capacity.** All four routing-side sweeps use age-based thresholds (default 1h) instead of LRU caps. Cardinality is bounded by configured key/model count + observed instance count from the Redis events:reservation stream; per-instance cardinality is recovered as remote instances time out. | Capacity-based eviction would discard hot entries under churn, potentially re-learning costs on the next request cycle. Staleness-based eviction is a better fit for routing state where "not heard from" is the meaningful signal. | `crates/gw-routing/src/routing_sweep.rs` |
| **`InFlightStore::count` is non-allocating.** A read-only query for an unknown key returns 0 without inserting. | The previous `get_or_create` semantic allocated an `InFlightCounter` for every distinct `KeyId` queried — a leak path when arbitrary keys are probed. The new semantic delegates to `count_if_present`, which reads without side-effects. | `crates/gw-routing/src/in_flight.rs` |
| **`Capability::Tagged` strings are bounded-interned.** Up to `CAPABILITY_INTERN_CAPACITY = 10_000` distinct unknown capability strings share a global `Arc<str>` via a Moka cache. LRU eviction at capacity is benign because `Capability::PartialEq` / `Hash` compare by `str` content — an evicted tag re-interned later compares equal to a live `Tagged` value with the same content. The previous `Box::leak`-per-unknown-string path is replaced. | Unbounded `Box::leak` per attacker-controllable header string was an OOM risk. The intern table bounds the worst case; re-interning after eviction is cheap and correctness is preserved by `Arc<str>`'s `str`-delegating `Eq`/`Hash`. | `crates/gw-capability/src/resolver.rs`, `crates/gw-capability/src/types.rs` |
| **Redis pool drain.** The bootstrap registers a small `tokio::spawn` task that awaits `ShutdownToken::cancelled()` then calls `RedisPool::quit()`. Its JoinHandle is registered with `ShutdownCoordinator`, so drain awaits the quit's completion. Connections in the fred pool close gracefully instead of relying on process exit. | Without `quit()`, fred's internal connection slots are dropped when the process exits, which may leave the server-side connection in a half-closed state. Graceful `quit()` sends `QUIT` to each Redis connection so the server releases resources promptly. | `crates/gw-gateway/src/lib.rs`, `crates/gw-runtime/src/shutdown.rs` |
| **Tunnel probe loop drain.** `TunnelStack::start_probe_loop` accepts a `ShutdownToken`; its `tokio::select!` cancellation arm exits the loop cleanly. The bootstrap registers the JoinHandle with `ShutdownCoordinator`. | The previous bare `tokio::spawn` infinite loop held `Arc<TunnelStack>` and transitively `Arc<DashMap<String, Arc<ReconnectingTransport>>>` for the entire runtime lifetime even after shutdown was requested. The cancellation arm releases the Arc chain on drain. | `crates/gw-proxy/src/tunnel_stack.rs`, `crates/gw-gateway/src/lib.rs`, `crates/gw-runtime/src/shutdown.rs` |

## 9. Known gaps (closed by `feature/known-gaps-closure`)

The 13 gaps G1-G13 catalogued in [`docs/ROADMAP.md`](ROADMAP.md) all landed across Waves 0-4 (see [`docs/ROADMAP.md`](ROADMAP.md) for current status). One-line summary:

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

### 9.1 Open follow-ups

- **G7**: workspace-global `signature_required` toggle (ADR-G7-2). *(The compressed-body parse gap — decompressed bytes not reused by codec — was closed in Phase B of 2026-05-28-systemic-fixes via `RequestBodyView::prefill_decoded`.)*
- **G8**: direct wire-to-wire codegen (codec-s3 N²) replacing the IR round-trip; multi-event SSE buffering across Pingora chunk boundaries. *(Cross-protocol non-streaming response body re-render landed in S-codec v2, 2026-05-24; entry trimmed accordingly.)*
- **G11**: ~~three-layer fallback (in-memory + on-disk file + periodic drainer + startup recovery) → billing-c-delta.~~ **Closed by resilience-fixes (T4.1–T4.5):** `EmergencyDlqWriter` async over sharded `SpillFile`, `BillingBacklog` (unbounded `VecDeque`), cascade Redis → SpillFile → BillingBacklog + drainer, `BacklogRecovery` on bootstrap, shutdown dump + exit code 2.
- **G3**: per-tenant timezone for the daily-quota day window (currently UTC-only).
- **G13**: reconcile `ApiKeyEntry.force_capabilities` (SmallVec, singular `force_`) vs `VendorKey.forced_capabilities` (Vec, plural) into one breaking rename.
- **Credit lease crash-recovery gap**: crash-recovery `UsageRecord`s (`inflight_reconciler`) currently emit `credits_consumed: None`, so Go skips the outstanding-ledger decrement on crash-recovered protected-mode requests. Persistence of `credits_consumed` + `coef_table_version` in the inflight hash would close this. Future PR.

### 9.2 Closed gaps (2026-05-29)

- **A1/A2/A3 (billing record durability)** — proxy-side `inflight.clear` was racing the async XADD; flusher panic could silently drop records. Closed by [`billing-record-durability`](spec/06-billing-ledger.md). Proxy no longer calls `inflight.clear`; ledger owns DEL after XADD-confirmed delivery; flusher panics are now isolated via `catch_unwind`; Closed branch of `Ledger::emit` falls back to `sync_xadd` instead of silently dropping.


Per-filter coverage that runs without Docker lives in `e2e_failure_modes.rs`. The docker-based black-box harness lands on `feature/e2e-blackbox` (described below).

- Tunnel data-path round-trip e2e test: W8 covers owned server/client round trip in `crates/gw-gateway/tests/e2e_tunnel.rs` (ignored local-listener tests) and focused bridge behavior in `gw-proxy` tunnel bridge tests.

## 10. End-to-end testing — the black-box harness

The docker-based E2E harness exercises the **unmodified production binary** through real network sockets, a real Redis + Postgres, real `events:entity` / `usage:records` streams, and the three real vendor SDKs. There is no test hook or conditional compilation — if a behaviour can be exercised here, it is real; if it cannot, no v1 deployment can either. This harness is described in this document (see above).

### 10.1 Three-actor model

Three **fakes** sit at the gateway's external boundaries; the gateway itself is the production binary under test.

```
                                  Docker network: aether-e2e
   ┌──────────────────────┐     ┌──────────────────────┐
   │  fake_client (host)  │     │  fake_web_server     │
   │  Python + 3 SDKs     │     │  Rust binary         │
   │  (openai / anthropic │     │  Simulates Go web    │
   │   / google-genai)    │     │  boundary            │
   └──────────┬───────────┘     └──────────┬───────────┘
              │ HTTP                       │ Postgres write
              │ (port 8000)                │ Redis XADD/XREAD
              ▼                            ▼
        ┌──────────────────────────────────────┐
        │  gw-gateway (under test)             │
        │  Production binary, no test hooks    │
        │  Ports: 8000/9090/8080               │
        └────────────────┬──────────────┬──────┘
                         │              │
                    TCP/TLS         Redis/PG
                         │              │
                         ▼              ▼
        ┌──────────────────────┐   ┌──────────────┐
        │  fake_upstream_server│   │ redis + pg   │
        │  Rust binary         │   │ (official    │
        │  Simulates OpenAI /  │   │  containers) │
        │  Anthropic / Gemini  │   └──────────────┘
        └──────────────────────┘
```

### 10.2 Where the components live

| Component | Path | What it is |
|-----------|------|------------|
| `fake_upstream_server` | [`crates/e2e-fake-upstream/`](../crates/e2e-fake-upstream/) | Rust (`hyper`, no Pingora). Scenario-driven HTTP server with a JSON/TOML DSL and TCP failure-injection primitives (RST, half-write, slow drip, 4xx/5xx, `Retry-After`, malformed SSE, `Content-Length` truncation). |
| `fake_web_server` | [`crates/e2e-fake-web/`](../crates/e2e-fake-web/) | Rust (`axum` admin API on port 7000 + `usage:records` drain loop). Writes entity rows to Postgres, emits `events:entity`, writes back `counter:user:*:balance_micro` from a mock cost computation. |
| `fake_client` harness | [`tests/e2e/client/`](../tests/e2e/client/) | Python `pytest` harness — 5 scenario files + `conftest.py` fixtures/helpers, driving the gateway through the real `openai` / `anthropic` / `google-genai` SDKs. |

These two fake crates must never depend on the gateway's own crates (`gw-codec` / `gw-proxy` / `gw-gateway` / `gw-auth` / `gw-store` / `gw-ledger`) — sharing a codec or store would make the fakes co-evolve with the code under test and mask wire/persistence drift. `gw-core` is allowed (`ids::*` newtypes only). The `e2e_fake_no_gw_codec_dep` lint in `scripts/check-invariants.sh` (§7) enforces this.

### 10.3 How to run it

```bash
make e2e-up      # docker-compose up: pg + redis + fake_upstream + fake_web, apply migrations, then gw-gateway
make e2e         # e2e-up, then run the pytest suite, then e2e-down
make e2e-down    # tear the stack down
```

The stack is described by [`tests/e2e/docker-compose.yml`](../tests/e2e/docker-compose.yml). The global `auth_configs` row that the JWT scenarios depend on is **boot-seeded once** by the Postgres initdb step from [`tests/e2e/seed_auth_config.sql`](../tests/e2e/seed_auth_config.sql); the gateway loads it at boot via `auth_config_or_load("global")`. The harness's per-test `Store::reset` deliberately **preserves** `auth_configs` (it truncates `TRUNCATE_TABLES` = `ENTITY_TABLES` minus `auth_configs`) so a gateway restart mid-suite keeps its JWT validator config.

### 10.4 Coverage matrix

30 scenarios across 5 files — all pass.

| File | Scenarios |
|------|-----------|
| `test_happy_streaming.py` | OpenAI streaming happy path · Anthropic streaming happy path · Gemini streaming happy path. |
| `test_auth_failures.py` | Expired API key → 401 · IP not allowlisted → 403 · HMAC required-but-missing → 401 · HMAC bad signature → 401 · HMAC timestamp drift → 401 · JWT `iss` mismatch → 401 · JWT `aud` mismatch → 401 · JWKS rotation: old `kid` rejected / new accepted. |
| `test_quotas.py` | Balance zero → 402 · Balance drained mid-traffic, next request → 402 · Daily-token quota exceeded → 403 · Yesterday's daily counter does not block today · Hard-cap input tokens → 400 · RPM exceeded → 429. |
| `test_routing_retry.py` | Connect-refused first key falls through to second key · Three keys connect-refused → 5xx · TLS handshake error → 503, no retry · No key advertises required capability → 503 · Sticky strategy binds user to same key, then TTL expires. |
| `test_cross_protocol.py` | OpenAI client → Anthropic upstream cross-protocol stream · OpenAI client → Gemini upstream cross-protocol stream · Mid-stream upstream RST after some bytes · Malformed SSE partial event · Gzip response decoded transparently. |
| `test_reliability.py` | DLQ grows when `usage:records` is corrupt · Entity update invalidates cache within 1 s · Gateway container restart recovers. |

The three documented skips have all been closed (e2e-skip-closure wave, 2026-05-22). Implementation detail:

- **Gemini streaming** (`test_gemini_stream_happy`): `ProtocolCodec` gained two default-providing methods — `path_implies_stream` (Gemini overrides to detect `:streamGenerateContent`) and `extract_path_model` (derives the model from `/v1beta/models/{model}:streamGenerateContent`). `request_filter` applies both after `parse_request` so the IR's `stream` and `model` are populated when the wire format puts them in the URL rather than the body.
- **TLS handshake error path** (`test_tls_handshake_error_returns_503_no_retry`): root cause was that `pingora 0.8` was declared without any TLS-backend feature, silently activating the `noop_tls` stub so `HttpPeer.tls=true` upstreams were dropped on the floor. Fixed by enabling `features = ["rustls"]` on `pingora` + `pingora-proxy` in workspace `Cargo.toml`; a second-order `rustls 0.23` `CryptoProvider` ambiguity (both `aws-lc-rs` and `ring` linked) is patched by `rustls::crypto::ring::default_provider().install_default()` at the top of `gw_gateway::run`. With real TLS handshakes restored, `fail_to_connect` fires with `InvalidCert` / `TLSHandshakeFailure` and the existing `classify_connect_error` mapping ticks `gw_retry_attempt_total{outcome="tls_blocked"}`.
- **Sticky TTL eviction** (`test_sticky_strategy_binds_user_to_same_key_then_expires`): `ReqCtx` gained a `conversation_id` field populated from the `X-Conversation-Id` request header in `early_request_filter`; `upstream::pick_peer` threads it through `PickCtx` so `Sticky::pick` writes to its moka cache instead of taking the no-conversation fallback. The hardcoded `DEFAULT_STICKY_TTL_SECS=1800` is now configurable via `[routing] sticky_ttl_secs` (default kept at 1800 in `config/default.toml`; e2e overrides to `2` so the TTL leg completes inside the test deadline).

### 10.5 What this harness found

Building the black-box harness surfaced real gateway behaviours and gaps that the in-process tests could not. Each item below is cited `file:line` so future work can address it; this list is the most valuable artefact of the harness work.

- **CLAUDE.md staleness — cross-protocol response re-render** *(corrected alongside this section)*. CLAUDE.md previously claimed cross-protocol *response* re-render was "deferred". It is not: G8 closed in `50cd9e7`, and `crates/gw-proxy/src/filters/response.rs:60-94,111-184` wires per-chunk cross-render via the `gw_codec_cross_render_total{from,to}` metric. CLAUDE.md is corrected alongside this section.
- **Gemini streaming gap.** *(closed by e2e-skip-closure W1-A, 2026-05-22)* `crates/gw-codec/src/gemini/{wire.rs,codec.rs}` — `GeminiRequest` has no `stream` field, and `parse_request` never sets `ir.stream=true` for `:streamGenerateContent`. A direct-Gemini-client streaming call was therefore rejected with `GW_REQUEST_NONSTREAM_NOT_SUPPORTED`. Fix: `ProtocolCodec` gained `path_implies_stream` + `extract_path_model` default-providing methods; Gemini overrides them and `request_filter` applies both after `parse_request`.
- **TLS handshake error path unreachable.** *(closed by e2e-skip-closure W3-A, 2026-05-22)* `crates/gw-proxy/src/gateway.rs` — TLS-handshake failures appeared to route through `fail_to_proxy`, not `fail_to_connect`, so `classify_connect_error` never ran and `GW_UPSTREAM_TLS_ERROR` was end-to-end unreachable. Root cause turned out to be that `pingora 0.8` was compiled without any TLS-backend feature, silently activating the `noop_tls` stub. Fix: enable `pingora`/`pingora-proxy`'s `rustls` feature in workspace `Cargo.toml` and install the `ring` `CryptoProvider` at startup. With real TLS handshakes happening, `fail_to_connect` fires with `InvalidCert` / `TLSHandshakeFailure` as expected.
- **`fail_to_proxy` envelope emitter.** *(closed by Phase E of 2026-05-28-systemic-fixes)* `crates/gw-proxy/src/gateway.rs` — per-branch classification now emits ErrorIR JSON envelope when `http_status > 0 && !body_started`; kind is `UpstreamProtocol` for HTTPStatus, `UpstreamMidStream` for in-flight, `UpstreamConnect` otherwise. 4 new error codes: `GW_UPSTREAM_TRUNCATED`, `GW_CLIENT_DISCONNECTED`, `GW_BAD_REQUEST`, `GW_UPSTREAM_PROTOCOL`.
- **Sticky strategy `conversation_id=None`.** *(closed by e2e-skip-closure W2-A, 2026-05-22)* `crates/gw-proxy/src/filters/upstream.rs:220` formerly hardcoded `conversation_id: None`, so `Sticky::pick` always took the no-conversation fallback branch and never wrote to its moka cache. Fix: `ReqCtx.conversation_id` populated from the `X-Conversation-Id` request header in `early_request_filter`, threaded into `PickCtx`. The hardcoded `DEFAULT_STICKY_TTL_SECS=1800` is now configurable via `[routing] sticky_ttl_secs`.
- **`events:entity` invalidation now clears Redis L1 + Moka (G6, T1-C, 2026-05-22).** `crates/gw-store/src/stream_consumer.rs::apply_entity_event` DELs `entity:{ns}:{id}` before invalidating Moka; multi-pod staleness in the Redis L1 layer is closed.
- **NOGROUP recovery.** *(closed by Phase C of 2026-05-28-systemic-fixes)* `crates/gw-store/src/stream_consumer.rs` — `StreamConsumer` migrated to `ManagedLoop` + `Supervisor`. NOGROUP → `Outcome::Recover` → automatic group recreation. `start()` is now sync infallible.
- **Malformed SSE observability.** *(closed by Phase F of 2026-05-28-systemic-fixes)* `crates/gw-proxy/src/filters/response.rs` and codec internals — all `parse_stream_chunk` / render errors now call `observe(CodecParse|CodecRender)` with a distinct `code` per call site, populating `gw_errors_total{kind=codec_parse|codec_render,stage=response}`.
- **`ProtocolKind` dual wire spelling by design; locked.** `crates/gw-core/src/protocol.rs` — `as_str()` returns `"openai"` (events stream / DB column) while `#[serde(rename_all = "snake_case")]` serializes to `"open_ai"` (entity payload JSONB, Go writes). Both forms locked in `crates/gw-core/tests/wire_enum_lock.rs` and enforced by the `wire_enum_lock` invariant. See §8 Deliberate behaviours.
- **`fake_web /usage/stats` 500s on a corrupted `usage:records`.** Minor — when `usage:records` is deliberately corrupted as a string key, the fake's stats endpoint 500s. This is a harness-side limitation, not a gateway issue.

## 11. Cross-render `WireCapabilities` matrix

Cross-protocol response re-render (`crates/gw-codec/src/cross_render.rs`) gates field preservation on per-wire-format capability flags. When the target format lacks a capability, the corresponding IR field is dropped and a lossy metric (`gw_codec_cross_render_lossy_total{from,to,field}`) fires. Same-protocol render never hits this path.

Source of truth: `WireFormat::capabilities()` in `cross_render.rs`.

| Capability | OpenAI Chat | OpenAI Responses | Anthropic Messages | Gemini Generate |
|------------|:-----------:|:----------------:|:------------------:|:---------------:|
| `supports_thinking` | | ✓ | ✓ | ✓ |
| `supports_reasoning` | | ✓ | | |
| `supports_tool_use_blocks` | | | ✓ | |
| `supports_tool_calls_flat` | ✓ | ✓ | | |
| `supports_function_call_parts` | | | | ✓ |
| `supports_reasoning_tokens` | | ✓ | | |
| `supports_refusal` | ✓ | ✓ | | |
| `supports_annotations` | ✓ | | | |
| `supports_server_tool_blocks` | | | ✓ | |
| `supports_redacted_thinking` | | | ✓ | |
| `supports_audio` | ✓ | | | |
| `supports_document_blocks` | | | ✓ | |
| `supports_service_tier` | ✓ | ✓ | ✓ | |
| `supports_reasoning_config` | ✓ | ✓ | ✓ | ✓ |
| `supports_grounding` | | | | ✓ |
| `supports_citations` | | | ✓ | |
| `supports_mcp` | | | ✓ | |

Notable cross-render drop paths:

- **Refusal**: `ResponseIR.refusal` + `FinishReasonIR::Refusal` are lossy-degraded to `Stop` when the target lacks `supports_refusal` (Anthropic, Gemini).
- **Annotations**: `ResponseIR.annotations` dropped for all targets except OpenAI Chat.
- **Server tool blocks**: `ResponseIR.server_tool_blocks` dropped for all targets except Anthropic Messages.
- **Redacted thinking**: `ResponseIR.redacted_thinking` dropped for all targets except Anthropic Messages.
- **Audio**: `metadata.extensions["openai_audio"]` and `UsageIR.audio_tokens_in/out` dropped for all targets except OpenAI Chat.
- **Grounding**: `metadata.extensions["grounding_metadata"]` dropped for all targets except Gemini Generate.
- **Service tier**: `ResponseIR.service_tier` and `UsageIR.service_tier` dropped only for Gemini Generate.
- **Gemini finish reasons**: Gemini-specific `Other("BLOCKLIST")`, `Other("PROHIBITED_CONTENT")`, `Other("SPII")`, `Other("MODEL_ARMOR")` map to `ContentFilter`; other unknowns degrade to `Stop`.
