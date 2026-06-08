# Spec Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 51 dated specs + 24 dated plans with 9 as-built domain specs + a ROADMAP, leaving an unambiguous standard that mirrors the code.

**Architecture:** Docs-only refactor. Author 9 capability-family specs (each anchored to its backing crate(s), each verified against source by a `check-spec.sh` lint), build a ROADMAP from the 2026-06-08 audit, then cut over (delete old corpus, repoint refs, update CLAUDE.md/AGENTS.md/architecture.md). No code changes.

**Tech Stack:** Markdown; bash/grep verification gates; the existing `crates/` tree as source of truth. Spec & design references: `docs/spec/CONSOLIDATION-DESIGN.md`.

**Git policy:** Each task ends with a commit step for checkpointing. Per repo rule, only run the commits when the user has authorized committing for this session; otherwise complete the file changes and defer the `git commit`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `scripts/check-spec.sh` | Lints one `docs/spec/0N-*.md`: required sections present, no aspirational language, as-built claims carry `.rs` anchors |
| `docs/spec/README.md` | Index: family → crate → status table |
| `docs/spec/01-protocol-codec.md` … `09-tunnel.md` | The 9 as-built domain specs |
| `docs/ROADMAP.md` | Unbuilt + divergent items with actionable design points |
| `CLAUDE.md` / `AGENTS.md` | Source-layout + sync-rule repointed; v2-cleanup section removed; counts fixed (mirrored byte-identical) |
| `docs/architecture.md` | Gains a `spec/` index section; keeps persistence/invariants |
| DELETE `docs/superpowers/specs/`, `docs/superpowers/plans/` | Old corpus (git retains history) |

---

## Phase 0 — Scaffold

### Task 1: Spec lint + skeletons

**Files:**
- Create: `scripts/check-spec.sh`
- Create: `docs/spec/README.md`
- Create: `docs/ROADMAP.md`

- [ ] **Step 1: Write the spec lint**

Create `scripts/check-spec.sh`:

```bash
#!/usr/bin/env bash
# Lints one as-built domain spec for structure + no-aspirational-language + anchored claims.
set -euo pipefail
f="${1:?usage: check-spec.sh docs/spec/0N-name.md}"
fail=0
need() { grep -qE "^## $1" "$f" || { echo "[$f] MISSING section: ## $1"; fail=1; }; }
need "Purpose"
need "As-built surface"
need "Not built"
# As-built specs must not contain aspirational / status language.
if grep -nE 'Phase [0-9]|will be added|unimplemented!|todo!|TODO\(v2\)|integrated by|🚧|✅|\blanded\b' "$f"; then
  echo "[$f] aspirational/status language found (as-built specs describe only what exists)"; fail=1
fi
# The As-built surface must anchor claims to real source files.
awk '/^## As-built surface/{a=1;next}/^## /{a=0}a' "$f" | grep -qE '\.rs' \
  || { echo "[$f] As-built surface has no .rs anchors"; fail=1; }
[ "$fail" = 0 ] && echo "[$f] ok"
exit "$fail"
```

- [ ] **Step 2: Make it executable and verify it fails on an empty file**

Run:
```bash
chmod +x scripts/check-spec.sh
printf '# stub\n' > /tmp/empty-spec.md
bash scripts/check-spec.sh /tmp/empty-spec.md; echo "exit=$?"
```
Expected: prints `MISSING section: ## Purpose` (and others), `exit=1`.

- [ ] **Step 3: Write the README index skeleton**

Create `docs/spec/README.md`:

```markdown
# Gateway Spec Standard (as-built)

Authoritative, code-mirroring design standard. Each spec describes **only what exists in `crates/`**; anything unbuilt or divergent lives in [`../ROADMAP.md`](../ROADMAP.md). Cross-system overview is in [`../architecture.md`](../architecture.md); request lifecycle in [`../workflow.md`](../workflow.md).

| Spec | Backing crate(s) | Status |
|------|------------------|--------|
| [01-protocol-codec](01-protocol-codec.md) | gw-codec, gw-codec-macros | TBD |
| [02-routing](02-routing.md) | gw-routing, gw-capability | TBD |
| [03-key-health](03-key-health.md) | gw-health | TBD |
| [04-rate-limiting](04-rate-limiting.md) | gw-auth | TBD |
| [05-auth-permissions](05-auth-permissions.md) | gw-auth, gw-proxy | TBD |
| [06-billing-ledger](06-billing-ledger.md) | gw-ledger, gw-billing | TBD |
| [07-vendor-integration](07-vendor-integration.md) | gw-adapter, gw-egress-impersonate, gw-core::impersonate | TBD |
| [08-async-tasks](08-async-tasks.md) | gw-task, gw-task-runtime, gw-task-worker | TBD |
| [09-tunnel](09-tunnel.md) | gw-tunnel-client | TBD |

> History: this standard replaced 51 dated specs + 24 dated plans on 2026-06-08 (see git history and `CONSOLIDATION-DESIGN.md`).
```

