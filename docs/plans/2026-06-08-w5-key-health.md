# W5 Key-Health Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement ROADMAP `## 03 key-health`: vendor-aware upstream error analyzers on the live `gw-adapter::UpstreamErrorAnalyzer` path, quota-remaining state/probing that routing can deselect, and failure-payload-aware `gw-health::KeyHealthStore::record`.

**Architecture:** W5 is a key-health wave, not a W8 adapter migration. It wires the existing `gw-adapter` error-analyzer seam into the proxy response classification path while leaving outbound request rewrite on the current `inject_vendor_headers` path. Quota state is persisted on `vendor_keys` as slow-changing entity state, loaded into `VendorKey::quota_state`, filtered before strategy selection, and refreshed by response headers plus a quota-remaining probe scheduler.

**Tech Stack:** Rust 1.88 workspace; Pingora response filters in `gw-proxy`; `gw-adapter` analyzer trait; `gw-health` circuit/cooldown state; `gw-store` PgSQL/Redis entity cache; GitLab W0 gates (`cargo fmt`, `cargo clippy`, `cargo test`, `check-invariants.sh`, `check-spec.sh`).

---

## Scope Guard

This plan assumes W0 gates are already available. W5 execution may change Rust source, config, migrations, specs, and ROADMAP, but this document creation turn must only add or modify `docs/plans/2026-06-08-w5-key-health.md`.

W5 deliberately does **not** implement W8's full `AdapterRegistry::for_vendor` outbound rewrite migration. The F1 conclusion still applies: K-B analyzers land in `gw-adapter` and are consumed by the proxy only. Task-worker does not call adapters.

## File Structure

Implementation touches these areas:

- Modify: `crates/gw-adapter/src/analyzer.rs` — concrete built-in analyzers and reusable body/header extraction helpers.
- Modify: `crates/gw-adapter/src/registry.rs` — explicit registrations plus fallback built-in analyzer selection by canonical vendor id.
- Modify: `crates/gw-proxy/Cargo.toml` — add `gw-adapter`.
- Modify: `crates/gw-proxy/src/gateway.rs` — hold `Arc<AdapterRegistry>`, classify final upstream errors with headers + body head, merge adapter outcome into `ClassifierOutcome`.
- Modify: `crates/gw-gateway/Cargo.toml`, `crates/gw-gateway/src/lib.rs` — construct/pass the adapter registry and quota writer/prober handles.
- Modify: `crates/gw-proxy/src/ctx.rs` — store quota snapshot/update state needed between response header/body/logging phases.
- Modify: `crates/gw-proxy/src/filters/response.rs` — `QuotaSnapshotExtractor` for vendor quota headers.
- Modify: `crates/gw-core/src/entity.rs` — `QuotaState` and `VendorKey::quota_state`.
- Modify: `crates/gw-store/src/loader.rs` — fold `vendor_keys.quota_*` columns into `VendorKeyRow`.
- Create: `crates/gw-store/migrations/0010_vendor_key_quota_state.sql` — persisted `quota_*` columns.
- Create: `crates/gw-store/src/quota_state.rs` — bounded writer queue and PgSQL/Redis/event update path for quota state.
- Modify: `crates/gw-store/src/lib.rs` — export quota-state writer/prober types.
- Modify: `crates/gw-routing/src/candidates.rs` and any test key builders in `gw-routing` / `gw-proxy` — drop exhausted quota candidates.
- Modify: `crates/gw-health/src/store.rs`, `crates/gw-health/src/format_health.rs` — consume `OutcomeKind::Failure { http_status, retry_after_secs }` for graded cooldown.
- Modify: `config/default.toml`, `crates/gw-gateway/src/config.rs` — add a separate quota-remaining probe config, not `[health.quota_probe]`.
- Update after code lands: `docs/spec/03-key-health.md`, `docs/spec/07-vendor-integration.md`, `docs/workflow.md`, `docs/architecture.md`, relevant `docs/go-contracts/parts/0*.md`, and remove W5 rows from `docs/ROADMAP.md`.

## Task W5.1: Implement vendor analyzers in `gw-adapter`

**Files:**
- Modify: `crates/gw-adapter/src/analyzer.rs`
- Modify: `crates/gw-adapter/src/registry.rs`
- Test: `cargo test -p gw-adapter analyzer`

- [ ] **Step 1: Write failing analyzer tests.**

Append tests to `crates/gw-adapter/src/analyzer.rs`:

```rust
#[cfg(test)]
mod vendor_analyzer_tests {
    use super::*;

    struct Headers(Vec<(&'static str, &'static str)>);
    impl HeaderView for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| *v)
        }
    }

    #[test]
    fn codex_429_reads_retry_after_and_marks_rate_limit() {
        let analyzer = KeywordErrorAnalyzer::codex();
        let out = analyzer.analyze(429, br#"{"error":{"message":"rate limit reached"}}"#, &Headers(vec![("Retry-After", "17")]));
        assert_eq!(out.kind, Some(ErrorKind::RateLimit));
        assert_eq!(out.retry_after_secs, Some(17));
        let cooldown = out.cooldown.expect("429 produces cooldown");
        assert_eq!(cooldown.reason_code, 7);
        assert_eq!(cooldown.ttl_secs, 17);
    }

    #[test]
    fn anthropic_disabled_body_marks_account_blocked() {
        let analyzer = KeywordErrorAnalyzer::anthropic();
        let out = analyzer.analyze(403, br#"{"type":"error","error":{"message":"workspace account disabled"}}"#, &Headers(vec![]));
        assert_eq!(out.kind, Some(ErrorKind::Quota));
        let cooldown = out.cooldown.expect("disabled account produces cooldown");
        assert_eq!(cooldown.reason_code, 13);
        assert_eq!(cooldown.ttl_secs, 3600);
    }

    #[test]
    fn gemini_resource_exhausted_body_marks_quota() {
        let analyzer = KeywordErrorAnalyzer::gemini();
        let out = analyzer.analyze(429, br#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"quota exceeded"}}"#, &Headers(vec![]));
        assert_eq!(out.kind, Some(ErrorKind::Quota));
        let cooldown = out.cooldown.expect("quota exhausted produces cooldown");
        assert_eq!(cooldown.reason_code, 7);
        assert_eq!(cooldown.ttl_secs, 60);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail.**

Run:

```bash
cargo test -p gw-adapter analyzer
```

Expected: FAIL with unresolved `KeywordErrorAnalyzer`.

- [ ] **Step 3: Add concrete analyzers.**

In `crates/gw-adapter/src/analyzer.rs`, add this implementation below `DefaultErrorAnalyzer`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinAnalyzerKind {
    Codex,
    Anthropic,
    Gemini,
}

pub struct KeywordErrorAnalyzer {
    kind: BuiltinAnalyzerKind,
}

impl KeywordErrorAnalyzer {
    pub fn codex() -> Self {
        Self { kind: BuiltinAnalyzerKind::Codex }
    }

    pub fn anthropic() -> Self {
        Self { kind: BuiltinAnalyzerKind::Anthropic }
    }

    pub fn gemini() -> Self {
        Self { kind: BuiltinAnalyzerKind::Gemini }
    }
}

fn body_contains_any(body_head: &[u8], needles: &[&str]) -> bool {
    let body = String::from_utf8_lossy(body_head).to_ascii_lowercase();
    needles.iter().any(|needle| body.contains(&needle.to_ascii_lowercase()))
}

fn retry_after_header(headers: &dyn HeaderView) -> Option<u32> {
    headers.get("retry-after").and_then(|s| s.parse::<u32>().ok())
}

fn cooldown(reason_code: u16, ttl_secs: u32, reset_consecutive: bool) -> CooldownDirective {
    CooldownDirective {
        reason_code,
        ttl_secs,
        reset_consecutive,
    }
}

impl UpstreamErrorAnalyzer for KeywordErrorAnalyzer {
    fn analyze(&self, status: u16, body_head: &[u8], headers: &dyn HeaderView) -> ErrorOutcome {
        let retry_after_secs = retry_after_header(headers);
        match self.kind {
            BuiltinAnalyzerKind::Codex => {
                if status == 429 {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::RateLimit),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
            BuiltinAnalyzerKind::Anthropic => {
                if (status == 401 || status == 403)
                    && body_contains_any(body_head, &["disabled", "deactivated", "suspended"])
                {
                    return ErrorOutcome {
                        kind: Some(ErrorKind::Quota),
                        cooldown: Some(cooldown(13, 3600, false)),
                        ..Default::default()
                    };
                }
                if status == 429 {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::RateLimit),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
            BuiltinAnalyzerKind::Gemini => {
                if status == 429
                    && body_contains_any(body_head, &["resource_exhausted", "quota exceeded", "quota"])
                {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::Quota),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
        }
        DefaultErrorAnalyzer.analyze(status, body_head, headers)
    }
}
```

If `retry_after_header` already exists in the file, keep a single helper and reuse it.

- [ ] **Step 4: Register analyzer fallback by canonical vendor id.**

In `crates/gw-adapter/src/registry.rs`, import `KeywordErrorAnalyzer` and add fallback selection after explicit `error_analyzers` lookup:

```rust
use crate::analyzer::{DefaultErrorAnalyzer, KeywordErrorAnalyzer, UpstreamErrorAnalyzer};

fn builtin_error_analyzer_for(vendor: &VendorId) -> Option<Arc<dyn UpstreamErrorAnalyzer>> {
    let id = vendor.as_ref().to_ascii_lowercase();
    if id.contains("codex") || id.contains("openai") {
        return Some(Arc::new(KeywordErrorAnalyzer::codex()));
    }
    if id.contains("anthropic") || id.contains("claude") {
        return Some(Arc::new(KeywordErrorAnalyzer::anthropic()));
    }
    if id.contains("gemini") || id.contains("google") {
        return Some(Arc::new(KeywordErrorAnalyzer::gemini()));
    }
    None
}
```

Change `error_analyzer_for` to:

```rust
pub fn error_analyzer_for(&self, v: &VendorId) -> Arc<dyn UpstreamErrorAnalyzer> {
    self.error_analyzers
        .get(v)
        .map(|e| e.value().clone())
        .or_else(|| builtin_error_analyzer_for(v))
        .unwrap_or_else(|| (self.default_error_analyzer.clone()) as Arc<dyn UpstreamErrorAnalyzer>)
}
```

- [ ] **Step 5: Run tests.**

Run:

```bash
cargo test -p gw-adapter analyzer
```

Expected: PASS.

## Task W5.2: Wire analyzers into the production response classification path

**Files:**
- Modify: `crates/gw-proxy/Cargo.toml`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-gateway/Cargo.toml`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `cargo test -p gw-proxy adapter_error_analyzer`

- [ ] **Step 1: Write failing merge tests in `gateway.rs`.**

Add a focused test module near the existing `gateway.rs` unit tests:

```rust
#[cfg(test)]
mod adapter_error_analyzer_tests {
    use super::*;
    use bytes::BytesMut;
    use gw_adapter::{ErrorKind, ErrorOutcome};
    use gw_core::error::{ClassifierOutcome, CooldownDirective, ErrorClass, Retryability};
    use gw_core::ratelimit::RateLimitKind;

