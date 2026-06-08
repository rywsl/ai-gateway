# W3 Rate Limiting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Do **not** modify unrelated dirty files; do **not** revert user work. W3 execution may modify only the files listed in this plan.

**Goal:** Complete W3 from `docs/plans/2026-06-08-roadmap-implementation-program.md`: add an RL-Core per-vendor-key RPM gate, populate `RateLimitKind` instead of hard-coded `Unknown`, and make RL-Adaptive learn only from `RateLimitKind::Rpm | RateLimitKind::Unknown`.

**Architecture:** Keep per-user RPM in `request_filter`, because that is where merged permissions are known. Add per-vendor-key RPM in `pick_peer` after the final vendor key is selected, because `request_filter` does not know the vendor key. Move the placeholder `gw_core::ratelimit` inline module into a focused file that owns `RateLimitKind` plus `detect_rate_limit_kind`, then thread the classified kind through `DefaultClassifier`, `gateway.rs`, and `AdaptiveRpmManager`.

**Tech Stack:** Rust 1.88, Pingora 0.8, governor direct rate limiters, DashMap, existing `gw-core`, `gw-auth`, and `gw-proxy` crates.

---

## Scope And Assumptions

- W0 is already present; W3 does not depend on W2, W4, W5, W6, W7, or W8.
- This plan expands `docs/ROADMAP.md` `## 04 rate-limiting` and the W3 brief in `docs/plans/2026-06-08-roadmap-implementation-program.md`.
- `RateLimitKind::Unknown` remains learnable for RL-Adaptive so vendors with opaque 429s do not lose the current fallback behavior.
- `RateLimitKind::Tpm`, `Daily`, `Monthly`, and `Concurrent` must not mutate adaptive RPM state.
- The per-key RPM gate is a hard local admission check for the selected vendor key. If the selected key is over its configured or overridden key RPM, return local `GW_QUOTA_RPM_KEY_EXCEEDED` 429 rather than sending upstream.
- Because `AGENTS.md` requires as-built docs sync when code lands, Task 5 updates `docs/spec/04-rate-limiting.md`, `docs/workflow.md`, `docs/architecture.md`, and `docs/ROADMAP.md` during execution. This planning pass itself only creates this file.
- No blocking clarification is required before implementation.

## File Map

- `crates/gw-core/src/lib.rs`: replace the inline `ratelimit` placeholder module with `pub mod ratelimit;`.
- `crates/gw-core/src/ratelimit.rs`: new focused home for `RateLimitKind` and `detect_rate_limit_kind`.
- `crates/gw-core/src/error/classify_default.rs`: use `detect_rate_limit_kind` for upstream 429 classifier outcomes and rate-limit hints.
- `crates/gw-core/tests/ratelimit_kind.rs`: classifier heuristic tests.
- `crates/gw-core/tests/classifier_default.rs`: update or add assertions that upstream 429s carry the detected kind.
- `crates/gw-core/src/error/codes.rs`: add stable `GW_QUOTA_RPM_KEY_EXCEEDED`.
- `crates/gw-auth/src/ratelimit/mod.rs`: add per-key limiter state and `check_key` / `override_key_rpm` / `reset_key_rpm`.
- `crates/gw-auth/src/ratelimit/adaptive.rs`: add a `RateLimitKind` parameter to `observe_429` and skip non-RPM known kinds.
- `crates/gw-auth/tests/ratelimit.rs`: per-key gate tests.
- `crates/gw-auth/tests/adaptive_rpm.rs`: update current calls and add non-RPM skip tests.
- `crates/gw-proxy/src/gateway.rs`: classify upstream 429s before calling `observe_429`.
- `crates/gw-proxy/src/filters/upstream.rs`: pass the shared `RateLimitGuard` into `pick_peer` and call `check_key` for the selected key.
- `docs/spec/04-rate-limiting.md`, `docs/workflow.md`, `docs/architecture.md`, `docs/ROADMAP.md`: execution-time docs sync after code lands.

---

### Task 1: RL-Core Per-Key RPM Gate

**Files:**
- Modify: `crates/gw-auth/src/ratelimit/mod.rs`
- Test: `crates/gw-auth/tests/ratelimit.rs`

- [ ] **Step 1: Write the failing tests**

Append these tests to `crates/gw-auth/tests/ratelimit.rs`:

```rust
use gw_core::ids::KeyId;

#[test]
fn key_rpm_limit_rejects_with_stable_code() {
    let g = RateLimitGuard::new();
    let key = KeyId::from("vendor-key-rpm-1");

    g.check_key(&key, 1).expect("first request within key rpm");
    let err = g.check_key(&key, 1).unwrap_err();

    assert_eq!(err.status, 429);
    assert_eq!(&*err.code, "GW_QUOTA_RPM_KEY_EXCEEDED");
    assert_eq!(err.retry_after_secs, Some(60));
}

#[test]
fn key_rpm_zero_disables_enforcement() {
    let g = RateLimitGuard::new();
    let key = KeyId::from("vendor-key-unlimited");

    for _ in 0..100 {
        g.check_key(&key, 0)
            .expect("configured key rpm 0 means unlimited");
    }
}

#[test]
fn key_rpm_buckets_are_per_key() {
    let g = RateLimitGuard::new();
    let a = KeyId::from("vendor-key-a");
    let b = KeyId::from("vendor-key-b");

    g.check_key(&a, 1).expect("a first request ok");
    assert!(g.check_key(&a, 1).is_err(), "a should be exhausted");

    g.check_key(&b, 1)
        .expect("b has an independent one-request bucket");
}

#[test]
fn key_rpm_override_and_reset_rebuild_limiter() {
    let g = RateLimitGuard::new();
    let key = KeyId::from("vendor-key-override");

    g.override_key_rpm(&key, 1);
    g.check_key(&key, 60)
        .expect("first request under override should pass");
    assert!(g.check_key(&key, 60).is_err(), "override rpm=1 should bind");

    g.reset_key_rpm(&key);
    g.check_key(&key, 60)
        .expect("reset should rebuild from configured rpm");
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-auth --test ratelimit key_rpm_limit_rejects_with_stable_code
```

Expected: compile failure because `RateLimitGuard::check_key`, `override_key_rpm`, and `reset_key_rpm` do not exist.

- [ ] **Step 3: Implement the per-key limiter**

In `crates/gw-auth/src/ratelimit/mod.rs`, add `KeyId` to the existing imports and extend `RateLimitGuard`:

```rust
use gw_core::ids::{KeyId, UserId};

pub struct RateLimitGuard {
    pub per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_tpm: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_streams: DashMap<UserId, Arc<AtomicUsize>>,
    pub per_key: DashMap<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>,
    pub per_key_overrides: DashMap<KeyId, u32>,
    pub total_streams: Arc<AtomicUsize>,
}
```

Update `new()`:

```rust
Self {
    per_user: DashMap::new(),
    per_user_tpm: DashMap::new(),
    per_user_streams: DashMap::new(),
    per_key: DashMap::new(),
    per_key_overrides: DashMap::new(),
    total_streams: Arc::new(AtomicUsize::new(0)),
}
```

Add these methods inside `impl RateLimitGuard`:

```rust
pub fn check_key(
    &self,
    key_id: &KeyId,
    configured_rpm: u32,
) -> Result<RateLimitTicket, ErrorIR<'static>> {
    let limit = self
        .per_key_overrides
        .get(key_id)
        .map(|v| *v)
        .unwrap_or(configured_rpm);
    if limit == 0 {
        return Ok(RateLimitTicket);
    }

    let limiter = {
        let mut entry = self.per_key.entry(key_id.clone()).or_insert_with(|| {
            let q = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
            (limit, Arc::new(RateLimiter::direct(q)))
        });
        if entry.0 != limit {
            let q = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
            *entry = (limit, Arc::new(RateLimiter::direct(q)));
        }
        entry.1.clone()
    };

    if limiter.check().is_err() {
        return Err(key_rpm_err());
    }
    Ok(RateLimitTicket)
}

pub fn override_key_rpm(&self, key_id: &KeyId, rpm: u32) {
    self.per_key_overrides.insert(key_id.clone(), rpm);
    self.per_key.remove(key_id);
}

pub fn reset_key_rpm(&self, key_id: &KeyId) {
    self.per_key_overrides.remove(key_id);
    self.per_key.remove(key_id);
}
```

Add the error helper near `rpm_err()`:

```rust
fn key_rpm_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_RPM_KEY_EXCEEDED),
        message: Cow::Borrowed("vendor key rpm exceeded"),
        retryable: true,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}
```

- [ ] **Step 4: Run the focused tests**

Run:

```bash
cargo test -p gw-auth --test ratelimit key_rpm
```

Expected: all per-key tests pass.

---

### Task 2: Stable Error Code And Proxy Wiring

