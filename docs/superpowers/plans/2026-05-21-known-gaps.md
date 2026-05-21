# 2026-05-21 — Known-Gaps Closure Plan

> **Source spec**: [`docs/superpowers/specs/2026-05-21-known-gaps-design.md`](../specs/2026-05-21-known-gaps-design.md). Read it first — this plan executes that spec's ADRs.
>
> **Execution model**: subagent-driven. Each task is one subagent's scope, designed to be implementable in isolation, with explicit path-restricted commits to avoid parallel race. TDD discipline throughout.
>
> **Branch**: `feature/known-gaps-closure` (already created from `origin/main`).

## Wave overview

```
Wave 0  prep / shared scaffolding             [SERIAL, single subagent]
Wave 1  auth & permission hardening (5 tasks) [SERIAL — all touch request.rs]
Wave 2  quota / markup / strategy / DLQ (4)   [PARALLEL, 4 subagents]
Wave 3  codec / retry / capability (3 way)    [PARALLEL, 3 subagents]
Wave 4  cross-protocol stream render          [SERIAL, depends on Wave 3]
Wave 5  consolidation                          [SERIAL]
```

Max parallel width: **4** (Wave 2). Wave 1 is serial by design (Plan A from spec review) — T1-A/B/D/E all edit `filters/request.rs`, so race-conflict avoidance trumps parallel speedup. Critical path: Wave 0 → 1 → 2 → 3 → 4 → 5.

## Task index

| ID | Wave | Title | Spec ADR | Touches |
|----|------|-------|----------|---------|
| T0-A | 0 | Error codes + ReqCtx fields (BytesMut accum) + Wired placeholders + EntityStore.auth_config | (shared) | `gw-core`, `gw-proxy::ctx`, `gw-proxy::gateway`, `gw-gateway::lib`, `gw-store::entity_store`, `gw-store::loader` |
| T0-B | 0 | StrategySet scaffold | G12 prereq | `gw-routing::strategy::set` |
| T0-C | 0 | 0004 capabilities migration + entity fields | G13 prereq | `gw-store/migrations`, `gw-core::entity` |
| T1-A | 1 | Enforce `ApiKeyEntry.expires_at` | G1 | `gw-proxy::filters::request` |
| T1-B | 1 | Enforce `ApiKeyEntry.ip_allowlist` | G2 | `gw-proxy::filters::request`, `gw-auth::ip_filter` |
| T1-C | 1 | Vendor.timeout_overrides precedence | G4 | `gw-proxy::filters::upstream` |
| T1-D | 1 | Wire `verify_body` at request_filter EOS | G7 | `gw-proxy::body_verify`, `gw-proxy::filters::request` |
| T1-E | 1 | Hybrid JwtValidator (JWKS + iss/aud/nbf) on client bearer path | G6 | `gw-auth::jwt`, `gw-proxy::filters::request`, `gw-gateway::lib` |
| T2-A | 2 | Daily quota: store helper + dual enforcement | G3 | `gw-store::counter_store`, `gw-proxy::filters::request`, `gw-proxy::filters::logging` |
| T2-B | 2 | Pricing.model_markup → MarkupChainSnapshot | G5 | `gw-proxy::filters::request`, `gw-auth::merge` |
| T2-C | 2 | Sticky moka + shared StrategySet | G12 | `gw-routing::strategy::sticky`, `gw-routing::strategy::set`, `gw-proxy::filters::upstream`, `gw-gateway::lib` |
| T2-D | 2 | usage:records:dlq fallback | G11 | `gw-ledger::flusher` |
| T3-A | 3 | parse_stream_chunk → SmallVec | G9 | `gw-codec::trait_def`, all codec impls, `gw-proxy::filters::response` |
| T3-B | 3 | fail_to_connect retry hook | G10 | `gw-proxy::gateway`, `gw-routing::candidates`, `gw-proxy::filters::upstream` |
| T3-C | 3 | CapabilityResolver injection + EntityStoreResolveBackend | G13 | `gw-capability::resolver`, `gw-gateway::lib`, `gw-proxy::gateway`, `gw-proxy::filters::request`, `gw-routing::candidates`, `gw-proxy::filters::upstream` |
| T4-A | 4 | Cross-protocol stream chunk render | G8 | `gw-proxy::filters::response`, `gw-proxy::ctx` |
| T5-A | 5 | Consolidate: invariants, docs, final test sweep | (shared) | `scripts/check-invariants.sh`, `docs/architecture.md` |

---

## Conventions (apply to all tasks)

1. **TDD**: write the failing test first, run `cargo test -p <crate> --test <test>` to confirm RED, write the impl, confirm GREEN, then commit. Each task's "Test code skeleton" section is the failing test; do not modify it during impl unless the test itself is wrong.
2. **Path-restricted commits**: each task lists the exact `git add <path1> <path2> ...` it must use. Never `git add .`. If a subagent finds files outside its allowlist needing changes, escalate (`NEEDS_CONTEXT`).
3. **One subagent = one commit**. The commit message in each task is final — don't rewrite the subject. Body may add a "Tests" section listing the test names you added.
4. **Verify before commit**:
   - `cargo build -p <crate>` clean
   - `cargo test -p <crate>` passing (including the new test)
   - `cargo clippy -p <crate> --all-targets -- -D warnings` clean
   - `cargo fmt --check` clean for files touched
5. **Wave boundary**: do not start a Wave N+1 task until all Wave N tasks are committed. Wave 0 → Wave 5 is strict.
6. **Cargo.lock drift**: if a task adds a dependency, commit `Cargo.lock` alongside the code in the same commit. If a different task surfaces `Cargo.lock` drift, the wave coordinator (you, the controller) does a small `chore: refresh Cargo.lock` commit at wave close.
7. **Subagent dispatch**: use the templates at `~/.claude/plugins/cache/claude-plugins-official/superpowers/5.1.0/skills/subagent-driven-development/{implementer,spec-reviewer,code-quality-reviewer}-prompt.md`. Spec reviewer must pass before code-quality reviewer; both must pass before next-wave dispatch.

---

# Wave 0 — Prep

Serial. One subagent. Lays foundation that Wave 1+ depends on but doesn't itself implement any gap behaviour.

## T0-A — Error codes + ReqCtx fields + Wired placeholders + AuthConfig cache

**Goal**: declare every new `ErrorIR.code` constant used in Wave 1+, extend `ReqCtx`, add `Option<None>` placeholders on `LlmGateway` / `Wired` so subsequent tasks can flip them on without touching shared structures, **and add the `EntityStore.auth_config` cache + loader** that T1-E will populate.

**Files touched** (path allowlist):
- `crates/gw-core/src/error/codes.rs`
- `crates/gw-proxy/src/ctx.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-store/src/entity_store.rs`
- `crates/gw-store/src/loader.rs`

**Steps**:

1. **Add new error codes** at `crates/gw-core/src/error/codes.rs`:

   ```rust
   pub const GW_AUTH_KEY_EXPIRED: &str = "GW_AUTH_KEY_EXPIRED";
   pub const GW_AUTH_KEY_IP_NOT_ALLOWED: &str = "GW_AUTH_KEY_IP_NOT_ALLOWED";
   pub const GW_AUTH_JWT_INVALID_ISS: &str = "GW_AUTH_JWT_INVALID_ISS";
   pub const GW_AUTH_JWT_INVALID_AUD: &str = "GW_AUTH_JWT_INVALID_AUD";
   pub const GW_AUTH_JWT_NOT_YET_VALID: &str = "GW_AUTH_JWT_NOT_YET_VALID";
   pub const GW_AUTH_SIGNATURE_INVALID: &str = "GW_AUTH_SIGNATURE_INVALID";
   pub const GW_AUTH_SIGNATURE_MISSING: &str = "GW_AUTH_SIGNATURE_MISSING";
   pub const GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT: &str = "GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT";
   pub const GW_QUOTA_DAILY_TOKENS_EXCEEDED: &str = "GW_QUOTA_DAILY_TOKENS_EXCEEDED";
   pub const GW_ROUTING_NO_CAPABILITY_MATCH: &str = "GW_ROUTING_NO_CAPABILITY_MATCH";
   pub const GW_UPSTREAM_CONNECT_FAILED: &str = "GW_UPSTREAM_CONNECT_FAILED";
   pub const GW_UPSTREAM_TLS_ERROR: &str = "GW_UPSTREAM_TLS_ERROR";
   ```

   If the codes file uses an enum + str map, follow the existing pattern. Don't restructure.

