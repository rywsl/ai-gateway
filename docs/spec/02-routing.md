# 02 — Routing & Capability (as-built)
- Family: R (routing) · Backing crates: gw-routing, gw-capability · Source-of-truth: `crates/gw-routing/src/{affinity.rs,strategy/,candidates.rs,pipeline/,failover.rs,in_flight.rs}`, `crates/gw-capability/src/{resolver.rs,filter.rs}`, `crates/gw-proxy/src/filters/upstream.rs` · Status: Candidate filter + 8 named selection strategies (plus a config-driven `MultiDim` variant) + scoring pipeline + API-key affinity wiring + response-side failover classifier + capability matching are all wired into the proxy upstream path.

## Purpose
Given a resolved `ModelGroup` and request context, select one upstream `VendorKey` to proxy to: filter the group's members down to admissible candidates, rank/pick one via the group's configured strategy, and (on a retryable upstream response) hand the next candidate to the failover executor. Capability matching constrains which channels are even eligible. Cost is never computed here — `CostFirst` ranks by a priority/markup proxy, not per-token price (see §Deliberate behaviours).

## As-built surface

### Candidate filter
`candidates.rs::get_candidate_keys` (`crates/gw-routing/src/candidates.rs`) walks `group.vendor_key_ids`, loads each `VendorKey` via `EntityStore::vendor_key_or_load`, and admits a key only when **all** of:
1. `status == KeyStatus::Active`;
2. `accessible_models.contains(model)`;
3. `accessible_endpoints.contains(target_protocol)` OR `protocol_conversion == true`;
4. caller's `MergedPermissions.allowed_channels` is empty OR contains the `key_id`;
5. `in_flight.count(key_id) < in_flight_per_key`;
6. `key_id` not in `retry_blacklist` (already-attempted keys this request);
7. when `required_caps` is `Some`, `gw_capability::candidate_matches(req, parse_capabilities(forced_capabilities), exclusive_capability_match)` holds.

Health/circuit checks are **not** done here — they run later inside `Strategy::pick` via `KeyHealthStore::is_available`. The function returns `Vec<Arc<VendorKey>>` (freshly cloned keys).

### Strategies
`strategy/mod.rs` (`crates/gw-routing/src/strategy/mod.rs`) defines the `Strategy` trait (`name`, `pick`) and an enum-dispatch `StrategyAny` to avoid trait-object vtables on the hot path. `StrategyAny` has 9 variants: the 8 named strategies plus `MultiDim` (a config-preset, weights-driven variant). `RankedStrategy::ranked_candidates` returns a stable ranked fallback list for proxy first-attempt selection; each ranked dispatch first calls `frequency_tracker.record_pick` when present.

The 8 named strategies (`crates/gw-routing/src/strategy/*.rs`):
- **`FixedOrder`** (`fixed_order.rs`) — score `-(priority)`; lowest score wins ⇒ highest priority first. Routed through `pipeline::execute`.
- **`RoundRobin`** (`round_robin.rs`) — per-`ModelName` `AtomicUsize` counter (`DashMap`), `fetch_add % len`, skipping unavailable keys for up to `candidates.len()` positions. Has its own `pick` (not pipeline-routed).
- **`LoadBalance`** (`load_balance.rs`) — composite score `α·in_flight_ratio + β·p50_norm + γ·error_rate_30s` with `α=0.40, β=0.35, γ=0.25` (`pipeline/policies.rs`). Uses `pipeline::extract` + `pick_best_index` with a per-request `max_p50` normaliser.
- **`WeightedRr`** (`weighted_rr.rs`) — Nginx-style smooth weighted round-robin over `(ModelName, KeyId)` `AtomicI32` `current_weight`; eventually-consistent under concurrency. Own `pick`.
- **`CostFirst`** (`cost_first.rs`) — pipeline-routed; score is `cost_weight = priority × model_markup` when `ctx.pricing.is_some()`, else falls back to `-(priority)`. Lowest score wins.
- **`LatencyFirst`** (`latency_first.rs`) — pipeline-routed; score is `latency_p50` ms (`f64::MAX` when no sample); tiebreak `PriorityDesc`.
- **`CacheAffinity`** (`cache_affinity.rs`) — own `pick`; SHA-256 over `model || ":" || message_hash` (first 8 bytes LE `u64`) modulo the available-candidate count. With no `message_hash`, degrades to first-available.
- **`Sticky`** (`sticky.rs`) — own `pick`; `conversation_id → key_id` bindings in a `moka::sync::Cache` (`DEFAULT_STICKY_TTL_SECS = 30*60`, `DEFAULT_STICKY_CAP = 100_000`). Hit refreshes TTL; rebind hashes `conversation_id` over the available set; emits `gw_sticky_hit_total` / `gw_sticky_bind_total`. No `conversation_id` ⇒ first-available.