The `Status` cells are filled in Task 13 once each spec lands.

- [ ] **Step 4: Write the ROADMAP skeleton**

Create `docs/ROADMAP.md`:

```markdown
# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.

## 01 protocol-codec
## 02 routing
## 03 key-health
## 04 rate-limiting
## 05 auth-permissions
## 06 billing-ledger
## 07 vendor-integration
## 08 async-tasks
## 09 tunnel
## Cross-cutting
```

- [ ] **Step 5: Commit**

```bash
git add scripts/check-spec.sh docs/spec/README.md docs/ROADMAP.md
git commit -m "docs(spec): scaffold as-built spec standard (lint + index + roadmap skeleton)"
```

## Phase 1 — Author the 9 domain specs

Each task: read the listed source files, write the spec to the template in `CONSOLIDATION-DESIGN.md §5`, then pass `check-spec.sh`. The **As-built** bullets below are the audit-confirmed real surface to document; the **→ ROADMAP** bullets must NOT appear in the spec (they go to `docs/ROADMAP.md` in Phase 2). These tasks are independent and parallelizable.

### Task 2: 01-protocol-codec

**Files:** Create `docs/spec/01-protocol-codec.md` · Read: `crates/gw-codec/src/{any.rs,passthrough.rs,cross_render.rs,cli/,openai/,anthropic/,gemini/,openai/responses/}`, `crates/gw-core/src/protocol.rs`, `crates/gw-proxy/src/filters/response.rs`

- [ ] **Step 1: Write the spec.** Document the As-built surface:
  - `AnyCodec` (`any.rs`) variants OpenAi/OpenAiResponses/Anthropic/Gemini; `detect` + per-codec `detect_request`/`parse_request`/`render_request`/`parse_response`/`render_response`.
  - Same-protocol passthrough: `passthrough.rs::same_protocol_passthrough` (zero-copy + model rewrite).
  - Cross-protocol render: `cross_render.rs::CrossRender::run` + `adapt_lossy`; metrics `gw_codec_cross_render_total` (stream, `response.rs:475`), `gw_codec_cross_render_nonstream_total` (`response.rs:750`), `gw_codec_cross_render_lossy_total{from,to,field}` (`cross_render.rs`).
  - SSE accumulators: `openai/state.rs`, `anthropic/state.rs`, `gemini/state.rs`, `openai/responses/state.rs`; tool-call block mapping `anthropic/via.rs`.
  - CLI variants (`cli/`): `ApiShape` (`gw-core/protocol.rs`) ClaudeCli/OpenAiCli/OpenAiCompact/GeminiCli + `is_cli`/`format_id`/`protocol_kind`; `detect_cli`; `CliCodec` (`extract_hints`, `usage_extractor`, `passthrough_request/response_chunk`, `ttft_event_types`); per-shape usage extractors; error code `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL`; metric `gw_cli_cross_protocol_denied_total`.
  - Note in "Not built → ROADMAP": kiro/S5 EventStream codec; `gw_cli_usage_extract_total` metric; `UsageSource::CliExtracted`; CLI lives as a parallel `cli/` module, not `AnyCodec` variants.

- [ ] **Step 2: Verify.** Run `bash scripts/check-spec.sh docs/spec/01-protocol-codec.md` — expect `ok`. Spot-check 3 `.rs` anchors resolve: `grep -n 'fn same_protocol_passthrough' crates/gw-codec/src/passthrough.rs`.

- [ ] **Step 3: Commit.** `git add docs/spec/01-protocol-codec.md && git commit -m "docs(spec): 01 protocol-codec (as-built)"`

