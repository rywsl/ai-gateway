# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.

## 01 protocol-codec

### Kiro / S5 EventStream codec — `capability-gap`
- **Intent:** decode Kiro upstreams (AWS EventStream binary frames + embedded `<thinking>` tags) — the heaviest of the six vendor codecs.
- **Design points:** new `crates/gw-codec/src/kiro/` module: EventStream framer (length-prefixed + `crc32fast` CRCs — add the dep), wire request/response types, thinking-tag parser, request/response roundtrip. Register a Kiro variant in `gw-codec/src/any.rs::AnyCodec`. No code exists today (only stray comments in `gw-adapter`).
- **Source:** deleted `codec-s5-kiro-eventstream` + `adapter-a-t6-kiro-vendor` (git history).

### `gw_cli_usage_extract_total` metric — `doc-divergence`
- **Intent:** observability for CLI usage extraction (source = stream_event | response_body | estimated).
- **Design points:** emit `metrics::counter!("gw_cli_usage_extract_total", "shape" => …, "source" => …)` at the CLI usage-extraction sites in `crates/gw-proxy/src/filters/response.rs`; register in `crates/gw-gateway/src/metrics_catalog.rs`. `docs/workflow.md` currently advertises it but it is never emitted.
- **Source:** deleted `cli-variants-design` ADR-CLI-10.

### `UsageSource::CliExtracted` provenance — `doc-divergence`
- **Intent:** let billing distinguish CLI-extracted usage from parsed/estimated.
- **Design points:** the ledger replaced the old `usage_source` field with `usage_provenance` (`crates/gw-ledger/src/usage.rs`); decide whether a CLI-extracted provenance value is needed and add it there rather than reviving `UsageSource`.
- **Source:** deleted `cli-variants-design` ADR-CLI-10.

## 02 routing

### R-AB multi-score routing core — `capability-gap`
- **Intent:** weighted multi-dimension candidate ranking returning a stable ranked fallback list.
- **Design points:** `CandidateFilter` chain + `ScoringDimension` set + `MultiScoreStrategy`, with `Strategy::pick` returning a ranked `SmallVec` instead of today's single `Option<&Arc<VendorKey>>` (`crates/gw-routing/src/strategy/mod.rs`, `candidates.rs`). Failover would then walk a stable ranked list instead of re-running `pick` with a blacklist.
- **Source:** deleted `routing-r-ab-multi-score` (re-affirmed by `v2-integration` §335).

### R-D AffinityManager (api-key → vendor-key affinity) — `capability-gap`
- **Intent:** keep a user pinned to the same upstream key across requests to preserve prompt-cache continuity and reduce re-billing.
- **Design points:** new `crates/gw-routing/src/affinity.rs`: `AffinityManager` (`record_hit`/`get`/`invalidate_vendor_key`/`sweep_expired`), `AffinityRecord`, an `AffinityDimension`, and a sweep loop. Distinct from the existing request-level `strategy/cache_affinity.rs` (message-hash).
- **Source:** deleted `routing-r-d-affinity-manager`.

### R-F stream probe (first-chunk failover window) — `capability-gap`
- **Intent:** allow failover when a stream fails *after* the upstream's first byte but *before* any SSE event reaches the client.
- **Design points:** `StreamPhase` (Connecting/Probing/Streaming) + `probe_timeout` watchdog + `ProbeFailure` (upstream-error-body / malformed-frame / timeout) + `gw_stream_probe_*` metrics. Today `gw-proxy/src/gateway.rs` conflates first-upstream-byte with first-client-byte via `body_started`.
- **Source:** deleted `routing-r-f-stream-probe`.

### R-E failover gaps + 3 error codes — `capability-gap`
- **Intent:** complete failover semantics.
- **Design points:** add `401|402|403 → SwitchKey` (currently NotRetryable, never switched); add a `total_budget_ms` failover deadline (today only an attempt-count cap, `failover.rs::FailoverExecutor`); define the three missing codes in `crates/gw-core/src/error/codes.rs`: `GW_ROUTING_NO_FALLBACK_LEFT`, `GW_ROUTING_FAILOVER_TIMEOUT`, `GW_ROUTING_STREAM_NOT_RETRIABLE` (only `GW_ROUTING_NO_AVAILABLE_CHANNEL` exists).
- **Source:** deleted `routing-r-e-failover`.

## 03 key-health

### K-B vendor-specific error analyzers — `capability-gap`
- **Intent:** per-vendor 401/403/429 classification (account-disabled vs transient vs token-expired) instead of one blanket policy.
- **Design points:** the `UpstreamErrorAnalyzer` trait in `crates/gw-adapter/src/analyzer.rs` is a dead skeleton (only `DefaultErrorAnalyzer`, never invoked in prod). Real classification lives in `crates/gw-core/src/error/classify_default.rs`. Add vendor analyzers there, plus body/keyword tables (account-disabled patterns) — today `gateway.rs` passes `None` for the body so 4xx bodies are never inspected.
- **Source:** deleted `key-health-k-b-vendor-error-hook`.

