# Sensitive Filter Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Rust gateway consume the Go-owned global sensitive-filter policy, apply reversible request/response substitutions in the hot path, and mask detailed archive surfaces before `usage:detailed` persistence.

**Architecture:** Add shared policy types in `gw-core`, a compiled regex engine in `gw-runtime`, and a `sensitive_filter_policy` cache/cold-loader in `gw-store`. Thread request-local transit state through `ReqCtx`, apply substitutions around upstream request rendering and response body filtering, and mask all detailed archive surfaces before `DetailedArchive::try_send`. Replace the old raw-archive invariant with a policy-engine-only masking invariant.

**Tech Stack:** Rust 1.88, workspace crates `gw-core`, `gw-runtime`, `gw-store`, `gw-proxy`, `gw-ledger`, Rust `regex`, Moka entity caches, Pingora filters.

## Global Constraints

- Policy scope is global only and keyed as `entity:sensitive_filter_policy:global`.
- Regex engine is Rust `regex`; invalid snapshots are rejected.
- Empty/missing policy is a no-op.
- Invalid newly loaded policy keeps the last good compiled policy where available.
- Reversible maps are request-local memory only and never persisted.
- Archive masking must happen before `usage:detailed` XADD.
- The previous `no_detailed_redaction` invariant must be replaced, not bypassed.
- Use TDD: write the failing test, run it and verify the expected failure, then implement the minimal code.

---

## File Structure

- Modify `crates/gw-core/src/entity.rs`: add sensitive-filter policy wire types.
- Modify `crates/gw-runtime/src/lib.rs`: export new engine.
- Create `crates/gw-runtime/src/sensitive_filter.rs`: compile and apply rules.
- Modify `crates/gw-store/src/loader.rs`: load global policy from Redis/PG.
- Modify `crates/gw-store/src/entity_store.rs`: policy cache/accessors.
- Modify `crates/gw-store/src/stream_consumer.rs`: invalidate `sensitive_filter_policy`.
- Modify `crates/gw-proxy/src/ctx.rs`: request-local transit map/reverse stream buffer.
- Modify `crates/gw-proxy/src/filters/upstream.rs`: apply request substitution after outbound body render.
- Modify `crates/gw-proxy/src/filters/response.rs` or `gateway.rs`: reverse response substitution.
- Modify `crates/gw-proxy/src/gateway.rs`: mask request/response archive surfaces before `try_send`.
- Modify `crates/gw-ledger/src/detailed_archive.rs`: update module docs from raw wire-fidelity wording to filtered archive wording and keep serialization tests passing.
- Modify `scripts/check-invariants.sh`: replace raw no-redaction rule with policy-engine invariant.
- Modify `docs/architecture.md`, `docs/spec/06-billing-ledger.md`, and workflow docs mentioning raw detailed archive.
- Add/extend tests in `gw-core`, `gw-runtime`, `gw-store`, `gw-proxy`, and `gw-ledger`.

## Task 1: Core Wire Types

**Files:**
- Modify: `crates/gw-core/src/entity.rs`

**Interfaces:**
- Produces: `SensitiveFilterPolicy`, `SensitiveFilterGroup`, `SensitiveFilterRule`, `SensitiveFilterMode`, `SensitiveFilterTarget`.
- Consumes later: `gw-runtime` engine and `gw-store` loader.

- [ ] **Step 1: Write failing serde tests**

Add tests near other `entity.rs` tests:

```rust
#[test]
fn sensitive_filter_policy_deserializes_go_shape() {
    let raw = r#"{
      "id":"global",
      "version":12,
      "groups":[{
        "id":"g1",
        "name":"Default",
        "enabled":true,
        "priority":1000,
        "rules":[{
          "id":"r1",
          "name":"AK",
          "enabled":true,
          "priority":1,
          "mode":"archive_mask",
          "pattern":"ak-[A-Za-z0-9_-]+",
          "replacement":"<AK>",
          "targets":["archive","request_body"]
        }]
      }]
    }"#;
    let policy: SensitiveFilterPolicy = serde_json::from_str(raw).unwrap();
    assert_eq!(policy.id, "global");
    assert_eq!(policy.groups[0].rules[0].mode, SensitiveFilterMode::ArchiveMask);
    assert_eq!(policy.groups[0].rules[0].targets[0], SensitiveFilterTarget::Archive);
}

#[test]
fn sensitive_filter_policy_rejects_unknown_target() {
    let raw = r#"{
      "id":"global",
      "version":1,
      "groups":[{
        "id":"g1","name":"Default","enabled":true,"priority":1,
        "rules":[{
          "id":"r1","name":"Bad","enabled":true,"priority":1,
          "mode":"archive_mask","pattern":"x","replacement":"<X>","targets":["bad"]
        }]
      }]
    }"#;
    assert!(serde_json::from_str::<SensitiveFilterPolicy>(raw).is_err());
}
```