### Task 3: 02-routing

**Files:** Create `docs/spec/02-routing.md` · Read: `crates/gw-routing/src/{strategy/,candidates.rs,pipeline/,failover.rs,in_flight.rs}`, `crates/gw-capability/src/`, `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Write the spec.** As-built surface:
  - 8 strategies (`strategy/*.rs`: fixed_order, round_robin, load_balance, weighted_rr, cost_first, latency_first, cache_affinity, sticky), `StrategyAny`, `strategy/set.rs::StrategySet`.
  - Candidate selection `candidates.rs::get_candidate_keys`; scoring `pipeline/mod.rs` + `pipeline/policies.rs`. **State explicitly: CostFirst ranks by `priority × model_markup` (`pipeline/mod.rs:229`), not per-token cost** — name vs behaviour, no `cost_micro`.
  - Failover `failover.rs::FailoverExecutor` + `classify_for_failover` (retryable set 429/500/502/503/504, before-body only); per-key in-flight cap `PER_KEY_INFLIGHT_CAP`; sticky `moka` cache (30 min TTL / 100k).
  - Capability: `gw-capability::CapabilityResolver` (model → required caps → matching keys), CLI same-protocol constraint (`upstream.rs:223`).
  - Not built → ROADMAP: R-AB multi-score rewrite (CandidateFilter/ScoringDimension/SelectorAny); R-D AffinityManager; R-F stream probe; R-E gaps (401/402/403 switch, `total_budget_ms`, codes `GW_ROUTING_NO_FALLBACK_LEFT`/`_FAILOVER_TIMEOUT`/`_STREAM_NOT_RETRIABLE`).

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/02-routing.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/02-routing.md && git commit -m "docs(spec): 02 routing (as-built)"`

### Task 4: 03-key-health

**Files:** Create `docs/spec/03-key-health.md` · Read: `crates/gw-health/src/{format_health.rs,store.rs,quota_probe.rs,lib.rs}`, `crates/gw-proxy/src/error/listeners/key_health.rs`

- [ ] **Step 1: Write the spec.** As-built surface:
  - `format_health.rs::FormatHealth` (sliding window + 3-state circuit + `cooldown_until_ms`/`reason`/`consecutive_failures`); `store.rs::KeyHealthStore` `DashMap<(KeyId,ApiFormat),Arc<FormatHealth>>`; `is_available` consumed by routing; `record` from `gw-proxy/filters/logging.rs`; `apply_cooldown` from `error/listeners/key_health.rs`.
  - Cooldown-recovery probe `quota_probe.rs` (probes keys in cooldown, 2xx → `clear_cooldown`; default-disabled). **Name it precisely as cooldown-recovery, not quota-remaining.**
  - Deliberate behaviour: `record` accumulates a bool; `OutcomeKind::Failure{http_status,retry_after}` payload is currently inert (cooldown decided by the classifier upstream).
  - Not built → ROADMAP: K-B vendor error analyzers; K-C in-Rust OAuth (see 07); K-D quota-remaining probe + `QuotaState`.

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/03-key-health.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/03-key-health.md && git commit -m "docs(spec): 03 key-health (as-built)"`

### Task 5: 04-rate-limiting

**Files:** Create `docs/spec/04-rate-limiting.md` · Read: `crates/gw-auth/src/{ratelimit/mod.rs,ratelimit/adaptive.rs,ip_ratelimit.rs,ip_filter.rs,ip_allowlist.rs}`, `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Write the spec.** As-built surface:
  - `RateLimitGuard` (`ratelimit/mod.rs`): `per_user`, `per_user_tpm`, `per_user_streams`, `total_streams`; global stream cap `GW_QUOTA_CONCURRENT_EXCEEDED`.
  - Adaptive RPM `adaptive.rs::AdaptiveRpmManager` (`observe_429`, `try_admit` GCRA, AIMD confidence/decay/probe), wired `upstream.rs:332`.
  - IP: `ip_ratelimit.rs` per-IP governor + LRU sweep; `ip_filter.rs::check_ip` (blocklist + allowlist + ratelimit); `ip_allowlist.rs`; codes `GW_QUOTA_IP_RPM_EXCEEDED`, `GW_AUTH_IP_NOT_IN_ALLOWLIST`; config `ip_filter.allowlist_mode`.
  - Not built → ROADMAP: RL-Core per-key RPM gate (`check_key`/`override_key_rpm`/`GW_QUOTA_RPM_KEY_EXCEEDED`) + `RateLimitKind` classifier; RL-Adaptive should gate learning on `Rpm|Unknown` (currently learns from every 429).

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/04-rate-limiting.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/04-rate-limiting.md && git commit -m "docs(spec): 04 rate-limiting (as-built)"`

### Task 6: 05-auth-permissions

**Files:** Create `docs/spec/05-auth-permissions.md` · Read: `crates/gw-auth/src/{jwt.rs,jwt_blacklist.rs,merge.rs,mgmt.rs}`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/signature.rs` (or wherever HMAC lives), `crates/gw-core/src/entity.rs`

- [ ] **Step 1: Write the spec.** As-built surface:
  - Credential extraction (`request.rs`): `Authorization: Bearer` → JWT (hybrid `jwt.rs` validator: iss/aud/nbf/leeway, HS256/RS256; or legacy `verify_jwt`); `(x-api-key, x-api-key-id)` pair; mgmt `parse_mgmt_authorization` (`mt_<id>.<secret>`). **No-credential → 401 `GW_AUTH_INVALID_APIKEY`.**
  - JWT blacklist `jwt_blacklist.rs::check_jwt` (FailClosed, `jwt:blacklist:` namespace, enforce-when-`jti`-present).
  - Key admission gates: `check_api_key_status` (`GW_AUTH_KEY_DISABLED`), `check_api_key_expiry` (`GW_AUTH_KEY_EXPIRED`), closed interval.
  - 5-tier permission merge `merge.rs` (global→user-group→user→sub-account→api-key) + `MarkupChainSnapshot`; model-permission regex with 100 ms ReDoS budget + auto-disable.
  - HMAC signature: ±300 s replay window; global `[signature].required` toggle (`config/default.toml` + `request.rs`) **plus** per-key `signature_required`. Document the global toggle as a real, supported feature.
  - Daily token quota: `request.rs` pre-check + `logging.rs` `daily_tokens_key` (`counter:user:{eid}:daily_tokens:{date}`), `GW_QUOTA_DAILY_TOKENS_EXCEEDED` — **Rust-side** (resolves the gap-inventory "归 Go" conflict).
  - Management tokens `ManagementTokenEntry` (`entity.rs`), migration `0007`.
  - GW_AUTH_* / GW_QUOTA_* code catalog for this surface.
  - Not built → ROADMAP: (none major — this domain is built; note only that the `no_global_hmac_force` lint is inert against the shipped toggle).

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/05-auth-permissions.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/05-auth-permissions.md && git commit -m "docs(spec): 05 auth-permissions (as-built)"`

### Task 7: 06-billing-ledger

**Files:** Create `docs/spec/06-billing-ledger.md` · Read: `crates/gw-ledger/src/{usage.rs,flusher.rs,freeze.rs,deduct_gc.rs,inflight.rs,inflight_reaper.rs,inflight_reconciler.rs,lease.rs,coef.rs,credit_estimator.rs,counter_handler.rs,protection_config.rs}`, `crates/gw-billing/src/{formulas.rs,lib.rs}`, `crates/gw-proxy/src/{billing_lifecycle.rs,filters/request.rs}`

- [ ] **Step 1: Write the spec.** As-built surface:
  - `UsageRecord` (`usage.rs`): `vendor_exposure_state` + `usage_provenance` (**no `cost_micro`**, no legacy `usage_source`); stream `usage:records`; per-attempt records.
  - Flusher `flusher.rs`: `usage:records` XADD + `usage:records:dlq` (single tier; on DLQ-fail → `gateway_usage_records_lost_total`).
  - Freeze `freeze.rs::RequestFreeze` (in-process `AtomicI64`) + `release`; `deduct_gc.rs` DEDUCT GC.
  - Billing robustness: `billing_lifecycle.rs` state machine; `inflight.rs::InflightLedger` + `inflight_reaper.rs` (T0 emit-lag watchdog) + `inflight_reconciler.rs` (crash recovery); boot wiring `gw-gateway/src/lib.rs`.
  - Credit-lease: `lease.rs`/`coef.rs`/`credit_estimator.rs`; codes `GW_QUOTA_LEASE_INSUFFICIENT`/`GW_QUOTA_LEASE_EXPIRED`; `counter_handler.rs` `events:counter` (ns `protected_lease`/`coef_table`/`coef_table_commit`); fail-closed on no-coefs.
  - Detailed-log: 5 per-surface XADD, raw/lz4/zstd tiering, drop-on-outage.
  - `gw-billing`: task freeze/settle multipliers (`formulas.rs`, image/video/audio/batch) — **not per-token cost** (guarded by `no_billing_cost_per_token`). Document why the crate legitimately exists (resolves the v2-integration "forbidden" conflict).
  - Cost boundary: Rust does no per-token cost; freeze uses flat `freeze_estimate` (1-micro/token, `request.rs:1514`).
  - Not built / divergent → ROADMAP: C-beta `EstimatePricing` entity/formula; C-gamma frozen→Redis sync + `events:counter ns=user_balance`; C-delta DLQ three-tier never-drop + frozen DLQ + retry loop; **bug:** lease `EXPIRED` returns 402 not 503; **bug:** `lease_grace_ms` configured but never applied.

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/06-billing-ledger.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/06-billing-ledger.md && git commit -m "docs(spec): 06 billing-ledger (as-built)"`

### Task 8: 07-vendor-integration

**Files:** Create `docs/spec/07-vendor-integration.md` · Read: `crates/gw-adapter/src/`, `crates/gw-egress-impersonate/src/`, `crates/gw-core/src/impersonate/`, `crates/gw-store/src/loader.rs`, `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/error/listeners/oauth.rs`

- [ ] **Step 1: Write the spec.** As-built surface (**describe what really runs, not the adapter spec**):
  - Upstream header injection is inline in `upstream.rs::inject_vendor_headers`: `auth_template` `{api_key}` substitution (honors header-name; Bearer/x-api-key/x-goog-api-key); host from `vendor.base_urls.first()`; W3C traceparent.
  - OAuth: `VendorKey.oauth` `OAuthCredentials` decrypted by `loader.rs`; `upstream.rs:1040` injects pre-decrypted `access_token`, emits `gw_oauth_token_expired_total`/`gw_oauth_token_near_expiry_total`, **sends even if expired**. **Refresh is delegated to Go** via `error/listeners/oauth.rs` reverse-XADD `events:entity`. State this as the deliberate, documented mechanism.
  - Egress impersonation: `egress_mode` Rustls|Impersonate; profiles `gw-core/impersonate/{claude_code,openai_codex,gemini_cli}.rs` (deterministic per-key identity; Stainless/UA/anthropic-beta/anthropic-version headers); `gw-egress-impersonate` `wreq`+BoringSSL loopback (JA3/JA4, HTTP/2 SETTINGS, `pool.rs`); per-key `identity_snapshot` lazy-persist; kill-switch `[egress].disabled`.
  - `gw-adapter`: only `NoopAdapter` + `DefaultErrorAnalyzer` exist; **the crate is not wired into any binary** — state this plainly as the current reality.
  - Not built → ROADMAP: the entire A-T0..T6 adapter layer (registry never called, 0/6 vendor adapters; decide: wire or delete `gw-adapter`); K-C in-Rust OAuth refresh stack; kiro adapter; vertex SA-JWT; codex body transform.

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/07-vendor-integration.md` → `ok`. Confirm the dead-crate claim: `grep -rl 'gw-adapter' crates/*/Cargo.toml | grep -v 'gw-adapter/Cargo.toml'` (expect empty).
- [ ] **Step 3: Commit.** `git add docs/spec/07-vendor-integration.md && git commit -m "docs(spec): 07 vendor-integration (as-built)"`

### Task 9: 08-async-tasks

**Files:** Create `docs/spec/08-async-tasks.md` · Read: `crates/gw-task/src/{scripts.rs,redis_contract.rs}`, `crates/gw-task-runtime/src/{exec_loop.rs,heartbeat_loop.rs,watchdog_loop.rs,release.rs}`, `crates/gw-task-worker/src/lib.rs`, `crates/gw-proxy/src/filters/task.rs`, `config/default.toml`

- [ ] **Step 1: Write the spec.** As-built surface:
  - Submit endpoint + freeze (`filters/task.rs`, freeze into in-process `gw_store::deduction::DEDUCT`); Redis contract `gw-task/scripts.rs` (`SUBMIT_LUA`/`SETTLE_LUA`/`RECOVER_LEASE_LUA`) + `redis_contract.rs`; worker claim via XREADGROUP.
  - Runtime: `exec_loop.rs` (single synchronous upstream POST), `heartbeat_loop.rs`, `watchdog_loop.rs` (lease recovery, **no leader election**); worker identity `gw-task-worker` `entity:worker` heartbeat.
  - Default-off: `task.enabled = false` (`config/default.toml`).
  - Deliberate/known: `SETTLE_LUA` only `HSET`+`ZREM`; `release.rs` exists but is unused and on a mismatched key namespace — document as current reality, flag in ROADMAP.
  - Not built / bug → ROADMAP: settle must release freeze + deduct balance + XADD usage (**currently leaks freeze, bills nothing**); gateway task-freeze should be Redis-based (in-process breaks multi-gateway); async submit→poll loop + backoff + codec render (`ir_wire_body`); watchdog leader election.

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/08-async-tasks.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/08-async-tasks.md && git commit -m "docs(spec): 08 async-tasks (as-built)"`