2. **Extend `ReqCtx`** at `crates/gw-proxy/src/ctx.rs`:
   - Add `pub retry_blacklist: Vec<gw_core::ids::KeyId>` (initialise `Vec::new()` in `Default`/`new`).
   - Add `pub pending_client_body: Option<bytes::BytesMut>` — **accumulator**, not single-shot. T4-A iterates a `SmallVec` of stream events and appends each rendered chunk into this `BytesMut`; the outer body filter writes the whole buffer in one shot. Using `Option<bytes::Bytes>` would silently drop earlier events.
   - Verify `retry_count: u32` already exists at line 87 (per audit); if missing, add it.
   - Verify `required_caps: Option<RequiredCapabilities>` exists at line 111; if missing, add it gated behind `gw-capability` import. **Do not** add `gw-capability` as a `gw-proxy` direct dep if it isn't already — that's T3-C's job. If absent here, leave `required_caps` as a `Vec<&'static str>` placeholder typed properly in T3-C.

3. **Add `Option<Arc<…>>` fields to `LlmGateway`** at `crates/gw-proxy/src/gateway.rs`:
   - `pub jwt_validator: Option<std::sync::Arc<gw_auth::jwt::JwtValidator>>` — initialise `None` in `LlmGateway::new`.
   - `pub strategies: std::sync::Arc<gw_routing::strategy::set::StrategySet>` — **this depends on T0-B**; add the field but use a sentinel `Arc::new(StrategySet::placeholder())` until T0-B lands the real struct. (T0-B runs concurrently in Wave 0 — see ordering note.)
   - `pub capability_resolver: Option<std::sync::Arc<gw_capability::CapabilityResolver>>` — initialise `None`.

   **Cross-task ordering inside Wave 0**: T0-A and T0-B both touch `crates/gw-proxy/src/gateway.rs` and `crates/gw-routing`. Run **T0-B first**, then T0-A picks up the real `StrategySet`. T0-C is independent of both.

4. **Add matching fields to `Wired`** at `crates/gw-gateway/src/lib.rs`:
   - `jwt_validator: Option<Arc<JwtValidator>>` (init `None` in `bootstrap`)
   - `strategies: Arc<StrategySet>` (init with `Arc::new(StrategySet::new_default())` — relies on T0-B)
   - `capability_resolver: Option<Arc<CapabilityResolver>>` (init `None`)

   Update the `LlmGateway::new(...)` call site to pass these through.

5. **Add `AuthConfig` cache + loader** in `gw-store`. T1-E will populate it; this task only declares the surface so T1-E doesn't have to touch `gw-store` files that other Wave 1 / Wave 2 tasks are also editing.

   In `crates/gw-store/src/entity_store.rs`, add a Moka cache field next to the existing per-entity caches:

   ```rust
   pub auth_config: moka::future::Cache<String, Arc<Versioned<AuthConfig>>>,
   ```

   Initialise in `EntityStore::new(...)` with the same TTL pattern as `jwks` (3600 s). Add a method:

   ```rust
   pub async fn auth_config_or_load(&self, id: &str) -> Result<Arc<Versioned<AuthConfig>>, LoaderError> {
       loader::cached_or_load(&self.auth_config, id, |k| loader::load_auth_config(&self.redis, &self.pg, k)).await
   }
   ```

   In `crates/gw-store/src/loader.rs`, add `pub async fn load_auth_config(...)` mirroring the shape of `load_jwks` (read Redis L1 first, fall back to Postgres `auth_configs` table by id, write back to Redis on miss). If the table does not exist yet — it should, since `AuthConfig` is in `entity.rs:280-287` — surface this in T1-E for a migration follow-up, **but** for v1.5 absent table is acceptable: the loader returns `LoaderError::NotFound` which T1-E maps to "permissive defaults".

6. **Test code skeleton** — a single compile-test that asserts the struct fields exist:

   File: `crates/gw-proxy/tests/wave0_scaffold.rs` (new)

   ```rust
   //! Wave 0 compile-time assertions. If this builds, the scaffolding is in place.
   use gw_proxy::ctx::ReqCtx;

   #[test]
   fn reqctx_has_wave0_fields() {
       let ctx = ReqCtx::default();
       let _: &Vec<gw_core::ids::KeyId> = &ctx.retry_blacklist;
       let _: &Option<bytes::BytesMut> = &ctx.pending_client_body;
       let _: u32 = ctx.retry_count;
   }

   #[test]
   fn error_codes_are_defined() {
       use gw_core::error::codes::*;
       assert_eq!(GW_AUTH_KEY_EXPIRED, "GW_AUTH_KEY_EXPIRED");
       assert_eq!(GW_UPSTREAM_CONNECT_FAILED, "GW_UPSTREAM_CONNECT_FAILED");
       assert_eq!(GW_ROUTING_NO_CAPABILITY_MATCH, "GW_ROUTING_NO_CAPABILITY_MATCH");
   }

   #[test]
   fn entity_store_has_auth_config_cache() {
       // Pure compile check — if EntityStore::auth_config exists this builds.
       fn _t(s: &gw_store::entity_store::EntityStore) {
           let _: &moka::future::Cache<String, _> = &s.auth_config;
       }
   }
   ```

7. **Verify**:
   - `cargo test -p gw-proxy --test wave0_scaffold`
   - `cargo build --workspace`
   - `cargo clippy --workspace --all-targets -- -D warnings`

8. **Commit**:
   ```
   git add crates/gw-core/src/error/codes.rs crates/gw-proxy/src/ctx.rs \
           crates/gw-proxy/src/gateway.rs crates/gw-gateway/src/lib.rs \
           crates/gw-store/src/entity_store.rs crates/gw-store/src/loader.rs \
           crates/gw-proxy/tests/wave0_scaffold.rs

   git commit -m "wave0(scaffold): error codes, ReqCtx fields, Wired placeholders, AuthConfig cache

   Lays foundation for Wave 1+ tasks:
   - Error codes for all new auth/quota/routing/upstream rejections.
   - ReqCtx fields: retry_blacklist for cross-key retry (G10),
     pending_client_body: Option<BytesMut> as an accumulator so T4-A
     can append multiple stream events per chunk without losing earlier
     ones (G8 + G9 interaction).
   - Option<Arc<_>> placeholders on LlmGateway / Wired for JwtValidator
     (T1-E), StrategySet (T0-B), CapabilityResolver (T3-C).
   - EntityStore.auth_config Moka cache + loader::load_auth_config so
     T1-E can build JwtValidator from AuthConfig.global without
     touching gw-store files concurrently with other tasks.

   Spec ADR refs: G1, G2, G3, G6 (hybrid), G7, G8, G9, G10, G11, G13.

   No behaviour change. Compile-test in tests/wave0_scaffold.rs asserts
   the struct shape."
   ```

---

## T0-B — StrategySet container

**Goal**: introduce a per-process container that owns one instance of each stateful strategy (especially `Sticky`), so requests share state instead of constructing fresh strategies in `strategy_for()`.

**Files touched**:
- `crates/gw-routing/src/strategy/set.rs` (new)
- `crates/gw-routing/src/strategy/mod.rs`

**Steps**:

1. Create `crates/gw-routing/src/strategy/set.rs`:

   ```rust
   //! StrategySet — per-process container of stateful strategy instances.
   //!
   //! Sticky (and any future stateful strategy) must be shared across requests.
   //! Stateless strategies (FixedOrder, RoundRobin, etc.) are kept here too for
   //! a uniform dispatch surface.

   use crate::strategy::{
       cache_affinity::CacheAffinity, cost_first::CostFirst, fixed_order::FixedOrder,
       latency_first::LatencyFirst, load_balance::LoadBalance, round_robin::RoundRobin,
       sticky::Sticky, weighted_rr::WeightedRr, StrategyKind,
   };

   pub struct StrategySet {
       pub fixed_order: FixedOrder,
       pub round_robin: RoundRobin,
       pub load_balance: LoadBalance,
       pub weighted_rr: WeightedRr,
       pub cost_first: CostFirst,
       pub latency_first: LatencyFirst,
       pub cache_affinity: CacheAffinity,
       pub sticky: Sticky, // T2-C migrates to moka-backed shared state
   }

   impl StrategySet {
       pub fn new_default() -> Self {
           Self {
               fixed_order: FixedOrder::default(),
               round_robin: RoundRobin::default(),
               load_balance: LoadBalance::default(),
               weighted_rr: WeightedRr::default(),
               cost_first: CostFirst::default(),
               latency_first: LatencyFirst::default(),
               cache_affinity: CacheAffinity::default(),
               sticky: Sticky::default(),
           }
       }

       /// Sentinel used during Wave 0 scaffolding until T2-C wires Sticky moka.
       /// Equivalent to new_default for now; kept as a separate constructor so
       /// future divergence is explicit.
       pub fn placeholder() -> Self { Self::new_default() }

       pub fn pick_for(&self, kind: StrategyKind) -> &dyn crate::strategy::Strategy {
           match kind {
               StrategyKind::FixedOrder => &self.fixed_order,
               StrategyKind::RoundRobin => &self.round_robin,
               StrategyKind::LoadBalance => &self.load_balance,
               StrategyKind::WeightedRr => &self.weighted_rr,
               StrategyKind::CostFirst => &self.cost_first,
               StrategyKind::LatencyFirst => &self.latency_first,
               StrategyKind::CacheAffinity => &self.cache_affinity,
               StrategyKind::Sticky => &self.sticky,
           }
       }
   }
   ```

   Match the existing `StrategyKind` variants exactly (read `crates/gw-routing/src/strategy/mod.rs` first).

2. Add `pub mod set;` to `crates/gw-routing/src/strategy/mod.rs` and re-export `pub use set::StrategySet;` if other modules use the unqualified name.

3. **Test code skeleton** — `crates/gw-routing/tests/strategy_set.rs` (new):

   ```rust
   use gw_routing::strategy::set::StrategySet;
   use gw_routing::strategy::StrategyKind;

   #[test]
   fn strategy_set_dispatches_each_kind() {
       let set = StrategySet::new_default();
       for kind in [
           StrategyKind::FixedOrder,
           StrategyKind::RoundRobin,
           StrategyKind::LoadBalance,
           StrategyKind::WeightedRr,
           StrategyKind::CostFirst,
           StrategyKind::LatencyFirst,
           StrategyKind::CacheAffinity,
           StrategyKind::Sticky,
       ] {
           let s = set.pick_for(kind);
           assert!(!s.name().is_empty(), "strategy {:?} returns name", kind);
       }
   }
   ```

4. **Verify**: `cargo test -p gw-routing --test strategy_set`, build, clippy, fmt.

5. **Commit**:
   ```
   git add crates/gw-routing/src/strategy/set.rs \
           crates/gw-routing/src/strategy/mod.rs \
           crates/gw-routing/tests/strategy_set.rs

   git commit -m "wave0(strategy): introduce StrategySet container for shared instances

   Stateful strategies (Sticky, and future LatencyFirst with EWMA tracking,
   etc.) must be instantiated once per process and shared across requests.
   StrategySet holds one instance of each StrategyKind variant and exposes
   pick_for(kind) -> &dyn Strategy for dispatch.

   T2-C migrates Sticky to moka-backed shared state; the container shape lets
   that happen without touching every call site. T0-A wires Arc<StrategySet>
   into LlmGateway/Wired.

   Spec ADR refs: G12."
   ```

---

## T0-C — 0004 capabilities migration + entity fields

**Goal**: declare the schema additions needed by T3-C (`Model.capabilities`, `VendorKey.forced_capabilities`, `VendorKey.exclusive_capability_match`). Entity struct + migration only — no read/write logic; loader changes are T3-C.

**Files touched**:
- `crates/gw-store/migrations/0004_capabilities.sql` (new)
- `crates/gw-core/src/entity.rs`

**Steps**:

1. Inspect `crates/gw-core/src/entity.rs` for `Model` struct and `VendorKey` struct shape. Confirm whether `Model` already exists (if not, the entity may live under a different name; check `gw-core::entity::*`).

2. Add fields:

   ```rust
   // On Model (or whichever struct represents a per-model entity)
   #[serde(default)]
   pub capabilities: Vec<gw_core::permissions::Capability>, // or wherever Capability lives
   ```

   ```rust
   // On VendorKey
   #[serde(default)]
   pub forced_capabilities: Vec<gw_core::permissions::Capability>,
   #[serde(default)]
   pub exclusive_capability_match: bool,
   ```

   If `Capability` enum doesn't live in `gw-core` yet (it might be in `gw-capability::types`), gate the field type so `gw-core` doesn't pick up a back-edge. Two options:
     a) Keep the field as `Vec<String>` here; T3-C reads and parses.
     b) Move `Capability` enum to `gw-core::permissions` (the natural home).
   Prefer (b) for type safety; if it triggers crate-layering noise, fall back to (a) and document.

3. Migration `crates/gw-store/migrations/0004_capabilities.sql`:

   ```sql
   -- 0004_capabilities.sql — model + vendor_key capability metadata.
   -- Adds JSONB columns; existing rows default to empty / false.

   ALTER TABLE models
     ADD COLUMN IF NOT EXISTS capabilities JSONB NOT NULL DEFAULT '[]'::jsonb;

   ALTER TABLE vendor_keys
     ADD COLUMN IF NOT EXISTS forced_capabilities JSONB NOT NULL DEFAULT '[]'::jsonb,
     ADD COLUMN IF NOT EXISTS exclusive_capability_match BOOLEAN NOT NULL DEFAULT FALSE;
   ```

   Check the actual table names in `0001_initial.sql` — adjust if `models` is named `model_groups` or similar (per the audit there's a `models` table for individual model entities; if not, the field may sit on `model_groups` instead). Pick the entity that maps 1:1 with `Model` in entity.rs.

4. **Test code skeleton** — a unit test that confirms the serde shape:

   File: `crates/gw-core/src/entity.rs` (inline `#[cfg(test)] mod tests {}` if pattern allows) or `crates/gw-core/tests/capabilities.rs` (new):

   ```rust
   #[test]
   fn model_serde_capabilities_default() {
       let json = r#"{ "id": "m1", "name": "gpt-4", "version": 1 }"#;
       let parsed: gw_core::entity::Model = serde_json::from_str(json).unwrap();
       assert!(parsed.capabilities.is_empty());
   }

   #[test]
   fn vendor_key_serde_capability_fields_default() {
       let json = r#"{ "id": "k1", "vendor_id": "v1", "version": 1 }"#;
       let parsed: gw_core::entity::VendorKey = serde_json::from_str(json).unwrap();
       assert!(parsed.forced_capabilities.is_empty());
       assert!(!parsed.exclusive_capability_match);
   }
   ```

   Adjust the minimal JSON to whatever the existing entity required-field set demands.

