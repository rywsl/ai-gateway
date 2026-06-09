# W5 Outbound Adapter Seam Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `gw-adapter::AdapterRegistry::for_vendor` the proxy outbound rewrite seam while preserving byte-identical direct vendor behavior and the existing impersonate egress path.

**Architecture:** `gw-proxy` remains the production consumer of outbound adapters; task submission and task runtime do not call `gw-adapter`. W5 migrates the existing header/auth rewrite behind the default adapter, exposes an OAuth runtime slot on `AdapterCtx` for W6, and lets impersonate vendors declare the already-selected transport profile without moving TLS fingerprint implementation out of `gw-core::impersonate` / `gw-egress-impersonate`. Heavy Kiro, Vertex, and Codex transforms stay as W7 consumers of this seam.

**Tech Stack:** Rust 1.88 workspace, Pingora request filters, `gw-adapter`, `gw-proxy`, `gw-gateway`, `gw-core::entity::VendorKey`, `gw-core::impersonate`, `gw-codec::wire::WireAnyRequest`, `insta`, repository spec checks.

---

## Scope Boundaries

- Modify source only in the implementation wave, not while reading this plan.
- Do not touch the blocked W2 source worktree.
- Do not add adapter calls to `crates/gw-task-runtime` or `crates/gw-proxy/src/filters/task.rs`.
- Do not implement OAuth token refresh, encrypted token write-back, Redis single-flight, or provider-specific token exchange in W5.
- Do not implement Kiro SigV4/EventStream, Vertex service-account JWT exchange, or Codex request-body transforms in W5.
- Do not replace `gw-core::impersonate` identity/header generation or `gw-egress-impersonate` loopback dispatch.

## File Structure

- Modify `crates/gw-adapter/src/ctx.rs`
  - Add an OAuth runtime cache/refresher slot as an optional trait-object handle.
  - Keep it optional and inert so W5 does not perform refresh.
- Modify `crates/gw-adapter/src/trait_def.rs`
  - Correct comments: proxy consumes outbound adapter; task worker does not.
  - Clarify that `transport_profile` is declaration only and does not own egress impersonation.
- Modify `crates/gw-adapter/src/noop.rs`
  - Preserve existing static API-key and OAuth bearer header rewrite behavior currently in `gw-proxy::filters::upstream::inject_vendor_headers`.
  - Honor `Vendor.auth_template`, strip client auth headers, set `Host`, `traceparent`, and `content-length`.
- Modify `crates/gw-adapter/src/registry.rs`
  - Keep unregistered vendors mapped to the default adapter.
  - Keep response analyzer lookup unchanged.
- Modify `crates/gw-adapter/src/lib.rs`
  - Export the OAuth slot trait/type added in `ctx.rs`.
- Add tests under `crates/gw-adapter/tests/`
  - Direct/default adapter byte snapshot.
  - OAuth credential injection and expiry metrics source lock.
  - Registry fallback still returns the default outbound adapter.
- Modify `crates/gw-proxy/src/ctx.rs`
  - Add optional `adapter_registry: Option<Arc<gw_adapter::AdapterRegistry>>` and `health_store: Option<Arc<gw_health::store::KeyHealthStore>>` handles to `ReqCtx`, or add only the handles needed by the final helper if the worker keeps them as `rewrite_request` parameters.
- Modify `crates/gw-proxy/src/gateway.rs`
  - Pass `self.adapter_registry` and `self.health` into `filters::upstream::rewrite_request`.
  - Continue using `self.adapter_registry` for response-side analyzers.
- Modify `crates/gw-proxy/src/filters/upstream.rs`
  - Replace direct calls to `inject_vendor_headers` with adapter registry dispatch.
  - Keep `inject_egress_headers` after adapter auth/transport rewrite so impersonate identity headers are still produced by the existing implementation.
  - Keep body rendering and `WireAnyRequest` patching separate; W5 may add the hook call but must not add heavy transform implementations.
- Modify `crates/gw-gateway/src/lib.rs`
  - No new config is expected; verify bootstrap already creates `Arc<gw_adapter::AdapterRegistry>`.
- Modify docs after source is green:
  - `docs/spec/07-vendor-integration.md`
  - `docs/ROADMAP.md`

## User Decision Needed