### Task 10: 09-tunnel

**Files:** Create `docs/spec/09-tunnel.md` · Read: `crates/gw-tunnel-client/src/`, `crates/gw-proxy/src/filters/upstream.rs` (tunnel cfg blocks), `crates/gw-proxy/Cargo.toml`, `crates/gw-gateway/Cargo.toml`, `config/default.toml`

- [ ] **Step 1: Write the spec.** As-built surface:
  - Feature-gated transport: `#[cfg(feature = "tunnel")]` `TunnelStack::resolve`/`peer_for_node` (`upstream.rs`); `VendorKey.tunnel_preference`; codes `GW_TUNNEL_NO_NODE`, `GW_TUNNEL_IMPERSONATE_CONFLICT`; `safe_dns.rs`; invariants `no_dead_tunnel_use_alias`/`tunnel_require_means_hard_fail`/`no_double_rustls`.
  - **State the boot-time/default reality plainly:** `default = []` in both Cargo.toml; the shipped release build (`.gitlab-ci.yml`) omits `--features tunnel`, so only the Direct arm (`#[cfg(not(feature = "tunnel"))]`) compiles.
  - Not built / bug → ROADMAP: tunnel off in the shipped binary (decide default); in the default build `require_tunnel=true` silently falls back to Direct; e2e tunnel test is `#[ignore]`.

