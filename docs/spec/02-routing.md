# 02 — Routing & Capability (as-built)
- Family: R (routing) · Backing crates: gw-routing, gw-capability · Source-of-truth: `crates/gw-routing/src/strategy/`, `crates/gw-routing/src/{candidates.rs,pipeline/,failover.rs,in_flight.rs}`, `crates/gw-capability/src/{resolver.rs,filter.rs}`, `crates/gw-proxy/src/filters/upstream.rs` · Status: Candidate filter + 8 named selection strategies (plus a config-driven `MultiDim` variant) + scoring pipeline + response-side failover classifier + capability matching are all wired into the proxy upstream path.

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
`strategy/mod.rs` (`crates/gw-routing/src/strategy/mod.rs`) defines the `Strategy` trait (`name`, `pick`) and an enum-dispatch `StrategyAny` to avoid trait-object vtables on the hot path. `StrategyAny` has 9 variants: the 8 named strategies plus `MultiDim` (a config-preset, weights-driven variant). Each `pick` first calls `frequency_tracker.record_pick` when present.

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
`pipeline/mod.rs` (`crates/gw-routing/src/pipeline/mod.rs`) defines the `ScoringPolicy` trait (`needed_dims`, `score`, `tiebreak`, `gate`, `before_extract`, `after_pick`), a `DimMask` bitset (`HEALTH`/`LATENCY`/`INFLIGHT`/`COST`/`ERROR_RATE`), `CandidateFeatures`, and the `execute` / `extract` / `pick_best_index` entry points. `execute` always unions `DimMask::HEALTH` into the needed dims, extracts features, applies the reservation gate (`reservation_view.gate_allows`) plus per-policy `gate`, scores, and keeps the **lowest** score (`is_better`), breaking ties per `TiebreakStrategy` (`InputOrder`/`PriorityDesc`/`PriorityAsc`).

`extract_features` resolves the health bucket **per candidate** via `gw_core::protocol::resolve_upstream_format(accessible_endpoints, api_format)` so protocol-conversion keys read the same `(key, ApiFormat)` bucket the response path writes; unavailable keys are dropped. It computes `cost_weight = priority × model_markup` (`markup` from `ctx.pricing.model_markup`, default `1.0`). Policy bodies live in `pipeline/policies.rs` (`FixedOrderPolicy`, `CostFirstPolicy`, `LatencyFirstPolicy`, `LoadBalancePolicy`) with constants `PER_KEY_INFLIGHT_CAP = 1024.0`, `LOAD_BALANCE_ALPHA/BETA/GAMMA`, `NEUTRAL_LATENCY = 0.5`.

### Failover
`failover.rs` (`crates/gw-routing/src/failover.rs`): `classify_for_failover(status, body_started, retryable_codes)` returns `BodyAlreadyStarted` if the body has started, else `NotRetryable` if `status` ∉ `retryable_codes`, else `Retryable429`/`Retryable5xx`. The default retryable set in proxy wiring is `{429, 500, 502, 503, 504}`. `FailoverExecutor` (`default max_retries = 3`) `next_candidate(remaining, attempt)` returns `Exhausted` when `attempt > max_retries` or the remaining list is empty, else `Retry(first remaining)`. `record_failover_metric` emits `gw_routing_failover_total{reason,strategy}` (no key-level label). Proxy `gateway.rs::response_filter` consumes `classify_for_failover` + `FailoverExecutor::next_candidate`, blacklists the failed key, and replays.

### Per-key in-flight cap
`in_flight.rs` (`crates/gw-routing/src/in_flight.rs`): `InFlightStore` (`DashMap<KeyId, Arc<InFlightCounter>>` of `AtomicU32`, `Relaxed`). `InFlightGuard::new` increments, `Drop` decrements (held on `ReqCtx`, releases on every termination path). `candidates::get_candidate_keys` drops keys at/over the cap; `LoadBalance` reads counts via `snapshot_counts`. The proxy constant `PER_KEY_INFLIGHT_CAP = 1024` (`upstream.rs`) is passed into `get_candidate_keys`.

### Capability resolution & matching
`gw-capability` (`crates/gw-capability/src/`): `resolver.rs::CapabilityResolver::resolve` builds a `RequiredCapabilities` set by layering `key_forced_caps`, `model_caps`, header `require`, and a `retry_upgrade` hint (each tagged with a `CapabilityOrigin`), then sets `MatchMode` to `Exclusive`/`Compatible` from `match_mode_exclusive`; emits `gw_capability_resolve_total{match}`. `resolve_with_shape` additionally adds `Capability::OpenAiResponsesShape` for `ApiShape::OpenAiResponses`. `parse_capability` maps known names (`vision`, `audio`, `video`, `thinking`, `tool_use`, `streaming_only`, `openai_responses_shape`, `vertex_ai_service_account`) to typed `Capability`, unknowns to `Capability::Tagged` (`types.rs`). `filter.rs::candidate_matches` is the pure routing-candidate match used by the candidate filter: an empty advertised set is non-restrictive; otherwise every required cap must be present, and in exclusive mode (candidate or required) the candidate must advertise no cap outside the required set. `filter.rs::CapabilityFilter::evaluate` (ApiKey-keyed) additionally short-circuits to `ApiKeyDisabled` via a `RedosTracker` and returns `Match`/`Missing`/`ExtraCaps`.