- [ ] **Step 2: Run test and verify it fails**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
```

Expected: FAIL because types do not exist.

- [ ] **Step 3: Implement types**

Add to `crates/gw-core/src/entity.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterPolicy {
    pub id: String,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub groups: Vec<SensitiveFilterGroup>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub rules: Vec<SensitiveFilterRule>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterRule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    pub mode: SensitiveFilterMode,
    pub pattern: String,
    pub replacement: String,
    #[serde(default)]
    pub targets: Vec<SensitiveFilterTarget>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveFilterMode {
    TransitReversible,
    ArchiveMask,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveFilterTarget {
    RequestHeader,
    RequestBody,
    ResponseHeader,
    ResponseBody,
    Archive,
}
```

- [ ] **Step 4: Run test and verify it passes**

```bash
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/entity.rs
git commit -m "feat(gw-core): add sensitive filter policy entity"
```

## Task 2: Runtime Regex Engine

**Files:**
- Create: `crates/gw-runtime/src/sensitive_filter.rs`
- Modify: `crates/gw-runtime/src/lib.rs`

**Interfaces:**
- Consumes: `gw_core::entity::SensitiveFilterPolicy`.
- Produces: `CompiledSensitiveFilterPolicy`, `TransitMap`, streaming reverse replacement.

- [ ] **Step 1: Write failing engine tests**

Create `crates/gw-runtime/src/sensitive_filter.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::*;

    fn policy() -> SensitiveFilterPolicy {
        SensitiveFilterPolicy {
            id: "global".to_string(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_string(),
                name: "Default".to_string(),
                enabled: true,
                priority: 1,
                rules: vec![
                    SensitiveFilterRule {
                        id: "mask".to_string(),
                        name: "Mask".to_string(),
                        enabled: true,
                        priority: 1,
                        mode: SensitiveFilterMode::ArchiveMask,
                        pattern: "ak-[A-Za-z0-9_-]+".to_string(),
                        replacement: "<AK>".to_string(),
                        targets: vec![SensitiveFilterTarget::Archive],
                    },
                    SensitiveFilterRule {
                        id: "transit".to_string(),
                        name: "Transit".to_string(),
                        enabled: true,
                        priority: 2,
                        mode: SensitiveFilterMode::TransitReversible,
                        pattern: "sk-[A-Za-z0-9_-]+".to_string(),
                        replacement: "sk-safe".to_string(),
                        targets: vec![SensitiveFilterTarget::RequestBody, SensitiveFilterTarget::ResponseBody],
                    },
                ],
            }],
        }
    }

    #[test]
    fn archive_mask_replaces_enabled_archive_rules() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, b"ak-secret keep");
        assert_eq!(out.as_ref(), b"<AK> keep");
    }

    #[test]
    fn transit_replacement_is_unique_and_reversible() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one sk-two");
        assert_ne!(out.as_ref(), b"sk-one sk-two");
        assert_eq!(map.len(), 2);
        let restored = compiled.reverse_transit_response(&map, out.as_ref(), true);
        assert_eq!(restored.as_ref(), b"sk-one sk-two");
    }

    #[test]
    fn streaming_reverse_handles_split_surrogate() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one");
        let split = out.len() - 2;
        let mut stream = ReverseStream::new(&map);
        let a = stream.push(&out[..split], false);
        let b = stream.push(&out[split..], true);
        let mut joined = Vec::new();
        joined.extend_from_slice(&a);
        joined.extend_from_slice(&b);
        assert_eq!(joined, b"sk-one");
    }
}
```

Expected before implementation: FAIL because public types do not exist.

- [ ] **Step 2: Implement engine**

Implement:

```rust
pub struct CompiledSensitiveFilterPolicy { rules: Vec<CompiledRule> }
pub struct TransitMap { pairs: Vec<(bytes::Bytes, bytes::Bytes)> }
pub struct ReverseStream { map: TransitMap, tail: Vec<u8>, max_surrogate_len: usize }
```

Use Rust `regex::Regex`. Generate surrogates as:

```text
<replacement>-sf-<sequence>
```

where sequence is monotonically increasing within `replace_transit_request`. Store `(surrogate, original)` pairs in `TransitMap`.

- [ ] **Step 3: Export module**

Modify `crates/gw-runtime/src/lib.rs`:

```rust
pub mod sensitive_filter;
pub use sensitive_filter::{CompiledSensitiveFilterPolicy, ReverseStream, TransitMap};
```

- [ ] **Step 4: Run runtime tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-runtime sensitive_filter --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-runtime/src/lib.rs crates/gw-runtime/src/sensitive_filter.rs
git commit -m "feat(gw-runtime): add sensitive filter engine"
```