**Files:**
- Modify: `crates/gw-core/src/error/codes.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Test: existing proxy compile and rate-limit tests

- [ ] **Step 1: Add the stable code**

In `crates/gw-core/src/error/codes.rs`, add this constant next to `GW_QUOTA_RPM_EXCEEDED`:

```rust
pub const GW_QUOTA_RPM_KEY_EXCEEDED: &str = "GW_QUOTA_RPM_KEY_EXCEEDED";
```

- [ ] **Step 2: Thread `RateLimitGuard` into `pick_peer`**

In `crates/gw-proxy/src/filters/upstream.rs`, add a `ratelimit` parameter after `adaptive_rpm`:

```rust
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    latency_tracker: &Arc<LatencyTracker>,
    adaptive_rpm: &Arc<gw_auth::ratelimit::adaptive::AdaptiveRpmManager>,
    ratelimit: &Arc<gw_auth::ratelimit::RateLimitGuard>,
    egress: &Arc<gw_egress_impersonate::EgressHandle>,
    egress_disabled: bool,
    #[cfg(feature = "tunnel")] tunnel: Option<&Arc<crate::tunnel_stack::TunnelStack>>,
) -> Result<Box<HttpPeer>> {
```

In `crates/gw-proxy/src/gateway.rs`, pass `&self.ratelimit` at the call site:

```rust
let peer = filters::upstream::pick_peer(
    session,
    ctx,
    &self.store,
    &self.health,
    &self.in_flight,
    &self.strategies,
    &self.latency_tracker,
    &self.adaptive_rpm,
    &self.ratelimit,
    &self.egress,
    self.egress_disabled,
    #[cfg(feature = "tunnel")]
    self.tunnel.as_ref(),
)
.await?;
```

- [ ] **Step 3: Gate the selected vendor key**

In `crates/gw-proxy/src/filters/upstream.rs`, immediately after:

```rust
let picked_arc = Arc::clone(picked);
```

add:

```rust
if let Err(err) = ratelimit.check_key(&picked_arc.key_id, picked_arc.rpm_limit) {
    return emit_envelope_and_bail(session, ctx, err).await;
}
```

This gate must stay after final selection. Do not put it in `request_filter`; that phase has no `picked_vendor_key`.

- [ ] **Step 4: Run compile-focused verification**

Run:

```bash
cargo test -p gw-proxy --no-run
cargo test -p gw-auth --test ratelimit
```

Expected: both commands pass. If `pick_peer` signature tests fail to compile, update only the call sites that pass through the same shared `RateLimitGuard`.

---

### Task 3: `RateLimitKind` Classifier

**Files:**
- Create: `crates/gw-core/src/ratelimit.rs`
- Modify: `crates/gw-core/src/lib.rs`
- Modify: `crates/gw-core/src/error/classify_default.rs`
- Test: `crates/gw-core/tests/ratelimit_kind.rs`
- Test: `crates/gw-core/tests/classifier_default.rs`

- [ ] **Step 1: Write the failing classifier tests**

Create `crates/gw-core/tests/ratelimit_kind.rs`:

```rust
use gw_core::error::class::HeadersAccessor;
use gw_core::ratelimit::{detect_rate_limit_kind, RateLimitKind};
use std::collections::HashMap;

#[derive(Default)]
struct Headers(HashMap<&'static str, &'static str>);

impl HeadersAccessor for Headers {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).copied()
    }
}

fn headers(items: &[(&'static str, &'static str)]) -> Headers {
    Headers(items.iter().copied().collect())
}

