# W1 API-Key Affinity Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the existing `gw-routing::affinity::AffinityManager` into live proxy upstream selection for authenticated API-key requests, selected vendor-key recording, and vendor-key invalidation.

**Architecture:** Keep `AffinityManager` as the single request-external binding table owned by gateway bootstrap and shared into `LlmGateway`. `request_filter` already stores authenticated `ApiKeyId` in `ReqCtx.api_key_id`; `upstream::pick_peer` should consult the manager after ranked candidates are computed and before normal first-candidate selection, while `upstream_request_filter` records only after the upstream request rewrite succeeds. Vendor-key invalidation should clear affinity via the existing `EntityStore::on_vendor_key_invalidate` hook and the existing error listeners that mark a vendor key unhealthy or OAuth-failed.

**Tech Stack:** Rust, Pingora `ProxyHttp`, `gw-proxy`, `gw-routing`, `gw-gateway`, `gw-store`, `cargo test`.

---

## Source Evidence

- Master W1 scope is `docs/plans/2026-06-09-remaining-roadmap-execution.md` lines 116-153: store authenticated `ApiKeyId`, consult `AffinityManager::get(AffinityDimension::ApiKey(...))`, record selected vendor keys, invalidate on vendor-key invalidation, and leave `strategy/cache_affinity.rs` unchanged.
- W0 unblock matrix is `docs/plans/2026-06-09-w0-contract-evidence.md` lines 276-288: W1 can start after W0 with no new contract decision.
- API-key identity already exists: `crates/gw-proxy/src/filters/request.rs` resolves `ApiKeyId` from `x-api-key-id`, validates it, then writes `ctx.api_key_id = api_key_id.clone()`.
- Request context already carries the identity and selection state: `crates/gw-proxy/src/ctx.rs` has `api_key_id`, `picked_vendor_key`, `picked_candidates`, and `retry_blacklist`.
- Upstream selection lives in `crates/gw-proxy/src/filters/upstream.rs`: `get_candidate_keys` creates available candidates, `strategy.ranked_candidates` creates the ranked list, and `ctx.picked_vendor_key` is written after the peer is successfully constructed.
- Gateway construction lives in `crates/gw-gateway/src/lib.rs`: `Wired` mirrors `LlmGateway::new`, `bootstrap` creates shared routing collaborators, and `run` passes them into `LlmGateway::new`.
- Vendor-key invalidation hook already exists: `EntityStore::on_vendor_key_invalidate` and `EntityStore::invalidate_vendor_key` in `crates/gw-store/src/entity_store.rs`; `StreamConsumer` calls it on `events:entity ns=vendor_key`.
- Existing error listener fanout lives in `crates/gw-proxy/src/filters/logging.rs`; `HandlerCtx` currently carries `vendor_key_id`, `key_health`, `entity_store`, and `resilience`.

## User Decision Needed

None. API-key identity field, ranked candidate point, selected-key recording point, gateway construction point, and vendor-key invalidation path are all identifiable from the current code.

## File Structure

- Modify: `crates/gw-proxy/src/ctx.rs`
  - No new fields required for API-key identity; only update if tests need constructor defaults after adding gateway fields elsewhere.
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
  - Add a small pure helper that returns an affinity-preferred candidate from an already ranked candidate slice.
  - Thread `AffinityManager` into `pick_peer`.
  - Add `record_api_key_affinity` and call it only after outbound rewrite succeeds.
- Modify: `crates/gw-proxy/src/gateway.rs`
  - Add `affinity: Arc<gw_routing::affinity::AffinityManager>` to `LlmGateway`.
  - Pass it into `pick_peer`, `upstream_request_filter`, and `filters::logging::run`.
- Modify: `crates/gw-proxy/src/error/handler.rs`
  - Add `affinity: Option<&AffinityManager>` to `HandlerCtx`.
- Modify: `crates/gw-proxy/src/error/listeners/key_health.rs`
  - Invalidate affinity for the cooled-down vendor key after `apply_cooldown`.