None at planning time. Current API shape can support F1 if W5 treats `NoopAdapter` as the legacy direct rewrite adapter, leaves impersonate transport execution in `gw-core::impersonate` / `gw-egress-impersonate`, and adds the OAuth runtime slot as optional inert state.

If implementation discovers that direct vendors cannot remain byte-identical after moving `inject_vendor_headers` into the default adapter, or that impersonate requires duplicating `gw-core::impersonate` protocol logic inside `gw-adapter`, stop and return a decision packet with:

```text
User Decision Needed: adapter seam boundary conflict
Observed conflict:
Direct byte-identical evidence:
Impersonate existing-path evidence:
Options:
1. Keep legacy inline rewrite for affected vendors and make adapter seam opt-in.
2. Move more rewrite ownership into gw-adapter and accept a larger behavioral migration.
Recommended option:
```

## Task W5.1: Lock Adapter Context And Trait Boundary

**Files:**
- Modify: `crates/gw-adapter/src/ctx.rs`
- Modify: `crates/gw-adapter/src/trait_def.rs`
- Modify: `crates/gw-adapter/src/lib.rs`
- Test: `crates/gw-adapter/src/trait_def.rs`
- Test: `crates/gw-adapter/tests/ctx_surface.rs`

- [ ] **Step 1: Write the failing context-surface test**

Create `crates/gw-adapter/tests/ctx_surface.rs` with this complete test:

```rust
use gw_adapter::AdapterCtx;

#[test]
fn adapter_ctx_exposes_optional_oauth_runtime_slot() {
    fn assert_has_field(ctx: &AdapterCtx<'_>) -> bool {
        ctx.oauth_runtime.is_none()
    }

    let _field_reader: fn(&AdapterCtx<'_>) -> bool = assert_has_field;
}
```

Run:

```bash
cargo test -p gw-adapter adapter_ctx_exposes_optional_oauth_runtime_slot
```

Expected: FAIL with a compile error containing `no field 'oauth_runtime' on type`.

- [ ] **Step 2: Add an inert OAuth runtime slot**

In `crates/gw-adapter/src/ctx.rs`, add the trait and field exactly near the existing `health_store` field:

```rust
use async_trait::async_trait;

#[async_trait]
pub trait OAuthRuntime: Send + Sync {
    async fn get_access_token(
        &self,
        _vendor_key: &VendorKey,
    ) -> Result<Option<secrecy::SecretString>, String> {
        Ok(None)
    }
}
```

Then add this field to `AdapterCtx<'a>`:

```rust
    /// Optional W6 runtime OAuth cache/refresher seam. W5 only carries this
    /// handle so adapters can be wired without double-writing refresh state.
    pub oauth_runtime: Option<&'a Arc<dyn OAuthRuntime>>,
```

In `crates/gw-adapter/src/lib.rs`, update the export line:

```rust
pub use ctx::{AdapterCtx, HeadersAccessor, HeadersMut, OAuthRuntime, UrlAccessor, UrlMut};
```

- [ ] **Step 3: Correct adapter boundary comments**

Replace the top comment in `crates/gw-adapter/src/trait_def.rs` with:

```rust
//! `UpstreamAdapter` v2 - 4 hooks + `transport_profile` declaration.
//!
//! Spec: A-T0 ADR-AT0-1 and F1. Each vendor adapter implements this trait
//! for proxy outbound request rewriting. The task worker does not call
//! adapters; task submission must pre-render wire bytes before enqueue and
//! the worker sends those bytes verbatim.
```

Replace the `transport_profile` doc with:

```rust
    /// Browser-fingerprint declaration. `None` means standard Pingora
    /// transport; `Some(profile)` lets the proxy select its existing
    /// impersonate-capable
    /// egress path. The adapter does not implement TLS fingerprinting itself.
```

- [ ] **Step 4: Run adapter trait tests**

Run:

```bash
cargo test -p gw-adapter trait_surface_tests adapter_ctx_exposes_optional_oauth_runtime_slot
```

Expected: PASS.

- [ ] **Step 5: Commit the boundary/API change**

```bash
git add crates/gw-adapter/src/ctx.rs crates/gw-adapter/src/trait_def.rs crates/gw-adapter/src/lib.rs crates/gw-adapter/tests/ctx_surface.rs
git commit -m "feat(adapter): add outbound seam context slot"
```

## Task W5.2: Move Legacy Header Rewrite Into The Default Adapter