- [ ] **Step 2: Verify.** `bash scripts/check-spec.sh docs/spec/09-tunnel.md` → `ok`.
- [ ] **Step 3: Commit.** `git add docs/spec/09-tunnel.md && git commit -m "docs(spec): 09 tunnel (as-built)"`

## Phase 2 — Populate ROADMAP

### Task 11: Fill docs/ROADMAP.md from the audit

**Files:** Modify `docs/ROADMAP.md`

- [ ] **Step 1: Populate each family section.** For every "Not built → ROADMAP" bullet enumerated in Tasks 2–10, write one entry under the matching `##` section using this format:

```markdown
### <Item> — `<priority>`
- **Intent:** <1–2 lines why it was specced>
- **Design points:** <interface sigs / key constants / integration接点 with current code>
- **Source:** deleted spec `<name>` (git history)
```

Required entries (priority in parens):
  - **01:** kiro/S5 EventStream codec (`capability-gap`); `gw_cli_usage_extract_total` metric (`doc-divergence`); `UsageSource::CliExtracted` (`doc-divergence`).
  - **02:** R-AB multi-score rewrite (`capability-gap`); R-D AffinityManager (`capability-gap`); R-F stream probe (`capability-gap`); R-E 401/403 switch + `total_budget_ms` + 3 missing codes (`capability-gap`).
  - **03:** K-B vendor error analyzers (`capability-gap`); K-D quota-remaining probe + `QuotaState` (`capability-gap`); inert `record` failure payload (`doc-divergence`).
  - **04:** RL-Core per-key RPM gate + `RateLimitKind` (`capability-gap`); RL-Adaptive learn-only-on-Rpm|Unknown (`runtime-risk` — mis-learns non-RPM 429s).
  - **05:** `no_global_hmac_force` lint inert vs shipped toggle (`doc-divergence`).
  - **06:** C-beta `EstimatePricing` (`capability-gap`); C-gamma frozen→Redis sync + `events:counter ns=user_balance` (`capability-gap`); C-delta DLQ three-tier never-drop (`runtime-risk`); lease `EXPIRED` 402→503 (`runtime-risk`); `lease_grace_ms` unused (`runtime-risk`).
  - **07:** full A-T0..T6 adapter layer + decide wire-or-delete `gw-adapter` (`capability-gap`); K-C in-Rust OAuth refresh (`capability-gap`); kiro adapter / vertex SA-JWT / codex body transform (`capability-gap`).
  - **08:** settle release+deduct+usage (`runtime-risk` — freeze leak + zero billing); Redis-based task freeze (`runtime-risk`); async submit→poll + backoff + codec (`capability-gap`); watchdog leader election (`capability-gap`).
  - **09:** tunnel off in shipped binary — decide default (`capability-gap`); `require_tunnel=true` silent Direct fallback in default build (`runtime-risk`); enable ignored e2e tunnel test (`doc-divergence`).
  - **Cross-cutting:** **wire `check-invariants.sh` into CI** — `.gitlab-ci.yml` only builds (`runtime-risk`, highest leverage).