#[test]
fn detects_rpm_from_request_rate_headers() {
    let h = headers(&[("anthropic-ratelimit-requests-limit", "60")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Rpm);

    let h = headers(&[("x-ratelimit-remaining-requests", "0")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Rpm);
}

#[test]
fn detects_tpm_from_token_headers() {
    let h = headers(&[("anthropic-ratelimit-tokens-remaining", "0")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Tpm);

    let h = headers(&[("x-ratelimit-limit-tokens", "1000")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Tpm);
}

#[test]
fn detects_window_kinds_from_payload_text() {
    let daily = serde_json::json!({"error": {"message": "daily quota exceeded"}});
    let monthly = serde_json::json!({"error": {"message": "monthly usage limit reached"}});
    let concurrent = serde_json::json!({"error": {"message": "too many concurrent requests"}});

    assert_eq!(detect_rate_limit_kind(&Headers::default(), Some(&daily)), RateLimitKind::Daily);
    assert_eq!(detect_rate_limit_kind(&Headers::default(), Some(&monthly)), RateLimitKind::Monthly);
    assert_eq!(detect_rate_limit_kind(&Headers::default(), Some(&concurrent)), RateLimitKind::Concurrent);
}

#[test]
fn unknown_when_no_specific_signal_exists() {
    let h = headers(&[("x-ratelimit-limit", "60")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Unknown);
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-core --test ratelimit_kind
```

Expected: compile failure because `detect_rate_limit_kind` is undefined.

- [ ] **Step 3: Move the placeholder module into a real file**

Replace the inline module in `crates/gw-core/src/lib.rs`:

```rust
pub mod ratelimit;
```

Create `crates/gw-core/src/ratelimit.rs`:

```rust
use crate::error::class::HeadersAccessor;

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitKind {
    Rpm,
    Tpm,
    Daily,
    Monthly,
    Concurrent,
    Unknown,
}

pub fn detect_rate_limit_kind(
    headers: &dyn HeadersAccessor,
    vendor_payload: Option<&serde_json::Value>,
) -> RateLimitKind {
    if any_header(
        headers,
        &[
            "anthropic-ratelimit-tokens-limit",
            "anthropic-ratelimit-tokens-remaining",
            "x-ratelimit-limit-tokens",
            "x-ratelimit-remaining-tokens",
        ],
    ) {
        return RateLimitKind::Tpm;
    }
    if any_header(
        headers,
        &[
            "anthropic-ratelimit-requests-limit",
            "anthropic-ratelimit-requests-remaining",
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
        ],
    ) {
        return RateLimitKind::Rpm;
    }

    let Some(payload) = vendor_payload else {
        return RateLimitKind::Unknown;
    };
    let text = payload.to_string().to_ascii_lowercase();
    if contains_any(&text, &["token", "tokens", "tpm"]) {
        RateLimitKind::Tpm
    } else if contains_any(&text, &["request per minute", "requests per minute", "rpm"]) {
        RateLimitKind::Rpm
    } else if contains_any(&text, &["daily", "per day", "day quota"]) {
        RateLimitKind::Daily
    } else if contains_any(&text, &["monthly", "per month", "month quota"]) {
        RateLimitKind::Monthly
    } else if contains_any(&text, &["concurrent", "in-flight", "in flight"]) {
        RateLimitKind::Concurrent
    } else {
        RateLimitKind::Unknown
    }
}

fn any_header(headers: &dyn HeadersAccessor, names: &[&str]) -> bool {
    names.iter().any(|name| headers.get(name).is_some())
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}
```

- [ ] **Step 4: Populate classifier outcomes**

In `crates/gw-core/src/error/classify_default.rs`, change the 429 arm so it computes the kind once:

```rust
429 => {
    let kind = crate::ratelimit::detect_rate_limit_kind(headers, _vendor_payload);
    ClassifierOutcome {
        class: ErrorClass::RateLimitedUpstream(kind),
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective {
            reason_code: 7,
            ttl_secs: retry_after.unwrap_or(60),
            reset_consecutive: false,
        }),
        rate_limit_hint: Some(RateLimitHint {
            kind,
            retry_after_secs: retry_after,
        }),
        ..Default::default()
    }
}
```

- [ ] **Step 5: Add classifier integration assertions**

In `crates/gw-core/tests/classifier_default.rs`, add a 429 assertion using an existing test helper or a small local `HeadersAccessor` test double:

```rust
#[test]
fn upstream_429_rate_limit_hint_uses_detected_kind() {
    use gw_core::error::class::{ClassifyCtx, ErrorClassifier, HeadersAccessor};
    use gw_core::error::classify_default::DefaultClassifier;
    use gw_core::protocol::ProtocolKind;
    use gw_core::ratelimit::RateLimitKind;

    struct Headers;
    impl HeadersAccessor for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            match name {
                "anthropic-ratelimit-tokens-remaining" => Some("0"),
                _ => None,
            }
        }
    }

    let ctx = ClassifyCtx {
        key_id: "k1",
        vendor_id: "v1",
        api_format: ProtocolKind::OpenAi,
        protocol: ProtocolKind::OpenAi,
        has_oauth: false,
    };

    let out = DefaultClassifier.classify_upstream(&ctx, 429, &Headers, None);
    let hint = out.rate_limit_hint.expect("429 should carry a hint");
    assert_eq!(hint.kind, RateLimitKind::Tpm);
}
```

- [ ] **Step 6: Run the core tests**

Run:

```bash
cargo test -p gw-core --test ratelimit_kind
cargo test -p gw-core classifier_default
```

Expected: both commands pass.

---

### Task 4: RL-Adaptive Learns Only From RPM Or Unknown

**Files:**
- Modify: `crates/gw-auth/src/ratelimit/adaptive.rs`
- Modify: `crates/gw-auth/tests/adaptive_rpm.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`

- [ ] **Step 1: Write the failing adaptive tests**

In `crates/gw-auth/tests/adaptive_rpm.rs`, import `RateLimitKind`:

```rust
use gw_core::ratelimit::RateLimitKind;
```

Update existing `observe_429` calls to pass `RateLimitKind::Rpm` as the first argument. Then add:

```rust
#[test]
fn test_observe_429_skips_non_rpm_known_kinds() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg {
        min_header_observations: 1,
        ..AdaptiveCfg::default()
    });
    let key_id = KeyId::from("vendor-key-tpm-429");

    manager.observe_429(&key_id, 120, Some(10), 120, Some(60), RateLimitKind::Tpm);
    manager.observe_429(&key_id, 120, Some(10), 120, Some(60), RateLimitKind::Daily);
    manager.observe_429(&key_id, 120, Some(10), 120, Some(60), RateLimitKind::Monthly);
    manager.observe_429(&key_id, 120, Some(10), 120, Some(60), RateLimitKind::Concurrent);

    assert!(
        manager.state.get(&key_id).is_none(),
        "non-RPM known rate-limit kinds must not mutate adaptive RPM state"
    );
}

#[test]
fn test_observe_429_unknown_still_learns_for_opaque_vendors() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg {
        min_header_observations: 1,
        ..AdaptiveCfg::default()
    });
    let key_id = KeyId::from("vendor-key-unknown-429");

    manager.observe_429(&key_id, 120, Some(80), 120, Some(30), RateLimitKind::Unknown);

    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, Some(80));
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-auth --test adaptive_rpm test_observe_429_skips_non_rpm_known_kinds
```

Expected: compile failure because `observe_429` does not accept `RateLimitKind`.

- [ ] **Step 3: Gate `AdaptiveRpmManager::observe_429`**

In `crates/gw-auth/src/ratelimit/adaptive.rs`, import `RateLimitKind`:

```rust
use gw_core::ratelimit::RateLimitKind;
```

Change the signature to:

```rust
pub fn observe_429(
    &self,
    key_id: &KeyId,
    configured_rpm: u32,
    header_limit: Option<u32>,
    current_local_rpm: u32,
    retry_after_secs: Option<u32>,
    kind: RateLimitKind,
) {
```

Add this guard before `gw_adaptive_rpm_observe_total` is incremented:

```rust
if !matches!(kind, RateLimitKind::Rpm | RateLimitKind::Unknown) {
    return;
}
```

- [ ] **Step 4: Classify gateway 429s before adaptive learning**

In `crates/gw-proxy/src/gateway.rs`, import the classifier:

```rust
use gw_core::ratelimit::{detect_rate_limit_kind, RateLimitKind};
```

Add a small header adapter near the existing `header_u32` helper:

```rust
struct ResponseHeaders<'a>(&'a ResponseHeader);

