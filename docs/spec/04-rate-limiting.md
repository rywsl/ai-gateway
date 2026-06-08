# 04 — Rate Limiting (as-built)

- Family: RL · Backing crate: `gw-auth` (`ratelimit`, `ip_*`) · Source-of-truth: `crates/gw-auth/src/ratelimit/mod.rs`, `crates/gw-auth/src/ratelimit/adaptive.rs`, `crates/gw-auth/src/ip_ratelimit.rs`, `crates/gw-auth/src/ip_filter.rs`, `crates/gw-auth/src/ip_allowlist.rs`; wired in `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/early.rs`, `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/gateway.rs` · Status: per-user RPM/TPM/concurrent-stream caps, an adaptive per-vendor-key RPM ceiling learned from upstream 429s, and per-IP RPM + blocklist/allowlist gating all exist and are wired into the filter chain.

## Purpose

Bound request volume at three independent granularities so a single user, a single client IP, or a single upstream vendor key cannot exhaust shared capacity:

- **Per-user request rate** (RPM), **per-user token rate** (TPM, post-response), and **per-user concurrent stream** caps, plus a process-global concurrent-stream counter for observability.
- **Per-vendor-key adaptive RPM**: learn the vendor's true rate limit from observed upstream `429` responses and proactively throttle sends to that key before the vendor rejects them.
- **Per-IP request rate** plus scope-aware IP **blocklist** and **allowlist** gating, evaluated before auth-derived limits.

Token *cost* is never computed here (the Go billing service owns cost); rate limiting only counts requests, streams, and—for TPM—upstream-reported token counts.

## As-built surface

### Per-user guard — `RateLimitGuard`

`RateLimitGuard` (`crates/gw-auth/src/ratelimit/mod.rs:27`) holds four maps/counters: `per_user` and `per_user_tpm` (`DashMap<UserId, Arc<DefaultDirectRateLimiter>>`), `per_user_streams` (`DashMap<UserId, Arc<AtomicUsize>>`), and `total_streams` (`Arc<AtomicUsize>`).

- `check(&self, perm: &MergedPermissions)` (`ratelimit/mod.rs:50`) — when `perm.rpm_limit > 0`, lazily builds a `governor` direct limiter at `Quota::per_minute(rpm_limit)` keyed on `perm.effective_user_id`, and calls `lim.check()`. A failure returns `rpm_err()` → `GW_QUOTA_RPM_EXCEEDED` (429, `retryable: true`, `retry_after_secs: Some(60)`, `ratelimit/mod.rs:138`). It is invoked once per request in `request_filter` step 8 (`crates/gw-proxy/src/filters/request.rs:1208`); the returned `RateLimitTicket` is dropped immediately, so RPM is debited exactly once per request.
- `check_stream_slot(&self, uid, max)` (`ratelimit/mod.rs:67`) — increments the per-user `AtomicUsize`; if `max > 0 && n+1 > max` it decrements and returns `concurrent_err()` → `GW_QUOTA_CONCURRENT_EXCEEDED` (429, `retry_after_secs: Some(5)`, `ratelimit/mod.rs:152`). On success it increments `total_streams` and returns a `StreamSlot` whose `Drop` (`ratelimit/mod.rs:131`) decrements both counters (RAII release on every termination path). Called only for streaming requests in `request_filter` (`request.rs:1213`, with `merged.max_concurrent_streams`). `total_streams` is the process-global concurrent-stream gauge source.
- `deduct_tpm(&self, uid, perm, tokens)` (`ratelimit/mod.rs:96`) — when `perm.tpm_limit > 0` and `tokens` is non-zero, debits a per-user TPM `governor` limiter via `check_n(tokens)`. On exhaustion returns `tpm_err()` → `GW_QUOTA_TPM_EXCEEDED` (429, `ratelimit/mod.rs:166`). This runs **after** the response is fully sent (response-side `logging` filter, `crates/gw-proxy/src/filters/logging.rs:65`) because the token count comes from the upstream response; a failure here does not block the current request — it only causes the *next* request from that user to be rejected until the window slides.

### Adaptive per-vendor-key RPM — `AdaptiveRpmManager`

`AdaptiveRpmManager` (`crates/gw-auth/src/ratelimit/adaptive.rs:13`) holds `state: DashMap<KeyId, Arc<KeyAdaptiveState>>`, a cached `limiters: DashMap<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>` (the `u32` records the per-minute limit the cached GCRA was built for), and an `AdaptiveCfg`.