5. **Verify**: `cargo test -p gw-core --test capabilities` (or inline tests), build.

6. **Commit**:
   ```
   git add crates/gw-store/migrations/0004_capabilities.sql \
           crates/gw-core/src/entity.rs \
           crates/gw-core/tests/capabilities.rs

   git commit -m "wave0(schema): add capabilities fields to Model and VendorKey + 0004 migration

   Declares the schema additions needed by T3-C's CapabilityResolver:
   - Model.capabilities: Vec<Capability> — what the model supports
   - VendorKey.forced_capabilities: Vec<Capability> — what the key advertises
   - VendorKey.exclusive_capability_match: bool — strict-match flag

   Migration 0004 adds JSONB columns with safe defaults so existing rows are
   unaffected. The loader and resolver wiring lands in T3-C.

   Spec ADR refs: G13."
   ```

---

# Wave 1 — Auth & permission hardening (serial — Plan A)

> **Why serial**: T1-A, T1-B, T1-D, T1-E all edit `crates/gw-proxy/src/filters/request.rs`. Four subagents writing to the same file in parallel would race-conflict on every commit. The controller serialises Wave 1 in the order **T1-A → T1-B → T1-D → T1-E → T1-C**. Each subagent rebases on the previous commit before starting. T1-C touches `filters/upstream.rs` and could technically run in parallel with one of the above, but trading parallel-1-extra for predictability is the explicit user decision (`牺牲开发速度换稳定`).
>
> **Dispatch protocol per Wave-1 task**: implementer → spec compliance reviewer → code quality reviewer. Only when both reviewers pass does the next task dispatch.

## T1-A — Enforce `ApiKeyEntry.expires_at`

**Files touched**:
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/tests/expires_at.rs` (new)

**Steps**:

1. **Test first** — `crates/gw-proxy/tests/expires_at.rs`:

   ```rust
   //! G1 — ApiKeyEntry.expires_at enforcement.
   use gw_proxy::filters::request::check_api_key_expiry;
   use gw_core::entity::ApiKeyEntry;
   use time::OffsetDateTime;

   fn entry_with(exp: Option<OffsetDateTime>) -> ApiKeyEntry {
       let mut e = ApiKeyEntry::default(); // or test-builder, mirror existing pattern
       e.expires_at = exp;
       e
   }

   #[test]
   fn none_means_unlimited() {
       assert!(check_api_key_expiry(&entry_with(None), OffsetDateTime::now_utc()).is_ok());
   }

   #[test]
   fn future_passes() {
       let now = OffsetDateTime::now_utc();
       let exp = now + time::Duration::hours(1);
       assert!(check_api_key_expiry(&entry_with(Some(exp)), now).is_ok());
   }

   #[test]
   fn past_rejects() {
       let now = OffsetDateTime::now_utc();
       let exp = now - time::Duration::seconds(1);
       assert!(check_api_key_expiry(&entry_with(Some(exp)), now).is_err());
   }

   #[test]
   fn exact_moment_rejects() {
       let now = OffsetDateTime::now_utc();
       assert!(check_api_key_expiry(&entry_with(Some(now)), now).is_err());
   }
   ```

2. **Implement** — add `pub fn check_api_key_expiry(entry: &ApiKeyEntry, now: OffsetDateTime) -> Result<(), ()>` in `crates/gw-proxy/src/filters/request.rs` as a small helper. Call it after `store.apikey_or_load(&akid)` resolves the entry (around line 145–166 per audit). On `Err`, emit `ErrorIR { status: 401, code: GW_AUTH_KEY_EXPIRED, .. }` via `error_emit::send_error` and `short_circuit(401)`.

   Helper sketch:

   ```rust
   pub fn check_api_key_expiry(entry: &ApiKeyEntry, now: OffsetDateTime) -> Result<(), ()> {
       match entry.expires_at {
           None => Ok(()),
           Some(exp) if now < exp => Ok(()),
           _ => Err(()),
       }
   }
   ```

   Keep the helper pure (no async, no I/O); inject `now` so the test can pin it.

3. **Verify**: `cargo test -p gw-proxy --test expires_at`, clippy, fmt.

4. **Commit**:
   ```
   git add crates/gw-proxy/src/filters/request.rs crates/gw-proxy/tests/expires_at.rs
   git commit -m "feat(auth): enforce ApiKeyEntry.expires_at (G1)

   Adds check_api_key_expiry helper called right after apikey_or_load
   resolves the entry. Expired keys short-circuit with 401
   GW_AUTH_KEY_EXPIRED before any IR parse or permission merge happens.

   Closed interval: a request issued exactly at expires_at rejects.

   Spec ADR: G1-1. Tests cover None/future/past/exact-moment."
   ```

---

## T1-B — Enforce `ApiKeyEntry.ip_allowlist`

**Files touched**:
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-auth/src/ip_filter.rs`
- `crates/gw-auth/tests/ip_filter.rs` (extend if exists, else new)
- `crates/gw-proxy/tests/ip_allowlist.rs` (new)

**Steps**:

1. **Test first** — extend `gw-auth` CIDR matcher with multi-CIDR helper, add unit tests:

   `crates/gw-auth/tests/ip_filter.rs`:

   ```rust
   use gw_auth::ip_filter::match_any_cidr;
   use std::net::IpAddr;

   #[test]
   fn empty_allowlist_returns_false() {
       let ip: IpAddr = "1.2.3.4".parse().unwrap();
       assert!(!match_any_cidr(ip, &[]));
   }

   #[test]
   fn ipv4_prefix_match() {
       let ip: IpAddr = "10.0.0.5".parse().unwrap();
       assert!(match_any_cidr(ip, &["10.0.0.0/24".into()]));
   }

   #[test]
   fn ipv4_no_match() {
       let ip: IpAddr = "192.168.1.1".parse().unwrap();
       assert!(!match_any_cidr(ip, &["10.0.0.0/24".into()]));
   }

   #[test]
   fn ipv6_prefix_match() {
       let ip: IpAddr = "2001:db8::1".parse().unwrap();
       assert!(match_any_cidr(ip, &["2001:db8::/32".into()]));
   }

   #[test]
   fn malformed_cidr_excluded() {
       let ip: IpAddr = "1.2.3.4".parse().unwrap();
       assert!(!match_any_cidr(ip, &["not-a-cidr".into()]));
   }
   ```

2. **Implement** `match_any_cidr` in `crates/gw-auth/src/ip_filter.rs`. Reuse whatever CIDR parser the existing `ip_filter` module uses (likely `ipnetwork` or `cidr` crate; check `Cargo.toml`). Don't add a new dep.

3. **Call site in `request_filter`**: after `check_blocklist(ip, Some(&user_id), api_key_id, store)` (around line 244), if `entry.value.ip_allowlist.is_some() && !allowlist.is_empty()`, call `match_any_cidr`. On no match → emit `GW_AUTH_KEY_IP_NOT_ALLOWED` 403, short_circuit.

4. **Integration test** — `crates/gw-proxy/tests/ip_allowlist.rs` exercises the filter path. If the test harness can't reach the full filter, write a pure helper `pub fn check_api_key_ip_allowlist(entry: &ApiKeyEntry, client_ip: IpAddr) -> Result<(), ()>` and test that.

5. **Verify** + **Commit**:
   ```
   git add crates/gw-auth/src/ip_filter.rs crates/gw-auth/tests/ip_filter.rs \
           crates/gw-proxy/src/filters/request.rs crates/gw-proxy/tests/ip_allowlist.rs
   git commit -m "feat(auth): enforce ApiKeyEntry.ip_allowlist per-key (G2)

   Adds match_any_cidr helper in gw-auth::ip_filter (reuses existing CIDR
   parser dep) plus a check_api_key_ip_allowlist gate in request_filter,
   called right after the User/ApiKey-scoped blocklist re-check.

   Empty/None allowlist remains unrestricted. Non-empty allowlist with no
   matching CIDR rejects with 403 GW_AUTH_KEY_IP_NOT_ALLOWED. Malformed
   CIDR entries fail closed (treated as no match) and produce a warn log.

   Spec ADR: G2-1. IPv4/IPv6 prefix tests in tests/ip_filter.rs."
   ```

---

## T1-C — `Vendor.timeout_overrides` precedence

