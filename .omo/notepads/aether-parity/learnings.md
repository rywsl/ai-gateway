# Learnings — Aether Parity

## 2026-05-26 Session Start
- Plan has 43 tasks (39 impl + F1-F4), 7 waves + final wave
- Wave 1: Tasks 1-6 (Go contract docs + error codes + config schema) — ALL PARALLEL
- Task 4 is sequential AFTER Tasks 1-3 (index update needs files to exist)
- ratelimit.rs is single file, needs conversion to module dir (Task 7)
- DefaultErrorAnalyzer in crates/gw-adapter/src/analyzer.rs, NOT gw-health
- Go contract new files: 05-wallet-payment.md, 06-subscription-video.md, 07-oauth-ldap.md (existing 01-04)
- no_cost_micro and no_billing_crate lints — gw-billing must NOT compute token prices
- no_v1_markers_in_prod lint in error mode — no todo!()/unimplemented!()

## 2026-05-26 OAuth/LDAP Contract
- OAuth / LDAP contract doc should treat Go as sole writer for provider config, token refresh, LDAP bind/group mapping; Rust only reads Redis-standardized cache shapes.
- Auth cache invalidation needs a global `auth_config` / `cache_seed` bump after any OAuth or LDAP config change, and the write order must be: content first, seed last.
- Gateway config additions should use `#[serde(default)]` on the top-level `Config` fields plus per-field default helpers/`Default` impls so older `local.toml` / env-only deployments keep extracting cleanly.
- New config sections landed with non-secret defaults in `config/default.toml`: `[ip_filter]`, `[ip_ratelimit]`, `[adaptive_rpm]`, and `[task]`; stream names stay TOML-safe while secrets remain env-only.

## 2026-05-26 Wallet/Payment Contract
- Added `docs/go-contracts/parts/05-wallet-payment.md` in the same numbered style as parts/01-02: numbered subsections, field-schema tables, JSON examples, and cross-links back to `main.md` and `02-go-as-consumer.md`.
- Wallet/payment contract uses shared conventions: `ts_ms` millisecond timestamps, micro amounts only, `snake_case` identifiers, and Redis entity envelope `v/enc/d/EXPIRE` for wallet/payment write-through mirrors.
- Wallet settlement section explicitly cross-references `usage:records` as the authoritative Rust→Go settlement input and keeps cost computation on the Go side only; Rust remains responsible only for `frozen_micro` lifecycle.
- gw-core error codes file appends new sections at EOF; added IP rate-limit/allowlist codes and task worker lifecycle codes without duplicating existing GW_AUTH_IP_BLOCKED

## 2026-05-26 Subscription/Video Contract
- Added `docs/go-contracts/parts/06-subscription-video.md` with the same table-heavy contract style: overview, Redis keys, PgSQL tables, explicit failure modes, and JSON examples for subscription caches / plans / video task payloads.
- Subscription enforcement insight: `subscriptions` stays Go-side business authority, but Rust quota/RPM enforcement only changes after Go projects entitlement into existing `users` / `user_groups` / `api_keys` entities and emits normal `events:entity` invalidation.
- Video task correlation uses `usage:records.operation_id` as the primary join key for `/v1/videos` or other long-running operations, with `request_id` retained as the fallback audit join when upstream omits an operation name.

## 2026-05-26 Go-Contract Index Verification
- Verified `docs/go-contracts/main.md` already references `parts/05-wallet-payment.md`, `parts/06-subscription-video.md`, and `parts/07-oauth-ldap.md` exactly once each and in the correct 05 → 06 → 07 order; no `main.md` edit was needed.
- Verified all referenced files exist on disk under `docs/go-contracts/parts/`.

## 2026-05-26 Task 7 IP Rate Limiter
- `ratelimit.rs` can be converted to `ratelimit/mod.rs` without changing `lib.rs` re-exports; `cargo test -p gw-auth` passed immediately after the move.
- ADR-RLIP-1 lives at `docs/superpowers/specs/2026-05-19-ratelimit-rl-ip.md`, not the shorter path in the task text.
- `IpRateLimitCfg` in `gw-gateway` has defaults: `global_per_ip_rpm = 600`, `max_entries = 100_000`, and `sweep_interval_secs = 300`; config validation rejects zero values.