**Files:**
- Modify: `crates/gw-adapter/src/ctx.rs`
- Modify: `crates/gw-adapter/src/noop.rs`
- Test: `crates/gw-adapter/tests/noop_behavior.rs`
- Test: `crates/gw-adapter/tests/registry_lookup.rs`

- [ ] **Step 1: Add request metadata to `AdapterCtx`**

In `crates/gw-adapter/src/ctx.rs`, add these fields after `url_mut`:

```rust
    pub auth_template: &'a str,
    pub upstream_host: Option<&'a str>,
    pub content_len: usize,
```

Run:

```bash
cargo test -p gw-adapter
```

Expected: FAIL in tests or implementations constructing `AdapterCtx` without these fields.

- [ ] **Step 2: Write the default-adapter direct snapshot test**

Add or replace `crates/gw-adapter/tests/noop_behavior.rs` with a complete test module that builds an `AdapterCtx` using in-memory header and URL accessors, calls `NoopAdapter::rewrite_auth`, and snapshots sorted headers.

Use these fixed inputs:

```rust
let vendor_key_id = gw_core::ids::KeyId::from("vk-baseline");
let vendor_id = gw_core::ids::VendorId::from("openai");
let trace_id = "0000000000000000aaaaaaaaaaaaaaaa";
let request_id = "req-baseline-0001";
let auth_template = "Bearer {api_key}";
let upstream_host = Some("api.openai.com");
let content_len = 42usize;
```

The expected rendered output must match the existing direct-mode proxy snapshot except for randomized `traceparent` span id normalized to `SPANID`:

```text
POST /v1/chat/completions
accept: application/json
authorization: Bearer sk-vendor-secret
content-length: 42
content-type: application/json
host: api.openai.com
traceparent: 00-0000000000000000aaaaaaaaaaaaaaaa-SPANID-01
```

Run:

```bash
cargo test -p gw-adapter direct_noop_rewrite_matches_legacy_proxy_snapshot -- --nocapture
```

Expected: FAIL before `NoopAdapter` owns the full legacy rewrite.

- [ ] **Step 3: Preserve template-driven header names**

Move the current `is_header_token` logic from `crates/gw-proxy/src/filters/upstream.rs` into `crates/gw-adapter/src/noop.rs`.

The adapter rewrite must:

```rust
ctx.headers_mut.remove("authorization");
ctx.headers_mut.remove("x-api-key");
ctx.headers_mut.remove("x-api-key-id");

let auth_secret = if let Some(oauth) = ctx.vendor_key.oauth.as_ref() {
    oauth.access_token.expose_secret().to_owned()
} else {
    ctx.vendor_key.api_key.expose_secret().to_owned()
};

let (header_name, value_template) = match ctx.auth_template.split_once(':') {
    Some((name, rest)) if is_header_token(name.trim()) => (name.trim(), rest.trim_start()),
    Some(_) => {
        metrics::counter!("gw_upstream_auth_header_invalid_total").increment(1);
        ("authorization", ctx.auth_template)
    }
    None => ("authorization", ctx.auth_template),
};
let auth_value = value_template.replace("{api_key}", &auth_secret);
ctx.headers_mut.set(header_name, &auth_value);
if let Some(host) = ctx.upstream_host {
    ctx.headers_mut.set("host", host);
}
let span_id = hex::encode(rand::random::<[u8; 8]>());
let traceparent = format!("00-{}-{}-01", ctx.trace_id, span_id);
ctx.headers_mut.set("traceparent", &traceparent);
ctx.headers_mut.set("content-length", &ctx.content_len.to_string());
```

Keep near-expiry and expired OAuth metrics in the default adapter:

```rust
if let Some(oauth) = ctx.vendor_key.oauth.as_ref() {
    let now = time::OffsetDateTime::now_utc();
    if now >= oauth.access_expires_at {
        metrics::counter!(
            "gw_oauth_token_expired_total",
            "vendor_key_id" => ctx.vendor_key.key_id.as_ref().to_string(),
        )
        .increment(1);
    } else if now >= oauth.access_expires_at - time::Duration::seconds(60) {
        metrics::counter!(
            "gw_oauth_token_near_expiry_total",
            "vendor_key_id" => ctx.vendor_key.key_id.as_ref().to_string(),
        )
        .increment(1);
    }
}
```

- [ ] **Step 4: Add OAuth injection coverage**