`strategy/set.rs::StrategySet` is the per-process container holding one pre-built `StrategyAny` per kind (plus a `HashMap<String, StrategyAny>` of `MultiDim` presets). `pick_for(&StrategyKind)` returns `&StrategyAny` (borrow, stays on enum dispatch). An unknown `MultiDim` preset is fail-operational: warn-once (`UNKNOWN_PRESETS_WARNED` `DashSet`) + `gw_routing_unknown_preset_total` metric + fall back to `fixed_order`, surfacing `GW_ROUTING_UNKNOWN_PRESET`. `Sticky`'s TTL is parameterised via `StrategySet::new(sticky_ttl)`.

### Scoring pipeline
`pipeline/mod.rs` (`crates/gw-routing/src/pipeline/mod.rs`) defines the `ScoringPolicy` trait (`needed_dims`, `score`, `tiebreak`, `gate`, `before_extract`, `after_pick`), a `DimMask` bitset (`HEALTH`/`LATENCY`/`INFLIGHT`/`COST`/`ERROR_RATE`), `CandidateFeatures`, and the `execute` / `ranked` / `extract` / `pick_best_index` entry points. `execute` always unions `DimMask::HEALTH` into the needed dims, extracts features, applies the reservation gate (`reservation_view.gate_allows`) plus per-policy `gate`, scores, and keeps the **lowest** score (`is_better`), breaking ties per `TiebreakStrategy` (`InputOrder`/`PriorityDesc`/`PriorityAsc`). `ranked` applies the same gates/scores/tiebreaks and returns a `SmallVec<[Arc<VendorKey>; 8]>` ordered best-to-worst, so failover can walk the same first-attempt ranking.

`extract_features` resolves the health bucket **per candidate** via `gw_core::protocol::resolve_upstream_format(accessible_endpoints, api_format)` so protocol-conversion keys read the same `(key, ApiFormat)` bucket the response path writes; unavailable keys are dropped. It computes `cost_weight = priority × model_markup` (`markup` from `ctx.pricing.model_markup`, default `1.0`). Policy bodies live in `pipeline/policies.rs` (`FixedOrderPolicy`, `CostFirstPolicy`, `LatencyFirstPolicy`, `LoadBalancePolicy`) with constants `PER_KEY_INFLIGHT_CAP = 1024.0`, `LOAD_BALANCE_ALPHA/BETA/GAMMA`, `NEUTRAL_LATENCY = 0.5`.

### Failover
`failover.rs` (`crates/gw-routing/src/failover.rs`): `classify_for_failover(status, body_started, retryable_codes)` returns `BodyAlreadyStarted` if the client-visible body has started, else `NotRetryable` if `status` ∉ `retryable_codes`, else `SwitchKey4xx` for 401/402/403, `Retryable429` for 429, or `Retryable5xx` for the rest. The default retryable set in proxy wiring is `{401, 402, 403, 429, 500, 502, 503, 504}`. `FailoverExecutor` (`default max_retries = 3`, `total_budget_ms = 30000`) `next_candidate_with_elapsed(remaining, attempt, elapsed_ms)` returns `Timeout` when the elapsed wall-clock budget is spent, `Exhausted` when `attempt > max_retries` or the remaining list is empty, else `Retry(first remaining)`. `record_failover_metric` emits `gw_routing_failover_total{reason,strategy}` (no key-level label). Proxy `gateway.rs::response_filter` consumes `classify_for_failover` + `FailoverExecutor::next_candidate_with_elapsed`, blacklists the failed key, and replays. Timeout falls through to client error classification with `ctx.upstream_error_code = GW_ROUTING_FAILOVER_TIMEOUT`.

### Stream probe failover window
Streaming requests use explicit phases:
- `Connecting`: upstream request has been sent but no upstream response body bytes have been observed.
- `Probing`: upstream response body bytes have arrived, but no valid client-visible SSE event/body chunk has been committed downstream.
- `Streaming`: at least one valid client-visible event/body chunk has been committed downstream.

The proxy may fail over across vendor keys during `Connecting` and `Probing`. Probe retry is allowed for upstream error bodies, malformed first frames, and probe timeout, provided `client_body_committed == false`. Probe bytes are buffered in `ReqCtx.probe_buffer`; partial first events are withheld from the downstream client, while a valid first event commits the full buffered chunk to the normal response path. `body_started` remains a compatibility alias for `client_body_committed`, and is set only by `response_body_filter` after final downstream body mutation chooses a non-empty client-visible body. Once `Streaming` is reached, failover keeps the existing no-retry behavior and mid-stream failures surface as stream/upstream errors for the current attempt.