## 2026-05-26 Task 8 IP Allowlist
- `IpBlockMode` was added to `gw_core::entity::IpBlockEntry` with `#[serde(default)]` and default `Deny`, preserving existing deny-only JSON rows loaded by `gw-store`.
- `gw-auth::ip_allowlist::IpAllowlistGuard` intentionally owns its local `IpAllowlistMode` enum so `gw-auth` does not depend on the business-layer `gw-gateway` config crate; future filter-chain wiring can map config `Off/Enforce` into this type.
- Guard semantics: all scoped `Deny` entries are checked first during snapshot iteration and immediately return `GW_AUTH_IP_BLOCKED`; `Allow` entries only constrain requests in `Enforce` mode and return `GW_AUTH_IP_NOT_IN_ALLOWLIST` when at least one applicable allow entry exists but none match.

## 2026-05-26 Task 10 Adaptive RPM Structures
- RL-Adaptive spec lives at `docs/superpowers/specs/2026-05-19-ratelimit-rl-adaptive.md`, not the shorter path in the task text.
- Vendor keys use `gw_core::ids::KeyId`; `VendorKey` has `key_id: KeyId`, so `AdaptiveRpmManager` state should be keyed by `KeyId`.
- `AdaptiveRpmCfg` already exists in `gw-gateway` with `f64` ratio fields; gw-auth `AdaptiveCfg` mirrors the same default values using `f32` per Task 10/spec API.

## 2026-05-26 Task 9 IP Filter Orchestration
- `gw-auth::ip_filter::check_blocklist` must now ignore `IpBlockMode::Allow`; otherwise allowlist rows are treated as blocklist hits before unified `check_ip` can reach the allowlist guard.
- Unified `check_ip` keeps the required order in one place: deny-only `check_blocklist` → `IpAllowlistGuard::check` only when `IpAllowlistMode::Enforce` → `IpRateLimitGuard::check`.
- Current `IpRateLimitGuard` is in-memory governor-based and has no Redis dependency, so the Redis-outage fail-open case is represented by the absence of store I/O in the ratelimit step; only real quota exhaustion returns `GW_QUOTA_IP_RPM_EXCEEDED`.

## 2026-05-26 Task 11 Adaptive RPM Observe
- Task 11 keeps learned raw upstream RPM in `KeyAdaptiveState.learned_rpm`; the AIMD buffer is applied only by `AdaptiveRpmManager::should_enforce`, so tests can inspect both the learned limit and enforced safe ceiling.
- Current `AdaptiveRpmManager` stores `Arc<KeyAdaptiveState>` in `DashMap`; mutable updates use `Arc::make_mut`, requiring `KeyAdaptiveState: Clone` until Task 12/13 move fields to atomics or another interior-mutability shape.
- `observe_429` signature includes `configured_rpm` for first-seen keys: `observe_429(&KeyId, configured_rpm, header_limit, current_local_rpm)`.

## 2026-05-26 Task 14 IP Filter Chain Integration
- `gw-proxy::filters::early::run` now calls unified `gw_auth::ip_filter::check_ip` pre-auth with `None` user/api-key scope, so global deny/allowlist/IP-RPM failures short-circuit before auth.
- `gw-proxy::filters::request::run` now calls the same `check_ip` after auth with `Some(&user_id)` and `api_key_id.as_ref()`, before the legacy per-key `ApiKeyEntry.ip_allowlist` helper.
- `gw-gateway::bootstrap` maps config `IpFilterAllowlistMode` to auth `IpAllowlistMode`, constructs shared `Arc<IpAllowlistGuard>` / `Arc<IpRateLimitGuard>`, and spawns a periodic IP ratelimit sweep using `ip_ratelimit.sweep_interval_secs`.