- Modify: `crates/gw-proxy/src/error/listeners/oauth.rs`
  - Invalidate affinity for the OAuth refresh-failed vendor key before the reverse-XADD branch returns.
- Modify: `crates/gw-proxy/src/filters/logging.rs`
  - Accept `affinity` and include it in `HandlerCtx`.
- Modify: `crates/gw-proxy/tests/listeners_wire.rs`, `crates/gw-proxy/tests/error_handler.rs`, `crates/gw-proxy/tests/filters_logging_handler.rs`
  - Update `HandlerCtx` assertions and listener wiring tests.
- Modify: `crates/gw-gateway/src/lib.rs`
  - Construct one `Arc<AffinityManager>` in bootstrap, register the store invalidation hook, carry it through `Wired`, and pass it into `LlmGateway::new`.
- Modify: `docs/spec/02-routing.md`
  - Change API-key affinity status from residual to wired and cite the proxy/bootstrap/listener points.
- Modify: `docs/ROADMAP.md`
  - Close the R-D entry after implementation verification.
- Do not modify: `crates/gw-routing/src/strategy/cache_affinity.rs`.

## Task 1: Add Proxy Affinity Selection Tests

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Write failing unit tests in the existing `#[cfg(test)]` module**

Add these tests near the existing upstream filter unit tests. Reuse the file's existing `vendor_key_with_mapping` helper if in scope; otherwise add the local `key` helper shown here inside the test module.

```rust
use gw_core::ids::ApiKeyId;
use gw_core::protocol::ProtocolKind;
use gw_routing::affinity::{AffinityDimension, AffinityManager};

fn key(id: &str) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v-affinity"),
        api_key: secrecy::SecretString::from("sk-test".to_string()),
        accessible_models: vec![ModelName::from("m1")],
        model_mapping: Default::default(),
        accessible_endpoints: vec![ProtocolKind::OpenAi],
        protocol_conversion: false,
        priority: 0,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: gw_core::entity::KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    })
}

#[test]
fn api_key_affinity_prefers_recorded_candidate() {
    let affinity = AffinityManager::default();
    let mut ctx = ReqCtx::default();
    ctx.api_key_id = Some(ApiKeyId::from("ak-affinity"));
    let first = key("vk-first");
    let preferred = key("vk-preferred");
    let ranked = vec![first.clone(), preferred.clone()];
    affinity.record_hit(
        AffinityDimension::ApiKey(ApiKeyId::from("ak-affinity")),
        preferred.key_id.clone(),
        std::time::Instant::now(),
    );
    let health = KeyHealthStore::default();

    let picked = affinity_preferred_candidate(
        &ctx,
        &ranked,
        &affinity,
        &health,
        ProtocolKind::OpenAi,
    )
    .expect("recorded affinity candidate should be selected");

    assert_eq!(picked.key_id, preferred.key_id);
}

#[test]
fn api_key_affinity_ignores_missing_blacklisted_or_unhealthy_candidate() {
    let affinity = AffinityManager::default();
    let mut ctx = ReqCtx::default();
    ctx.api_key_id = Some(ApiKeyId::from("ak-affinity"));
    let first = key("vk-first");
    let preferred = key("vk-preferred");
    let ranked = vec![first, preferred.clone()];
    affinity.record_hit(
        AffinityDimension::ApiKey(ApiKeyId::from("ak-affinity")),
        preferred.key_id.clone(),
        std::time::Instant::now(),
    );
    ctx.retry_blacklist.push(preferred.key_id.clone());
    let health = KeyHealthStore::default();

    assert!(affinity_preferred_candidate(
        &ctx,
        &ranked,
        &affinity,
        &health,
        ProtocolKind::OpenAi,
    )
    .is_none());
}
```

- [ ] **Step 2: Run tests and verify the expected failure**

Run:

```bash
cargo test -p gw-proxy affinity_preferred_candidate --lib
```

Expected: compile failure containing `cannot find function 'affinity_preferred_candidate'` or the local equivalent unresolved-name error. If the file's current unit-test import block needs `Arc`, `VendorKey`, `KeyId`, `VendorId`, or `ModelName`, add only those imports and rerun until the only failure is the missing helper.