In `crates/gw-adapter/tests/noop_behavior.rs`, add a test `oauth_noop_rewrite_uses_access_token` with:

```rust
auth_template = "Authorization: Bearer {api_key}";
api_key = "static-api-key";
oauth.access_token = "oauth-access-token";
```

Expected sorted rendered headers include:

```text
authorization: Bearer oauth-access-token
```

Expected sorted rendered headers do not include:

```text
x-api-key:
x-api-key-id:
static-api-key
```

Run:

```bash
cargo test -p gw-adapter direct_noop_rewrite_matches_legacy_proxy_snapshot oauth_noop_rewrite_uses_access_token
```

Expected: PASS.

- [ ] **Step 5: Keep registry fallback behavior green**

Update `crates/gw-adapter/tests/registry_lookup.rs` only if `AdapterCtx` construction helper changes are needed by the test crate. Do not change these assertions:

```rust
assert_eq!(r.for_vendor(&v).name(), "noop");
assert_eq!(r.for_vendor(&v).name(), "marker");
```

Run:

```bash
cargo test -p gw-adapter
```

Expected: PASS.

- [ ] **Step 6: Commit the default adapter migration**

```bash
git add crates/gw-adapter/src/ctx.rs crates/gw-adapter/src/noop.rs crates/gw-adapter/tests/noop_behavior.rs crates/gw-adapter/tests/registry_lookup.rs
git commit -m "feat(adapter): route legacy auth rewrite through noop adapter"
```

## Task W5.3: Wire Proxy Outbound Rewrite Through The Registry

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/ctx.rs` if handles are stored on `ReqCtx`
- Test: `crates/gw-proxy/src/filters/upstream.rs`
- Test: `crates/gw-proxy/tests/filter_chain_order.rs`
- Test: `crates/gw-proxy/tests/cli_filter_chain.rs`

- [ ] **Step 1: Add a failing source tripwire for adapter dispatch**

In `crates/gw-proxy/src/filters/upstream.rs`, add this test module near existing source tripwires:

```rust
#[cfg(test)]
mod adapter_dispatch_source_tests {
    #[test]
    fn rewrite_request_dispatches_outbound_adapter_registry() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/upstream.rs"
        ))
        .expect("read source");
        assert!(
            src.contains(".for_vendor(&picked.vendor_id)")
                || src.contains(".for_vendor(&vendor.value.id)"),
            "rewrite_request must dispatch AdapterRegistry::for_vendor on the outbound path"
        );
        assert!(
            !src.contains("inject_vendor_headers(ctx, upstream_request"),
            "legacy inline header injection must not remain the outbound auth seam"
        );
    }
}
```

Run:

```bash
cargo test -p gw-proxy rewrite_request_dispatches_outbound_adapter_registry
```

Expected: FAIL because `rewrite_request` still calls `inject_vendor_headers` directly.

- [ ] **Step 2: Pass registry and health into `rewrite_request`**

Change the signature in `crates/gw-proxy/src/filters/upstream.rs`:

```rust
pub async fn rewrite_request(
    session: &mut Session,
    upstream_request: &mut RequestHeader,
    ctx: &mut ReqCtx,
    body_rules: &BodyRulesMap,
    adapter_registry: &Arc<gw_adapter::AdapterRegistry>,
    health_store: &Arc<gw_health::store::KeyHealthStore>,
) -> Result<()>
```

Change the caller in `crates/gw-proxy/src/gateway.rs`:

```rust
filters::upstream::rewrite_request(
    session,
    upstream_request,
    ctx,
    &self.body_rules,
    &self.adapter_registry,
    &self.health,
)
.await
```

Run:

```bash
cargo test -p gw-proxy rewrite_request_dispatches_outbound_adapter_registry
```

Expected: still FAIL until dispatch is implemented.

- [ ] **Step 3: Add proxy header and URL accessors**

In `crates/gw-proxy/src/filters/upstream.rs`, add local accessors above `rewrite_request`:

```rust
struct ProxyHeadersAccessor<'a>(&'a mut RequestHeader);

impl gw_adapter::HeadersAccessor for ProxyHeadersAccessor<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    fn set(&mut self, name: &str, value: &str) {
        let _ = self.0.insert_header(name.to_owned(), value.to_owned());
    }

    fn remove(&mut self, name: &str) {
        let _ = self.0.remove_header(name);
    }
}

struct ProxyUrlAccessor {
    scheme: String,
    host: String,
    path: String,
}