## 2026-05-26 Task 17 gw-task Contract
- `gw-task` is now a pure business-contract crate with no `gw-store` dependency; Task 18/19 should add Lua/Redis protocol helpers here without moving task namespaces into `gw-store`.
- Task DTOs mirror ADR-T3 entity fields: schema/status, user/API/vendor routing metadata, lifecycle timestamps, provider ids, JSON input/output payloads, and frozen/actual micro billing snapshots without doing any price computation.
- ADR-T4 transition helper allows `Pending→Queued→Running→Succeeded|Failed|Cancelled`, plus `Running→Cancelling→Cancelled` and the documented race where `Cancelling→Succeeded` can happen if the provider returns a 2xx result before cancellation wins.

## 2026-05-26 Task 12 Adaptive RPM Lifecycle
- `tick_decay` mutates `DashMap<KeyId, Arc<KeyAdaptiveState>>` entries via `iter_mut()` + `Arc::make_mut`, preserving the current Task 10/11 state shape while adding lifecycle behavior.
- Probe expansion in the current gw-auth-only manager updates raw `learned_rpm`; `should_enforce` still applies `buffer_ratio`, so a probed key can remain unenforced once confidence decays below `enforcement_threshold`.
- `evict(&KeyId)` only removes adaptive state because this crate-local manager is not wired to `RateLimitGuard::reset_key_rpm` yet; Task 13/integration should handle guard reset when the guard dependency exists.

## 2026-05-26 Task 13 Adaptive RPM Integration
- `gw-gateway::bootstrap` now constructs one shared `Arc<AdaptiveRpmManager>` from `[adaptive_rpm]`, spawns `spawn_decay_loop`, and threads the manager through `Wired` into `LlmGateway`.
- `EntityStore` gained a narrow vendor-key invalidation hook list; `StreamConsumer` routes `ns=vendor_key` through `invalidate_vendor_key`, and bootstrap registers `adaptive_rpm.evict(key_id)` there.
- The proxy response filter observes upstream 429s for the picked vendor key and records `x-ratelimit-limit` when available; request filtering consults `should_enforce` to lower the effective RPM ceiling before `RateLimitGuard::check`.

## 2026-05-26 Task 15 Rate-Limit Metrics
- Metrics in this repo are considered registered by adding them to `gw-gateway/src/metrics_catalog.rs`; runtime call sites then use the lightweight `metrics::counter!` / `metrics::gauge!` macros directly without per-metric static registries.
- Adaptive RPM metrics landed in `gw-auth::ratelimit::adaptive`: `observe_429` bumps `gw_adaptive_rpm_observe_total` and refreshes the learned/confidence gauges, `should_enforce` bumps `gw_adaptive_rpm_enforced_total`, and `tick_decay` owns both `gw_adaptive_rpm_probe_total` and `gw_adaptive_rpm_released_total` transitions.
- Unified IP filter metrics are split by concern: `ip_filter::check_blocklist` owns `gw_ip_blocklist_hit_total` + blocklist snapshot gauge, `IpAllowlistGuard::check` owns allowlist checks + allowlist snapshot gauge, and `IpRateLimitGuard` owns per-IP check/sweep counters plus the active-entry gauge.