- [ ] **Step 2: Verify.** `grep -c '^### ' docs/ROADMAP.md` ≥ 25; `grep -nE 'runtime-risk|capability-gap|doc-divergence' docs/ROADMAP.md | wc -l` ≥ 25.
- [ ] **Step 3: Commit.** `git add docs/ROADMAP.md && git commit -m "docs(roadmap): seed unbuilt + divergent items from 2026-06-08 audit"`

## Phase 3 — Finalize index

### Task 12: Fill README status cells

**Files:** Modify `docs/spec/README.md`

- [ ] **Step 1:** Replace each `TBD` in the status column with a one-line status (e.g. `complete` for fully-built domains like 01/04/05; `built; gaps in ROADMAP` for 02/03/06/08/09; `mostly Go-delegated; adapter layer in ROADMAP` for 07).
- [ ] **Step 2: Verify.** `! grep -q 'TBD' docs/spec/README.md`.
- [ ] **Step 3: Commit.** `git add docs/spec/README.md && git commit -m "docs(spec): finalize README index status"`

## Phase 4 — Cutover

### Task 13: Update CLAUDE.md + AGENTS.md (byte-identical)

**Files:** Modify `CLAUDE.md`, `AGENTS.md`

- [ ] **Step 1: Edit `CLAUDE.md`:**
  - "Source layout" block: replace `docs/superpowers/specs/` + `plans/` lines with `docs/spec/` (9 domain specs) and `docs/ROADMAP.md`.
  - "Sync rule" table: repoint the "design specs" / spec-referencing rows to `docs/spec/0N-*.md`; the `architecture.md`/`workflow.md` rows stay.
  - **Delete** the entire "## v2 cleanup status" section (it lists deleted sub-specs).
  - Fix counts: "15 crates" → "21 crates"; migrations "0001..0003" → list all 10 (`0001_initial`, `0002_gemini_file_mapping`, `0003_model_mapping`, `0004_capabilities`, `0004_task_worker`, `0005_proxy_nodes`, `0006_auth_configs`, `0007_management_tokens`, `0008_signature_secret`, `0009_egress_impersonation`) and note the duplicate `0004_` prefix.
  - Capability-surface bullets that link to `docs/superpowers/specs/...` → repoint to the matching `docs/spec/0N-*.md`.