### Proxy wiring & CLI same-protocol constraint
`upstream.rs` (`crates/gw-proxy/src/filters/upstream.rs`): calls `get_candidate_keys` with `ctx.required_caps`, drops `Impersonate` candidates when egress is globally disabled, then for CLI client shapes (`ApiShape::is_cli`) `retain`s only candidates whose `accessible_endpoints` contain the client's own `protocol_kind` — i.e. CLI traffic requires a **same-protocol** upstream; an empty set emits `gw_cli_cross_protocol_denied_total` + `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` (503). An empty candidate set otherwise re-runs the filter with `required_caps = None` to distinguish `GW_ROUTING_NO_CAPABILITY_MATCH` from `GW_ROUTING_NO_AVAILABLE_CHANNEL` (both 503). Strategy is dispatched via `StrategySet::pick_for`; the first-attempt pick is additionally subjected to an adaptive-RPM admit check that may divert to an alternate within-ceiling candidate (fail-operational to `first`). On a failover retry attempt the previously chosen `picked_vendor_key` is re-located in the candidate list rather than re-picked.

## Deliberate behaviours
- **`CostFirst` does not read any per-token cost field.** Its score is `priority × model_markup` (`pipeline/mod.rs::extract_features` → `cost_weight`; `pipeline/policies.rs::CostFirstPolicy::score`). It never touches `input_price_micro_per_token` / `output_price_micro_per_token`. With `ctx.pricing == None` it degrades to `-(priority)` (priority-desc), identical to `FixedOrder`. Per-token cost computation is a Go-side billing concern, not a Rust routing input.
- **Health is checked in the strategy/pipeline, not the candidate filter.** `get_candidate_keys` is the cheap pre-filter; `KeyHealthStore::is_available` runs inside `pick`/`extract_features`, per the `(key, resolved-upstream-format)` bucket.
- **Failover is before-body only.** Once `body_started`, `classify_for_failover` returns `BodyAlreadyStarted` and no retry occurs, even for a 5xx/429 status.
- **Empty advertised capability set ⇒ non-restrictive.** A channel with no `forced_capabilities` matches any requirement (`candidate_matches`); `required_caps == None` disables the capability filter entirely.
- **Unknown `MultiDim` preset is fail-operational**, not fatal: warn-once + metric + fall back to `fixed_order` (`strategy/set.rs`).
- **`CacheAffinity` with no message hash degrades to first-available** rather than funnelling all of a model's traffic to one key (`cache_affinity.rs`).

## Cross-process touchpoints
- **Failover retryable status set** is supplied by proxy config (`gateway.rs`, default `{429,500,502,503,504}`) and consumed by `classify_for_failover`; it governs which upstream HTTP statuses trigger a Rust-side replay rather than being surfaced to the client.
- **Capability inputs** (`model_caps`, `key_forced_caps`, `match_mode_exclusive`, `exclusive_capability_match`) come from persisted entities loaded through `EntityStore` (`resolver.rs::ResolveBackend`, `backend::EntityStoreResolveBackend`); `exclusive_capability_match` is a `VendorKey` column.
- No routing decision writes to Redis/PgSQL directly; selected-key identity flows into the usage/billing surfaces owned by other families. (The authoritative Rust↔Go contract index lives in `docs/go-contracts/`.)

## Invariants & lints
- `no_cost_micro` / `no_billing_crate` (`scripts/check-invariants.sh`) — reinforce that routing must not compute per-token cost; consistent with `CostFirst`'s priority/markup-only scoring.
- `no_v1_markers_in_prod` (`error` mode) — placeholder/not-yet-built panic and comment markers are banned in these crates.
- In-flight counter uses `Relaxed` ordering by design (best-effort concurrency hint; documented in `in_flight.rs`).
- Routing error codes that exist today: `GW_ROUTING_NO_AVAILABLE_CHANNEL`, `GW_ROUTING_NO_CAPABILITY_MATCH`, `GW_ROUTING_UNKNOWN_PRESET`, `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL`, `GW_TUNNEL_NO_NODE` (`crates/gw-core/src/error/codes.rs`).

## Not built → ROADMAP
The following are not present in the verified sources and are recorded only as pointers (no design detail here):
- **R-AB multi-score rewrite** — a generalised ranked-list selector (`CandidateFilter` / `ScoringDimension` / `SelectorAny`) replacing the current single-best-score pipeline.
- **R-D `AffinityManager`** — a dedicated affinity-binding manager distinct from the current `Sticky`/`CacheAffinity` per-strategy state.
- **R-F stream probe** — pre-commit stream health probing.
- **R-E gaps** — `401`/`402`/`403`-driven key-switch failover; a `total_budget_ms` request time budget; and the error codes `GW_ROUTING_NO_FALLBACK_LEFT`, `GW_ROUTING_FAILOVER_TIMEOUT`, `GW_ROUTING_STREAM_NOT_RETRIABLE` (none defined in `crates/gw-core/src/error/codes.rs`).