**Files touched**:
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-proxy/tests/timeouts.rs` (new)

**Steps**:

1. **Test first** — `crates/gw-proxy/tests/timeouts.rs`:

   ```rust
   use gw_proxy::filters::upstream::apply_timeout_precedence;
   use gw_core::entity::{TimeoutSpec, Pricing, Vendor};

   fn ts(connect: Option<u64>, read: Option<u64>) -> TimeoutSpec {
       TimeoutSpec {
           connect_ms: connect, read_ms: read,
           total_ms: None, stream_total_ms: None,
       }
   }

   #[test]
   fn pricing_wins_over_vendor() {
       let v = Vendor { timeout_overrides: Some(ts(Some(1000), None)), ..Default::default() };
       let p = Pricing { timeouts: Some(ts(Some(500), None)), ..Default::default() };
       let eff = apply_timeout_precedence(&v, &p);
       assert_eq!(eff.connect_ms, Some(500));
   }

   #[test]
   fn vendor_fills_when_pricing_absent() {
       let v = Vendor { timeout_overrides: Some(ts(None, Some(30000))), ..Default::default() };
       let p = Pricing { timeouts: None, ..Default::default() };
       let eff = apply_timeout_precedence(&v, &p);
       assert_eq!(eff.read_ms, Some(30000));
   }

   #[test]
   fn default_when_both_absent() {
       let v = Vendor { timeout_overrides: None, ..Default::default() };
       let p = Pricing { timeouts: None, ..Default::default() };
       let eff = apply_timeout_precedence(&v, &p);
       assert!(eff.total_ms.is_none());
   }
   ```

2. **Implement** `pub fn apply_timeout_precedence(vendor: &Vendor, pricing: &Pricing) -> TimeoutSpec` as a pure helper. Logic: start from `TimeoutSpec::default()`, overlay Vendor fields where Some, overlay Pricing fields where Some.

3. **Wire into `pick_peer`** around line 240–259: replace the current "apply Pricing timeouts" block with `apply_timeout_precedence(&vendor.value, &pricing.value)` and apply the result.

4. **Verify** + **Commit**:
   ```
   git add crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/tests/timeouts.rs
   git commit -m "feat(routing): apply Vendor.timeout_overrides between default and Pricing (G4)

   Introduces apply_timeout_precedence(vendor, pricing) helper enforcing the
   ladder: hardcoded default < Vendor.timeout_overrides < Pricing.timeouts.
   Pricing wins because it's the most specific (per-model); Vendor wins over
   defaults to let operators tune at the vendor level.

   Wired into pick_peer after vendor_or_load resolves the vendor and Pricing
   is already loaded.

   Spec ADR: G4-1. Tests cover all four field permutations."
   ```

---

## T1-D — Wire `verify_body` at request_filter EOS

**Files touched**:
- `crates/gw-proxy/src/body_verify.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/tests/hmac_body_verify.rs` (new)

**Steps**:

1. **Read** `crates/gw-proxy/src/body_verify.rs` to understand the current `verify_body` shape and TODO.

2. **Test first** — `crates/gw-proxy/tests/hmac_body_verify.rs`:

   ```rust
   use gw_proxy::body_verify::{verify_body, SignatureCheckResult};
   use gw_core::entity::ApiKeyEntry;

   // Helper: build an ApiKeyEntry that requires signature with a known secret.
   fn entry_signed(secret: &str) -> ApiKeyEntry {
       let mut e = ApiKeyEntry::default();
       e.signature_required = true;
       e.signature_algo = Some("HMAC-SHA256".into());
       e.signature_secret_hash = /* however the secret is stored — see existing code */;
       e
   }

   #[test]
   fn not_required_is_ok() {
       let e = ApiKeyEntry::default(); // signature_required = false
       let r = verify_body(&e, &[], None, None);
       assert!(matches!(r, SignatureCheckResult::NotRequired));
   }

   #[test]
   fn required_but_missing_headers() {
       let e = entry_signed("s3cret");
       let r = verify_body(&e, b"body", None, None);
       assert!(matches!(r, SignatureCheckResult::Missing));
   }

   #[test]
   fn required_with_drifted_timestamp() {
       let e = entry_signed("s3cret");
       let stale = (time::OffsetDateTime::now_utc() - time::Duration::seconds(400)).unix_timestamp();
       let sig = compute_known_good_sig("s3cret", b"body", stale); // helper
       let r = verify_body(&e, b"body", Some(&sig), Some(stale));
       assert!(matches!(r, SignatureCheckResult::TimestampDrift));
   }

   #[test]
   fn required_with_valid_sig_passes() {
       let e = entry_signed("s3cret");
       let now = time::OffsetDateTime::now_utc().unix_timestamp();
       let sig = compute_known_good_sig("s3cret", b"body", now);
       let r = verify_body(&e, b"body", Some(&sig), Some(now));
       assert!(matches!(r, SignatureCheckResult::Valid));
   }
   ```

   `compute_known_good_sig` is a test helper that uses `gw-auth::signature` directly to build the canonical signing string and HMAC. Place it in the test file.

3. **Adjust `verify_body`** so its return is a `SignatureCheckResult` enum (`NotRequired | Valid | Missing | Invalid | TimestampDrift`) callable from `request_filter`. Move the TODO comments away.

4. **Wire into `request_filter`**: after the 16 MiB body-read loop completes (around line 273-274), if `entry.value.signature_required`, call `verify_body(...)`. Map result to error codes per ADR-G7-1:
   - `NotRequired` / `Valid` → continue
   - `Missing` → `401 GW_AUTH_SIGNATURE_MISSING`
   - `Invalid` → `401 GW_AUTH_SIGNATURE_INVALID`
   - `TimestampDrift` → `401 GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`

5. **Verify** + **Commit**:
   ```
   git add crates/gw-proxy/src/body_verify.rs \
           crates/gw-proxy/src/filters/request.rs \
           crates/gw-proxy/tests/hmac_body_verify.rs
   git commit -m "feat(auth): wire verify_body into request_filter at EOS (G7)

   Activates the HMAC body-verify path that has been scaffolded in body_verify.rs
   with a TODO. After the request body is fully buffered (existing 16 MiB read
   loop), if ApiKeyEntry.signature_required is true the body + signature header
   + timestamp are verified via gw-auth::signature::verify_required, with the
   existing ±300s replay window.

   Distinct rejection codes:
   - GW_AUTH_SIGNATURE_MISSING  (header absent when required)
   - GW_AUTH_SIGNATURE_INVALID  (HMAC mismatch)
   - GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT  (outside ±300s)

   Spec ADR: G7-1. G7-2 (workspace-global force HMAC) intentionally out of scope."
   ```

---

## T1-E — Hybrid JwtValidator (JWKS + iss/aud/nbf) on client bearer path

**Spec ADR**: G6-1 (hybrid). Critical constraint: do **not** drop the v1 JWKS-rotation path; layer `iss`/`aud`/`nbf` on top of it.

**Files touched**:
- `crates/gw-auth/src/jwt.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-auth/tests/jwt_validator_hybrid.rs` (new)

**Steps**:

1. **Read** `crates/gw-auth/src/jwt.rs` (legacy `verify_jwt` at 26-38 uses JWKS via `kid`; existing `JwtValidator` at 62+ takes a static secret).

2. **Extend `JwtValidator`** to optionally hold a `JwksResolver`:

   ```rust
   #[async_trait::async_trait]
   pub trait JwksResolver: Send + Sync {
       async fn resolve(&self, kid: &str) -> Option<Vec<u8>>;
   }

   pub struct JwtValidatorConfig {
       pub static_secret_hs256: Option<Vec<u8>>,
       pub static_pem_rs256: Option<Vec<u8>>,
       pub expected_iss: Option<String>,
       pub expected_aud: Option<String>,
       pub leeway_secs: u64,
       pub require_nbf: bool,
   }

   pub struct JwtValidator {
       cfg: JwtValidatorConfig,
       jwks: Option<std::sync::Arc<dyn JwksResolver>>,
   }
   ```

   `validate(token).await` dispatch:
   1. Decode header → `alg`, optional `kid`.
   2. Key resolution:
      - `HS256` → `cfg.static_secret_hs256` (reject `InvalidSignature` if absent).
      - `RS256` + `kid present` + `self.jwks.is_some()` → `jwks.resolve(kid).await` (None → fall through).
      - `RS256` fall-through → `cfg.static_pem_rs256` (reject if absent).
   3. `jsonwebtoken::decode` with `Validation { iss, aud, leeway, validate_nbf, validate_exp }`.
   4. Map `jsonwebtoken::errors::ErrorKind`:
      - `InvalidIssuer` → `JwtError::InvalidIss`
      - `InvalidAudience` → `JwtError::InvalidAud`
      - `ImmatureSignature` → `JwtError::NotYetValid`
      - `ExpiredSignature` → `JwtError::Expired`
      - `*` → `JwtError::InvalidSignature`

3. **Provide `EntityStoreJwksResolver` adapter** in `crates/gw-auth/src/jwt.rs` (kept in `gw-auth` so the trait stays free of `gw-store` types):

   ```rust
   pub struct EntityStoreJwksResolver {
       pub store: std::sync::Arc<gw_store::entity_store::EntityStore>,
   }

   #[async_trait::async_trait]
   impl JwksResolver for EntityStoreJwksResolver {
       async fn resolve(&self, kid: &str) -> Option<Vec<u8>> {
           self.store
               .jwks_or_load(&kid.into()).await
               .ok()
               .map(|j| j.value.pem.clone().into_bytes())
       }
   }
   ```

   Mirror the exact JWKS lookup shape of legacy `verify_jwt` so the behaviour is bit-equivalent.

4. **Bootstrap wiring** in `crates/gw-gateway/src/lib.rs::bootstrap`:

   ```rust
   let auth_cfg = store.auth_config_or_load("global").await.ok();
   let validator = auth_cfg.map(|cfg| {
       let mut vcfg = JwtValidatorConfig::default();
       vcfg.expected_iss = cfg.value.expected_iss.clone();
       vcfg.expected_aud = cfg.value.expected_aud.clone();
       vcfg.leeway_secs = cfg.value.leeway_secs;
       vcfg.require_nbf = cfg.value.require_nbf;
       // HS256 / static RS256 PEM populated from AuthConfig if present.
       let mut v = JwtValidator::new(vcfg);
       v.attach_jwks(std::sync::Arc::new(EntityStoreJwksResolver { store: store.clone() }));
       std::sync::Arc::new(v)
   });
   ```

   `validator` is `Option<Arc<JwtValidator>>` and lands on `Wired.jwt_validator` (already declared in T0-A).

5. **Test first** — `crates/gw-auth/tests/jwt_validator_hybrid.rs`:

   ```rust
   use gw_auth::jwt::{JwtValidator, JwtValidatorConfig, JwksResolver, JwtError};

   struct StaticJwks { kid: String, pem: Vec<u8> }
   #[async_trait::async_trait]
   impl JwksResolver for StaticJwks {
       async fn resolve(&self, kid: &str) -> Option<Vec<u8>> {
           if kid == self.kid { Some(self.pem.clone()) } else { None }
       }
   }

   #[tokio::test]
   async fn hs256_static_secret_happy() {
       let mut cfg = JwtValidatorConfig::default();
       cfg.static_secret_hs256 = Some(b"top-secret".to_vec());
       cfg.expected_iss = Some("my-iss".into());
       cfg.expected_aud = Some("my-aud".into());
       cfg.leeway_secs = 5;
       cfg.require_nbf = true;
       let v = JwtValidator::new(cfg);
       let tok = make_hs256_token("top-secret", &claims(now_unix(), Some("my-iss"), Some("my-aud")));
       assert!(v.validate(&tok).await.is_ok());
   }

   #[tokio::test]
   async fn rs256_jwks_kid_lookup_happy() {
       let (pem, signing_key) = generate_rs256_keypair();
       let resolver = StaticJwks { kid: "k1".into(), pem };
       let mut v = JwtValidator::new(JwtValidatorConfig {
           expected_iss: Some("my-iss".into()),
           expected_aud: Some("my-aud".into()),
           leeway_secs: 5, require_nbf: true,
           ..JwtValidatorConfig::default()
       });
       v.attach_jwks(std::sync::Arc::new(resolver));
       let tok = make_rs256_token(&signing_key, "k1", &claims(now_unix(), Some("my-iss"), Some("my-aud")));
       assert!(v.validate(&tok).await.is_ok());
   }

   #[tokio::test]
   async fn iss_mismatch_rejects() {
       let mut cfg = JwtValidatorConfig::default();
       cfg.static_secret_hs256 = Some(b"top-secret".to_vec());
       cfg.expected_iss = Some("my-iss".into());
       cfg.leeway_secs = 5;
       let v = JwtValidator::new(cfg);
       let tok = make_hs256_token("top-secret", &claims(now_unix(), Some("WRONG"), None));
       assert!(matches!(v.validate(&tok).await, Err(JwtError::InvalidIss)));
   }

   #[tokio::test]
   async fn aud_mismatch_rejects() { /* mirror iss test */ }

   #[tokio::test]
   async fn nbf_future_rejects() { /* … */ }

   #[tokio::test]
   async fn nbf_within_leeway_passes() { /* … */ }

   #[tokio::test]
   async fn unknown_kid_falls_back_to_static_pem() {
       // resolver returns None for the token's kid -> static_pem_rs256 used.
       let (pem, signing_key) = generate_rs256_keypair();
       let resolver = StaticJwks { kid: "different".into(), pem: vec![] };
       let mut cfg = JwtValidatorConfig::default();
       cfg.static_pem_rs256 = Some(pem);
       cfg.leeway_secs = 5;
       let mut v = JwtValidator::new(cfg);
       v.attach_jwks(std::sync::Arc::new(resolver));
       let tok = make_rs256_token(&signing_key, "unknown", &claims(now_unix(), None, None));
       assert!(v.validate(&tok).await.is_ok());
   }
   ```

   Test helpers (`make_hs256_token`, `make_rs256_token`, `generate_rs256_keypair`, `now_unix`, `claims`) live in the test file.

6. **Wire into request_filter** at `crates/gw-proxy/src/filters/request.rs` around line 133:

   ```rust
   let sub = match &self.jwt_validator {
       Some(v) => match v.validate(bearer).await {
           Ok(sub) => sub,
           Err(e) => { emit_jwt_error(&mut session, &e).await; return Ok(true); }
       },
       None => verify_jwt(bearer, store).await
           .map_err(|_| { /* short_circuit 401 GW_AUTH_INVALID_BEARER */ })?,
   };
   ```

   `emit_jwt_error` maps `JwtError::{InvalidIss, InvalidAud, NotYetValid, Expired, InvalidSignature}` to the matching `GW_AUTH_JWT_*` code (declared in T0-A) and short-circuits 401.

7. **Acceptance gating**:
   - With `AuthConfig.global` present + JWKS-rotating RS256 token → `kid` resolved, `iss`/`aud`/`nbf` enforced.
   - With `AuthConfig.global` absent → legacy `verify_jwt` runs; no regression vs v1.
   - HS256 with no resolver → static secret path, works.
   - All `iss`/`aud`/`nbf`/`exp` violations land the distinct `GW_AUTH_JWT_*` code.

8. **Verify** + **Commit**:
   ```
   git add crates/gw-auth/src/jwt.rs \
           crates/gw-auth/tests/jwt_validator_hybrid.rs \
           crates/gw-proxy/src/filters/request.rs \
           crates/gw-proxy/src/gateway.rs \
           crates/gw-gateway/src/lib.rs
   git commit -m "feat(auth): hybrid JwtValidator with JWKS + iss/aud/nbf on client bearer (G6)

   JwtValidator gains an optional Arc<dyn JwksResolver>; v1's RS256+kid
   JWKS rotation path is preserved via EntityStoreJwksResolver (wraps
   EntityStore::jwks_or_load verbatim). iss/aud/nbf/leeway enforcement
   layered on top.

   Bootstrap loads AuthConfig.global (best-effort). Absent -> validator is
   None and request_filter falls back to legacy verify_jwt — no regression
   for deployments that have not yet deployed the entity. Present ->
   validator is wired with the JWKS resolver and AuthConfig-driven
   iss/aud/nbf/leeway settings.

   Distinct 401 codes:
   - GW_AUTH_JWT_INVALID_ISS
   - GW_AUTH_JWT_INVALID_AUD
   - GW_AUTH_JWT_NOT_YET_VALID
   (GW_AUTH_JWT_EXPIRED existed already.)

   Spec ADR: G6-1 (hybrid). Management-token path unchanged."
   ```

---

# Wave 2 — Quota, markup, strategy state, DLQ (4 parallel)

## T2-A — Daily quota: store helper + dual enforcement

**Files touched**:
- `crates/gw-store/src/counter_store.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/src/filters/logging.rs`
- `crates/gw-store/tests/daily_tokens.rs` (new)
- `crates/gw-proxy/tests/daily_quota.rs` (new)

**Steps**:

1. **Test first — store helper** (`crates/gw-store/tests/daily_tokens.rs`):

   ```rust
   use gw_store::counter_store::daily_tokens_key;
   use gw_core::ids::UserId;

   #[test]
   fn key_shape() {
       let k = daily_tokens_key(&UserId("u1".into()), "2026-05-21");
       assert_eq!(k, "counter:user:u1:daily_tokens:2026-05-21");
   }
   ```

2. **Implement** `pub fn daily_tokens_key(uid: &UserId, day_utc: &str) -> String` in `counter_store.rs`. No clock access here.

3. **Test first — quota enforcement helper** (`crates/gw-proxy/tests/daily_quota.rs`):

   ```rust
   use gw_proxy::filters::request::check_daily_quota;

   #[test]
   fn zero_means_unlimited() {
       assert!(check_daily_quota(0, 0, 1_000_000).is_ok());
   }

   #[test]
   fn under_quota_passes() {
       assert!(check_daily_quota(50_000, 500, 100_000).is_ok());
   }

   #[test]
   fn over_quota_rejects() {
       assert!(check_daily_quota(99_500, 1_000, 100_000).is_err());
   }
   ```

4. **Implement** in `request.rs` a pure helper:

   ```rust
   pub fn check_daily_quota(consumed_today: u64, freeze_estimate: u64, quota: u64) -> Result<(), ()> {
       if quota == 0 { return Ok(()); }
       if consumed_today.saturating_add(freeze_estimate) > quota { return Err(()); }
       Ok(())
   }
   ```

   Call site: in `request_filter` after balance check, before USER_CIRCUIT (around line 414). Compute `day_utc = OffsetDateTime::now_utc().date()` formatted as `YYYY-MM-DD`. Read `GET counter:user:{eid}:daily_tokens:{day}` via `CounterStore::get_str_key` (add a helper if needed). On Redis error fail open. On over-quota emit `403 GW_QUOTA_DAILY_TOKENS_EXCEEDED`.

5. **Post-increment** in `logging.rs`: at the same point `UsageRecord` is built and total tokens computed, also `INCRBY counter:user:{eid}:daily_tokens:{day} (input+output)` with `EXPIRE 172800` (48 h). Use `tokio::spawn` or the existing `try_send` mpsc into the ledger flusher — pick the lighter; daily quota is best-effort. Add metric `gw_daily_quota_increment_error_total` on Redis failure.

6. **Verify** + **Commit**:
   ```
   git add crates/gw-store/src/counter_store.rs \
           crates/gw-store/tests/daily_tokens.rs \
           crates/gw-proxy/src/filters/request.rs \
           crates/gw-proxy/src/filters/logging.rs \
           crates/gw-proxy/tests/daily_quota.rs
   git commit -m "feat(quota): enforce UserEntry.daily_quota_tokens with UTC day window (G3)

   Pre-check in request_filter: GET counter:user:{eid}:daily_tokens:{YYYY-MM-DD},
   reject 403 GW_QUOTA_DAILY_TOKENS_EXCEEDED when consumed + est_input would
   exceed quota. Redis failure fails open (consistent with balance check).

   Post-increment in logging: INCRBY actual usage (input+output) with EXPIRE 48h.
   Best-effort; Redis failure increments gw_daily_quota_increment_error_total.

   quota = 0 / None means unlimited. UTC day boundary; no per-timezone v1.

   Spec ADRs: G3-1, G3-2."
   ```

---

## T2-B — `Pricing.model_markup` → `MarkupChainSnapshot`

**Files touched**:
- `crates/gw-auth/src/merge.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-auth/tests/merge_model_markup.rs` (new)

**Steps**:

1. **Test first** — `crates/gw-auth/tests/merge_model_markup.rs`:

   ```rust
   use gw_auth::merge::{merge_permissions, AuthCtx};
   use gw_core::entity::Pricing;
   use std::sync::Arc;

   #[test]
   fn pricing_model_markup_lands_in_chain() {
       let pricing = Arc::new(versioned(Pricing { model_markup: 1.25, ..Default::default() }));
       let auth = make_auth_ctx_minimal(); // mirror existing test helpers
       let store = mock_store_with_user_etc();
       let snap = merge_permissions(&auth, &store, 100_000, Some(pricing)).unwrap();
       assert_eq!(snap.markup_chain.iter().find(|m| m.origin == "model").unwrap().value, 1.25);
   }

   #[test]
   fn no_pricing_defaults_to_one() {
       let auth = make_auth_ctx_minimal();
       let store = mock_store_with_user_etc();
       let snap = merge_permissions(&auth, &store, 100_000, None).unwrap();
       assert_eq!(snap.markup_chain.iter().find(|m| m.origin == "model").map(|m| m.value), Some(1.0));
   }
   ```

2. **Implement**:
   - Extend `merge_permissions(...)` signature with `pricing: Option<Arc<Versioned<Pricing>>>`.
   - Replace the hardcoded `model_markup: 1.0` placeholder (current `crates/gw-auth/src/merge.rs:156`) with `pricing.as_ref().map(|p| p.value.model_markup).unwrap_or(1.0)`.
   - In `crates/gw-proxy/src/filters/request.rs` after IR parse, `let pricing = store.pricing_or_load(&model_name).await.ok()`, pass into `merge_permissions`.
   - Remove the now-redundant lazy load in `crates/gw-proxy/src/filters/upstream.rs:166` — replace with `let pricing = ctx.pricing.clone()` (stash on ctx in request_filter).

3. **Verify** + **Commit**:
   ```
   git add crates/gw-auth/src/merge.rs \
           crates/gw-auth/tests/merge_model_markup.rs \
           crates/gw-proxy/src/filters/request.rs \
           crates/gw-proxy/src/filters/upstream.rs
   git commit -m "feat(billing): capture Pricing.model_markup into MarkupChainSnapshot (G5)

   Pricing is now pre-loaded in request_filter (after IR parse exposes the
   model name) and passed into merge_permissions, which populates the
   model_markup chain entry from Pricing.model_markup instead of the 1.0
   placeholder.

   The lazy load in upstream_peer becomes a ctx pickup; EntityStore memoises
   so there is no extra Postgres round-trip in the happy path.

   Rust still does not compute cost: the chain is emitted verbatim into
   UsageRecord.markup_chain_snapshot. Go multiplies. no_cost_micro and
   no_markup_product_scalar invariants stay green.

   Spec ADR: G5-1."
   ```

---

## T2-C — Sticky moka + shared StrategySet

**Files touched**:
- `crates/gw-routing/src/strategy/sticky.rs`
- `crates/gw-routing/src/strategy/set.rs`
- `crates/gw-routing/Cargo.toml` (add `moka` if not already there)
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-routing/tests/sticky_ttl.rs` (new)