- [ ] **Step 2: Mirror to `AGENTS.md`.** Make `AGENTS.md` byte-identical to `CLAUDE.md` except the H1 title. Verify:
```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "bodies identical"
```
Expected: `bodies identical`.

- [ ] **Step 3: Commit.** `git add CLAUDE.md AGENTS.md && git commit -m "docs: repoint CLAUDE/AGENTS to spec/ standard; drop v2-cleanup; fix counts"`

### Task 14: architecture.md index + repoint all cross-refs

**Files:** Modify `docs/architecture.md`; sweep `docs/` + `crates/` + `README.md`

- [ ] **Step 1:** Add a `## Spec index` section to `architecture.md` linking the 9 `spec/0N-*.md` + `ROADMAP.md`; keep persistence/invariants/deliberate-behaviours as the cross-system home.
- [ ] **Step 2: Repoint every dangling reference.** Find them:
```bash
grep -rn 'superpowers/specs\|superpowers/plans' docs/ crates/ README.md Makefile 2>/dev/null
```
Replace each hit with the matching `docs/spec/0N-*.md` or `docs/ROADMAP.md` (specs) — plan references usually drop or point to ROADMAP. Re-run the grep until empty.
- [ ] **Step 3: Commit.** `git add -A && git commit -m "docs(architecture): add spec index; repoint all superpowers/* references"`