- [ ] **Step 3: Add the minimal pure helper**

Add this helper above `pick_peer` in `crates/gw-proxy/src/filters/upstream.rs`:

```rust
fn affinity_preferred_candidate<'a>(
    ctx: &ReqCtx,
    ranked_candidates: &'a [Arc<VendorKey>],
    affinity: &gw_routing::affinity::AffinityManager,
    health: &KeyHealthStore,
    api_format: gw_core::protocol::ProtocolKind,
) -> Option<&'a Arc<VendorKey>> {
    let api_key_id = ctx.api_key_id.as_ref()?;
    let dimension = gw_routing::affinity::AffinityDimension::ApiKey(api_key_id.clone());
    let record = affinity.get(&dimension, std::time::Instant::now())?;
    ranked_candidates.iter().find(|candidate| {
        candidate.key_id == record.vendor_key_id
            && !ctx
                .retry_blacklist
                .iter()
                .any(|blacklisted| blacklisted == &candidate.key_id)
            && health.is_available(&candidate.key_id, api_format)
    })
}
```

- [ ] **Step 4: Run tests and verify they pass**

Run:

```bash
cargo test -p gw-proxy affinity_preferred_candidate --lib
```

Expected: the two affinity helper tests pass.

## Task 2: Wire Affinity Selection and Recording

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: Write the recording test first**

In the same upstream filter unit-test module, add:

```rust
#[test]
fn record_api_key_affinity_records_only_when_api_key_and_vendor_are_present() {
    let affinity = AffinityManager::default();
    let mut ctx = ReqCtx::default();
    assert!(!record_api_key_affinity(&ctx, &affinity));

    ctx.api_key_id = Some(ApiKeyId::from("ak-record"));
    assert!(!record_api_key_affinity(&ctx, &affinity));

    let picked = key("vk-recorded");
    ctx.picked_vendor_key = Some(picked.clone());
    assert!(record_api_key_affinity(&ctx, &affinity));

    let record = affinity
        .get(
            &AffinityDimension::ApiKey(ApiKeyId::from("ak-record")),
            std::time::Instant::now(),
        )
        .expect("record should exist after selected send");
    assert_eq!(record.vendor_key_id, picked.key_id);
}
```

- [ ] **Step 2: Run tests and verify the expected failure**

Run:

```bash
cargo test -p gw-proxy record_api_key_affinity --lib
```

Expected: compile failure containing `cannot find function 'record_api_key_affinity'`.

- [ ] **Step 3: Add the recording helper**

Add this helper near `affinity_preferred_candidate`:

```rust
pub fn record_api_key_affinity(
    ctx: &ReqCtx,
    affinity: &gw_routing::affinity::AffinityManager,
) -> bool {
    let (Some(api_key_id), Some(picked)) = (ctx.api_key_id.as_ref(), ctx.picked_vendor_key.as_ref())
    else {
        return false;
    };
    affinity.record_hit(
        gw_routing::affinity::AffinityDimension::ApiKey(api_key_id.clone()),
        picked.key_id.clone(),
        std::time::Instant::now(),
    );
    true
}
```

- [ ] **Step 4: Add the gateway-held manager**

In `crates/gw-proxy/src/gateway.rs`, add a field:

```rust
pub affinity: Arc<gw_routing::affinity::AffinityManager>,
```

Add an `affinity: Arc<gw_routing::affinity::AffinityManager>` parameter to `LlmGateway::new` immediately after `strategies`, assign it in `Self { ... }`, and pass it to `filters::upstream::pick_peer`.

- [ ] **Step 5: Thread affinity into `pick_peer`**

Change the `pick_peer` signature in `crates/gw-proxy/src/filters/upstream.rs`:

```rust
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    affinity: &Arc<gw_routing::affinity::AffinityManager>,
    latency_tracker: &Arc<LatencyTracker>,
    adaptive_rpm: &Arc<gw_auth::ratelimit::adaptive::AdaptiveRpmManager>,
    ratelimit: &Arc<gw_auth::ratelimit::RateLimitGuard>,
    egress: &Arc<gw_egress_impersonate::EgressHandle>,
    egress_disabled: bool,
    #[cfg(feature = "tunnel")] tunnel: Option<&Arc<crate::tunnel_stack::TunnelStack>>,
) -> Result<Box<HttpPeer>> {
```

Update the call in `gateway.rs` by inserting `&self.affinity` after `&self.strategies`.

- [ ] **Step 6: Make affinity influence the existing ranked pick**

Replace the start of the current `let picked = match failover_pick { ... }` block with this shape, preserving the existing adaptive-RPM branch:

```rust
let affinity_pick = if failover_pick.is_none() {
    affinity_preferred_candidate(
        ctx,
        &ranked_candidates,
        affinity.as_ref(),
        health.as_ref(),
        client_codec_kind,
    )
} else {
    None
};
let picked = match failover_pick.or(affinity_pick) {
    Some(candidate) => candidate,
    None => {
        let first = ranked_candidates
            .first()
            .ok_or_else(|| Error::new(ErrorType::Custom("gw_routing_strategy_empty")))?;
        if adaptive_rpm.try_admit(&first.key_id) {
            first
        } else if let Some(alt) = ranked_candidates
            .iter()
            .find(|c| c.key_id != first.key_id && adaptive_rpm.try_admit(&c.key_id))
        {
            alt
        } else {
            first
        }
    }
};
```

Expected behavior: failover-requested keys still win during retry, API-key affinity wins on first attempt when the recorded key is still in the ranked candidate set and healthy, and normal ranked routing remains the fallback.

- [ ] **Step 7: Record only after outbound rewrite succeeds**

In `LlmGateway::upstream_request_filter`, immediately after:

```rust
filters::upstream::rewrite_request(session, upstream_request, ctx, &self.body_rules)
    .await?;
```

add:

```rust
filters::upstream::record_api_key_affinity(ctx, self.affinity.as_ref());
```

This is the recording point because `rewrite_request` succeeded and the request is committed to the selected vendor key. Do not record in `pick_peer`; peer construction can still be followed by request rewrite failure.

- [ ] **Step 8: Construct and pass the manager from bootstrap**

In `crates/gw-gateway/src/lib.rs`, add `affinity: Arc<gw_routing::affinity::AffinityManager>` to `Wired` next to `strategies`.

In bootstrap, before registering vendor-key invalidation hooks, construct:

```rust
let affinity = Arc::new(gw_routing::affinity::AffinityManager::default());
```

In the `Ok(Wired { ... })` block, add `affinity`.

In `run`, pass `wired.affinity` into `LlmGateway::new` immediately after `wired.strategies`.

- [ ] **Step 9: Run selection and build checks**

Run:

```bash
cargo test -p gw-proxy affinity --lib
cargo check -p gw-proxy -p gw-gateway
```

Expected: affinity unit tests pass and both crates check cleanly. If `LlmGateway::new` tests fail to compile because constructor call sites changed, update those call sites to pass `Arc::new(gw_routing::affinity::AffinityManager::default())`.