### K-D quota-remaining probe + `QuotaState` — `capability-gap`
- **Intent:** feed routing a quota-remaining signal so exhausted accounts are deselected before a guaranteed 429.
- **Design points:** `VendorKey.quota_state: QuotaState` + entity `quota_*` columns; `QuotaSnapshotExtractor` (codex/anthropic rate-limit headers) in `response.rs`; a `QuotaProber` + scheduler writing remaining quota back to the entity. **Do not confuse** with the existing `crates/gw-health/src/quota_probe.rs`, which is an unrelated default-off cooldown-recovery prober.
- **Source:** deleted `key-health-k-d-quota-probe`.

### Inert `record()` failure payload — `doc-divergence`
- **Intent:** let `OutcomeKind::Failure{http_status, retry_after}` drive graded cooldown.
- **Design points:** `crates/gw-health/src/store.rs::record` currently discards the payload (`let _ = outcome`) and only counts a bool; either wire the payload into a graded-cooldown decision or drop the unused fields.
- **Source:** deleted `key-health-k-a-state-machine` ADR-KA-4.


## 04 rate-limiting

### RL-Core per-key RPM gate + `RateLimitKind` classifier — `capability-gap`
- **Intent:** a synchronous per-vendor-key RPM gate and a typed 429 classifier — the dependency RL-Adaptive's own spec assumes exists.
- **Design points:** add `per_key` / `check_key` / `override_key_rpm` / `reset_key_rpm` to `RateLimitGuard` (`crates/gw-auth/src/ratelimit/mod.rs`) + code `GW_QUOTA_RPM_KEY_EXCEEDED`. `RateLimitKind` already exists as an RL-Core placeholder at `crates/gw-core/src/ratelimit` (variants Rpm/Tpm/Daily/Monthly/Concurrent/Unknown) but call sites hard-code `Unknown`; implement `detect_rate_limit_kind` to populate it.
- **Source:** deleted `ratelimit-rl-core`.

### RL-Adaptive learns from non-RPM 429s — `runtime-risk`
- **Intent:** only RPM-class 429s should lower a key's learned RPM ceiling.
- **Design points:** `crates/gw-proxy/src/gateway.rs:854` calls `observe_429` unconditionally; `detect_rpm_kind` (`gateway.rs:861`) only bumps a counter. Gate `AdaptiveRpmManager::observe_429` (`crates/gw-auth/src/ratelimit/adaptive.rs`) on `RateLimitKind::Rpm | Unknown`; skip Tpm/Daily/Monthly/Concurrent. Depends on the classifier above.
- **Source:** deleted `ratelimit-rl-adaptive` ADR-RLA-6.

## 05 auth-permissions

### `no_global_hmac_force` lint is inert vs the shipped global toggle — `doc-divergence`
- **Intent:** keep invariant lints meaningful.
- **Design points:** `scripts/check-invariants.sh` (`no_global_hmac_force`, ~line 241) greps for identifier spellings (`global_signature_required`, `signature_required_workspace_wide`, `force_signature_global`) that do not match the shipped `cfg.signature.required` field, so the lint never fires. Either retire the lint (the global `[signature].required` toggle is a supported feature — see `spec/05`) or repoint it.
- **Source:** deleted `known-gaps-design` ADR-G7-2.

## 06 billing-ledger