**Steps**:

1. **Add moka dependency** to `crates/gw-routing/Cargo.toml` if absent. Check workspace `Cargo.toml` for an existing pin to reuse.

2. **Test first** — `crates/gw-routing/tests/sticky_ttl.rs`:

   ```rust
   use gw_routing::strategy::sticky::Sticky;
   use gw_core::ids::{KeyId, UserId};

   #[test]
   fn returns_bound_key_on_second_pick() {
       let sticky = Sticky::new_with_ttl(std::time::Duration::from_secs(30 * 60), 100);
       let user = UserId("u1".into());
       let model = "gpt-4".to_string();
       let candidates = [arc_key("k1"), arc_key("k2"), arc_key("k3")];

       let first = sticky.pick(&candidates, &user, &model);
       let second = sticky.pick(&candidates, &user, &model);
       assert_eq!(first.unwrap().id, second.unwrap().id);
   }

   #[test]
   fn evicts_after_ttl() {
       let sticky = Sticky::new_with_ttl(std::time::Duration::from_millis(50), 100);
       let user = UserId("u1".into());
       let model = "gpt-4".to_string();
       let candidates = [arc_key("k1"), arc_key("k2")];

       let first = sticky.pick(&candidates, &user, &model);
       std::thread::sleep(std::time::Duration::from_millis(100));
       // After TTL the binding is gone; pick may roll a different key,
       // but the cache_size metric should drop to 0 after run_pending_tasks.
       sticky.invalidate_expired();
       assert_eq!(sticky.bindings_active(), 0);
   }

   #[test]
   fn shared_instance_across_threads() {
       let sticky = std::sync::Arc::new(Sticky::new_with_ttl(
           std::time::Duration::from_secs(30 * 60), 100,
       ));
       let s2 = sticky.clone();
       let candidates = std::sync::Arc::new([arc_key("k1"), arc_key("k2")]);
       let c2 = candidates.clone();

       let t = std::thread::spawn(move || {
           s2.pick(&c2[..], &UserId("u1".into()), &"m".into())
       });
       let here = sticky.pick(&candidates[..], &UserId("u1".into()), &"m".into());
       assert_eq!(here.unwrap().id, t.join().unwrap().unwrap().id);
   }

   fn arc_key(id: &str) -> std::sync::Arc<gw_core::entity::VendorKey> { /* … */ }
   ```