impl gw_adapter::UrlAccessor for ProxyUrlAccessor {
    fn scheme(&self) -> &str {
        &self.scheme
    }
    fn host(&self) -> &str {
        &self.host
    }
    fn path(&self) -> &str {
        &self.path
    }
    fn set_scheme(&mut self, scheme: &str) {
        self.scheme = scheme.to_owned();
    }
    fn set_host(&mut self, host: &str) {
        self.host = host.to_owned();
    }
    fn set_path(&mut self, path: &str) {
        self.path = path.to_owned();
    }
}
```

Use `ProxyUrlAccessor` only as a mutable adapter view in W5. If an adapter changes URL fields, apply the changed path/host/scheme back to `RequestHeader` in the same helper; direct/noop must leave it unchanged.

- [ ] **Step 4: Replace `inject_vendor_headers` with adapter dispatch**

Create a helper in `crates/gw-proxy/src/filters/upstream.rs`:

```rust
async fn apply_outbound_adapter(
    ctx: &mut ReqCtx,
    upstream_request: &mut RequestHeader,
    picked: &Arc<gw_core::entity::VendorKey>,
    vendor: &Arc<gw_core::versioned::Versioned<Vendor>>,
    content_len: usize,
    adapter_registry: &Arc<gw_adapter::AdapterRegistry>,
    health_store: &Arc<gw_health::store::KeyHealthStore>,
    client_protocol: gw_core::protocol::ProtocolKind,
    upstream_protocol: gw_core::protocol::ProtocolKind,
) -> Result<()> {
    let model = ctx
        .model
        .clone()
        .unwrap_or_else(|| gw_core::ids::ModelName::from(""));
    let user_id = ctx
        .effective_user_id()
        .cloned()
        .unwrap_or_else(|| gw_core::ids::UserId::from(""));
    let api_format = ctx
        .client_codec
        .as_ref()
        .expect("client codec set")
        .kind();
    let upstream_host = vendor.value.base_urls.first().and_then(|u| u.host_str());
    let mut headers = ProxyHeadersAccessor(upstream_request);
    let mut url = ProxyUrlAccessor {
        scheme: vendor
            .value
            .base_urls
            .first()
            .map(|u| u.scheme().to_owned())
            .unwrap_or_else(|| "https".to_owned()),
        host: upstream_host.unwrap_or_default().to_owned(),
        path: headers.0.uri.path().to_owned(),
    };
    let mut adapter_ctx = gw_adapter::AdapterCtx {
        vendor_id: vendor.value.id.clone(),
        vendor_key_id: picked.key_id.clone(),
        vendor_key: picked.clone(),
        client_protocol,
        upstream_protocol,
        model,
        user_id,
        api_format,
        health_store,
        oauth_runtime: None,
        headers_mut: gw_adapter::HeadersMut { inner: &mut headers },
        url_mut: gw_adapter::UrlMut { inner: &mut url },
        auth_template: vendor.value.auth_template.as_str(),
        upstream_host,
        content_len,
        trace_id: ctx.trace_id.as_ref(),
        request_id: ctx.request_id.as_ref(),
    };
    let adapter = adapter_registry.for_vendor(&picked.vendor_id);
    adapter
        .rewrite_auth(&mut adapter_ctx)
        .await
        .map_err(|_| Error::new(ErrorType::Custom("gw_adapter_rewrite_auth_failed")))?;
    adapter
        .rewrite_transport(&mut adapter_ctx)
        .map_err(|_| Error::new(ErrorType::Custom("gw_adapter_rewrite_transport_failed")))?;
    Ok(())
}
```

Replace both current calls:

```rust
inject_vendor_headers(ctx, upstream_request, &picked, &vendor, outbound.len());
```

with:

```rust
apply_outbound_adapter(
    ctx,
    upstream_request,
    &picked,
    &vendor,
    outbound.len(),
    adapter_registry,
    health_store,
    client_codec.kind(),
    upstream_kind,
)
.await?;
```

For the CLI branch, use the same helper before `inject_egress_headers`.

- [ ] **Step 5: Keep egress impersonate on the existing path**

Do not move this call:

```rust
inject_egress_headers(ctx, upstream_request, &picked).await;
```

It must stay after `apply_outbound_adapter(...)` in both CLI and non-CLI branches. Confirm `pick_peer` still sets:

```rust
ctx.egress_profile = impersonate_profile;
ctx.egress_upstream = Some((host, port));
```

Run:

```bash
rg -n "inject_egress_headers\\(|ctx\\.egress_profile|gw_egress_impersonate" crates/gw-proxy/src/filters/upstream.rs
```

Expected: output still shows existing `ctx.egress_profile` assignment, `inject_egress_headers` definition, and calls from `rewrite_request`.

- [ ] **Step 6: Remove or narrow legacy inline helper**

Remove `inject_vendor_headers` only after `NoopAdapter` owns all behavior and tests are green. If keeping a test-only helper for snapshot comparison, annotate it with `#[cfg(test)]` and ensure the source tripwire does not see production calls.