## Task 3: Entity Store Loader and Invalidation

**Files:**
- Modify: `crates/gw-store/src/loader.rs`
- Modify: `crates/gw-store/src/entity_store.rs`
- Modify: `crates/gw-store/src/stream_consumer.rs`

**Interfaces:**
- Consumes: `SensitiveFilterPolicy`.
- Produces: `EntityStore::sensitive_filter_policy_or_load`.

- [ ] **Step 1: Write failing invalidation test**

Add to `stream_consumer.rs` tests:

```rust
#[tokio::test]
async fn apply_entity_event_invalidates_sensitive_filter_policy() {
    let store = Arc::new(EntityStore::new());
    store.sensitive_filter_policy.insert(
        "global".to_string(),
        Arc::new(Versioned {
            version: 1,
            value: Arc::new(SensitiveFilterPolicy { id: "global".to_string(), version: 1, groups: vec![] }),
        }),
    ).await;
    apply_entity_event(&store, "sensitive_filter_policy", "global").await;
    assert!(store.sensitive_filter_policy.get("global").await.is_none());
}
```

Expected: FAIL because cache field does not exist.

- [ ] **Step 2: Add cache and loader accessor**

Modify `EntityStore`:

```rust
pub sensitive_filter_policy: Cache<String, Arc<Versioned<SensitiveFilterPolicy>>>,
```

Build with `max_capacity(16)` and `time_to_live(Duration::from_secs(300))`.

Add:

```rust
pub async fn sensitive_filter_policy_or_load(&self) -> Option<Arc<Versioned<SensitiveFilterPolicy>>> {
    let id = "global".to_string();
    if let Some(hit) = self.sensitive_filter_policy.get(&id).await { return Some(hit); }
    let loader = self.loader.as_ref()?.clone();
    let deadline = self.cold_load_timeout;
    self.sensitive_filter_policy.try_get_with_by_ref(&id, async move {
        with_cold_load_timeout("sensitive_filter_policy", deadline, async move {
            load_sensitive_filter_policy(&loader, "global").await
        }).await.map(Arc::new).ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
    }).await.ok()
}

pub async fn invalidate_sensitive_filter_policy(&self, id: &str) {
    self.sensitive_filter_policy.invalidate(&id.to_string()).await;
}
```

Add `load_sensitive_filter_policy` to `loader.rs`. Use `load_entity` for Redis L1 and a direct PostgreSQL row query fallback that assembles `SensitiveFilterPolicy` in Rust:

```rust
pub async fn load_sensitive_filter_policy(
    ctx: &LoaderCtx,
    id: &str,
) -> anyhow::Result<Versioned<SensitiveFilterPolicy>> {
    load_entity(ctx, "sensitive_filter_policy", id, SENSITIVE_FILTER_POLICY_SQL, |value| {
        serde_json::from_value(value.clone()).map_err(Into::into)
    }).await
}
```