## 2026-05-26 Task 16 CLI Variants Design Spec
- Aether CLI handlers live in `src/api/handlers/base/cli_adapter_base.py` (not `src/services/provider/base/`); class hierarchy: `CliAdapterBase` → per-provider adapters, `CliMessageHandlerBase` with 6 mixins (Request, Stream, Sync, Event, Monitor, Prefetch).
- 4 FORMAT_IDs confirmed: `claude:cli`, `openai:cli`, `openai:compact`, `gemini:cli`; each has a `@register_cli_adapter` decorated adapter class.
- CLI detection heuristics per Aether: Claude uses Bearer without x-api-key on `/v1/messages`; OpenAI uses dedicated `/v1/responses` and `/v1/responses/compact` paths; Gemini uses `x-app: cli` or User-Agent containing "gemini-cli"/"geminicli".
- CLI body is transparent passthrough (`PassthroughRequestBuilder`); no IR conversion. Cross-format conversion only happens when the selected upstream vendor key requires a different protocol format.
- Token/billing extraction from raw SSE events: Claude uses `message_start.usage` + `message_delta.usage`; OpenAI uses `response.completed.response.usage`; Gemini uses `usageMetadata` fields (`promptTokenCount`, `candidatesTokenCount`, `thoughtsTokenCount`, `cachedContentTokenCount`).
- Spec created at `docs/superpowers/specs/2026-05-26-cli-variants-design.md` with 10 ADRs (CLI-1 through CLI-10): FORMAT_ID registry, transparent proxy model, filter chain preservation, billing without IR, CliCodec trait surface, detection heuristic, streaming support, cross-protocol constraints, adapter collaboration, observability/metrics.
- Key design: `ApiShape` enum extended to 8 values (4 standard + 4 CLI); `CliCodec` trait is narrower than `ProtocolCodec` (only detect + hints + usage extraction); CLI requests prohibited from cross-protocol re-render; new error code `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` for cross-protocol CLI denial.

## 2026-05-26 Task 20 CLI Codec Scaffolding
- `ApiShape` in `gw-core/src/protocol.rs` extended with `ClaudeCli`, `OpenAiCli`, `OpenAiCompact`, `GeminiCli` + `format_id()`, `is_cli()`, `protocol_kind()` methods.
- CLI codec module at `gw-codec/src/cli/` with `mod.rs` (trait + detection + FORMAT_ID constants) and `traits.rs` (CliUsageExtractor, CliUsageDelta, CliUsageFull, CliHints).
- `detect_cli(path, headers)` returns `Option<ApiShape>` — separate from `AnyCodec::detect`. Claude CLI detected by Bearer auth without x-api-key on `/v1/messages`; OpenAI by `/v1/responses[/compact]`; Gemini by `x-app: cli` or UA containing "gemini-cli"/"geminicli".
- `CliCodec` trait has 4 methods: `shape()`, `extract_hints()`, `usage_extractor()`, `ttft_event_types()`. No parse/render — CLI body is passthrough.
- `CliHints` derives `Deserialize` so it can be used for shallow body parse (`serde_json::from_slice`).
- `extractor_for_shape` in `gw-proxy/src/filters/response.rs` needed CLI fallback arms to maintain exhaustiveness; CLI shapes delegate to base protocol extractors since the caller should branch on `is_cli()` before reaching that function.
- `AnyCodec` enum in `any.rs` was NOT extended (Tasks 21-22 will add CLI variants there); only `ApiShape` was extended.
- 16 tests covering FORMAT_ID registration, `is_cli()`, `protocol_kind()`, trait surface (compile-time + runtime), detection heuristics (8 cases), and `CliHints` deserialization.

## 2026-05-26 Task 18 Lua Scripts
- `gw-task` now exposes Redis task contract scripts as plain `pub const &str` in `crates/gw-task/src/scripts.rs`; this keeps the crate pure and avoids adding a `gw-store` or Redis client dependency.
- Task script tests intentionally assert command/key-parameterization shape only (`HSET`/`XADD`/`ZADD`, `HSET`/`ZREM`, `ZRANGEBYSCORE`/`XADD`) because script execution belongs to the runtime/store integration layer.
- `SETTLE_LUA` records settlement metadata fields such as `settled_at_ms` but does not compute or mutate billing amounts; Go-side billing/cost ownership and the `no_cost_micro` invariant remain intact.