Run:

```bash
cargo test -p gw-proxy rewrite_request_dispatches_outbound_adapter_registry direct_mode_inject_vendor_headers
```

Expected: PASS if the old direct snapshot was migrated to compare adapter output, or PASS without `direct_mode_inject_vendor_headers` if the test was removed and no test target references it.

- [ ] **Step 7: Run proxy filter-chain tests**

Run:

```bash
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test filter_chain_order
```

Expected: PASS. Failures here indicate the registry dispatch was placed before body render, after archive capture, or after egress header injection in a way that changes filter ordering.

- [ ] **Step 8: Commit the production hook**

```bash
git add crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/ctx.rs
git commit -m "feat(adapter): route outbound requests through adapter registry"
```

## Task W5.4: Lock Direct And Impersonate Behavior

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Test: `crates/gw-adapter/tests/noop_behavior.rs`
- Test: `crates/gw-gateway/tests/e2e_impersonate.rs`

- [ ] **Step 1: Run direct adapter and proxy snapshots**

Run:

```bash
cargo test -p gw-adapter direct_noop_rewrite_matches_legacy_proxy_snapshot -- --nocapture
cargo test -p gw-proxy regression_baseline -- --nocapture
```

Expected: PASS. The normalized direct header snapshot must still render the same `authorization`, `host`, `traceparent`, and `content-length` values as before adapter dispatch.

- [ ] **Step 2: Add an impersonate source-boundary test**

In `crates/gw-proxy/src/filters/upstream.rs`, add to `adapter_dispatch_source_tests`:

```rust
#[test]
fn impersonate_still_uses_existing_egress_path() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/filters/upstream.rs"
    ))
    .expect("read source");
    assert!(
        src.contains("gw_core::impersonate::ensure_identity"),
        "impersonate identity must remain in gw-core::impersonate"
    );
    assert!(
        src.contains("gw_egress_impersonate::EgressBind"),
        "impersonate transport must still dispatch through gw-egress-impersonate"
    );
    assert!(
        src.contains("inject_egress_headers(ctx, upstream_request, &picked).await"),
        "adapter auth rewrite must not replace egress identity/header injection"
    );
}
```

Run:

```bash
cargo test -p gw-proxy impersonate_still_uses_existing_egress_path
```

Expected: PASS.

- [ ] **Step 3: Run egress impersonate integration test lane**

Run the ignored egress test exactly as documented by the test file:

```bash
cargo test -p gw-gateway --test e2e_impersonate -- --ignored --nocapture
```

Expected: PASS when local Postgres/Redis test prerequisites are available. If the environment lacks those services, record the first missing-service error line and continue to the non-ignored verification commands; do not mark impersonate behavior verified from source tripwires alone.

- [ ] **Step 4: Verify no task-worker adapter dependency was introduced**

Run:

```bash
rg -n "gw_adapter|AdapterRegistry|UpstreamAdapter|for_vendor" crates/gw-task crates/gw-task-runtime crates/gw-proxy/src/filters/task.rs
```

Expected: no matches.

- [ ] **Step 5: Commit behavior locks**

```bash
git add crates/gw-proxy/src/filters/upstream.rs crates/gw-adapter/tests/noop_behavior.rs
git commit -m "test(adapter): lock direct and impersonate adapter behavior"
```

## Task W5.5: Update As-Built Docs And ROADMAP

**Files:**
- Modify: `docs/spec/07-vendor-integration.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Update vendor integration as-built text**

In `docs/spec/07-vendor-integration.md`, replace the current "inline, not via an adapter" language with a concise as-built section stating:

```markdown
### Outbound adapter dispatch