impl gw_core::error::class::HeadersAccessor for ResponseHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.headers.get(name).and_then(|value| value.to_str().ok())
    }
}
```

Change the response-path 429 block to classify once and pass the kind:

```rust
if status == 429 {
    if let Some(key) = ctx.picked_vendor_key.as_ref() {
        let kind = detect_rate_limit_kind(&ResponseHeaders(upstream_response), None);
        self.adaptive_rpm.observe_429(
            &key.key_id,
            key.rpm_limit,
            rpm_limit_header(upstream_response),
            key.rpm_limit,
            header_u32(upstream_response, "retry-after"),
            kind,
        );
        if matches!(kind, RateLimitKind::Rpm) {
            metrics::counter!("gw_adaptive_rpm_rpm_detected_total").increment(1);
        }
    }
}
```

After this change, remove the old `detect_rpm_kind` helper if it has no remaining callers.

- [ ] **Step 5: Run adaptive and proxy verification**

Run:

```bash
cargo test -p gw-auth --test adaptive_rpm
cargo test -p gw-proxy --no-run
```

Expected: both commands pass.

---

### Task 5: Docs Sync, ROADMAP Closure, And Final Verification

**Files:**
- Modify: `docs/spec/04-rate-limiting.md`
- Modify: `docs/workflow.md`
- Modify: `docs/architecture.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Update the rate-limiting spec**