## 2026-05-26 Task 19 Redis Task Contract
- `gw-task::redis_contract::RedisTaskContract` owns direct fred execution now: it preloads Task 18 Lua scripts, falls back from `EVALSHA` to `EVAL` on `NOSCRIPT`, and keeps key names at `entity:task:{id}`, `tasks:queue`, `tasks:cancel`, and `tasks:leases` with an optional prefix.
- Task hashes store flat `TaskRecord` fields rather than the generic entity `v/enc/d` envelope; JSON and optional values are string-encoded per field and reconstructed by `load`/`watch`/claim-related methods.
- The trait’s single-id `recover_lease(id, now_ms)` wraps the batch-shaped `RECOVER_LEASE_LUA` with a fixed scan limit, then reloads the requested task record; no billing price computation was introduced.
- Docker Redis coverage lives in ignored `crates/gw-task/tests/redis_contract.rs` tests for submit HGET/XADD, load round-trip, cancel stream fan-out, and claim via `XREADGROUP`.

## 2026-05-26 Task 22 CLI Gemini/OpenAI Compact
- Task 21 landed while Task 22 was in flight: `claude_cli.rs` and `openai_cli.rs` exist, and `CliCodec` now includes default transparent `passthrough_request` / `passthrough_response_chunk` helpers in addition to shape/hints/usage/TTFT.
- `openai:compact` is now split into `cli/openai_compact.rs` instead of living inside `openai_cli.rs`; it reuses the OpenAI Responses token schema (`response.usage` or top-level `usage`) and defaults `CliHints.stream` to `Some(false)`.
- `gemini:cli` extracts hints exclusively from the URL path (`/models/{model}:{action}`), treats `:streamGenerateContent` as streaming, and folds `usageMetadata.thoughtsTokenCount` into output tokens for CLI billing extraction.

## 2026-05-26 Task 21 CLI Codec Handlers
- `ClaudeCliCodec` and `OpenAiCliCodec` live under `crates/gw-codec/src/cli/` and implement the narrow `CliCodec` surface only; passthrough is represented by borrowed request/response body helpers and no IR parse/render path is added.
- Claude CLI usage extraction reads stream `message_start.message.usage` for input/cache tokens and `message_delta.usage` for output tokens; non-stream responses read top-level `usage`.
- OpenAI CLI and OpenAI Compact share `OpenAiCliUsageExtractor`; it accepts both stream `response.completed.response.usage` and non-stream top-level `usage`, with cached input tokens mapped from `input_tokens_details.cached_tokens`.
- `CliCodec::passthrough_request` / `passthrough_response_chunk` default to borrowed `Cow<[u8]>`, which gives tests and future proxy wiring an explicit transparent-body API without copying or converting bodies.

## 2026-05-26 Task 23 Exec Loop
- `gw-task-runtime::exec_loop::ExecLoop` consumes only the existing `gw_task::TaskContract` surface (`claim`, `update_running`, `settle`), so runtime stays out of `gw-proxy` and does not know Redis key internals beyond the contract.
- The v1 upstream execution envelope uses `TaskRecord.input_payload.upstream_url` (falling back to `vendor_key_hint`) plus optional `input_payload.body`; success stores upstream JSON as `output_payload` and copies `provider_task_id`/`id` into the running record before settle.
- Timeout config is keyed by `(vendor_id, TaskKind)` with default total/submit durations; timeout and non-2xx reqwest failures both settle as `failed` with `actual_micro=0`, preserving the no Rust cost-calculation invariant.
- Four-layer concurrency is present as semaphore gates (global/vendor/model/key) in the exec loop crate; current Task 23 implementation uses configured aggregate semaphores and leaves per-identity sharding for later routing/runtime work.

## 2026-05-26 Task 26 CLI Filter Chain Integration
- `ReqCtx` now carries `client_shape` / `upstream_shape` alongside base `AnyCodec`; CLI detection stores fine-grained `ApiShape` while retaining the base protocol codec for envelopes and shared protocol APIs.
- CLI request handling in `request_filter` deliberately branches only after auth, scoped IP checks, body read, and HMAC verify; it shallow-extracts `model` / `stream` via `CliCodec::extract_hints` and then rejoins the existing permission, balance, RPM, hard-cap, and freeze path.
- `upstream_peer` enforces ADR-CLI-8 by retaining only same-protocol vendor keys for CLI shapes and returns `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` with `gw_cli_cross_protocol_denied_total` when no same-protocol channel remains.
- `upstream_request_filter` and `response_body_filter` use the `CliCodec` passthrough APIs for raw body preservation; response usage is extracted via `CliUsageExtractor` from raw non-stream JSON or stream SSE frames instead of IR parsing.
- UsageRecord / detailed summary protocol strings now prefer `ApiShape::format_id()` so Go billing can distinguish `claude:cli`, `openai:cli`, `openai:compact`, and `gemini:cli` from their base protocol families.