- **`observe_429(key_id, configured_rpm, header_limit, current_local_rpm, retry_after_secs)`** (`adaptive.rs:37`) — increments `gw_adaptive_rpm_observe_total`, computes `observed_limit = header_limit.unwrap_or(current_local_rpm)`, and selects a required-observation threshold: `min_header_observations` (default 2) when an `x-ratelimit-limit` header *or* a `Retry-After` header is present, else `min_no_header_observations` (default 3). It records the observation against `KeyAdaptiveState` and, once `consistent_observations >= required`, sets `learned_rpm = Some(observed_limit)` and `confidence = 1.0` (`KeyAdaptiveState::record_observation` / `learn_if_ready`, `adaptive.rs:206`/`215`).
- **`should_enforce(key_id)`** (`adaptive.rs:79`) — pure query returning `Some(ceiling)` only when `confidence >= enforcement_threshold` (default 0.6) and a `learned_rpm` exists; the ceiling is `(learned_rpm * buffer_ratio) as u32` (default `buffer_ratio = 0.85`) clamped to `>= 1`.
- **`try_admit(key_id)`** (`adaptive.rs:99`) — admission check using a per-key GCRA. Returns `true` (admit, consuming a cell) when nothing is learned with enough confidence or the key is within its learned rate; returns `false` (deny, no cell consumed) when over the ceiling, incrementing `gw_adaptive_rpm_enforced_total`. The cached limiter is fetched-or-built under a single `DashMap::entry` lock and rebuilt in place when the enforced limit moves (the comment at `adaptive.rs:104` documents the prior TOCTOU panic this closes).
- **`tick_decay()`** (`adaptive.rs:135`) — AIMD background tick: decays each key's confidence by `confidence_decay_per_min` (default 0.02) per elapsed minute; when confidence falls below `enforcement_threshold` and `>= probe_after_min` (default 30) have passed since the last 429, it additively probes `learned_rpm` upward by `probe_increment` (default 5) clamped to `max_rpm` (default 10 000), firing `gw_adaptive_rpm_probe_total`. A key transitioning from enforced to released fires `gw_adaptive_rpm_released_total`.
- **`evict(key_id)`** (`adaptive.rs:176`) — drops both the state entry and the cached limiter (no stale-limiter leak).
- Metrics gauges `gw_adaptive_rpm_confidence` and `gw_adaptive_rpm_learned_value` are set via `set_adaptive_state_metrics` (`adaptive.rs:8`).

**Wiring.** `observe_429` is called from the response path in `crates/gw-proxy/src/gateway.rs:854` when `ctx.upstream_status == 429` and a vendor key was picked, passing `rpm_limit_header(upstream_response)` (`gateway.rs:160`) and `header_u32(upstream_response, "retry-after")` (`gateway.rs:148`). Enforcement is applied in `pick_peer` (`crates/gw-proxy/src/filters/upstream.rs:332`): after `Strategy::pick` selects `first`, `try_admit(&first.key_id)` is consulted; if denied, the loop diverts to another candidate whose `try_admit` passes, and only sends to `first` anyway when *every* candidate is over its ceiling (fail-operational back-pressure, not a hard reject — the per-user RPM bucket and response-side cooldown remain in force). Adaptive enforcement is applied only on the first-attempt pick; the failover path honors the `FailoverExecutor` choice unchanged.

### Per-IP rate limit — `IpRateLimitGuard`

`IpRateLimitGuard` (`crates/gw-auth/src/ip_ratelimit.rs:17`) holds `per_ip: DashMap<IpAddr, IpLimiterEntry>` (each entry carries a `governor` limiter plus a monotonic `sequence`), a `global_per_ip_rpm: NonZeroU32`, a `max_entries` cap (default `DEFAULT_MAX_ENTRIES = 100_000`, `ip_ratelimit.rs:10`), and a `next_sequence` `AtomicU64`.

- `check(ip)` (`ip_ratelimit.rs:40`) — increments `gw_ip_ratelimit_check_total`, lazily builds a per-IP `Quota::per_minute(global_per_ip_rpm)` limiter (assigning it the next monotonic sequence), updates the `gw_ip_ratelimit_entries_active` gauge, and on `lim.check()` failure returns `ip_rpm_err(ip)` → `GW_QUOTA_IP_RPM_EXCEEDED` (429, `retryable: false`, `retry_after_secs: Some(60)`, `ip_ratelimit.rs:104`).
- `sweep()` (`ip_ratelimit.rs:63`) — increments `gw_ip_ratelimit_sweep_total`; when `len > max_entries` it sorts entries by ascending `sequence` (insertion order — an LRU-by-creation approximation) and removes the oldest `len - max_entries`, returning the count removed and refreshing the active-entries gauge.

### IP filter orchestration — `check_ip`