3. **Implement**:
   - Replace the `DashMap<String, (KeyId, Instant)>` field on `Sticky` with `moka::sync::Cache<String, KeyId>` configured with `time_to_live` and `max_capacity`.
   - Add `Sticky::new_with_ttl(ttl, capacity)` + `Sticky::default()` that uses 30 min / 100_000.
   - Add `pub fn invalidate_expired(&self)` calling `cache.run_pending_tasks()`.
   - Add `pub fn bindings_active(&self) -> u64` returning `cache.entry_count()`.
   - Keep `Strategy::pick` signature unchanged.

4. **StrategySet update**: `Sticky::default()` in `StrategySet::new_default()` now produces the moka-backed instance. T0-B already wired the container.

5. **LlmGateway / Wired**: T0-A added `strategies: Arc<StrategySet>`; here we ensure the bootstrap passes `Arc::new(StrategySet::new_default())` (replace any `placeholder()` calls).

6. **`strategy_for(kind)`** in `crates/gw-proxy/src/filters/upstream.rs` — replace the `Sticky::default()` construction with `self.strategies.pick_for(kind)`.

7. **Metrics**: add `gw_sticky_bind_total`, `gw_sticky_hit_total`, `gw_sticky_evict_total`, `gw_sticky_bindings_active` to `crates/gw-gateway/src/metrics_catalog.rs`. Increment from `Sticky::pick`.

8. **Verify** + **Commit**:
   ```
   git add crates/gw-routing/Cargo.toml \
           crates/gw-routing/src/strategy/sticky.rs \
           crates/gw-routing/src/strategy/set.rs \
           crates/gw-routing/tests/sticky_ttl.rs \
           crates/gw-proxy/src/filters/upstream.rs \
           crates/gw-proxy/src/gateway.rs \
           crates/gw-gateway/src/lib.rs \
           crates/gw-gateway/src/metrics_catalog.rs \
           Cargo.lock
   git commit -m "feat(routing): Sticky moka cache + shared StrategySet (G12)

   Two fixes:
   1. Sticky's assignment map was DashMap with no TTL sweep -> moka::sync::Cache
      with time_to_live=30min, max_capacity=100_000. LRU eviction on overflow.
   2. strategy_for() constructed Sticky::default() per request, so bindings
      never persisted. LlmGateway now holds Arc<StrategySet>; strategy_for
      dispatches into the shared instance.

   Adopts routing-r-g ADR-RG-1 verbatim. pick_ordered/SmallVec interface
   (ADR-RG-2) remains deferred to v2 routing-r-ab.

   Metrics: gw_sticky_bind_total, gw_sticky_hit_total, gw_sticky_evict_total,
   gw_sticky_bindings_active.

   Spec ADR: G12-1."
   ```

---

## T2-D — `usage:records:dlq` fallback

**Files touched**:
- `crates/gw-ledger/src/flusher.rs`
- `crates/gw-ledger/tests/dlq.rs` (new)
- `scripts/check-invariants.sh`
- `crates/gw-gateway/src/metrics_catalog.rs`

**Steps**:

1. **Test first** — `crates/gw-ledger/tests/dlq.rs` (Docker-gated `#[ignore]` — uses testcontainers Redis, like other ledger tests). Sketch:

   ```rust
   #[tokio::test]
   #[ignore]
   async fn dlq_receives_record_when_primary_fails() {
       let redis = start_testcontainer_redis().await;
       // Pre-create usage:records as a non-stream key to force XADD to fail.
       redis.set("usage:records", "not-a-stream").await.unwrap();
       let ledger = Ledger::start(redis.clone(), 16, 1, Duration::from_millis(50));
       let rec = sample_usage_record();
       ledger.try_send(rec).unwrap();
       sleep(Duration::from_millis(200)).await;
       let dlq_len = redis.xlen("usage:records:dlq").await.unwrap();
       assert_eq!(dlq_len, 1);
   }
   ```

2. **Implement** in `crates/gw-ledger/src/flusher.rs`:
   - Add constant `DLQ_STREAM: &str = "usage:records:dlq"` and `DLQ_MAXLEN: usize = 100_000`.
   - On per-record XADD failure inside the flush loop (around line 86–98), wrap the original payload with `dlq_meta` JSON and XADD to `DLQ_STREAM` with `MAXLEN ~`. Increment `gw_usage_dlq_enqueued_total{outcome}` (`success` or `error`).
   - On batch RTT (`pipe.all()`) failure, drain the remaining records into the DLQ in a single pipeline.
   - Keep existing `gw_usage_records_lost_total` semantics: now means **both** primary and DLQ failed.

3. **Invariant lint** in `scripts/check-invariants.sh`: extend the existing `no_billing_records_stream` check to ensure `usage:records:dlq` (and only that exact suffix) is allowed as a non-violation. Add a new lint `no_cost_in_dlq` that greps the flusher source for `cost_micro` (must be absent).

4. **Metric**: add `gw_usage_dlq_enqueued_total{outcome}` to `metrics_catalog.rs`.

5. **Verify** + **Commit**:
   ```
   git add crates/gw-ledger/src/flusher.rs \
           crates/gw-ledger/tests/dlq.rs \
           scripts/check-invariants.sh \
           crates/gw-gateway/src/metrics_catalog.rs
   git commit -m "feat(ledger): single-fallback usage:records:dlq stream on flush failure (G11)

   On primary XADD usage:records failure (per-record or batch-RTT), the
   record is wrapped with dlq_meta and XADDed to usage:records:dlq
   (MAXLEN ~ 100_000). gw_usage_dlq_enqueued_total{outcome} tracks success/
   error. If DLQ itself fails the record is dropped and counts toward
   gw_usage_records_lost_total.

   v1.5 minimum per spec. Three-layer fallback (in-memory + on-disk file +
   periodic drainer + startup recovery) remains deferred to billing-c-delta.

   Invariants: no_billing_records_stream allows usage:records:dlq; new
   no_cost_in_dlq forbids cost_micro fields in flusher source.

   Spec ADR: G11-1."
   ```

---

# Wave 3 — Codec, retry, capability (3 parallel)

## T3-A — `parse_stream_chunk` → `SmallVec`

**Files touched**:
- `crates/gw-codec/src/trait_def.rs`
- `crates/gw-codec/src/openai/codec.rs`
- `crates/gw-codec/src/anthropic/codec.rs`
- `crates/gw-codec/src/gemini/codec.rs`
- `crates/gw-codec/src/passthrough.rs` (if it impls the trait)
- `crates/gw-codec/src/error_passthrough.rs` (if it impls the trait)
- `crates/gw-proxy/src/filters/response.rs`
- `crates/gw-codec/Cargo.toml` (add `smallvec` if needed)
- `crates/gw-codec/tests/multi_event_chunk.rs` (new)

**Steps**:

1. **Add `smallvec` dep** if not already present.

2. **Test first** — `crates/gw-codec/tests/multi_event_chunk.rs`:

   ```rust
   use gw_codec::{gemini::codec::GeminiCodec, ProtocolCodec};

   #[test]
   fn gemini_combined_final_event_splits_into_two() {
       let codec = GeminiCodec;
       // A Gemini SSE event payload that carries text + finishReason in one frame.
       let event = br#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#;
       let events = codec.parse_stream_chunk(event);
       assert_eq!(events.len(), 2);
       assert!(events.iter().any(|e| matches!(e, gw_core::ir::StreamEventIR::Delta { .. })));
       assert!(events.iter().any(|e| matches!(e, gw_core::ir::StreamEventIR::Finish { .. })));
   }

   #[test]
   fn openai_single_delta_does_not_spill() {
       use gw_codec::openai::codec::OpenAiCodec;
       let codec = OpenAiCodec;
       let event = br#"data: {"choices":[{"delta":{"content":"hi"}}]}"#;
       let events = codec.parse_stream_chunk(event);
       assert!(!events.spilled(), "single-event chunk must stay inline in SmallVec");
       assert_eq!(events.len(), 1);
   }
   ```