## 2026-05-26 Task 24 Task Runtime Loops
- `gw-task-runtime::cancel_loop` exposes a shared `RunningTaskRegistry` keyed by `TaskId`; cancel fan-out reads `tasks:cancel` via `tasks-cancel-fanout`, cancels the per-task token, and still calls `TaskContract::settle(..., Cancelled, actual_micro=0)` so cancellation does not skip settlement.
- `CancelSource` and `LeaseRecoveryStore` traits keep loop tests Redis-free while production adapters (`RedisCancelSource`, `RedisLeaseRecoveryStore`) own the Redis XREADGROUP / RECOVER_LEASE_LUA details.
- `watchdog_loop` uses batch expired-id discovery from `RECOVER_LEASE_LUA` and then calls `TaskContract::recover_lease(id, now_ms)` for each id, preserving the single task contract as the state-machine authority.
- `heartbeat_loop` reuses `TaskContract::update_running` for periodic lease renewal; with the Redis contract this maps to both `HSET entity:task:<id>.lease_until_ms` and `ZADD tasks:leases`.

## 2026-05-26 Task 25 gw-billing Formulas
- `gw-billing` is now a minimal business formula crate depending only on `gw-task`, `serde`, and `serde_json`; it imports `TaskKind` but no codec/proxy/store crates.
- Task formulas use configured base micro amounts, vendor multipliers, task-kind multipliers, and duration buckets only; they intentionally contain no per-token pricing names or token-count inputs.
- The historical `no_billing_crate` invariant was narrowed: `gw-billing` may be a workspace member, but supporting crates (`gw-core`, `gw-codec*`, `gw-store`, `gw-auth`, `gw-ledger`) must not depend on it.

## 2026-05-26 Task 27 CLI Doc Sync
- `docs/architecture.md` already had zero explicit "CLI" mentions before Task 27; workflow.md had extensive CLI documentation from Task 26 (`detect_cli`, `extract_cli_request_hints`, `CliCodec::passthrough_request/response_chunk`, `CliUsageExtractor`).
- Added to architecture.md: `gw-billing` in §2.1 crate table + layer diagram + dependency DAG; `gw-codec::cli` module mentioned in codec crate description; `CliCodec` and `CliUsageExtractor` traits in §3 public traits table; §3.1 Wire formats section with all 8 `ApiShape` FORMAT_IDs; `no_billing_crate` invariant updated in §7; two CLI deliberate behaviours in §8 (transparent proxy + cross-protocol denial).
- Added to workflow.md: CLI metrics (`gw_cli_cross_protocol_denied_total`, `gw_cli_usage_extract_total`) in §6 Observability table.
- Integration tests already covered by Task 26 at `crates/gw-proxy/tests/cli_filter_chain.rs` (4 tests: auth, ratelimit, billing/usage, body passthrough).

## 2026-05-26 Task 28 gw-task-worker Binary
- `gw-task-worker` is a separate workspace binary+library crate; the library owns config structs/tests so config can be TDD-tested without starting Redis or listeners.
- Worker config loads `config/task-default.toml` then `GW_TASK_*` env overrides, intentionally separate from gateway `GW_*`; ADR-T10 defaults are health `8081` and metrics `9091`.
- The binary constructs one fred `RedisPool`, wraps it in `gw_task::redis_contract::RedisTaskContract`, then starts `ExecLoop`, `CancelLoop`, `WatchdogLoop`, and `HeartbeatLoop` plus the health listener; metrics are served by `metrics-exporter-prometheus` on `:9091`.
- Health endpoints are support-only: `/livez` and `/readyz` return `200 ok`; no business task HTTP endpoint was added and `gw-proxy` was not imported.