`SENSITIVE_FILTER_POLICY_SQL` returns one JSONB payload and one version column. It uses nested `jsonb_build_object`/`jsonb_agg` with `ORDER BY` over Go-owned tables `sensitive_filter_groups`, `sensitive_filter_rules`, and `sensitive_filter_policy_meta`. If the row is absent, return an empty policy with version `0` from SQL so a missing policy is a no-op, not a loader error.

- [ ] **Step 3: Add stream consumer arm**

In `apply_entity_event`:

```rust
"sensitive_filter_policy" => {
    store.invalidate_sensitive_filter_policy(id).await;
}
```

- [ ] **Step 4: Run store tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-store sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-store apply_entity_event_invalidates_sensitive_filter_policy --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-store/src/loader.rs crates/gw-store/src/entity_store.rs crates/gw-store/src/stream_consumer.rs
git commit -m "feat(gw-store): load sensitive filter policy"
```

## Task 4: Request-Local Transit State

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`

**Interfaces:**
- Consumes: `CompiledSensitiveFilterPolicy`.
- Produces: request-local `TransitMap` on `ReqCtx`.

- [ ] **Step 1: Write failing upstream filter source/unit test**

Add a focused unit test in `filters/upstream.rs` for a pure helper:

```rust
#[test]
fn apply_sensitive_filter_to_upstream_body_stores_transit_map() {
    let policy = test_sensitive_filter_policy();
    let compiled = gw_runtime::CompiledSensitiveFilterPolicy::compile(&policy).unwrap();
    let mut ctx = ReqCtx::default();
    let out = apply_sensitive_filter_to_upstream_body(&mut ctx, &compiled, bytes::Bytes::from_static(b"sk-secret"));
    assert_ne!(out.as_ref(), b"sk-secret");
    assert!(ctx.sensitive_filter_transit.as_ref().is_some_and(|m| m.len() == 1));
}
```

Expected: FAIL because fields/helper do not exist.

- [ ] **Step 2: Add ReqCtx fields**

Add to `ReqCtx`:

```rust
pub sensitive_filter_transit: Option<gw_runtime::TransitMap>,
pub sensitive_filter_reverse_stream: Option<gw_runtime::ReverseStream>,
```

Initialize both to `None`. Reset both in `reset_response_attempt_state`.

- [ ] **Step 3: Apply substitution after outbound render**

In `filters/upstream::rewrite_request`, after `apply_outbound_adapter` and before `archive_upstream_request`:

```rust
let outbound = if let Some(compiled) = ctx.sensitive_filter_policy.as_ref() {
    apply_sensitive_filter_to_upstream_body(ctx, compiled, outbound)
} else {
    outbound
};
```

If the compiled policy is held outside ctx, pass it from `gateway.rs` into the rewrite function. The request must not call Go or Redis on the hot path for every chunk; use the cached/compiled policy.

- [ ] **Step 4: Run proxy upstream tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-proxy apply_sensitive_filter_to_upstream_body_stores_transit_map --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat(gw-proxy): substitute sensitive request data"
```

## Task 5: Response Reverse Replacement

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/gateway.rs` only if the outer filter must call the helper.

**Interfaces:**
- Consumes: `ReqCtx.sensitive_filter_transit`.
- Produces: downstream response body with surrogates restored to originals.

- [ ] **Step 1: Write failing response tests**

Add tests in `filters/response.rs` for a pure helper:

```rust
#[test]
fn reverse_sensitive_filter_response_restores_non_stream_body() {
    let (body, map) = test_transit_body_and_map();
    let mut ctx = ReqCtx::default();
    ctx.sensitive_filter_transit = Some(map);
    let restored = reverse_sensitive_filter_response(&mut ctx, body, true);
    assert_eq!(restored.as_ref(), b"sk-secret");
}

#[test]
fn reverse_sensitive_filter_response_handles_split_stream_body() {
    let (body, map) = test_transit_body_and_map();
    let split = body.len() - 2;
    let mut ctx = ReqCtx::default();
    ctx.sensitive_filter_transit = Some(map);
    let a = reverse_sensitive_filter_response(&mut ctx, body.slice(..split), false);
    let b = reverse_sensitive_filter_response(&mut ctx, body.slice(split..), true);
    let mut joined = Vec::new();
    joined.extend_from_slice(&a);
    joined.extend_from_slice(&b);
    assert_eq!(joined, b"sk-secret");
}
```