3. **Change trait** in `crates/gw-codec/src/trait_def.rs`:

   ```rust
   fn parse_stream_chunk(&self, chunk: &[u8])
       -> smallvec::SmallVec<[gw_core::ir::StreamEventIR; 1]>;
   ```

   Remove the `Option<>` wrapper. Existing impls return `SmallVec::new()` for empty (used to be `None`), `smallvec![one]` for the single case.

4. **Update codec impls**:
   - OpenAI / Anthropic: wrap existing single-result logic in `smallvec![]` or return `SmallVec::new()`.
   - Gemini: when the payload contains both `text` and `finishReason` on the same candidate, emit both `Delta` and `Finish` events.
   - Passthrough / error_passthrough: trivial port.

5. **Update consumer** in `crates/gw-proxy/src/filters/response.rs` (around `handle_event`, line 93–116). Rename or restructure so the framing loop iterates `for ev in events { … }` and updates ctx per event. The loop already drains chunks at `\n\n` boundaries; the inner shape now handles multi-event-per-frame.

6. **Verify** + **Commit**:
   ```
   git add crates/gw-codec/Cargo.toml \
           crates/gw-codec/src/trait_def.rs \
           crates/gw-codec/src/openai/codec.rs \
           crates/gw-codec/src/anthropic/codec.rs \
           crates/gw-codec/src/gemini/codec.rs \
           crates/gw-codec/src/passthrough.rs \
           crates/gw-codec/src/error_passthrough.rs \
           crates/gw-codec/tests/multi_event_chunk.rs \
           crates/gw-proxy/src/filters/response.rs \
           Cargo.lock
   git commit -m "feat(codec): parse_stream_chunk returns SmallVec<[StreamEventIR; 1]> (G9)

   Trait signature change: parse_stream_chunk(&self, chunk) -> SmallVec
   replaces the v1 Option<StreamEventIR>. Common case (one event per chunk)
   stays heap-free. Gemini's 'combined final event' (text + finishReason in
   one frame) now decomposes into [Delta, Finish] cleanly.

   Breaking change with no compat shim per user policy 'breaking changes OK'.

   Stateful cross-chunk accumulators (thinking signature continuation,
   tool_call delta coalescing) remain deferred to codec-s3 v2 ADR-S3-v2-1.

   Spec ADR: G9-1."
   ```

---

## T3-B — `fail_to_connect` retry hook

**Files touched**:
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-routing/src/candidates.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-gateway/src/metrics_catalog.rs`
- `crates/gw-routing/tests/candidates_blacklist.rs` (new)
- `crates/gw-proxy/tests/connect_retry.rs` (new, `#[ignore]` if it needs network)

**Steps**:

1. **Test first — candidate blacklist** (`crates/gw-routing/tests/candidates_blacklist.rs`):

   ```rust
   use gw_routing::candidates::get_candidate_keys;
   use gw_core::ids::KeyId;

   #[test]
   fn blacklisted_key_filtered() {
       let group = make_group_with_keys(&["k1", "k2", "k3"]);
       let merged = make_merged_perm_allows_all();
       let blacklist = vec![KeyId("k2".into())];

       let cands = get_candidate_keys(&group, &merged, &health, &in_flight, &blacklist);
       assert!(cands.iter().all(|k| k.id != KeyId("k2".into())));
       assert_eq!(cands.len(), 2);
   }
   ```

2. **Implement candidate filter**: extend `get_candidate_keys` signature with `retry_blacklist: &[KeyId]`. Early-skip clause filters keys whose id is in the list.

3. **Pingora `fail_to_connect`**: override in `LlmGateway`:

   ```rust
   async fn fail_to_connect(&self, _session: &mut Session, _peer: &HttpPeer,
                             ctx: &mut ReqCtx, e: Box<Error>) -> Box<Error> {
       let kind = classify_connect_error(&e);
       if matches!(kind, ConnectErrorKind::TlsError) {
           return short_circuit_with(401? /* see below */, GW_UPSTREAM_TLS_ERROR);
       }
       if ctx.retry_count < MAX_CONNECT_RETRIES {
           if let Some(k) = ctx.picked_vendor_key.as_ref().map(|p| p.id.clone()) {
               ctx.retry_blacklist.push(k);
           }
           ctx.retry_count += 1;
           // Pingora-specific: return e with `retry: true` flag; check actual API.
           // For Pingora 0.8 ProxyHttp, override:
           //   fn fail_to_connect(&self, ..., e: Box<Error>) -> Box<Error>
           // Set e.retry = true to instruct retry.
           let mut e = e; e.set_retry(true); return e;
       }
       short_circuit_with(503, GW_UPSTREAM_CONNECT_FAILED)
   }
   ```

   **Read Pingora 0.8 docs** (or `cargo doc -p pingora-proxy`) before settling on the exact signature. Pingora exposes either `fail_to_connect` with `Result<HttpPeer, _>` semantics or via setting an error retry flag. The above is illustrative.

4. **Pass `&ctx.retry_blacklist`** into `get_candidate_keys` from `pick_peer` (around line 173-182).

5. **Metrics**: `gw_retry_attempt_total{outcome}` (outcome = `recovered` / `exhausted` / `tls_blocked`).

6. **Verify** + **Commit**:
   ```
   git add crates/gw-routing/src/candidates.rs \
           crates/gw-routing/tests/candidates_blacklist.rs \
           crates/gw-proxy/src/gateway.rs \
           crates/gw-proxy/src/filters/upstream.rs \
           crates/gw-gateway/src/metrics_catalog.rs \
           crates/gw-proxy/tests/connect_retry.rs
   git commit -m "feat(routing): fail_to_connect retry hook with cross-key blacklist (G10)

   Implements ProxyHttp::fail_to_connect: on TCP/handshake failure other
   than TLS, the failing KeyId is pushed onto ctx.retry_blacklist and
   ctx.retry_count is incremented. While retry_count < MAX_CONNECT_RETRIES
   (default 2) Pingora is instructed to retry; the next upstream_peer call
   filters the blacklist via get_candidate_keys.

   TLS handshake errors short-circuit immediately with 503 GW_UPSTREAM_TLS_ERROR
   (config issue, not transient). Other connect errors exhausted -> 503
   GW_UPSTREAM_CONNECT_FAILED.

   v1.5 minimum: connect-only retry. Response-side (5xx/429) retry remains
   deferred to routing-r-e FailoverExecutor v2.

   Spec ADR: G10-1. Metric: gw_retry_attempt_total{outcome}."
   ```

---

## T3-C — CapabilityResolver injection + EntityStoreResolveBackend

**Files touched**:
- `crates/gw-capability/src/resolver.rs`
- `crates/gw-capability/src/backend.rs` (new)
- `crates/gw-capability/src/lib.rs`
- `crates/gw-store/src/loader.rs` (load capabilities fields)
- `crates/gw-store/src/entity_store.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-routing/src/candidates.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-gateway/src/metrics_catalog.rs`
- `crates/gw-proxy/tests/capability_filter.rs` (new)

**Steps**:

1. **Test first** — `crates/gw-proxy/tests/capability_filter.rs`:

   ```rust
   use gw_capability::{CapabilityResolver, Capability, RequiredCapabilities};
   use gw_proxy::filters::request::resolve_capability;

   #[tokio::test]
   async fn vision_required_matches_vision_keys_only() {
       let backend = FakeBackend::new()
           .with_model("gpt-4-vision", vec![Capability::Vision])
           .with_key("k1", vec![Capability::Vision], false)
           .with_key("k2", vec![], false);
       let resolver = CapabilityResolver::new(std::sync::Arc::new(backend));
       let req = ResolveRequest {
           api_key_id: "any", model_alias: "gpt-4-vision",
           header_require: None, protocol: ProtocolKind::OpenAi,
           retry_upgrade: None,
       };
       let caps = resolver.resolve(&req).await.unwrap();
       assert!(caps.set.contains(&Capability::Vision));
   }
   ```

2. **Implement `EntityStoreResolveBackend`** in `crates/gw-capability/src/backend.rs`:

   ```rust
   pub struct EntityStoreResolveBackend {
       store: std::sync::Arc<gw_store::entity_store::EntityStore>,
   }

   #[async_trait::async_trait]
   impl ResolveBackend for EntityStoreResolveBackend {
       async fn get_model_caps(&self, model: &str) -> Vec<Capability> {
           self.store.model_or_load(&model.into()).await
               .ok().map(|m| m.value.capabilities.clone()).unwrap_or_default()
       }
       async fn get_key_caps(&self, key_id: &KeyId) -> Vec<Capability> {
           self.store.vendor_key_or_load(key_id).await
               .ok().map(|k| k.value.forced_capabilities.clone()).unwrap_or_default()
       }
       async fn is_exclusive_matcher(&self, key_id: &KeyId) -> bool {
           self.store.vendor_key_or_load(key_id).await
               .ok().map(|k| k.value.exclusive_capability_match).unwrap_or(false)
       }
   }
   ```

3. **Loader** (`crates/gw-store/src/loader.rs`): extend the Postgres queries to select the new columns. If using `sqlx::raw_sql` (per the no-offline-cache policy), update the column list and row deserialisation.

4. **Inject into `LlmGateway`** — already scaffolded at T0-A as `Option<Arc<CapabilityResolver>>`. Construct in bootstrap, replace `None` with `Some(Arc::new(CapabilityResolver::new(Arc::new(EntityStoreResolveBackend::new(store.clone())))))`.

5. **Call in `request_filter`** at the T6-A TODO site (around line 96-104): build `ResolveRequest`, call `resolver.resolve(req).await`, stash on `ctx.required_caps`.

6. **Candidate filter** (`crates/gw-routing/src/candidates.rs`): extend signature with `Option<&RequiredCapabilities>`. Iterate candidates; for each, query `(forced_capabilities, exclusive_capability_match)` from the candidate (or via a `CapabilityFilter` helper) and reject if mismatch. **If `gw-routing` cannot depend on `gw-capability`** (layering rule check), pass the `CapabilityFilter::evaluate` function via callback or move `Capability` into `gw-core::permissions` (preferred — already noted in T0-C).

7. **Upstream filter call**: `crates/gw-proxy/src/filters/upstream.rs` passes `ctx.required_caps.as_ref()` to `get_candidate_keys`. On empty candidates → `503 GW_ROUTING_NO_CAPABILITY_MATCH` (distinct from `GW_ROUTING_NO_AVAILABLE_CHANNEL`).

8. **Metric**: `gw_capability_resolve_total{outcome}`.