Proxy outbound request rewrite now dispatches through `AdapterRegistry::for_vendor` from `crates/gw-proxy/src/filters/upstream.rs::rewrite_request`. Unregistered/direct vendors receive `NoopAdapter`, which preserves the previous static API-key/OAuth bearer header rewrite, `auth_template` header-name behavior, host rewrite, `traceparent`, and `content-length` update semantics.

`gw-adapter` is not a task-worker dependency. Async task submission may pre-render wire bytes, and the task worker sends stored bytes verbatim.

Impersonate vendors continue to use `gw-core::impersonate` for identity/header derivation and `gw-egress-impersonate` for the BoringSSL loopback transport. The adapter seam only precedes that path; it does not reimplement TLS fingerprinting.
```

Keep the OAuth section explicit that W5 still injects pre-existing tokens and does not refresh them. Keep the K-C gap in the residual list unless W6 has already landed on the active branch.

- [ ] **Step 2: Update ROADMAP entry**

In `docs/ROADMAP.md`, remove the completed "Full A-T0..A-T6 outbound adapter registry" capability-gap entry or replace it with a landed note if the file's current style preserves landed history. Leave these entries in place:

```markdown
### K-C in-Rust OAuth refresh/write-back
### Heavy outbound adapter transforms
```

The remaining heavy transform entry should say it now depends on the landed W5 adapter seam instead of saying the seam is absent.

- [ ] **Step 3: Run spec checks**

Run:

```bash
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
rg -n "task runtime call|task worker.*adapter|not via an adapter|AdapterRegistry::for_vendor is not called|only.*response-side" docs/spec/07-vendor-integration.md docs/ROADMAP.md
```

Expected:
- `check-spec.sh` exits 0.
- `rg` finds no stale statement that task workers call adapters, outbound adapter dispatch is absent, or only response analyzers are wired.

- [ ] **Step 4: Commit docs closeout**

```bash
git add docs/spec/07-vendor-integration.md docs/ROADMAP.md
git commit -m "docs(adapter): close adapter seam roadmap entry"
```

## Task W5.6: Final Verification

**Files:**
- All files modified by Tasks W5.1-W5.5

- [ ] **Step 1: Run formatting and adapter/proxy tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p gw-adapter
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test filter_chain_order
```

Expected: PASS.

- [ ] **Step 2: Run impersonate lane**

Run:

```bash
cargo test -p gw-gateway --test e2e_impersonate -- --ignored --nocapture
```

Expected: PASS when local integration prerequisites are running. If skipped by missing local services, include the exact missing prerequisite in the execution report.

- [ ] **Step 3: Run invariant and spec checks**

Run:

```bash
bash scripts/check-invariants.sh
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 4: Inspect final source boundaries**

Run:

```bash
rg -n "gw_adapter|AdapterRegistry|UpstreamAdapter|for_vendor" crates/gw-task crates/gw-task-runtime crates/gw-proxy/src/filters/task.rs
rg -n "for_vendor\\(|NoopAdapter|OAuthRuntime|inject_egress_headers|ensure_identity|gw_egress_impersonate" crates/gw-adapter/src crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/gateway.rs
```

Expected:
- First command has no matches.
- Second command shows adapter dispatch in `rewrite_request`, default adapter behavior in `gw-adapter`, optional OAuth slot, and existing impersonate functions still present.

- [ ] **Step 5: Report verification**

The execution report must include:

```text
Status:
Commits:
Modified files:
Verification:
Direct byte-identical evidence:
Impersonate existing-path evidence:
User decision points:
```

## Self-Review

- Spec coverage: F1 proxy-only adapter boundary is covered by Tasks W5.1, W5.3, and W5.4. Direct byte-identical behavior is covered by Tasks W5.2 and W5.4. Existing impersonate behavior is covered by Tasks W5.3 and W5.4. W6 OAuth seam is covered by Task W5.1 without implementing refresh. W7 heavy transforms are explicitly excluded and left behind the seam.
- Placeholder scan: the plan contains no implementation placeholders. Each code step names concrete files, snippets, commands, and expected outcomes.
- Type consistency: `AdapterCtx` gains `oauth_runtime`, `auth_template`, `upstream_host`, and `content_len`; all later construction snippets use those exact names. Registry dispatch uses `AdapterRegistry::for_vendor`, `rewrite_auth`, and `rewrite_transport`, matching the current trait surface.