## Task 3: Wire Vendor-Key Invalidation

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-proxy/src/error/handler.rs`
- Modify: `crates/gw-proxy/src/error/listeners/key_health.rs`
- Modify: `crates/gw-proxy/src/error/listeners/oauth.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/tests/listeners_wire.rs`
- Modify: `crates/gw-proxy/tests/error_handler.rs`
- Modify: `crates/gw-proxy/tests/filters_logging_handler.rs`

- [ ] **Step 1: Wire StreamConsumer vendor-key invalidation first**

In `crates/gw-gateway/src/lib.rs`, after the existing adaptive RPM hook:

```rust
{
    let affinity_for_evict = affinity.clone();
    store_inner.on_vendor_key_invalidate(Arc::new(move |key_id| {
        affinity_for_evict.invalidate_vendor_key(key_id);
    }));
}
```

Expected behavior: any `events:entity ns=vendor_key id=<key>` invalidation clears matching API-key affinity records in the same process.

- [ ] **Step 2: Extend `HandlerCtx`**

In `crates/gw-proxy/src/error/handler.rs`, import the type and add the optional handle:

```rust
use gw_routing::affinity::AffinityManager;
```

```rust
pub struct HandlerCtx<'a> {
    pub request_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub vendor_key_id: Option<&'a str>,
    pub api_format: Option<gw_core::protocol::ProtocolKind>,
    pub key_health: Option<&'a KeyHealthStore>,
    pub entity_store: Option<&'a Arc<EntityStore>>,
    pub resilience: Option<&'a ResilienceState>,
    pub affinity: Option<&'a AffinityManager>,
}
```

Update `HandlerCtx::stub()` with `affinity: None`.

- [ ] **Step 3: Update logging fanout context**

Change `filters::logging::run` to accept:

```rust
affinity: &gw_routing::affinity::AffinityManager,
```

and set:

```rust
affinity: Some(affinity),
```

inside `HandlerCtx`. In `LlmGateway::logging`, pass `self.affinity.as_ref()` after `&self.resilience`.

- [ ] **Step 4: Add listener invalidation tests**

In `crates/gw-proxy/src/error/listeners/key_health.rs`, add:

```rust
#[test]
fn cooldown_invalidates_affinity_for_vendor_key() {
    let store = KeyHealthStore::default();
    let affinity = gw_routing::affinity::AffinityManager::default();
    affinity.record_hit(
        gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
        KeyId::from("vk-1"),
        std::time::Instant::now(),
    );
    let ctx = HandlerCtx {
        request_id: "test",
        api_key_id: Some("ak-1"),
        vendor_key_id: Some("vk-1"),
        api_format: Some(ProtocolKind::OpenAi),
        key_health: Some(&store),
        entity_store: None,
        resilience: None,
        affinity: Some(&affinity),
    };
    let oc = ClassifierOutcome {
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective {
            reason_code: 7,
            ttl_secs: 30,
            reset_consecutive: false,
        }),
        rate_limit_hint: None,
        ..Default::default()
    };

    KeyHealthListener.on_outcome(&ctx, &oc);

    assert!(affinity
        .get(
            &gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
            std::time::Instant::now(),
        )
        .is_none());
}
```

In `crates/gw-proxy/src/error/listeners/oauth.rs`, add:

```rust
#[test]
fn refresh_failed_invalidates_affinity_before_xadd() {
    let affinity = gw_routing::affinity::AffinityManager::default();
    affinity.record_hit(
        gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
        gw_core::ids::KeyId::from("vk-1"),
        std::time::Instant::now(),
    );
    let ctx = HandlerCtx {
        request_id: "test",
        api_key_id: Some("ak-1"),
        vendor_key_id: Some("vk-1"),
        api_format: None,
        key_health: None,
        entity_store: None,
        resilience: None,
        affinity: Some(&affinity),
    };

    OAuthListener.on_outcome(&ctx, &make_outcome(OAuthSignalKind::RefreshFailed));

    assert!(affinity
        .get(
            &gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
            std::time::Instant::now(),
        )
        .is_none());
}
```

- [ ] **Step 5: Run tests and verify the expected failure**

Run:

```bash
cargo test -p gw-proxy invalidates_affinity --lib
```

Expected: tests fail because listeners do not yet call `invalidate_vendor_key`, or compile fails until `HandlerCtx.affinity` is added to every constructor.

- [ ] **Step 6: Implement listener invalidation**

In `key_health.rs`, after `key_health.apply_cooldown(...)`:

```rust
if let Some(affinity) = ctx.affinity {
    affinity.invalidate_vendor_key(&key_id);
}
```

In `oauth.rs`, after confirming `sig.kind == OAuthSignalKind::RefreshFailed` and before requiring `entity_store`:

```rust
if let (Some(affinity), Some(key_id)) = (ctx.affinity, ctx.vendor_key_id) {
    affinity.invalidate_vendor_key(&gw_core::ids::KeyId::from(key_id));
}
```

- [ ] **Step 7: Update existing HandlerCtx tests**

In `crates/gw-proxy/tests/listeners_wire.rs`, add:

```rust
assert!(ctx.affinity.is_none());
```

In every literal `HandlerCtx { ... }`, add:

```rust
affinity: None,
```

Use `rg -n "HandlerCtx \\{" crates/gw-proxy` to find every construction site.

- [ ] **Step 8: Run listener verification**

Run:

```bash
cargo test -p gw-proxy --test listeners_wire
cargo test -p gw-proxy error_handler
cargo test -p gw-proxy affinity --lib
```

Expected: listener wire test passes, error handler tests pass, and affinity listener/unit tests pass.

## Task 4: Close Docs and Commit in Two Implementation Commits

**Files:**
- Modify: `docs/spec/02-routing.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Update routing spec**