    #[test]
    fn analyzer_cooldown_overrides_default_classifier_cooldown() {
        let default = ClassifierOutcome {
            class: ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown),
            retryable: Retryability::CrossKey,
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 60,
                reset_consecutive: false,
            }),
            ..Default::default()
        };
        let analyzer = ErrorOutcome {
            kind: Some(ErrorKind::Quota),
            retry_after_secs: Some(17),
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 17,
                reset_consecutive: false,
            }),
            ..Default::default()
        };
        let merged = merge_adapter_error_outcome(default, analyzer);
        assert_eq!(merged.cooldown.as_ref().unwrap().ttl_secs, 17);
    }

    #[test]
    fn body_head_is_capped_to_8k() {
        let mut body = BytesMut::from(&vec![b'a'; 9000][..]);
        let head = adapter_body_head(&mut body);
        assert_eq!(head.len(), 8192);
    }
}
```

- [ ] **Step 2: Run tests to verify failure.**

Run:

```bash
cargo test -p gw-proxy adapter_error_analyzer
```

Expected: FAIL with unresolved `gw_adapter`, `merge_adapter_error_outcome`, and `adapter_body_head`.

- [ ] **Step 3: Add the crate dependency.**

In `crates/gw-proxy/Cargo.toml`, add:

```toml
gw-adapter = { path = "../gw-adapter" }
```

- [ ] **Step 4: Add registry wiring to `LlmGateway`.**

In `crates/gw-proxy/src/gateway.rs`, add a field:

```rust
pub adapter_registry: Arc<gw_adapter::AdapterRegistry>,
```

Add a constructor parameter near `classifier`:

```rust
adapter_registry: Arc<gw_adapter::AdapterRegistry>,
```

Store it in `Self { ... }`.

In `crates/gw-gateway/Cargo.toml`, add:

```toml
gw-adapter = { path = "../gw-adapter" }
```

In `crates/gw-gateway/src/lib.rs`, add `adapter_registry: Arc<gw_adapter::AdapterRegistry>` to `Wired`, create it in bootstrap:

```rust
let adapter_registry = Arc::new(gw_adapter::AdapterRegistry::new());
```

Pass it into `LlmGateway::new`.

- [ ] **Step 5: Add classification helpers in `gateway.rs`.**

Add these helpers near `remaining_failover_candidates`:

```rust
fn adapter_body_head(body: &bytes::BytesMut) -> &[u8] {
    let cap = body.len().min(8192);
    &body[..cap]
}

fn merge_adapter_error_outcome(
    mut base: gw_core::error::ClassifierOutcome,
    adapter: gw_adapter::ErrorOutcome,
) -> gw_core::error::ClassifierOutcome {
    if let Some(cooldown) = adapter.cooldown {
        base.cooldown = Some(cooldown);
    }
    if let Some(oauth_signal) = adapter.oauth_signal {
        base.oauth_signal = Some(oauth_signal);
    }
    if adapter.retry_after_secs.is_some() {
        base.rate_limit_hint = Some(gw_core::error::RateLimitHint {
            kind: gw_core::ratelimit::RateLimitKind::Unknown,
            retry_after_secs: adapter.retry_after_secs,
        });
    }
    match adapter.kind {
        Some(gw_adapter::ErrorKind::RateLimit) => {
            base.class = gw_core::error::ErrorClass::RateLimitedUpstream(
                gw_core::ratelimit::RateLimitKind::Unknown,
            );
            base.retryable = gw_core::error::Retryability::CrossKey;
        }
        Some(gw_adapter::ErrorKind::Quota) => {
            base.class = gw_core::error::ErrorClass::QuotaDenied;
            base.retryable = gw_core::error::Retryability::CrossKey;
        }
        Some(gw_adapter::ErrorKind::Authentication) => {
            base.class = gw_core::error::ErrorClass::AuthDenied;
        }
        Some(gw_adapter::ErrorKind::UpstreamServer | gw_adapter::ErrorKind::Transient) => {
            base.class = gw_core::error::ErrorClass::UpstreamTransient;
        }
        Some(gw_adapter::ErrorKind::Unknown) | None => {}
    }
    base
}
```

- [ ] **Step 6: Reclassify at EOS with response body head.**

Keep the existing header-only classification in `response_filter` as the preliminary path. Then, in `response_body_filter`, after `let res = filters::response::run(...)` and before `error_passthrough_active` rendering, add a final classification call for terminal upstream errors:

```rust
if end_of_stream {
    self.classify_final_upstream_error(ctx);
}
```

Implement:

```rust
fn classify_final_upstream_error(&self, ctx: &mut ReqCtx) {
    let Some(status) = ctx.upstream_status else {
        return;
    };
    if status < 400 {
        return;
    }
    let Some(meta) = ctx.archive_upstream_response_meta.as_ref() else {
        return;
    };
    let picked = ctx.picked_vendor_key.as_ref();
    let classify_ctx = gw_core::error::ClassifyCtx {
        key_id: picked.map_or("", |k| k.key_id.0.as_str()),
        vendor_id: picked.map_or("", |k| k.vendor_id.0.as_str()),
        api_format: ctx
            .client_codec
            .as_ref()
            .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
        protocol: ctx
            .upstream_codec
            .as_ref()
            .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
        has_oauth: picked.is_some_and(|k| k.oauth.is_some()),
    };
    struct MetaHeaders<'a>(&'a [(String, String)]);
    impl gw_core::error::HeadersAccessor for MetaHeaders<'_> {
        fn get(&self, name: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }
    impl gw_adapter::HeaderView for MetaHeaders<'_> {
        fn get(&self, name: &str) -> Option<&str> {
            <Self as gw_core::error::HeadersAccessor>::get(self, name)
        }
    }

    let headers = MetaHeaders(&meta.headers);
    let base = self
        .classifier
        .classify_upstream(&classify_ctx, status, &headers, None);
    let adapter_outcome = picked
        .map(|key| {
            self.adapter_registry.error_analyzer_for(&key.vendor_id).analyze(
                status,
                adapter_body_head(&ctx.archive_upstream_response_body),
                &headers,
            )
        })
        .unwrap_or_default();
    ctx.error_outcome = Some(merge_adapter_error_outcome(base, adapter_outcome));
}
```

This makes body-aware K-B classification happen before logging fanout, so `KeyHealthListener` sees the analyzer-derived cooldown.

- [ ] **Step 7: Run focused tests.**

Run:

```bash
cargo test -p gw-proxy adapter_error_analyzer
cargo test -p gw-adapter analyzer
```

Expected: PASS.

## Task W5.3: Add persisted `QuotaState` and route around exhausted keys

**Files:**
- Modify: `crates/gw-core/src/entity.rs`
- Modify: `crates/gw-store/src/loader.rs`
- Create: `crates/gw-store/migrations/0010_vendor_key_quota_state.sql`
- Modify: `crates/gw-routing/src/candidates.rs`
- Update test key builders wherever `VendorKey` literals fail to compile
- Test: `cargo test -p gw-routing candidates`

- [ ] **Step 1: Write failing routing test.**

In `crates/gw-routing/src/candidates.rs`, add a unit test around the quota predicate helper you will add:

```rust
#[cfg(test)]
mod quota_state_tests {
    use gw_core::entity::{QuotaAvailability, QuotaState};