9. **Verify** + **Commit**:
   ```
   git add crates/gw-capability/src/backend.rs \
           crates/gw-capability/src/lib.rs \
           crates/gw-capability/src/resolver.rs \
           crates/gw-store/src/loader.rs \
           crates/gw-store/src/entity_store.rs \
           crates/gw-proxy/src/gateway.rs \
           crates/gw-proxy/src/filters/request.rs \
           crates/gw-proxy/src/filters/upstream.rs \
           crates/gw-routing/src/candidates.rs \
           crates/gw-gateway/src/lib.rs \
           crates/gw-gateway/src/metrics_catalog.rs \
           crates/gw-proxy/tests/capability_filter.rs
   git commit -m "feat(capability): inject CapabilityResolver into LlmGateway with EntityStore backend (G13)

   Production ResolveBackend impl reading Model.capabilities,
   VendorKey.forced_capabilities, and VendorKey.exclusive_capability_match
   from EntityStore. CapabilityResolver constructed in bootstrap, threaded
   through Wired -> LlmGateway -> ctx.

   request_filter resolves required caps after permission merge and stashes
   on ctx.required_caps. upstream_peer passes the set into get_candidate_keys,
   which CapabilityFilter-evaluates each candidate. Empty result -> 503
   GW_ROUTING_NO_CAPABILITY_MATCH (distinct from no-available-channel).

   Spec ADRs: G13-1, G13-2. Migration 0004 landed in T0-C. Metric:
   gw_capability_resolve_total{outcome}."
   ```

---

# Wave 4 — Cross-protocol stream render (serial, depends on G9)

## T4-A — Cross-protocol stream chunk render

**Files touched**:
- `crates/gw-proxy/src/filters/response.rs`
- `crates/gw-proxy/src/ctx.rs` (if `pending_client_body` from T0-A needs additional shape)
- `crates/gw-codec/tests/cross_protocol_stream.rs` (new)
- `crates/gw-gateway/src/metrics_catalog.rs`

**Steps**:

1. **Test first** — `crates/gw-codec/tests/cross_protocol_stream.rs`:

   ```rust
   use gw_codec::{anthropic::codec::AnthropicCodec, openai::codec::OpenAiCodec, ProtocolCodec};

   #[test]
   fn anthropic_chunk_rendered_as_openai_for_client() {
       // Upstream emits an Anthropic content_block_delta event.
       let upstream_chunk = br#"event: content_block_delta
   data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}

   "#;
       let upstream_codec = AnthropicCodec;
       let client_codec = OpenAiCodec;
       let events = upstream_codec.parse_stream_chunk(upstream_chunk);
       assert!(!events.is_empty());

       let mut buf = bytes::BytesMut::new();
       for ev in &events {
           client_codec.render_stream_chunk(ev, &mut buf);
       }
       let rendered = std::str::from_utf8(&buf).unwrap();
       assert!(rendered.contains("\"delta\":{\"content\":\"hello\"}"));
       assert!(rendered.starts_with("data: "));
   }

   #[test]
   fn gemini_combined_chunk_renders_all_events_to_openai_client() {
       // Gemini's combined-final-event shape: text + finishReason in one frame.
       // G9's SmallVec must yield [Delta, Finish]; G8 must render BOTH into the
       // client buffer (regression guard against single-shot overwrite).
       use gw_codec::gemini::codec::GeminiCodec;
       let upstream = GeminiCodec;
       let client = OpenAiCodec;
       let upstream_chunk = br#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#;
       let events = upstream.parse_stream_chunk(upstream_chunk);
       assert!(events.len() >= 2, "Gemini combined chunk must decompose into >=2 events");

       let mut buf = bytes::BytesMut::new();
       for ev in &events {
           client.render_stream_chunk(ev, &mut buf);
       }
       let rendered = std::str::from_utf8(&buf).unwrap();
       assert!(rendered.contains("\"content\":\"hi\""), "text delta must survive");
       assert!(rendered.contains("\"finish_reason\":\"stop\""), "finish event must survive");
   }
   ```

2. **Implement** in `crates/gw-proxy/src/filters/response.rs::handle_event`. **Critical**: when the upstream chunk decomposes into multiple events (G9), the rendered output must accumulate — overwriting `ctx.pending_client_body` per event would silently drop everything but the last (e.g. Gemini's combined `[Delta, Finish]` → only Finish survives, client sees no text).

   Use `ctx.pending_client_body: Option<BytesMut>` as a per-chunk accumulator. Take it out at the start of the chunk, append every cross-protocol render into it, put it back at the end:

   ```rust
   let cross_protocol = ctx.client_codec.kind() != ctx.upstream_codec.kind();
   let mut acc = ctx.pending_client_body.take().unwrap_or_else(bytes::BytesMut::new);

   for ev in events {
       // accumulate usage / TTFT etc (existing logic) — runs in both branches.
       …

       if cross_protocol {
           // Render INTO the existing buffer; do not allocate a fresh one
           // and do not overwrite.
           ctx.client_codec.render_stream_chunk(&ev, &mut acc);
           metrics::counter!("gw_codec_cross_render_total",
               "from" => ctx.upstream_codec.kind().as_str(),
               "to" => ctx.client_codec.kind().as_str()).increment(1);
       }
   }

   if cross_protocol && !acc.is_empty() {
       ctx.pending_client_body = Some(acc);
   }
   ```

   Update the `response_body_filter` outer call so that, after `handle_event` returns and we are about to write to the client:

   ```rust
   if let Some(buf) = ctx.pending_client_body.take() {
       *body = Some(buf.freeze()); // replaces upstream chunk
   }
   // else: forward upstream bytes verbatim (same-protocol passthrough)
   ```

   The `.take()` is critical — each Pingora chunk gets a fresh accumulator. Forgetting to clear it would carry old rendered bytes into the next chunk.

3. **Metric**: `gw_codec_cross_render_total{from,to}` plus `gw_codec_passthrough_total{kind}` for same-protocol counter.

4. **Verify** + **Commit**:
   ```
   git add crates/gw-proxy/src/filters/response.rs \
           crates/gw-proxy/src/ctx.rs \
           crates/gw-codec/tests/cross_protocol_stream.rs \
           crates/gw-gateway/src/metrics_catalog.rs
   git commit -m "feat(codec): cross-protocol stream chunk render via IR round-trip (G8)

   response_body_filter now, for each parsed StreamEventIR, checks
   ctx.client_codec.kind() vs ctx.upstream_codec.kind(). When they differ,
   re-renders via client_codec.render_stream_chunk and replaces the outbound
   body chunk via ctx.pending_client_body. Same-protocol stays zero-copy.

   v1.5 path: IR round-trip. Direct wire-to-wire codegen (codec-s3 N²)
   remains the v2 target.

   Spec ADR: G8-1. Metrics: gw_codec_cross_render_total{from,to},
   gw_codec_passthrough_total{kind}."
   ```

---

# Wave 5 — Consolidation

## T5-A — Invariants, architecture doc, final test sweep

**Files touched**:
- `scripts/check-invariants.sh`
- `docs/architecture.md`
- (possibly) `docs/workflow.md` if any filter behaviour materially changed

**Steps**:

1. **Update `scripts/check-invariants.sh`**:
   - Confirm `no_billing_records_stream` still passes with the new `usage:records:dlq` (the existing regex must allow this suffix).
   - Activate `no_cost_in_dlq` (added in T2-D).
   - Activate `no_global_hmac_force` (a grep that fails if a `signature_required_workspace_wide` config flag appears anywhere).
   - Confirm `no_legacy_breaker` / `no_legacy_retry` etc. all still green.

2. **Update `docs/architecture.md`**:
   - **§9 Known gaps**: strike-through or remove the lines for gaps 1–13 that landed in this branch. Add a single new sentence pointing at this plan and spec.
   - **§9 v2 follow-ups (new subsection)**: for the v1.5/v2 split (G7-2, G8 IR-round-trip vs S3 N², G9 stateful accum, G10 connect-only vs full failover, G11 single-fallback vs three-layer), list the spec/plan refs.
   - **§Deliberate behaviours (extend table)**: add the **daily-quota UTC midnight race** row: requests crossing the UTC midnight boundary may have their increment land on the **next** day's counter (the request started on day N but `logging` runs on day N+1). Affects < 0.001 % of traffic; daily quota is best-effort, not security. Cite `crates/gw-proxy/src/filters/logging.rs` and `crates/gw-proxy/src/filters/request.rs`.

3. **Update `docs/workflow.md`** if anything materially changed in the filter chain:
   - `request_filter` now does: expiry check → IP allowlist → HMAC verify → JWT validator(client path) → daily quota → capability resolve. Re-order the §2.2 list to reflect this.
   - `upstream_peer` now applies Vendor.timeout_overrides between defaults and Pricing.
   - `response_body_filter` now handles cross-protocol stream re-render and multi-event SmallVec.
   - `fail_to_connect` is now an override on `LlmGateway`.

4. **Final test sweep**:
   ```bash
   cargo build --workspace
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   bash scripts/check-invariants.sh
   ```

   All must pass.

5. **Commit**:
   ```
   git add scripts/check-invariants.sh docs/architecture.md docs/workflow.md
   git commit -m "wave5(consolidate): activate new invariants and refresh architecture+workflow docs

   - Activates no_cost_in_dlq and no_global_hmac_force in
     scripts/check-invariants.sh.
   - Updates docs/architecture.md §9 to reflect closed gaps 1-13 with v2
     follow-up pointers for the items where v1.5 lands a minimum subset
     (G7-2, G8, G9, G10, G11).
   - Refreshes docs/workflow.md to include expires_at/ip_allowlist/HMAC/
     JWT-validator/daily-quota/capability-resolve in request_filter, vendor
     timeout precedence in upstream_peer, multi-event SmallVec +
     cross-protocol render in response_body_filter, and the new
     fail_to_connect override on LlmGateway.

   Workspace build / test / clippy / fmt / invariants all green at HEAD."
   ```

---

# Dispatch checklist

Per task, the controller (you) dispatches **three** subagents in sequence:

1. **Implementer** (`subagent-driven-development/implementer-prompt.md` template) — fills in TASK DESCRIPTION with the section above (T0-A … T5-A). The implementer is bound by the path allowlist and the commit message.
2. **Spec compliance reviewer** (`spec-reviewer-prompt.md`) — verifies the implementer built **exactly** the ADR called out (no scope creep, no missed acceptance criteria).
3. **Code quality reviewer** (`code-reviewer.md`) — verifies single-responsibility, decomposition, no unwarranted file growth.

Only when both reviewers pass does the next Wave's tasks dispatch.

## End-of-branch closure

When all 16 commits land and Wave 5 passes:

1. Push branch to `origin/feature/known-gaps-closure`.
2. Open PR against `main` with the body being the §0 coverage map of the spec.
3. Once merged, delete the feature branch.

A separate `finishing-a-development-branch` skill invocation may help — the workflow you ran on the previous gap-residual branch is the model.