In `docs/spec/04-rate-limiting.md`, move the two W3 items out of the gap section and into as-built behavior:

```markdown
### Per-vendor-key RPM gate

`RateLimitGuard::check_key(key_id, configured_rpm)` enforces a synchronous
per-vendor-key request bucket. `configured_rpm = 0` disables the local gate for
that key. `override_key_rpm` and `reset_key_rpm` are runtime control-plane hooks:
an override replaces the configured key RPM and clears the cached limiter;
reset removes the override and rebuilds from the vendor key's configured RPM.
An exhausted key bucket returns `GW_QUOTA_RPM_KEY_EXCEEDED` (429,
`retry_after_secs = 60`) before the request is sent upstream.
```

Also update the adaptive section:

```markdown
`AdaptiveRpmManager::observe_429` accepts a `RateLimitKind`. Only
`RateLimitKind::Rpm` and `RateLimitKind::Unknown` mutate learned RPM state.
Known non-RPM 429s (`Tpm`, `Daily`, `Monthly`, `Concurrent`) are ignored by the
adaptive learner so token, calendar-window, or concurrency throttles do not
lower the per-key RPM ceiling.
```

- [ ] **Step 2: Update workflow and architecture docs**

In `docs/workflow.md`, update the response-filter paragraph that currently says adaptive observes every 429. It must say the 429 is first classified by `detect_rate_limit_kind`, then adaptive learning runs only for `Rpm | Unknown`.

In `docs/architecture.md`, update the stable-code table entry that says distinct 429 codes only include per-user RPM and upstream overloaded. Include:

```markdown
`GW_QUOTA_RPM_KEY_EXCEEDED` distinguishes a local per-vendor-key RPM gate from
`GW_QUOTA_RPM_EXCEEDED` (per-user RPM) and upstream 429s.
```

- [ ] **Step 3: Close ROADMAP W3 items**

In `docs/ROADMAP.md` `## 04 rate-limiting`, remove:

```markdown
### RL-Core per-key RPM gate + `RateLimitKind` classifier
### RL-Adaptive learns from non-RPM 429s
```

If the section becomes empty, leave:

```markdown
No open ROADMAP items.
```

- [ ] **Step 4: Run final verification**

Run:

```bash
cargo test -p gw-auth --test ratelimit
cargo test -p gw-auth --test adaptive_rpm
cargo test -p gw-core --test ratelimit_kind
cargo test -p gw-core classifier_default
cargo test -p gw-proxy --no-run
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check-invariants.sh
bash scripts/check-spec.sh docs/spec/04-rate-limiting.md
```

Expected: every command exits 0.

- [ ] **Step 5: Commit the W3 implementation**

Commit only W3 files:

```bash
git add \
  crates/gw-core/src/lib.rs \
  crates/gw-core/src/ratelimit.rs \
  crates/gw-core/src/error/classify_default.rs \
  crates/gw-core/src/error/codes.rs \
  crates/gw-core/tests/ratelimit_kind.rs \
  crates/gw-core/tests/classifier_default.rs \
  crates/gw-auth/src/ratelimit/mod.rs \
  crates/gw-auth/src/ratelimit/adaptive.rs \
  crates/gw-auth/tests/ratelimit.rs \
  crates/gw-auth/tests/adaptive_rpm.rs \
  crates/gw-proxy/src/gateway.rs \
  crates/gw-proxy/src/filters/upstream.rs \
  docs/spec/04-rate-limiting.md \
  docs/workflow.md \
  docs/architecture.md \
  docs/ROADMAP.md
git commit -m "feat: add per-key rate limit classification"
```

Expected: one W3 commit with no unrelated dirty files staged.

---

## Self-Review

- Spec coverage: RL-Core per-key RPM gate is covered by Tasks 1 and 2. `GW_QUOTA_RPM_KEY_EXCEEDED` is covered by Tasks 1 and 2. `RateLimitKind` classifier is covered by Task 3. RL-Adaptive learn-only-on-`Rpm | Unknown` is covered by Task 4. As-built docs and ROADMAP closure are covered by Task 5.
- Placeholder scan: no placeholder markers or unspecified "write tests" steps remain.
- Type consistency: `RateLimitKind`, `detect_rate_limit_kind`, `check_key`, `override_key_rpm`, `reset_key_rpm`, and `GW_QUOTA_RPM_KEY_EXCEEDED` are introduced before later tasks reference them.
- Scope check: W3 is independent of W2/W4/W5 and can be executed in its own branch or worktree. Current planning pass required no blocking clarification.