    #[test]
    fn exhausted_quota_without_expired_reset_is_not_routable() {
        let state = QuotaState {
            availability: QuotaAvailability::Exhausted,
            remaining: Some(0),
            limit: Some(100),
            reset_at_unix_s: Some(4_102_444_800),
            observed_at_unix_s: Some(1_717_200_000),
            source: Some("anthropic_header".to_string()),
        };
        assert!(!state.is_routable_at_unix_s(1_717_200_100));
    }

    #[test]
    fn exhausted_quota_after_reset_is_routable_for_probe_recovery() {
        let state = QuotaState {
            availability: QuotaAvailability::Exhausted,
            remaining: Some(0),
            limit: Some(100),
            reset_at_unix_s: Some(1_717_200_000),
            observed_at_unix_s: Some(1_717_199_900),
            source: Some("anthropic_header".to_string()),
        };
        assert!(state.is_routable_at_unix_s(1_717_200_001));
    }
}
```

- [ ] **Step 2: Run tests to verify failure.**

Run:

```bash
cargo test -p gw-routing quota_state
```

Expected: FAIL with unresolved `QuotaState`.

- [ ] **Step 3: Add `QuotaState` to `gw-core`.**

In `crates/gw-core/src/entity.rs`, add:

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAvailability {
    #[default]
    Unknown,
    Available,
    Exhausted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct QuotaState {
    #[serde(default)]
    pub availability: QuotaAvailability,
    #[serde(default)]
    pub remaining: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub reset_at_unix_s: Option<i64>,
    #[serde(default)]
    pub observed_at_unix_s: Option<i64>,
    #[serde(default)]
    pub source: Option<String>,
}

impl QuotaState {
    pub fn is_routable_at_unix_s(&self, now_unix_s: i64) -> bool {
        match self.availability {
            QuotaAvailability::Unknown | QuotaAvailability::Available => true,
            QuotaAvailability::Exhausted => self
                .reset_at_unix_s
                .is_some_and(|reset| reset <= now_unix_s),
        }
    }
}
```

Add to `VendorKey`:

```rust
pub quota_state: QuotaState,
```

- [ ] **Step 4: Add DB columns.**

Create `crates/gw-store/migrations/0010_vendor_key_quota_state.sql`:

```sql
-- 0010_vendor_key_quota_state.sql — K-D quota-remaining state for routing.
-- Slow-changing vendor quota state is stored as structured columns so Rust can
-- update it without rewriting encrypted payload fields.

ALTER TABLE vendor_keys
    ADD COLUMN IF NOT EXISTS quota_availability TEXT NOT NULL DEFAULT 'unknown',
    ADD COLUMN IF NOT EXISTS quota_remaining BIGINT NULL,
    ADD COLUMN IF NOT EXISTS quota_limit BIGINT NULL,
    ADD COLUMN IF NOT EXISTS quota_reset_at TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS quota_observed_at TIMESTAMPTZ NULL,
    ADD CONSTRAINT vendor_keys_quota_availability_chk
        CHECK (quota_availability IN ('unknown', 'available', 'exhausted'));
```

- [ ] **Step 5: Fold columns into the loader.**

In `VendorKeyRow`, add:

```rust
#[serde(default)]
quota_state: gw_core::entity::QuotaState,
```

Replace `VENDOR_KEY_SQL` with a nested `jsonb_set` that injects `quota_state`:

```rust
const VENDOR_KEY_SQL: &str = "\
SELECT jsonb_set(\
    jsonb_set(payload, '{identity_snapshot}', COALESCE(identity_snapshot, 'null'::jsonb), true),\
    '{quota_state}',\
    jsonb_build_object(\
        'availability', quota_availability,\
        'remaining', quota_remaining,\
        'limit', quota_limit,\
        'reset_at_unix_s', CASE WHEN quota_reset_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM quota_reset_at)::BIGINT END,\
        'observed_at_unix_s', CASE WHEN quota_observed_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM quota_observed_at)::BIGINT END,\
        'source', NULL\
    ),\
    true\
) AS payload, version FROM vendor_keys WHERE id = $1 AND deleted_at IS NULL";
```

Set `quota_state: row.quota_state` when constructing `VendorKey`.

- [ ] **Step 6: Filter exhausted candidates before strategy selection.**

In `crates/gw-routing/src/candidates.rs`, add:

```rust
fn now_unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

In `get_candidate_keys`, after the `status == Active` check:

```rust
if !key.quota_state.is_routable_at_unix_s(now_unix_s()) {
    continue;
}
```

When cloning `VendorKey` into `out`, copy:

```rust
quota_state: key.quota_state.clone(),
```

Update every `VendorKey { ... }` literal in tests to include:

```rust
quota_state: gw_core::entity::QuotaState::default(),
```

- [ ] **Step 7: Run tests.**

Run:

```bash
cargo test -p gw-routing quota_state
cargo test -p gw-store vendor_key_row
```

Expected: PASS.

## Task W5.4: Extract quota snapshots from response headers

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Test: `cargo test -p gw-proxy quota_snapshot`

- [ ] **Step 1: Write failing extractor tests.**

In `crates/gw-proxy/src/filters/response.rs`, add:

```rust
#[cfg(test)]
mod quota_snapshot_tests {
    use super::*;

    struct Headers(Vec<(&'static str, &'static str)>);
    impl QuotaHeaderView for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| *v)
        }
    }

    #[test]
    fn anthropic_remaining_headers_extract_available_state() {
        let snap = QuotaSnapshotExtractor::extract(&Headers(vec![
            ("anthropic-ratelimit-requests-remaining", "42"),
            ("anthropic-ratelimit-requests-limit", "100"),
        ]))
        .expect("snapshot");
        assert_eq!(snap.state.availability, gw_core::entity::QuotaAvailability::Available);
        assert_eq!(snap.state.remaining, Some(42));
        assert_eq!(snap.state.limit, Some(100));
    }

    #[test]
    fn zero_remaining_extracts_exhausted_state() {
        let snap = QuotaSnapshotExtractor::extract(&Headers(vec![
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-limit-requests", "500"),
            ("retry-after", "30"),
        ]))
        .expect("snapshot");
        assert_eq!(snap.state.availability, gw_core::entity::QuotaAvailability::Exhausted);
        assert_eq!(snap.retry_after_secs, Some(30));
    }
}
```

- [ ] **Step 2: Run tests to verify failure.**

Run:

```bash
cargo test -p gw-proxy quota_snapshot
```

Expected: FAIL with unresolved `QuotaSnapshotExtractor`.

- [ ] **Step 3: Add snapshot types and extractor.**

In `crates/gw-proxy/src/filters/response.rs`, add:

```rust
#[derive(Clone, Debug)]
pub struct QuotaSnapshot {
    pub state: gw_core::entity::QuotaState,
    pub retry_after_secs: Option<u32>,
}

pub trait QuotaHeaderView {
    fn get(&self, name: &str) -> Option<&str>;
}

pub struct QuotaSnapshotExtractor;

impl QuotaSnapshotExtractor {
    pub fn extract(headers: &dyn QuotaHeaderView) -> Option<QuotaSnapshot> {
        let remaining = first_u64(
            headers,
            &[
                "anthropic-ratelimit-requests-remaining",
                "x-ratelimit-remaining-requests",
                "x-rate-limit-remaining",
                "ratelimit-remaining",
            ],
        )?;
        let limit = first_u64(
            headers,
            &[
                "anthropic-ratelimit-requests-limit",
                "x-ratelimit-limit-requests",
                "x-ratelimit-limit",
                "ratelimit-limit",
            ],
        );
        let retry_after_secs = headers.get("retry-after").and_then(|s| s.parse::<u32>().ok());
        let now = unix_s();
        let reset_at_unix_s = retry_after_secs.map(|s| now + i64::from(s));
        let availability = if remaining == 0 {
            gw_core::entity::QuotaAvailability::Exhausted
        } else {
            gw_core::entity::QuotaAvailability::Available
        };
        Some(QuotaSnapshot {
            state: gw_core::entity::QuotaState {
                availability,
                remaining: Some(remaining),
                limit,
                reset_at_unix_s,
                observed_at_unix_s: Some(now),
                source: Some("response_header".to_string()),
            },
            retry_after_secs,
        })
    }
}

fn first_u64(headers: &dyn QuotaHeaderView, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| headers.get(name).and_then(|s| s.parse::<u64>().ok()))
}

fn unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 4: Store snapshots on the request context.**

In `crates/gw-proxy/src/ctx.rs`, add:

```rust
pub quota_snapshot: Option<crate::filters::response::QuotaSnapshot>,
```

Initialize it to `None` in `Default`.

In `gateway.rs::response_filter`, after `archive_upstream_response_meta` is populated, adapt meta headers to `QuotaHeaderView` and set:

```rust
if let Some(meta) = ctx.archive_upstream_response_meta.as_ref() {
    struct QuotaHeaders<'a>(&'a [(String, String)]);
    impl crate::filters::response::QuotaHeaderView for QuotaHeaders<'_> {
        fn get(&self, name: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }
    ctx.quota_snapshot = crate::filters::response::QuotaSnapshotExtractor::extract(
        &QuotaHeaders(&meta.headers),
    );
}
```