`check_ip(ip, user_id, api_key_id, store, allowlist_guard, ratelimit_guard, allowlist_mode)` (`crates/gw-auth/src/ip_filter.rs:25`) runs three gates in order:

1. `check_blocklist` (`ip_filter.rs:57`) — iterates the `store.ipblock_snapshot()` for `IpBlockMode::Deny` entries, gated by `IpBlockScope` (`Global` always; `User(uid)` / `ApiKey(aid)` only when the call carries the matching id). A CIDR `contains(&ip)` match increments `gw_ip_blocklist_hit_total` and returns `blocked()` → `GW_AUTH_IP_BLOCKED` (403). Malformed CIDRs are logged at `warn` and skipped (`parse_cidr`, `ip_filter.rs:129`).
2. When `allowlist_mode == IpAllowlistMode::Enforce`, `allowlist_guard.check(...)` runs (see below).
3. `ratelimit_guard.check(ip)` — a `GW_QUOTA_IP_RPM_EXCEEDED` result is propagated as the error; any *other* internal error is logged and **failed open** (`ip_filter.rs:42`).

`match_any_cidr(ip, allowlist)` (`ip_filter.rs:108`) is a standalone helper that returns `true` iff `ip` is contained in any CIDR in a `&[String]`, skipping malformed entries.

`check_ip` is invoked from both `early_request_filter` (`crates/gw-proxy/src/filters/early.rs:84`) and `request_filter` (`crates/gw-proxy/src/filters/request.rs:466`), each passing the process-wide `ip_allowlist_mode`.

### IP allowlist — `IpAllowlistGuard`

`IpAllowlistMode` is `Off | Enforce` (`crates/gw-auth/src/ip_allowlist.rs:11`); `IpAllowlistGuard::check` (`ip_allowlist.rs:26`) increments `gw_ip_allowlist_check_total`, iterates the same `ipblock_snapshot()` filtered by `scope_applies` (`ip_allowlist.rs:76`), and:

- treats `IpBlockMode::Deny` matches as an immediate `blocked()` → `GW_AUTH_IP_BLOCKED` (403),
- accumulates `IpBlockMode::Allow` entries (only under `Enforce`): tracks `has_applicable_allow` and whether any matched. If at least one in-scope Allow rule exists and none contained the IP, returns `not_in_allowlist()` → `GW_AUTH_IP_NOT_IN_ALLOWLIST` (403, `ip_allowlist.rs:106`).

Config key `ip_filter.allowlist_mode` (default `"Off"` in `config/default.toml:34`) selects the mode; it is threaded through `GatewayState` (`crates/gw-proxy/src/gateway.rs:195`) into both filter entry points.

### Error codes (all defined in `crates/gw-core/src/error/codes.rs`)

`GW_QUOTA_RPM_EXCEEDED`, `GW_QUOTA_TPM_EXCEEDED`, `GW_QUOTA_CONCURRENT_EXCEEDED`, `GW_QUOTA_IP_RPM_EXCEEDED`, `GW_AUTH_IP_BLOCKED`, `GW_AUTH_IP_NOT_IN_ALLOWLIST`.

## Deliberate behaviours

- **RPM is checked exactly once per request.** The `RateLimitTicket` from `RateLimitGuard::check` is dropped at the end of `request_filter`; there is no per-attempt re-check on failover (`crates/gw-proxy/src/filters/request.rs:1208`).
- **TPM is enforced post-response, never blocking the current request.** Because the token count is only known from the upstream response, `deduct_tpm` runs in the `logging` filter; an over-limit user is rejected on their *next* request, not the one that overshot (`crates/gw-auth/src/ratelimit/mod.rs:90`).
- **Adaptive enforcement is back-pressure, not a hard reject.** When every candidate key is over its learned ceiling, `pick_peer` sends to the originally-picked key anyway; the per-user RPM bucket and response-side cooldown are the real backstops (`crates/gw-proxy/src/filters/upstream.rs:323`).
- **The per-vendor-key adaptive ceiling is applied in `pick_peer`, not `request_filter`.** The vendor key is unknown until routing, so the former clamp in `request_filter` was dead code and was removed; enforcement now lives where the key is known (`crates/gw-proxy/src/filters/request.rs:1199`, comment).
- **Per-IP rate-limit failures fail OPEN; blocklist/allowlist failures fail CLOSED.** Only a genuine `GW_QUOTA_IP_RPM_EXCEEDED` from `ratelimit_guard.check` propagates; any other internal error there is logged and allowed through, whereas a blocklist or allowlist match hard-rejects (`crates/gw-auth/src/ip_filter.rs:42`).
- **Malformed CIDR config rows are skipped, not fatal.** Both blocklist and allowlist log the bad row at `warn` and continue, so one bad entry cannot mask the rest of the list (`crates/gw-auth/src/ip_filter.rs:92`, `crates/gw-auth/src/ip_allowlist.rs:44`).
- **IP-RPM eviction is creation-order, not true LRU.** `sweep()` orders by the monotonic insertion `sequence`, evicting oldest-created entries when over `max_entries`; it is not refreshed on access (`crates/gw-auth/src/ip_ratelimit.rs:63`).
- **Allowlist enforcement only bites when an in-scope Allow rule exists.** With `Enforce` set but zero applicable Allow entries, all IPs pass (`has_applicable_allow == false`); the allowlist is opt-in per scope (`crates/gw-auth/src/ip_allowlist.rs:68`).