In `docs/spec/02-routing.md`, update the API-key affinity section so it says the manager is now wired through:

```markdown
### API-key affinity manager
`affinity.rs` (`crates/gw-routing/src/affinity.rs`) defines `AffinityManager`, a request-external binding table distinct from `strategy/sticky.rs` and `strategy/cache_affinity.rs`. Proxy wiring now passes authenticated `ReqCtx.api_key_id` into `AffinityDimension::ApiKey`, consults the manager against the ranked candidate list in `crates/gw-proxy/src/filters/upstream.rs`, records a selected vendor key after outbound request rewrite succeeds, and invalidates bindings through vendor-key invalidation hooks plus key-health/OAuth error listeners.
```

Remove residual language that says `gw-proxy` does not pass authenticated `ApiKeyId` into routing.

- [ ] **Step 2: Close ROADMAP R-D entry**

In `docs/ROADMAP.md`, change the R-D entry status from open capability gap to closed/wired wording. Keep the design warning that `strategy/cache_affinity.rs` is separate and unchanged.

- [ ] **Step 3: Run full W1 verification**

Run:

```bash
cargo test -p gw-routing affinity
cargo test -p gw-proxy affinity
cargo test -p gw-proxy --test capability_filter
cargo test -p gw-proxy --test listeners_wire
bash scripts/check-spec.sh docs/spec/02-routing.md
git diff --check
```

Expected:
- `gw-routing affinity` passes existing pure manager tests.
- `gw-proxy affinity` passes selection, recording, and listener invalidation tests.
- `capability_filter` still passes, proving candidate filtering behavior remains intact.
- `listeners_wire` passes with the expanded `HandlerCtx`.
- `check-spec.sh` passes for routing docs.
- `git diff --check` reports no whitespace errors.

- [ ] **Step 4: Make the feat commit**

Stage only source and source-test changes:

```bash
git add \
  crates/gw-proxy/src/filters/upstream.rs \
  crates/gw-proxy/src/gateway.rs \
  crates/gw-proxy/src/error/handler.rs \
  crates/gw-proxy/src/error/listeners/key_health.rs \
  crates/gw-proxy/src/error/listeners/oauth.rs \
  crates/gw-proxy/src/filters/logging.rs \
  crates/gw-proxy/tests/listeners_wire.rs \
  crates/gw-proxy/tests/error_handler.rs \
  crates/gw-proxy/tests/filters_logging_handler.rs \
  crates/gw-gateway/src/lib.rs
git commit -m "feat(routing): wire api-key affinity into proxy selection"
```

- [ ] **Step 5: Make the docs commit**

Stage only docs:

```bash
git add docs/spec/02-routing.md docs/ROADMAP.md
git commit -m "docs(routing): close R-D affinity roadmap entry"
```

## Final Verification Before Handoff

- [ ] Run:

```bash
git log --oneline -2
git status --short
```

Expected: the two implementation commits are present in this order:

```text
docs(routing): close R-D affinity roadmap entry
feat(routing): wire api-key affinity into proxy selection
```

`git status --short` should show only unrelated pre-existing user changes, if any. Do not revert unrelated changes.