Expected: FAIL because helper does not exist.

- [ ] **Step 2: Implement response reverse helper**

Implement:

```rust
pub(crate) fn reverse_sensitive_filter_response(ctx: &mut ReqCtx, body: Bytes, eos: bool) -> Bytes
```

If `ctx.sensitive_filter_transit` is `None`, return `body`. Otherwise initialize `ctx.sensitive_filter_reverse_stream` from the map and use it for every chunk.

- [ ] **Step 3: Call helper before downstream forwarding**

Call `reverse_sensitive_filter_response` after codec rendering has produced the client-bound bytes and before the body is returned to Pingora. Ensure archive accumulators receive the same downstream-bound bytes that the client sees, then archive masking will run before persistence in Task 6.

- [ ] **Step 4: Run response tests**

```bash
cargo +1.88.0 test -p gw-proxy reverse_sensitive_filter_response --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-proxy): reverse sensitive response surrogates"
```

## Task 6: Archive Masking Before Detailed Archive XADD

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-ledger/src/detailed_archive.rs` module docs and existing serialization tests.

**Interfaces:**
- Consumes: compiled policy.
- Produces: all `CapturedSurface` payloads masked before `try_send`.

- [ ] **Step 1: Write failing archive masking test**

Add a pure helper test in `gateway.rs`:

```rust
#[test]
fn mask_archive_request_surface_masks_headers_and_body() {
    let compiled = test_compiled_archive_policy();
    let headers = vec![("authorization".to_string(), "Bearer ak-secret".to_string())];
    let body = std::sync::Arc::new(bytes::Bytes::from_static(b"{\"key\":\"ak-secret\"}"));
    let (headers, body) = mask_archive_request_surface(&compiled, headers, body);
    assert_eq!(headers[0].1, "Bearer <AK>");
    assert_eq!(body.as_ref().as_ref(), b"{\"key\":\"<AK>\"}");
}
```

Expected: FAIL because helper does not exist.

- [ ] **Step 2: Implement archive masking helpers**

Add helpers:

```rust
fn mask_archive_headers(compiled: &CompiledSensitiveFilterPolicy, headers: Vec<(String, String)>) -> Vec<(String, String)>
fn mask_archive_body(compiled: &CompiledSensitiveFilterPolicy, body: Arc<Bytes>) -> Arc<Bytes>
fn mask_archive_request_surface(...)
fn mask_archive_response_surface(...)
```

Use `SensitiveFilterTarget::Archive` for both headers and body in v1, matching the global archive target semantics.

- [ ] **Step 3: Apply helpers to every `CapturedSurface`**

Before each `self.detailed_archive.try_send(CapturedSurface { ... })` for:

- downstream request around `gateway.rs` request filter;
- upstream request around `upstream_request_filter`;
- upstream response around response emit;
- downstream response around response emit.

Do not mask summary payload unless it includes free-form body/header data. Keep existing loopback header removal.

- [ ] **Step 4: Run archive tests**

```bash
cargo +1.88.0 test -p gw-proxy mask_archive_request_surface --locked
cargo +1.88.0 test -p gw-ledger detailed_archive --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-ledger/src/detailed_archive.rs
git commit -m "feat(gw-proxy): mask detailed archive surfaces"
```

## Task 7: Policy Compilation and Hot Path Access

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/ctx.rs` if compiled policy is stored per request.

**Interfaces:**
- Consumes: `EntityStore::sensitive_filter_policy_or_load`.
- Produces: no per-chunk Redis/PG calls; compiled policy is reused safely.

- [ ] **Step 1: Write failing source/behavior test**

Add source-level regression test in `gateway.rs`:

```rust
#[test]
fn sensitive_filter_policy_loaded_once_before_upstream_rewrite() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gateway.rs")).unwrap();
    assert!(src.contains("sensitive_filter_policy_or_load"));
    assert!(src.contains("CompiledSensitiveFilterPolicy::compile"));
}
```

Expected: FAIL until gateway loads/compiles policy.