### C-beta `EstimatePricing` entity + freeze formula — `capability-gap`
- **Intent:** conservative per-model freeze estimate instead of a flat 1-micro/token.
- **Design points:** `EstimatePricing` entity (`conservative_per_token_micro`, `safety_factor`, `default_max_tokens`, `hard_cap_tokens`) + `compute_freeze_estimate = (input + output_cap) × per_token × safety_factor`, loaded from `entity:estimate_pricing`. Today `crates/gw-proxy/src/filters/request.rs` uses a flat `freeze_estimate` and `estimate_pricing_version` is a literal `0`. Must stay free of per-token *cost* (cost is Go's; `no_cost_micro`/`no_billing_cost_per_token` lints).
- **Source:** deleted `billing-c-beta-estimate-pricing`.

### C-gamma frozen→Redis sync + `events:counter ns=user_balance` consumer — `capability-gap`
- **Intent:** cross-Pod overdraft visibility (today `frozen_micro` is process-local).
- **Design points:** a `LedgerMsg::FrozenDelta` flusher path doing `HINCRBY counter:user:<uid>:frozen_micro` on freeze/release (`crates/gw-ledger/src/freeze.rs` + `flusher.rs`), and an `events:counter ns=user_balance` handler overwriting local `deducted_micro`/`base_micro` from Go's `value_after` (`crates/gw-ledger/src/counter_handler.rs` only handles lease namespaces today). Largely superseded by credit-lease for the hard cap, but the soft `available()` path still reads never-synced counters.
- **Source:** deleted `billing-c-gamma-balance-split`.

### C-delta DLQ three-tier "never silently drop" — `runtime-risk`
- **Intent:** guarantee no billing record is ever lost.
- **Design points:** today `crates/gw-ledger/src/flusher.rs` has one `usage:records:dlq` tier and, on DLQ-XADD failure, bumps `gateway_usage_records_lost_total` and drops. Add the spec's fallback chain: DLQ stream → in-memory retry queue → local file `${data_dir}/gw-ledger-emergency-dlq-*.jsonl`, a 30 s DLQ retry loop with attempt counter → `usage:records:dlq:permanent_failed`, and a separate `counter:frozen:dlq`.
- **Source:** deleted `billing-c-delta-stream-dlq`.

### credit-lease `GW_QUOTA_LEASE_EXPIRED` returns 402, should be 503 — `runtime-risk`
- **Intent:** surface a Go-pipeline outage as a retryable ops signal, not a user payment error.
- **Design points:** `crates/gw-proxy/src/filters/request.rs` (the `Err(LeaseError::Expired)` arm) sets `status: 402`; the spec table and `crates/gw-core/src/error/codes.rs` doc say 503. Change the status to 503.
- **Source:** deleted `credit-lease-design` §11.

### credit-lease `lease_grace_ms` never applied — `runtime-risk`
- **Intent:** tolerate small Pod/Go clock skew before expiring a lease.
- **Design points:** `ProtectionConfig.lease_grace_ms` (`crates/gw-ledger/src/protection_config.rs`, default 5_000) is parsed but never read; `LeaseGuard::try_acquire` (`crates/gw-ledger/src/lease.rs`) compares bare `now_ms > expires_at` with no grace term. Add `+ lease_grace_ms` to the expiry check.
- **Source:** deleted `credit-lease-design` §7.


## 07 vendor-integration

### Full A-T0..A-T6 vendor adapter layer + wire-or-delete `gw-adapter` — `capability-gap`
- **Intent:** per-vendor outbound rewrite (OAuth bearer, fingerprint headers, URL rewrite, body transforms) behind one trait, reused by proxy + task runtime.
- **Design points:** `crates/gw-adapter` ships only `NoopAdapter` + `DefaultErrorAnalyzer` and **is depended on by no binary** (`grep -rl gw-adapter crates/*/Cargo.toml` → only its own Cargo.toml); the registry is never called. Today the equivalent behavior is inline in `gw-proxy::inject_vendor_headers` + `gw-core::impersonate`. **First decide:** wire `AdapterRegistry::for_vendor` into `gw-proxy`/`gw-task-runtime` and build the 6 vendor adapters, OR delete the dead crate and bless the inline+impersonate design. `AdapterCtx` also lacks the `oauth_cache` field its hooks assume (`ctx.rs` comment).
- **Source:** deleted `adapter-a-t0..t6`, `adapter-layer-design`.

### K-C in-Rust OAuth refresh stack — `capability-gap`
- **Intent:** refresh vendor OAuth tokens in Rust instead of delegating to Go.
- **Design points:** today there is no `OAuthRefresher`/`OauthCache`/`get_access_token`; `gw-proxy/src/filters/upstream.rs` injects a pre-decrypted, possibly-expired `access_token` and Go refreshes async via the `events:entity` reverse-XADD (`error/listeners/oauth.rs`). If brought in-Rust: `OauthCache` with CAS-lock lazy refresh + per-vendor `OAuthRefresher` (e.g. `console.anthropic.com/v1/oauth/token`) + write-back of `oauth_*_enc` to `entity:vendor_key`. **Note:** the current Go-delegated design is deliberate and documented in `spec/07`; only build this if moving the boundary.
- **Source:** deleted `key-health-k-c-oauth-token`, `adapter-a-t1-claude-code`.

### kiro adapter / vertex_ai SA-JWT / codex body transform — `capability-gap`
- **Intent:** the heavier vendor adapters.
- **Design points:** kiro AWS SigV4 + EventStream (pairs with the codec gap in §01); vertex_ai RS256 service-account JWT signing + token exchange (`jsonwebtoken` is currently used only for *inbound* client JWTs); codex `patch_request` body image transform. None exist.
- **Source:** deleted `adapter-a-t3-vertex-ai`, `adapter-a-t5-codex`, `adapter-a-t6-kiro-vendor`.

## 08 async-tasks

### Settle must release freeze + deduct balance + emit usage — `runtime-risk`
- **Intent:** stop frozen funds leaking and tasks being billed nothing.
- **Design points:** `SETTLE_LUA` (`crates/gw-task/src/scripts.rs`) only does `HSET status` + `ZREM` lease. Add `HINCRBY` to release the task freeze, `HINCRBY` to add the actual deduction, and `XADD usage:records` for the completed task. `exec_loop.rs` already computes `actual_micro` via `gw_billing::calculate_settle_amount` but only stores it as a field.
- **Source:** deleted `task-worker-design` ADR-T3/T8.

### Redis-based task freeze (not in-process) — `runtime-risk`
- **Intent:** correct accounting across N gateways.
- **Design points:** `crates/gw-proxy/src/filters/task.rs` freezes into in-process `gw_store::deduction::DEDUCT`; spec wants `HINCRBY counter:user:<uid>:task_frozen_micro`. `gw-ledger::TaskFreeze` exists but is unused; the existing `release.rs` operates on a hash-tagged key namespace that doesn't match what the contract writes — reconcile the namespace as part of this.
- **Source:** deleted `task-worker-design` ADR-T8/T16.

### Async submit→poll long-task model — `capability-gap`
- **Intent:** actually poll provider LRO/video/batch jobs.
- **Design points:** `exec_loop.rs::call_upstream` does one synchronous POST treated as terminal. Add: write `provider_task_id`, enter a poll loop with per-vendor exponential backoff (cap 30 s), parse terminal responses via `gw-codec`, and have the gateway codec-render `ir_wire_body` for the worker to send verbatim (worker currently depends on none of gw-codec/gw-routing/gw-adapter).
- **Source:** deleted `task-worker-design` ADR-T6/T3.

### Watchdog leader election — `capability-gap`
- **Intent:** one replica runs the lease-recovery scan, not all.
- **Design points:** `watchdog_loop.rs` has no leader election; add `SETNX lock:tasks:watchdog` (TTL 10 s) gating the scan. (ZREM-guarded idempotency mitigates correctness today but not the design.)
- **Source:** deleted `task-worker-design` ADR-T11.

## 09 tunnel

### Tunnel is off in the shipped binary — `capability-gap`
- **Intent:** make the tunnel data path actually compile into the release artifact.
- **Design points:** `default = []` in `crates/gw-proxy/Cargo.toml` and `crates/gw-gateway/Cargo.toml`; `.gitlab-ci.yml` builds without `--features tunnel`, so only the `#[cfg(not(feature = "tunnel"))]` Direct arm ships. Decide whether to enable by default or build a tunnel-enabled artifact.
- **Source:** deleted `tunnel-t-design` §1/§9.

### `require_tunnel=true` silently falls back to Direct in the default build — `runtime-risk`
- **Intent:** honor the hard-fail contract the spec was written to guarantee.
- **Design points:** the `GW_TUNNEL_NO_NODE` hard-fail lives only in the `#[cfg(feature = "tunnel")]` arm (`gw-proxy/src/filters/upstream.rs`); the non-tunnel arm builds a Direct peer unconditionally and never consults `VendorKey.tunnel_preference.require_tunnel` (a non-gated `gw-core` field). Make the non-tunnel build reject `require_tunnel=true`.
- **Source:** deleted `tunnel-t-design` ADR-T-4 (invariant `tunnel_require_means_hard_fail`).

### Misc tunnel gaps (found during authoring) — `doc-divergence`
- **Intent:** finish partially-wired tunnel surfaces.
- **Design points:** `GW_TUNNEL_DNS_BLOCKED` is defined but never emitted; `last_pong_age_secs` is always `None`; `auth_token_enc` / `ControlAuth` are never sent on the control channel; the e2e tunnel test (`crates/gw-gateway/tests/e2e_tunnel.rs`) is `#[ignore]` + feature-gated. Emit/enable each.
- **Source:** authoring pass over `crates/gw-tunnel-client` + `gw-proxy` (2026-06-08).

## Cross-cutting

### Wire `check-invariants.sh` into CI — `runtime-risk`
- **Intent:** highest-leverage fix — without it every invariant lint is unenforced and doc⇄code drift is unchecked.
- **Design points:** `.gitlab-ci.yml` currently runs only `cargo build --release --locked -p gw-gateway` + a buildah push — no `cargo test`, `clippy`, `fmt`, or `bash scripts/check-invariants.sh`. Add a test/lint stage running all four (CLAUDE.md already calls them "CI gates"). Optionally add `scripts/check-spec.sh docs/spec/0*.md` to keep the new standard honest.
- **Source:** 2026-06-08 audit (cross-cutting C2).