Probe metrics are `gw_stream_probe_total{outcome}`, `gw_stream_probe_failover_total{reason,strategy,outcome}`, `gw_stream_probe_buffer_bytes`, and `gw_stream_probe_timeout_total`; API-key, vendor-key, model, and user labels are intentionally absent.

### Per-key in-flight cap
`in_flight.rs` (`crates/gw-routing/src/in_flight.rs`): `InFlightStore` (`DashMap<KeyId, Arc<InFlightCounter>>` of `AtomicU32`, `Relaxed`). `InFlightGuard::new` increments, `Drop` decrements (held on `ReqCtx`, releases on every termination path). `candidates::get_candidate_keys` drops keys at/over the cap; `LoadBalance` reads counts via `snapshot_counts`. The proxy constant `PER_KEY_INFLIGHT_CAP = 1024` (`upstream.rs`) is passed into `get_candidate_keys`.

### API-key affinity manager
`affinity.rs` (`crates/gw-routing/src/affinity.rs`) defines `AffinityManager`, a request-external binding table distinct from `strategy/sticky.rs` and `strategy/cache_affinity.rs`. It stores `AffinityRecord { dimension, vendor_key_id, expires_at }` in a `DashMap<AffinityDimension, AffinityRecord>`, where the first concrete dimension is `AffinityDimension::ApiKey(ApiKeyId)` and `AffinityDimension::Custom { name, value }` keeps the key space extensible without conflating dimensions that share the same textual value.

All time-dependent methods take an explicit `Instant`, so TTL behavior is deterministic in tests and callers can drive sweeps from their own clock source. `record_hit(dimension, vendor_key_id, now)` upserts a record with `expires_at = now + ttl`; a later hit refreshes both the bound key and expiry. `get(dimension, now)` returns a cloned record only when `expires_at > now`; expired records are synchronously removed only when the stored record still matches the expired snapshot and are treated as a miss. `invalidate_vendor_key(key_id)` removes every record still bound to that vendor key and returns the actual number of successful deletions. `sweep_expired(now)` removes all records still expired at deletion time and returns the actual number of successful deletions. `Default` uses a 30-minute TTL.

Proxy wiring passes authenticated `ReqCtx.api_key_id` into `AffinityDimension::ApiKey`, consults the manager against the ranked candidate list after strategy ranking and before normal first-candidate selection, and never overrides an explicit failover retry key. The affinity hit is honored only when the recorded vendor key remains in the ranked candidates, is not in `ctx.retry_blacklist`, and is available in `KeyHealthStore` for the resolved protocol bucket. `LlmGateway::upstream_request_filter` records the selected vendor key only after outbound request rewrite succeeds, so peer construction or rewrite failures do not refresh affinity. Gateway bootstrap owns one shared `Arc<AffinityManager>` and clears records through `EntityStore::on_vendor_key_invalidate`; key-health cooldown and OAuth `RefreshFailed` listeners also invalidate the affected vendor key.

### Capability resolution & matching
`gw-capability` (`crates/gw-capability/src/`): `resolver.rs::CapabilityResolver::resolve` builds a `RequiredCapabilities` set by layering `key_forced_caps`, `model_caps`, header `require`, and a `retry_upgrade` hint (each tagged with a `CapabilityOrigin`), then sets `MatchMode` to `Exclusive`/`Compatible` from `match_mode_exclusive`; emits `gw_capability_resolve_total{match}`. `resolve_with_shape` additionally adds `Capability::OpenAiResponsesShape` for `ApiShape::OpenAiResponses`. `parse_capability` maps known names (`vision`, `audio`, `video`, `thinking`, `tool_use`, `streaming_only`, `openai_responses_shape`, `vertex_ai_service_account`) to typed `Capability`, unknowns to `Capability::Tagged` (`types.rs`). `filter.rs::candidate_matches` is the pure routing-candidate match used by the candidate filter: an empty advertised set is non-restrictive; otherwise every required cap must be present, and in exclusive mode (candidate or required) the candidate must advertise no cap outside the required set. `filter.rs::CapabilityFilter::evaluate` (ApiKey-keyed) additionally short-circuits to `ApiKeyDisabled` via a `RedosTracker` and returns `Match`/`Missing`/`ExtraCaps`.