## 2026-05-26 Task 30 Gateway Task Detector
- Task detection is isolated in `gw_proxy::filters::task` with `detect_task_shape(enabled, method, path, body)` and `mark_task_context`; gateway config `task.enabled` is threaded into `LlmGateway` so `/v1/tasks*` detection is inert by default.
- `ReqCtx` now carries `is_task` plus `task_action` (`Submit`, `Load`, `Watch`, `Cancel`), set at the start of `request_filter` before auth/permission work so later task endpoint handling can branch without touching normal proxy routing.
- ADR-T15 endpoint shapes covered now: `POST /v1/tasks`, `GET /v1/tasks/:id`, `GET /v1/tasks/:id/watch`, `POST /v1/tasks/:id/cancel`; no `gw-task` or `gw-task-runtime` code was modified.

## 2026-05-26 Task 33 Task Worker Migration
- Existing PgSQL migrations are mixed-style, but newer additive migrations (`0004_capabilities.sql` onward) use leading SQL comments plus uppercase `CREATE TABLE IF NOT EXISTS`; `0004_task_worker.sql` follows that newer style rather than the lowercase minimal style from `0002`/`0003`.
- ADR-T5 splits task runtime truth across Redis and PgSQL: Redis keeps `entity:task:*` / queue / cancel / lease coordination hot, while PgSQL stores durable task metadata snapshots and worker registration visibility rows.
- ADR-T13 makes worker registration observability-only, so the SQL mirror only needs `worker_id`, heartbeat, status, and free-form metadata; no routing/capacity columns are required.

## 2026-05-26 Task 31 Gateway Task Endpoints
- `gw_task::TaskContract` uses `async fn` in trait and is not dyn-compatible, so gateway wiring stores the concrete `Arc<gw_task::redis_contract::RedisTaskContract>` while endpoint helper tests stay generic over `TaskContract`.
- Task endpoint short-circuit now happens after auth, scoped IP checks, body buffering, and HMAC verification in `request_filter`; this preserves the "do not bypass auth/billing path gates" constraint while skipping upstream vendor proxy phases.
- Gateway task responses are direct JSON/SSE writes from `filters::task`: submit returns `201 {task_id,status=queued,expires_at}`, load/cancel return serialized `TaskRecord`, and watch emits a single SSE `status` event from `TaskContract::watch` rather than executing work in-process.

## 2026-05-26 Task 36 Worker Registration
- Implemented worker registration entirely in `gw-task-worker`: startup writes `entity:worker:<uuid>` HASH and a dedicated loop refreshes `last_heartbeat_ms`; no `gw-task` or `gw-task-runtime` contract changes were needed.
- User task requirements overrode the draft ADR details: runtime now uses `uuid::Uuid::new_v4()` plus HASH fields `worker_id`, `start_time_ms`, `status=running`, `hostname`, and `last_heartbeat_ms`, while remaining observability-only and unused for routing.
- Registration heartbeat reuses the existing worker heartbeat cadence from config (`loops.heartbeat_interval_ms`) but stays separate from task lease heartbeats, so task lease renewal and worker liveness remain decoupled.

## 2026-05-26 Task 29 Deployment Separability
- ADR-T11/ADR-T16 lock the task topology to N gateways × M workers with Redis as the only runtime coordination surface; no gateway/worker direct HTTP or per-instance addressing is allowed.
- Gateway-side `TaskCfg` in `crates/gw-gateway/src/config.rs` defaults `enabled = false`, and current wiring only threads that boolean into task-shape detection, so the gateway can boot normally with no worker present and with task routes inert by default.
- `gw-task-worker` config is intentionally separate (`config/task-default.toml` + `GW_TASK_*` env overrides), which preserves the inverse startup order: workers can boot and idle against Redis before any gateway starts enqueueing tasks.