### Task 15: Delete the old corpus

**Files:** Delete `docs/superpowers/specs/`, `docs/superpowers/plans/`

- [ ] **Step 1:** `git rm -r docs/superpowers/specs docs/superpowers/plans`. If `docs/superpowers/` is now empty, `git rm` any remaining stray and remove the dir. (Keep `CONSOLIDATION-DESIGN.md` / `CONSOLIDATION-PLAN.md` — they live in `docs/spec/`, not here.)
- [ ] **Step 2: Verify.** `test ! -d docs/superpowers/specs && test ! -d docs/superpowers/plans && echo gone`.
- [ ] **Step 3: Commit.** `git commit -m "docs: delete 51 dated specs + 24 dated plans (superseded by spec/ standard; git retains history)"`

## Phase 5 — Consistency gates

### Task 16: Final verification

- [ ] **Step 1: No dangling references.**
```bash
grep -rn 'superpowers/specs\|superpowers/plans' docs/ crates/ README.md Makefile 2>/dev/null && echo "FAIL: dangling refs" || echo "ok: no dangling refs"
```
Expected: `ok: no dangling refs`.

- [ ] **Step 2: All 9 specs lint clean.**
```bash
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f"; done
```
Expected: nine `ok` lines, exit 0.

- [ ] **Step 3: Counts in CLAUDE.md match reality.**
```bash
echo "members: $(grep -cE '^\s*"crates/' Cargo.toml)"   # expect 21
ls crates/gw-store/migrations/*.sql | wc -l               # expect 10
grep -n '21 crates\|10 ' CLAUDE.md
```
Expected: CLAUDE.md reflects 21 crates and the 10-migration list.

- [ ] **Step 4: Index integrity.** `grep -cE '^\| \[0[0-9]-' docs/spec/README.md` → `9`; `diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo identical`.

- [ ] **Step 5: Commit (if any gate-fix edits were needed).** `git add -A && git commit -m "docs: consolidation consistency gates green"`

---

## Self-Review

- **Spec coverage:** Design §3 structure → Tasks 1,15. §4 family map → Tasks 2–10. §5 template → Task 1 lint + per-spec Step 1. §6 ROADMAP → Tasks 1,11. §7 conflict resolution → embedded in Tasks 6 (daily-quota), 7 (gw-billing), 6/7 (usage_source, OAuth), 13 (issues prune happens via Task 14 sweep — **add explicit prune below**). §8 index updates → Tasks 13,14. §9 deletion → Task 15. §10 execution phases → Phases 0–5. §11 out-of-scope (no code changes) → honored; bugs go to ROADMAP only.
- **Gap found & fixed:** Design §7 prunes the self-contradicting `issues/2026-05-29-functional-audit-findings.md` + `aether-coverage-gaps.md`. Add to **Task 14 Step 2**: reconcile or `git rm` those two files' contradictory status, committing in Task 14.
- **Placeholder scan:** README/ROADMAP `TBD`s are intentional skeletons, resolved in Tasks 11/12 with explicit gates (`! grep TBD`). No un-actioned placeholders remain.
- **Type/name consistency:** `scripts/check-spec.sh` name, `docs/spec/0N-*.md` paths, and `docs/ROADMAP.md` are used identically across all tasks.

> **Task 14 addendum (from self-review):** in Step 2, also handle `docs/issues/2026-05-29-functional-audit-findings.md` and `docs/issues/2026-05-29-aether-coverage-gaps.md` — flip the stale per-section "open" labels to match their "Fixed" tables, or `git rm` if fully superseded by ROADMAP. Include in the Task 14 commit.