- [ ] **Step 5: Run tests.**

Run:

```bash
cargo test -p gw-proxy quota_snapshot
```

Expected: PASS.

## Task W5.5: Persist quota snapshots and add the quota-remaining prober

**Files:**
- Create: `crates/gw-store/src/quota_state.rs`
- Modify: `crates/gw-store/src/lib.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Modify: `config/default.toml`
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Test: `cargo test -p gw-store quota_state`

- [ ] **Step 1: Write failing writer test.**

Create test module inside `crates/gw-store/src/quota_state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::{QuotaAvailability, QuotaState};

    #[test]
    fn update_sql_sets_columns_and_bumps_version() {
        let sql = update_sql();
        assert!(sql.contains("quota_availability = $2"));
        assert!(sql.contains("version = version + 1"));
        assert!(sql.contains("jsonb_set"));
    }

    #[test]
    fn exhausted_state_is_serializable_for_cache_writeback() {
        let state = QuotaState {
            availability: QuotaAvailability::Exhausted,
            remaining: Some(0),
            limit: Some(100),
            reset_at_unix_s: Some(1_717_200_030),
            observed_at_unix_s: Some(1_717_200_000),
            source: Some("response_header".to_string()),
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["availability"], "exhausted");
        assert_eq!(value["remaining"], 0);
    }
}
```

- [ ] **Step 2: Run tests to verify failure.**

Run:

```bash
cargo test -p gw-store quota_state
```

Expected: FAIL because the module is not present/exported.

- [ ] **Step 3: Implement quota-state writer queue.**

Create `crates/gw-store/src/quota_state.rs`:

```rust
use crate::redis_client::RedisCtx;
use fred::interfaces::{HashesInterface, StreamsInterface};
use gw_core::entity::{QuotaAvailability, QuotaState};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct QuotaStateWriter {
    pg: PgPool,
    redis: RedisCtx,
}

impl QuotaStateWriter {
    pub fn new(pg: PgPool, redis: RedisCtx) -> Self {
        Self { pg, redis }
    }

    pub async fn write(&self, key_id: &str, state: &QuotaState) -> anyhow::Result<()> {
        sqlx::query(update_sql())
            .bind(key_id)
            .bind(match state.availability {
                QuotaAvailability::Unknown => "unknown",
                QuotaAvailability::Available => "available",
                QuotaAvailability::Exhausted => "exhausted",
            })
            .bind(state.remaining.map(|v| v as i64))
            .bind(state.limit.map(|v| v as i64))
            .bind(state.reset_at_unix_s)
            .bind(state.observed_at_unix_s)
            .execute(&self.pg)
            .await?;

        let client = self.redis.pool.next();
        let redis_key = format!("entity:vendor_key:{key_id}");
        let _: Result<(), _> = client.hset(&redis_key, ("quota_state", serde_json::to_string(state)?)).await;
        let _: Result<(), _> = client
            .xadd::<(), _, _, _, _>(
                "events:entity",
                false,
                None::<()>,
                "*",
                vec![
                    ("ns", "vendor_key".to_string()),
                    ("id", key_id.to_string()),
                    ("op", "quota_state".to_string()),
                ],
            )
            .await;
        Ok(())
    }
}

pub fn update_sql() -> &'static str {
    "UPDATE vendor_keys \
     SET quota_availability = $2, \
         quota_remaining = $3, \
         quota_limit = $4, \
         quota_reset_at = CASE WHEN $5::BIGINT IS NULL THEN NULL ELSE to_timestamp($5::BIGINT) END, \
         quota_observed_at = CASE WHEN $6::BIGINT IS NULL THEN NULL ELSE to_timestamp($6::BIGINT) END, \
         payload = jsonb_set(payload, '{quota_state}', jsonb_build_object(\
             'availability', $2::TEXT, \
             'remaining', $3::BIGINT, \
             'limit', $4::BIGINT, \
             'reset_at_unix_s', $5::BIGINT, \
             'observed_at_unix_s', $6::BIGINT, \
             'source', 'quota_state_writer'\
         ), true), \
         version = version + 1, \
         updated_at = NOW() \
     WHERE id = $1 AND deleted_at IS NULL"
}

#[derive(Clone)]
pub struct QuotaStateUpdateQueue {
    writer: Arc<QuotaStateWriter>,
    max_pending: usize,
    pending: Arc<dashmap::DashSet<String>>,
}

impl QuotaStateUpdateQueue {
    pub fn new(writer: Arc<QuotaStateWriter>, max_pending: usize) -> Self {
        Self {
            writer,
            max_pending,
            pending: Arc::new(dashmap::DashSet::new()),
        }
    }