## 2026-05-26 Task 32 Task Billing Freeze
- Gateway task submit now consumes `gw_billing::TaskBillingConfig` from `[task]` and calls `calculate_freeze_amount` before `TaskContract::submit`; the amount is stored on `TaskRecord.frozen_micro` and in the in-process `DEDUCT.frozen_micro` guard state.
- Task submit short-circuit was moved after the shared balance gate in `request_filter` so insufficient balance can return `GW_TASK_FREEZE_FAILED` without bypassing auth/HMAC/IP/balance checks; helper tests inject `available_balance_micro` directly.
- Worker success settlement now calls `calculate_settle_amount` and passes the resulting task-level `actual_micro` into `TaskContract::settle`; failures/timeouts/cancellations still settle with `0`, causing the Redis task contract to release the full frozen amount.

## 2026-05-26 Task 34 Task Worker Observability
- Worker-side metrics module named `task_metrics` (not `metrics`) to avoid shadowing the `metrics` crate which provides the `counter!`/`histogram!` macros; constants are `pub const &str` and `describe_worker_metrics()` registers them at worker startup.
- Worker metrics placed at natural loop boundaries: `gw_task_claimed_total` after successful claim+permit acquisition, `gw_task_settled_total{status}` at each settle outcome (succeeded/failed), `gw_task_execution_duration_seconds` histogram covering the full upstream call+timeout window, `gw_task_lease_recovered_total` per watchdog recovery, `gw_task_heartbeat_total` per successful lease renewal, `gw_task_cancel_received_total` per cancel event processed.
- Gateway-side metrics use inline string literals (matching the existing `gw-proxy` pattern) rather than constants: `gw_task_gw_submit_total` after successful enqueue, `gw_task_gw_freeze_total{result=ok|insufficient}` at both freeze success and both insufficient-balance rejection paths (early check + CAS-loop), `gw_task_gw_load_total`/`gw_task_gw_watch_total`/`gw_task_gw_cancel_total` at endpoint entry.
- All 11 metrics registered in `gw-gateway/src/metrics_catalog.rs` so both gateway and worker processes describe them at startup; worker binary additionally calls `describe_worker_metrics()` for its own :9091 exporter.

## 2026-05-26 Task 35 Doc Sync
- architecture.md already had `gw-task`, `gw-task-runtime`, `gw-billing` in workspace layout, §6.2 deployment model, and task freeze Redis keys from Tasks 27/29; Task 35 added `gw-task-worker` binary (Execution layer), `0004_task_worker.sql` migration, standalone task Redis namespace rows, §6.3 task error codes, and split gateway/worker listener tables.
- workflow.md already had CLI metrics and no task-specific content; Task 35 added step 0 (task-shape detection) and step 17 (task endpoint short-circuit) to the `request_filter` chain, §6.1 task metrics tables (11 metrics total), and §7 full task lifecycle section with sequence diagram, worker loop table, and state machine.
- Task Redis keys span two categories: coordination keys (streams `tasks:queue`/`tasks:cancel`, sorted set `tasks:leases`) owned by both gateway and worker, and entity keys (`entity:task:*`, `entity:worker:*`) that follow the gateway's existing entity pattern but store flat fields rather than the `v/enc/d` envelope.

## 2026-05-26 Task 37 Invariant Boundaries
- `scripts/check-invariants.sh` lint style here is simple active-block shell checks: `echo "[name] ..."`, a grep/cargo-based predicate, then `bad`/`ok`; adding new invariants should preserve that pattern instead of introducing helper abstractions.
- New task/CLI boundary lints now enforce four things explicitly: `gw-task-runtime` cannot depend on `gw-proxy`, `gw-billing` cannot mention `cost_per_token`/`price_per_token`, CLI codec sources cannot contain `skip_auth`/`bypass_auth`, and `docs/architecture.md` must continue naming `gw-task`, `gw-task-runtime`, `gw-task-worker`, and `gw-billing`.