- [ ] **Step 2: Load and compile once per request**

Before request body/upstream rewrite needs filtering, load:

```rust
if let Some(policy) = self.store.sensitive_filter_policy_or_load().await {
    match gw_runtime::CompiledSensitiveFilterPolicy::compile(policy.value.as_ref()) {
        Ok(compiled) => ctx.sensitive_filter_policy = Some(std::sync::Arc::new(compiled)),
        Err(err) => { warn + metric; }
    }
}
```

Prefer storing `Arc<CompiledSensitiveFilterPolicy>` in `ReqCtx` for per-request use. A later optimization can cache compiled policies by version; this task keeps correctness first.

- [ ] **Step 3: Run gateway policy tests**

```bash
cargo +1.88.0 test -p gw-proxy sensitive_filter_policy_loaded_once_before_upstream_rewrite --locked
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-proxy): load sensitive filter policy"
```

## Task 8: Update Invariants and Documentation

**Files:**
- Modify: `scripts/check-invariants.sh`
- Modify: `docs/architecture.md`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: docs mentioning raw detailed archive from `rg -n "wire-fidelity|no_detailed_redaction|redaction" docs crates scripts`

**Interfaces:**
- Produces: documented new filtered archive contract.

- [ ] **Step 1: Write failing invariant check**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
./scripts/check-invariants.sh
```

Expected before edits: FAIL if new sensitive-filter code introduces words caught by `no_detailed_redaction`, or PASS for the wrong reason while docs still state raw archive. Record the current output in the task notes.

- [ ] **Step 2: Replace invariant**

Change the block labelled `no_detailed_redaction` to:

```bash
echo "[sensitive_filter_archive_boundary] archive masking must use sensitive filter engine only"
hits=$(grep -RnE 'redact_body|redact_headers|redact_message_content|redacted_preview|REDACT_HEADER_DENYLIST|REDACT_HEADER_ALLOWLIST' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "ad hoc detailed archive redaction helper present; use sensitive_filter engine"
else
  ok "no ad hoc detailed archive redaction helpers"
fi
grep -Rq "sensitive_filter" crates/gw-proxy/src || bad "sensitive filter archive boundary missing in gw-proxy"
```

- [ ] **Step 3: Update docs**

Change wording from “wire-fidelity archive” to “filtered wire archive when sensitive-filter policy is enabled.” Explicitly state:

- request/response bodies can be masked before `usage:detailed`;
- reversible transit maps are not persisted;
- raw archive is only true when no sensitive-filter rules are enabled.

- [ ] **Step 4: Run invariant check**

```bash
./scripts/check-invariants.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-invariants.sh docs/architecture.md docs/spec/06-billing-ledger.md
git commit -m "docs: document sensitive filter archive boundary"
```

## Task 9: Runtime Verification

**Files:**
- Modify only if verification exposes defects in files from prior tasks.

- [ ] **Step 1: Run focused tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-runtime sensitive_filter --locked
cargo +1.88.0 test -p gw-store sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-proxy sensitive_filter --locked
```

Expected: PASS.

- [ ] **Step 2: Run existing archive/response gates**

```bash
cargo +1.88.0 test -p gw-ledger --test dlq --test flusher --test inflight_clear_via_flusher --locked
cargo +1.88.0 test -p gw-proxy --test archive_state --test response_hot_path --locked
```

Expected: PASS.

- [ ] **Step 3: Run workspace checks**

```bash
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 check --workspace --lib --bins --locked
./scripts/check-invariants.sh
```

Expected: PASS.

- [ ] **Step 4: Commit verification fixes**

Run this step only when Steps 1-3 required code changes. Replace the file list with the exact files changed by those fixes:

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-runtime/src/sensitive_filter.rs
git commit -m "fix(gateway): stabilize sensitive filter runtime"
```

## Plan Self-Review

- Spec coverage: covers entity load, regex compile, reversible transit, streaming reverse, archive masking, invariants, and tests.
- Placeholder scan: clean; crate paths and commands are explicit.
- Type consistency: Go snapshot names align with Rust serde names and namespace `sensitive_filter_policy`.
- Scope check: Go/admin implementation is intentionally in the companion Go plan.