    pub fn schedule(&self, key_id: String, state: QuotaState) {
        if self.pending.len() >= self.max_pending {
            metrics::counter!("gw_quota_state_update_total", "result" => "dropped").increment(1);
            return;
        }
        if !self.pending.insert(key_id.clone()) {
            metrics::counter!("gw_quota_state_update_total", "result" => "deduped").increment(1);
            return;
        }
        let writer = Arc::clone(&self.writer);
        let pending = Arc::clone(&self.pending);
        tokio::spawn(async move {
            let result = writer.write(&key_id, &state).await;
            match result {
                Ok(()) => metrics::counter!("gw_quota_state_update_total", "result" => "ok").increment(1),
                Err(e) => {
                    tracing::warn!(target: "gw_store::quota_state", key_id = %key_id, error = %e, "quota state write failed");
                    metrics::counter!("gw_quota_state_update_total", "result" => "failed").increment(1);
                }
            }
            pending.remove(&key_id);
        });
    }
}
```

Export it in `crates/gw-store/src/lib.rs`:

```rust
pub mod quota_state;
```

- [ ] **Step 4: Wire response snapshots to the queue.**

Add `quota_state_updates: Option<Arc<gw_store::quota_state::QuotaStateUpdateQueue>>` to `LlmGateway`, `Wired`, and constructor args.

In `filters/logging.rs`, after `health.record(...)`, schedule the quota snapshot:

```rust
if let (Some(queue), Some(pk), Some(snapshot)) = (
    quota_state_updates.as_ref(),
    ctx.picked_vendor_key.as_ref(),
    ctx.quota_snapshot.as_ref(),
) {
    queue.schedule(pk.key_id.0.clone(), snapshot.state.clone());
}
```

Thread the queue into `filters::logging::run` from `gateway.rs`. If changing the function signature, update only the call sites in `gateway.rs`.

- [ ] **Step 5: Add the quota-remaining prober config.**

In `config/default.toml`, add a new section separate from `[health.quota_probe]`:

```toml
[quota_state_probe]
enabled = false
interval_secs = 300
probe_endpoint = "/v1/models"
max_pending = 1000
```

In `crates/gw-gateway/src/config.rs`, add:

```rust
#[derive(Clone, Debug, serde::Deserialize)]
pub struct QuotaStateProbeCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_quota_state_probe_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_quota_state_probe_endpoint")]
    pub probe_endpoint: String,
    #[serde(default = "default_quota_state_probe_max_pending")]
    pub max_pending: usize,
}
```

Use defaults `300`, `"/v1/models"`, and `1000`, and validate `interval_secs > 0` when enabled.

- [ ] **Step 6: Add a prober scheduler.**

In `crates/gw-store/src/quota_state.rs`, add `QuotaStateProbeRunner` that iterates cached `EntityStore::vendor_key` entries and probes only keys whose `quota_state.availability == Exhausted` and `reset_at_unix_s <= now`. Use the configured endpoint and `reqwest::Client`. A 2xx response with quota headers schedules an updated `QuotaState`; non-2xx leaves the exhausted state untouched.

Keep this separate from `crates/gw-health/src/quota_probe.rs`; that file is cooldown-recovery only and must not be reused for K-D.

- [ ] **Step 7: Run focused tests.**

Run:

```bash
cargo test -p gw-store quota_state
cargo test -p gw-proxy quota_snapshot
```

Expected: PASS.

## Task W5.6: Make `record()` consume failure payload for graded cooldown

**Files:**
- Modify: `crates/gw-health/src/store.rs`
- Modify: `crates/gw-health/src/format_health.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Test: `cargo test -p gw-health record_429_failure_payload`
- Test: `cargo test -p gw-health record_5xx_payload`

- [ ] **Step 1: Write failing health-store tests.**

Add to `crates/gw-health/src/store.rs` tests:

```rust
#[test]
fn record_429_failure_payload_applies_retry_after_cooldown() {
    let store = KeyHealthStore::default();
    let key = KeyId::from("vk-429");
    store.record(
        &key,
        gw_core::protocol::ProtocolKind::OpenAi,
        OutcomeKind::Failure {
            http_status: 429,
            retry_after_secs: Some(23),
        },
    );
    let snap = store.peek(&key, gw_core::protocol::ProtocolKind::OpenAi);
    assert!(snap.in_cooldown);
    assert_eq!(snap.cooldown_reason, CooldownReason::Status429RateLimit);
}

#[test]
fn record_5xx_payload_uses_transient_cooldown() {
    let store = KeyHealthStore::default();
    let key = KeyId::from("vk-500");
    store.record(
        &key,
        gw_core::protocol::ProtocolKind::OpenAi,
        OutcomeKind::Failure {
            http_status: 500,
            retry_after_secs: None,
        },
    );
    let snap = store.peek(&key, gw_core::protocol::ProtocolKind::OpenAi);
    assert!(snap.in_cooldown);
    assert_eq!(snap.cooldown_reason, CooldownReason::StatusTransient5xx);
}
```

- [ ] **Step 2: Run tests to verify failure.**

Run:

```bash
cargo test -p gw-health record_429_failure_payload
cargo test -p gw-health record_5xx_payload
```

Expected: FAIL because `record()` currently discards `OutcomeKind::Failure`.

- [ ] **Step 3: Add failure-payload cooldown mapping.**

In `crates/gw-health/src/store.rs`, replace `record` with:

```rust
pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind) {
    let health = self.get_or_create(key, fmt);
    let ok = matches!(outcome, OutcomeKind::Success);
    health.record(ok);
    if let OutcomeKind::Failure {
        http_status,
        retry_after_secs,
    } = outcome
    {
        if let Some(directive) = failure_payload_directive(http_status, retry_after_secs) {
            health.apply_cooldown(directive);
        }
    }
}

fn failure_payload_directive(
    http_status: u16,
    retry_after_secs: Option<u32>,
) -> Option<CooldownDirective> {
    match http_status {
        429 => Some(CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: retry_after_secs.unwrap_or(60),
            reset_consecutive: false,
        }),
        500..=599 => Some(CooldownDirective {
            reason: if http_status == 529 {
                CooldownReason::Status529Overloaded
            } else {
                CooldownReason::StatusTransient5xx
            },
            ttl_secs: retry_after_secs.unwrap_or(30),
            reset_consecutive: false,
        }),
        _ => None,
    }
}
```