## Cross-process touchpoints

- **IP blocklist/allowlist source.** Both guards read `EntityStore::ipblock_snapshot()` — the in-memory projection of `ipblock` entities owned by Go and pushed through the entity-sync channel. Scope (`Global` / `User` / `ApiKey`) and mode (`Deny` / `Allow`) come from `gw_core::entity::{IpBlockMode, IpBlockScope}` (`crates/gw-auth/src/ip_filter.rs:13`, `crates/gw-auth/src/ip_allowlist.rs:1`). Rust never writes these rows.
- **Adaptive RPM learns from upstream vendor headers.** `observe_429` consumes the upstream `x-ratelimit-limit` and `Retry-After` response headers (parsed in `crates/gw-proxy/src/gateway.rs:148`/`160`); the learned ceiling is process-local state and is not persisted or shared across pods.
- **Per-user RPM/TPM/stream limits derive from merged permissions.** `rpm_limit`, `tpm_limit`, and `max_concurrent_streams` come from `MergedPermissions`, assembled from the 5-tier permission entities that Go owns; rate limiting consumes them read-only.
- Rate-limit state itself (governor buckets, per-IP buckets, adaptive state) is per-process and is not a cross-process contract; there is no Redis/PgSQL touchpoint owned by this family.

## Invariants & lints

- `scripts/check-invariants.sh` is the architectural-lint gate; this spec passes `bash scripts/check-spec.sh docs/spec/04-rate-limiting.md`.
- **RAII stream-slot release.** `StreamSlot::drop` must decrement both the per-user counter and `total_streams` on every path; the soak harness's resource-gauge-release invariant (CLAUDE.md §Observability / chaos-soak) covers concurrent-stream leak detection.
- **Adaptive limiter cache coherence.** The cached GCRA's stored `u32` limit must equal the currently-enforced limit or be rebuilt in place under the entry lock (`crates/gw-auth/src/ratelimit/adaptive.rs:118`); `evict` must drop the cached limiter alongside the state (`adaptive.rs:176`).
- **No cost computation.** This family counts requests/streams/tokens only; the `no_cost_micro` / `no_billing_crate` invariants forbid any micro-cost arithmetic in `gw-auth` (CLAUDE.md §What this gateway does).

## Not built → ROADMAP

- **RL-Core per-key RPM gate.** There is no synchronous per-vendor-key RPM gate keyed on the vendor key's own `rpm_limit` field: no `check_key` / `override_key_rpm` entry point and no `GW_QUOTA_RPM_KEY_EXCEEDED` code exist (the `VendorKey.rpm_limit` field is present in `crates/gw-proxy/src/filters/upstream.rs` test fixtures but is not consulted by any limiter). Per-key RPM is governed today only indirectly via the *adaptive* ceiling learned from upstream 429s. The RL-Core surface is referenced as Go-delegated / placeholder in `crates/gw-core/src/lib.rs:17` and `crates/gw-proxy/src/gateway.rs:159`.
- **`RateLimitKind` 429 classifier is not wired into adaptive learning.** `gw_core::ratelimit::RateLimitKind { Rpm, Tpm, Daily, Monthly, Concurrent, Unknown }` (`crates/gw-core/src/lib.rs:20`) exists and is used by the error-classification listeners, but the value is computed as `Unknown` at the call sites and is **not** consulted before adaptive learning.
- **RL-Adaptive over-learning (runtime risk).** `AdaptiveRpmManager::observe_429` is invoked unconditionally on every upstream `429` (`crates/gw-proxy/src/gateway.rs:854`) — the `detect_rpm_kind(upstream_response)` check that follows (`gateway.rs:861`) only increments an observability counter and does not gate learning. Adaptive RPM therefore learns from TPM-exhaustion, daily/monthly-quota, and concurrency 429s as if they were RPM limits, which can drive the learned per-key ceiling to a wrong value. The intended behaviour is to learn only on `RateLimitKind::Rpm | Unknown`; gating `observe_429` on the classified kind is the corrective work.