### Proxy wiring & CLI same-protocol constraint
`upstream.rs` (`crates/gw-proxy/src/filters/upstream.rs`): calls `get_candidate_keys` with `ctx.required_caps`, drops `Impersonate` candidates when egress is globally disabled, then for CLI client shapes (`ApiShape::is_cli`) `retain`s only candidates whose `accessible_endpoints` contain the client's own `protocol_kind` — i.e. CLI traffic requires a **same-protocol** upstream; an empty set emits `gw_cli_cross_protocol_denied_total` + `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` (503). An empty candidate set otherwise re-runs the filter with `required_caps = None` to distinguish `GW_ROUTING_NO_CAPABILITY_MATCH` from `GW_ROUTING_NO_AVAILABLE_CHANNEL` (both 503). Strategy is dispatched via `StrategySet::pick_for`; first-attempt routing calls `RankedStrategy::ranked_candidates`, stores the ranked list on `ctx.picked_candidates`, and then applies adaptive-RPM admission, which may divert to the next ranked candidate within ceiling (fail-operational to `first`). On a failover retry attempt the previously chosen `picked_vendor_key` is re-located in the stored ranked list rather than re-picked.

## Deliberate behaviours
- **`CostFirst` does not read any per-token cost field.** Its score is `priority × model_markup` (`pipeline/mod.rs::extract_features` → `cost_weight`; `pipeline/policies.rs::CostFirstPolicy::score`). It never touches `input_price_micro_per_token` / `output_price_micro_per_token`. With `ctx.pricing == None` it degrades to `-(priority)` (priority-desc), identical to `FixedOrder`. Per-token cost computation is a Go-side billing concern, not a Rust routing input.
- **Health is checked in the strategy/pipeline, not the candidate filter.** `get_candidate_keys` is the cheap pre-filter; `KeyHealthStore::is_available` runs inside `pick`/`extract_features`, per the `(key, resolved-upstream-format)` bucket.
- **Failover is before-client-commit only.** Header/status retries still use `classify_for_failover`; stream probe retries may also occur after upstream bytes arrive but before `client_body_committed`. Once a client-visible event/body chunk is committed, `body_started` remains true and `classify_for_failover` returns `BodyAlreadyStarted`.
- **Empty advertised capability set ⇒ non-restrictive.** A channel with no `forced_capabilities` matches any requirement (`candidate_matches`); `required_caps == None` disables the capability filter entirely.
- **Unknown `MultiDim` preset is fail-operational**, not fatal: warn-once + metric + fall back to `fixed_order` (`strategy/set.rs`).
- **Failover walks the first-attempt ranked list.** Pipeline-backed strategies (`FixedOrder`, `CostFirst`, `LatencyFirst`, `LoadBalance`, `MultiDim`) rank by their scoring policy; stateful/hash strategies keep their selected key first and append remaining candidates in input order to avoid changing their state semantics.
- **`CacheAffinity` with no message hash degrades to first-available** rather than funnelling all of a model's traffic to one key (`cache_affinity.rs`).
- **`AffinityManager` remains request-external state.** It does not read auth headers or mutate `ReqCtx` directly; proxy wiring supplies the API-key dimension and selected-key recording point.

## Cross-process touchpoints
- **Failover retryable status set** is supplied by proxy config (`gateway.rs`, default `{429,500,502,503,504}`) and consumed by `classify_for_failover`; it governs which upstream HTTP statuses trigger a Rust-side replay rather than being surfaced to the client.
- **Capability inputs** (`model_caps`, `key_forced_caps`, `match_mode_exclusive`, `exclusive_capability_match`) come from persisted entities loaded through `EntityStore` (`resolver.rs::ResolveBackend`, `backend::EntityStoreResolveBackend`); `exclusive_capability_match` is a `VendorKey` column.
- **API-key affinity invalidation:** vendor-key entity invalidation, key-health cooldown, and OAuth `RefreshFailed` listener paths all call `AffinityManager::invalidate_vendor_key` for the affected key.
- No routing decision writes to Redis/PgSQL directly; selected-key identity flows into the usage/billing surfaces owned by other families. (The authoritative Rust↔Go contract index lives in `docs/go-contracts/`.)

## Invariants & lints
- `no_cost_micro` / `no_billing_crate` (`scripts/check-invariants.sh`) — reinforce that routing must not compute per-token cost; consistent with `CostFirst`'s priority/markup-only scoring.
- `no_v1_markers_in_prod` (`error` mode) — placeholder/not-yet-built panic and comment markers are banned in these crates.
- In-flight counter uses `Relaxed` ordering by design (best-effort concurrency hint; documented in `in_flight.rs`).
- Routing error codes that exist today: `GW_ROUTING_NO_AVAILABLE_CHANNEL`, `GW_ROUTING_NO_CAPABILITY_MATCH`, `GW_ROUTING_NO_FALLBACK_LEFT`, `GW_ROUTING_FAILOVER_TIMEOUT`, `GW_ROUTING_STREAM_NOT_RETRIABLE`, `GW_ROUTING_UNKNOWN_PRESET`, `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL`, `GW_TUNNEL_NO_NODE` (`crates/gw-core/src/error/codes.rs`).

## Not built
No open routing ROADMAP entries remain in this spec.