This consumes the payload without replacing the classifier listener path. If a classifier already applied a longer cooldown, `FormatHealth::apply_cooldown` keeps the max deadline.

- [ ] **Step 4: Pass Retry-After into `OutcomeKind::Failure`.**

In `crates/gw-proxy/src/filters/logging.rs`, when constructing `OutcomeKind::Failure`, use `ctx.error_outcome.rate_limit_hint.retry_after_secs`:

```rust
let retry_after_secs = ctx
    .error_outcome
    .as_ref()
    .and_then(|outcome| outcome.rate_limit_hint.as_ref())
    .and_then(|hint| hint.retry_after_secs);

OutcomeKind::Failure {
    http_status: ctx.upstream_status.unwrap_or(0),
    retry_after_secs,
}
```

- [ ] **Step 5: Run focused tests.**

Run:

```bash
cargo test -p gw-health record_429_failure_payload
cargo test -p gw-health record_5xx_payload
cargo test -p gw-proxy adapter_error_analyzer
```

Expected: PASS.

## Task W5.7: Update as-built docs and ROADMAP after implementation

**Files:**
- Modify: `docs/spec/03-key-health.md`
- Modify: `docs/spec/07-vendor-integration.md`
- Modify: `docs/workflow.md`
- Modify: `docs/architecture.md`
- Modify: `docs/go-contracts/main.md`
- Modify: `docs/go-contracts/parts/01-go-as-producer.md`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/ROADMAP.md`
- Test: `bash scripts/check-spec.sh docs/spec/03-key-health.md`

- [ ] **Step 1: Update `docs/spec/03-key-health.md`.**

Move K-B, K-D, and `record()` payload from forward pointers to built behavior:

```markdown
- **K-B vendor-specific error analyzers are live through `gw-adapter`.** The proxy keeps the current outbound rewrite path, but response-side error classification calls `AdapterRegistry::error_analyzer_for(vendor_id)` at end-of-stream with upstream headers and an 8 KiB body head. The merged `ClassifierOutcome` feeds `KeyHealthListener`, `OAuthListener`, metrics, and passthrough rendering.
- **K-D quota-remaining state is persisted on `vendor_keys`.** `QuotaState` is loaded into `VendorKey::quota_state`; routing drops exhausted keys until their reset deadline passes. Response headers update `vendor_keys.quota_*` through `QuotaStateUpdateQueue`, and `QuotaStateProbeRunner` probes reset-eligible exhausted keys. This is distinct from `gw-health/src/quota_probe.rs`, which remains cooldown-recovery only.
- **`record()` consumes failure payload.** `OutcomeKind::Failure { http_status, retry_after_secs }` applies graded 429/5xx cooldowns in addition to the sliding-window circuit sample. Explicit classifier cooldowns still win through max-deadline semantics.
```

- [ ] **Step 2: Update vendor, workflow, architecture, and Go contracts.**

Record:

```markdown
- `gw-adapter::UpstreamErrorAnalyzer` is now a production response-classification surface consumed by `gw-proxy`; task-worker remains non-consumer.
- `vendor_keys` has structured `quota_availability`, `quota_remaining`, `quota_limit`, `quota_reset_at`, and `quota_observed_at` columns; Rust writes them and emits `events:entity ns=vendor_key op=quota_state`.
- `config/default.toml` has `[quota_state_probe]`, separate from `[health.quota_probe]`.
```

In Go contracts, state that Go may read the columns for admin/reporting but Rust is the runtime writer for quota snapshots.

- [ ] **Step 3: Remove completed ROADMAP entries.**

In `docs/ROADMAP.md`, remove these from `## 03 key-health`:

```markdown
### K-B vendor-specific error analyzers — `capability-gap`
### K-D quota-remaining probe + `QuotaState` — `capability-gap`
### `record()` failure payload — `doc-divergence`
```

Keep unrelated key-health or K-C OAuth items.

- [ ] **Step 4: Run spec checks.**

Run:

```bash
bash scripts/check-spec.sh docs/spec/03-key-health.md
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
```

Expected: both `ok`.

## Task W5.8: Final verification

**Files:**
- All files modified by Tasks W5.1-W5.7

- [ ] **Step 1: Run focused tests.**

Run:

```bash
cargo test -p gw-adapter analyzer
cargo test -p gw-proxy adapter_error_analyzer
cargo test -p gw-proxy quota_snapshot
cargo test -p gw-routing quota_state
cargo test -p gw-store quota_state
cargo test -p gw-store vendor_key_row
cargo test -p gw-health record_429_failure_payload
cargo test -p gw-health record_5xx_payload
```

Expected: all PASS.

- [ ] **Step 2: Run workspace gates.**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f" || exit 1; done
```

Expected: all PASS.

- [ ] **Step 3: Optional smoke for request-path changes.**

Run when Docker/Postgres/Redis are available:

```bash
make e2e-soak-smoke
```

Expected: soak smoke exits 0 and the report shows no usage-record/accounting drift.

## Self-Review

- Spec coverage: K-B is covered by Tasks W5.1-W5.2; K-D by W5.3-W5.5; `record()` payload by W5.6; docs/ROADMAP by W5.7; verification by W5.8.
- Placeholder scan: no deferred-work marker phrases remain.
- Boundary check: W5 only wires analyzer classification; it does not migrate outbound request rewriting to adapters and does not make task-worker an adapter consumer.
- Risk check: K-D introduces a Rust runtime writer to `vendor_keys.quota_*`; docs/go-contracts must name Rust as writer and Go as reader/admin surface to avoid double-writer ambiguity.
