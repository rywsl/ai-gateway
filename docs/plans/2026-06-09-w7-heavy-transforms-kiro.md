# W7 Heavy Vendor Transforms And Kiro Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land Kiro EventStream, Kiro CLI shape registration, Vertex AI service-account auth, and Codex request-body transforms behind the W5 outbound adapter seam.

## Execution status

Status after W7 implementation on branch `sp/w7-heavy-transforms`: **DONE_WITH_CONCERNS**.

Already present on `main` before this W7 run (W6 / earlier commits): `ProtocolKind::Kiro`, `ApiShape::KiroEventStream`, `ApiShape::KiroCli`, `AnyCodec::Kiro`, EventStream codec registration, Kiro cross-render format, and cross-process Kiro protocol/shape strings.

Completed in this W7 run:

- `7d0a959` `feat(adapter): execute request patch hook` — added `WireAnyRequest::Kiro`, `AdapterCtx.body_sha256_hex`, production opt-in `patch_request` execution before archive/send, final body length/hash recomputation, and proxy regression coverage.
- `7cedbf8` `feat(adapter): add kiro outbound transform` — added `KiroAdapter` SigV4 header injection, final body hash signing, secret redaction, and builtin vendor-id registration for `kiro`.
- `d8b0591` `feat(adapter): add vertex service-account transform` — added `VertexJwtClaims`, runtime-token bearer injection via W6 `OAuthRuntime`, quota project header injection, and builtin `vertex` / `google_vertex` registration.
- `4ace5ca` `feat(adapter): add codex request transform` — added `CodexAdapter` semantic OpenAI request metadata injection via `WireAnyRequest::OpenAi`, preserving model/messages/stream/tools/tool_choice and leaving Codex identity headers in `gw-core::impersonate::openai_codex`.
- `f5e9ddc` `docs(vendor): close heavy adapter transform roadmap entry` — updates specs, go-contracts, ROADMAP, and this execution status.
- `528ae95` `fix(adapter): lock kiro signing dependencies` — updates `Cargo.lock` for the new Kiro SigV4 signing dependencies required by `--locked` verification.
- `984d21f` `fix(adapter): preserve direct header snapshot` — keeps direct/noop snapshot coverage byte-identical while retaining the adapter hook regression.
- `3aa8eb3` `docs(go-contracts): add spec check anchors` — adds the required `check-spec` structure and source anchors to go-contract part docs.

Residual concern: Kiro CLI marker detection, usage extraction, and passthrough semantics were **not implemented** because no real Kiro CLI traffic sample exists. `ApiShape::KiroCli` remains registered and documented as a shape boundary; docs explicitly retain the detector/usage residual instead of treating it as complete.

**Architecture:** Split work into one shared contract lane, one Kiro CLI contract lane, and three independent vendor lanes. The shared lane makes `ProtocolKind`, `ApiShape`, `AnyCodec`, `WireAnyRequest`, Go contract strings, and `apply_outbound_adapter` hook execution agree before vendor-specific adapters rely on them. The Kiro CLI lane registers CLI detection and hints as a separate `ApiShape` under `ProtocolKind::Kiro`; it must not be conflated with Kiro EventStream. Vendor lanes then add isolated adapters for Kiro SigV4/EventStream, Vertex service-account JWT/token exchange, and Codex semantic request mutation.

**Tech Stack:** Rust 1.88 workspace, `gw-core`, `gw-codec`, `gw-adapter`, `gw-proxy`, `gw-capability`, Pingora request rewrite filters, AWS EventStream, AWS SigV4, Google OAuth service-account JWT bearer grant, Go PgSQL/Redis contracts.

---

## User Decision Needed

User decision recorded on 2026-06-09: Kiro EventStream shape continues, and W7 must also add a Kiro CLI ApiShape/lane because Kiro CLI behavior is required. EventStream implementation is no longer blocked by the CLI protocol details.

```text
Confirmed W7 Kiro contract:
- ProtocolKind::Kiro
- ApiShape::KiroEventStream
- ApiShape::format_id() == "kiro:eventstream"
- ApiShape::KiroCli
- ApiShape::KiroCli::format_id() == "kiro:cli"
- ProtocolKind JSONB / as_str() wire string == "kiro"
- vendor_keys.payload.accessible_endpoints == ["kiro"]
- EventStream uses ApiShape::KiroEventStream and is not a CLI variant
- CLI uses ApiShape::KiroCli and is a CLI variant under the same ProtocolKind::Kiro
```

- [ ] **Implementation-time blocker:** Do not block Kiro EventStream on CLI-specific reverse engineering. If the exact Kiro CLI request markers or hint fields need more evidence, implement the enum/contract shape first and pause only the CLI detector/hints step until sample CLI traffic is available.
- [ ] **Implementation-time blocker:** If sample Kiro CLI traffic proves that `kiro:cli` must use a different wire string, update the CLI lane and Go contract strings together before committing that lane. Keep `kiro:eventstream` unchanged.

## File Map

- Modify: `crates/gw-core/src/protocol.rs` for `ProtocolKind::Kiro`, `ApiShape::KiroEventStream`, `ApiShape::KiroCli`, format strings, CLI flags, and protocol mapping.
- Modify: `crates/gw-core/tests/wire_enum_lock.rs` for serde-wire lock coverage.
- Modify: `crates/gw-codec/src/any.rs` for `AnyCodec::Kiro`, detection order, dispatch, `kind()`, and `api_shape()`.
- Modify: `crates/gw-codec/src/cli/mod.rs` for Kiro CLI detection.
- Create: `crates/gw-codec/src/cli/kiro_cli.rs` for Kiro CLI shape and hints.
- Modify: `crates/gw-codec/src/cross_render.rs` for `WireFormat::KiroEventStream`, labels, capabilities, and `from_any_codec`.
- Modify: `crates/gw-codec/src/wire/any.rs` for `WireAnyRequest::Kiro`.
- Modify: `crates/gw-codec/src/kiro/mod.rs` to remove the "not added" module comment and keep EventStream behavior as the codec implementation.
- Modify: `crates/gw-codec/tests/kiro_eventstream.rs`, `crates/gw-codec/tests/cli_variants.rs`, `crates/gw-codec/tests/cross_protocol_matrix.rs`, `crates/gw-codec/tests/lossy_paths.rs`, and `crates/gw-codec/tests/wire_any.rs` for codec and CLI registration coverage.
- Create: `crates/gw-adapter/src/kiro.rs` for Kiro SigV4 and EventStream adapter behavior.
- Create: `crates/gw-adapter/src/vertex.rs` for service-account JWT bearer exchange and auth injection.
- Create: `crates/gw-adapter/src/codex.rs` for Codex request mutation through `patch_request`.
- Modify: `crates/gw-adapter/src/lib.rs`, `crates/gw-adapter/src/registry.rs`, `crates/gw-adapter/src/trait_def.rs`, and `crates/gw-adapter/Cargo.toml` for adapter module exports and dependencies.
- Modify: `crates/gw-adapter/src/ctx.rs` and `crates/gw-proxy/src/filters/upstream.rs` to carry `body_sha256_hex`, invoke `patch_request` after render and before archive/send, then refresh `content-length`.
- Modify: `docs/spec/01-protocol-codec.md`, `docs/spec/07-vendor-integration.md`, `docs/go-contracts/parts/03-shared-schema.md`, `docs/go-contracts/parts/08-control-plane.md`, and `docs/ROADMAP.md` for as-built contract closure.

---

## Shared Contract Lane

### Task 1: Register Kiro Protocol Shapes

**Files:**
- Modify: `crates/gw-core/src/protocol.rs`
- Modify: `crates/gw-core/tests/wire_enum_lock.rs`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/go-contracts/parts/08-control-plane.md`

- [ ] **Step 1: Add the enum-lock failing test**

Add assertions that lock both EventStream and CLI serde JSONB strings and runtime strings:

```rust
#[test]
fn kiro_protocol_and_shape_wire_strings_are_locked() {
    assert_eq!(
        serde_json::to_string(&gw_core::protocol::ProtocolKind::Kiro).unwrap(),
        r#""kiro""#
    );
    assert_eq!(gw_core::protocol::ProtocolKind::Kiro.as_str(), "kiro");
    assert_eq!(
        serde_json::to_string(&gw_core::protocol::ApiShape::KiroEventStream).unwrap(),
        r#""kiro_event_stream""#
    );
    assert_eq!(
        serde_json::to_string(&gw_core::protocol::ApiShape::KiroCli).unwrap(),
        r#""kiro_cli""#
    );
    assert_eq!(
        gw_core::protocol::ApiShape::KiroEventStream.format_id(),
        "kiro:eventstream"
    );
    assert_eq!(
        gw_core::protocol::ApiShape::KiroCli.format_id(),
        "kiro:cli"
    );
    assert!(!gw_core::protocol::ApiShape::KiroEventStream.is_cli());
    assert!(gw_core::protocol::ApiShape::KiroCli.is_cli());
    assert_eq!(
        gw_core::protocol::ApiShape::KiroEventStream.protocol_kind(),
        gw_core::protocol::ProtocolKind::Kiro
    );
    assert_eq!(
        gw_core::protocol::ApiShape::KiroCli.protocol_kind(),
        gw_core::protocol::ProtocolKind::Kiro
    );
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test -p gw-core wire_enum_lock --locked
```

Expected: FAIL because `ProtocolKind::Kiro`, `ApiShape::KiroEventStream`, and `ApiShape::KiroCli` do not exist.

- [ ] **Step 3: Implement the protocol enums**

In `crates/gw-core/src/protocol.rs`, add:

```rust
pub enum ProtocolKind {
    OpenAi,
    Anthropic,
    Gemini,
    Kiro,
}
```

Update `ProtocolKind::as_str()`:

```rust
Self::Kiro => "kiro",
```

Add to `ApiShape`:

```rust
KiroEventStream,
KiroCli,
```

Update `format_id()`, `is_cli()`, and `protocol_kind()`:

```rust
Self::KiroEventStream => "kiro:eventstream",
Self::KiroCli => "kiro:cli",
```

```rust
Self::KiroEventStream => ProtocolKind::Kiro,
Self::KiroCli => ProtocolKind::Kiro,
```

```rust
Self::KiroCli => true,
```

- [ ] **Step 4: Update Go contract strings**

In `docs/go-contracts/parts/03-shared-schema.md`, update the `vendor_keys.payload.accessible_endpoints` explanation to include:

```json
{
  "accessible_endpoints": ["kiro"]
}
```

Add a sentence:

```text
For Kiro, both the `ProtocolKind` JSONB string and `ProtocolKind::as_str()` string are `kiro`; EventStream uses `ApiShape::KiroEventStream` with `ApiShape::format_id() == "kiro:eventstream"`, while CLI uses `ApiShape::KiroCli` with `ApiShape::format_id() == "kiro:cli"`.
```

In `docs/go-contracts/parts/08-control-plane.md`, update the capability example:

```json
"protocols": ["openai","anthropic","gemini","kiro"],
"api_shapes": ["openai:chat","openai:responses","anthropic:messages","gemini:generate","kiro:eventstream","kiro:cli"],
"cli_variants": ["claude:cli","openai:cli","openai:compact","gemini:cli","kiro:cli"]
```

- [ ] **Step 5: Verify protocol shape**

Run:

```bash
cargo test -p gw-core wire_enum_lock --locked
bash scripts/check-spec.sh docs/go-contracts/parts/03-shared-schema.md
bash scripts/check-spec.sh docs/go-contracts/parts/08-control-plane.md
```

Expected: PASS.

### Task 2: Register Kiro In AnyCodec And Cross Render

**Files:**
- Modify: `crates/gw-codec/src/any.rs`
- Modify: `crates/gw-codec/src/cross_render.rs`
- Modify: `crates/gw-codec/src/kiro/mod.rs`
- Modify: `crates/gw-codec/tests/kiro_eventstream.rs`
- Modify: `crates/gw-codec/tests/cross_protocol_matrix.rs`
- Modify: `crates/gw-codec/tests/lossy_paths.rs`

- [ ] **Step 1: Add failing AnyCodec detection coverage**

Extend `crates/gw-codec/tests/kiro_eventstream.rs` with:

```rust
use gw_codec::any::AnyCodec;
use gw_codec::trait_def::Headers;
use gw_core::protocol::{ApiShape, ProtocolKind};

#[test]
fn kiro_any_codec_detects_eventstream_shape() {
    let mut headers = Headers::new();
    headers.insert(
        "content-type".into(),
        "application/vnd.amazon.eventstream".into(),
    );
    let frame = gw_codec::kiro::EventStreamFrame {
        headers: Vec::new(),
        payload: br#"{"prompt":"hello"}"#.to_vec(),
    };
    let body = gw_codec::kiro::encode_frame(&frame);
    let codec = AnyCodec::detect("/kiro/invoke", &headers, &body).expect("kiro codec");
    assert_eq!(codec.kind(), ProtocolKind::Kiro);
    assert_eq!(codec.api_shape(), ApiShape::KiroEventStream);
}
```

- [ ] **Step 2: Run the failing codec test**

Run:

```bash
cargo test -p gw-codec kiro_any_codec_detects_eventstream_shape --locked
```

Expected: FAIL because `AnyCodec::Kiro` is not registered.

- [ ] **Step 3: Add `AnyCodec::Kiro` dispatch**

In `crates/gw-codec/src/any.rs`, import `crate::kiro::Kiro`, add the enum variant, and update every `match self` dispatch arm:

```rust
Kiro(Kiro),
```

Add detection after Gemini:

```rust
if let Some(c) = Kiro::detect_request(path, h, body_head) {
    return Some(AnyCodec::Kiro(c));
}
```

Map kind and shape:

```rust
AnyCodec::Kiro(_) => ProtocolKind::Kiro,
AnyCodec::Kiro(_) => ApiShape::KiroEventStream,
```

- [ ] **Step 4: Register cross-render format**

In `crates/gw-codec/src/cross_render.rs`, add `WireFormat::KiroEventStream` with:

```rust
Self::KiroEventStream => "kiro_eventstream",
```

Map from codec:

```rust
AnyCodec::Kiro(_) => Self::KiroEventStream,
```

Set capabilities conservatively:

```rust
WireCapabilities {
    supports_thinking: true,
    supports_reasoning: false,
    supports_tool_use_blocks: false,
    supports_tool_calls_flat: false,
    supports_function_call_parts: false,
    supports_reasoning_tokens: false,
    supports_prediction: false,
    supports_thoughts_tokens: false,
    supports_cache_creation_detail: false,
    supports_cached_tokens: false,
    supports_server_tool_usage: false,
    supports_refusal: false,
    supports_annotations: false,
    supports_server_tool_blocks: false,
    supports_redacted_thinking: false,
    supports_audio: false,
    supports_document_blocks: false,
    supports_service_tier: false,
    supports_inference_geo: false,
    supports_reasoning_config: false,
    supports_grounding: false,
    supports_citations: false,
    supports_mcp: false,
}
```

Use the existing `WireCapabilities` field names exactly as defined in `cross_render.rs`.

- [ ] **Step 5: Update Kiro module comment**

Replace the module comment in `crates/gw-codec/src/kiro/mod.rs` that says Kiro is not added to `gw-core::protocol` with:

```rust
//! Kiro / S5 EventStream support.
//!
//! Kiro streams use AWS EventStream binary frames. This module keeps the
//! binary framer and embedded `<thinking>` payload parser inside `gw-codec`;
//! the public cross-process shape is `ProtocolKind::Kiro` plus
//! `ApiShape::KiroEventStream`.
```

- [ ] **Step 6: Verify codec registration**

Run:

```bash
cargo test -p gw-codec kiro --locked
cargo test -p gw-codec cross_protocol_matrix --locked
cargo test -p gw-codec lossy_paths --locked
```

Expected: PASS.

### Task 2A: Register Kiro CLI Shape And Hints

**Files:**
- Modify: `crates/gw-core/src/protocol.rs`
- Modify: `crates/gw-codec/src/cli/mod.rs`
- Create: `crates/gw-codec/src/cli/kiro_cli.rs`
- Modify: `crates/gw-codec/tests/cli_variants.rs`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/go-contracts/parts/08-control-plane.md`

- [ ] **Step 1: Add failing CLI shape coverage**

Extend `crates/gw-codec/tests/cli_variants.rs` with a Kiro CLI case that asserts the separate CLI shape:

```rust
#[test]
fn kiro_cli_shape_and_hints_are_separate_from_eventstream() {
    let codec = gw_codec::cli::kiro_cli::KiroCliCodec;
    assert_eq!(codec.shape(), gw_core::protocol::ApiShape::KiroCli);
    assert_ne!(codec.shape(), gw_core::protocol::ApiShape::KiroEventStream);

    let hints = codec.extract_hints(
        "/kiro/cli",
        Some(br#"{"model":"kiro-cli-model","stream":true}"#),
    );
    assert_eq!(hints.model.as_deref(), Some("kiro-cli-model"));
    assert_eq!(hints.stream, Some(true));
}
```

Add detector coverage in the same test file using the actual Kiro CLI request marker selected during implementation. The detector assertion must return `Some(ApiShape::KiroCli)`, and an AWS EventStream request must still return no CLI shape from `detect_cli`.

- [ ] **Step 2: Run the failing CLI tests**

Run:

```bash
cargo test -p gw-codec cli_variants --locked
```

Expected: FAIL because `KiroCliCodec`, `ApiShape::KiroCli`, and CLI detection are not fully registered.

- [ ] **Step 3: Implement `KiroCliCodec`**

Create `crates/gw-codec/src/cli/kiro_cli.rs` following the existing CLI codec pattern:

```rust
pub struct KiroCliCodec;
```

The codec must return `ApiShape::KiroCli` from `shape()` and extract hints with the existing `CliHints` structure. Use structured JSON parsing for body fields and preserve unknown fields by ignoring them.

- [ ] **Step 4: Register Kiro CLI detection**

In `crates/gw-codec/src/cli/mod.rs`, export `kiro_cli`, include `ApiShape::KiroCli` in the format/id and `is_cli()` tests, and add a detector branch for Kiro CLI request markers. Keep detection order explicit so Kiro EventStream remains owned by `AnyCodec::Kiro` and `ApiShape::KiroEventStream`, while Kiro CLI remains owned by `detect_cli` and `ApiShape::KiroCli`.

- [ ] **Step 5: Update Go contract strings**

In `docs/go-contracts/parts/03-shared-schema.md`, state that Kiro has two shapes under one protocol:

```text
Kiro EventStream and Kiro CLI share `ProtocolKind::Kiro` / `kiro`; EventStream uses `kiro:eventstream` and CLI uses `kiro:cli`. Runtime code must choose by `ApiShape`, not by protocol string alone.
```

In `docs/go-contracts/parts/08-control-plane.md`, ensure `api_shapes` includes both `kiro:eventstream` and `kiro:cli`, and `cli_variants` includes `kiro:cli`.

- [ ] **Step 6: Verify CLI lane**

Run:

```bash
cargo test -p gw-core wire_enum_lock --locked
cargo test -p gw-codec cli_variants --locked
bash scripts/check-spec.sh docs/go-contracts/parts/03-shared-schema.md
bash scripts/check-spec.sh docs/go-contracts/parts/08-control-plane.md
```

Expected: PASS.

### Task 3: Wire Adapter `patch_request` Into The Proxy

**Files:**
- Modify: `crates/gw-adapter/src/ctx.rs`
- Modify: `crates/gw-codec/src/wire/any.rs`
- Modify: `crates/gw-codec/tests/wire_any.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs` test module

- [ ] **Step 1: Add a failing proxy test proving `patch_request` executes**

In the `upstream.rs` test module, add an adapter that mutates an OpenAI request via `patch_request`:

```rust
struct BodyPatchAdapter;

#[async_trait::async_trait]
impl gw_adapter::UpstreamAdapter for BodyPatchAdapter {
    fn name(&self) -> &'static str {
        "body_patch"
    }

    async fn rewrite_auth(
        &self,
        ctx: &mut gw_adapter::AdapterCtx<'_>,
    ) -> Result<(), gw_adapter::AdapterError> {
        ctx.headers_mut.set("authorization", "Bearer patched");
        Ok(())
    }

    fn rewrite_transport(
        &self,
        _ctx: &mut gw_adapter::AdapterCtx<'_>,
    ) -> Result<(), gw_adapter::AdapterError> {
        Ok(())
    }

    fn patch_request(
        &self,
        _ctx: &gw_adapter::AdapterCtx<'_>,
        wire: &mut gw_codec::wire::WireAnyRequest<'_>,
    ) -> Result<(), gw_adapter::AdapterError> {
        match wire {
            gw_codec::wire::WireAnyRequest::OpenAi(req) => {
                req.model = std::borrow::Cow::Borrowed("patched-model");
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
```

Assert that the outbound body contains `"model":"patched-model"` and `content-length` equals the patched body length.

- [ ] **Step 2: Run the failing proxy test**

Run:

```bash
cargo test -p gw-proxy patch_request_updates_outbound_body_and_content_length --locked
```

Expected: FAIL because `apply_outbound_adapter` only calls `rewrite_auth` and `rewrite_transport`.

- [ ] **Step 3: Add `WireAnyRequest` conversion coverage**

In `crates/gw-codec/tests/wire_any.rs`, add tests for each supported standard shape:

```rust
#[test]
fn wire_any_kind_includes_kiro() {
    let frame = gw_codec::kiro::EventStreamFrame {
        headers: Vec::new(),
        payload: br#"{"prompt":"hello"}"#.to_vec(),
    };
    let req = gw_codec::wire::WireAnyRequest::Kiro(frame);
    assert_eq!(req.kind(), gw_core::protocol::ProtocolKind::Kiro);
}
```

- [ ] **Step 4: Implement Kiro wire request variant**

In `crates/gw-codec/src/wire/any.rs`, add:

```rust
use crate::kiro::EventStreamFrame;

pub enum WireAnyRequest<'a> {
    OpenAi(Box<OpenAiRequest<'a>>),
    Anthropic(AnthropicRequest<'a>),
    Gemini(GeminiRequest<'a>),
    Kiro(EventStreamFrame),
}
```

Update `kind()`:

```rust
Self::Kiro(_) => ProtocolKind::Kiro,
```

- [ ] **Step 5: Add body hash to adapter context**

In `crates/gw-adapter/src/ctx.rs`, add:

```rust
pub body_sha256_hex: &'a str,
```

Populate it in `crates/gw-proxy/src/filters/upstream.rs` from the outbound bytes before building `AdapterCtx`, then recompute it after `patch_request`:

```rust
let body_sha256_hex = sha2::Sha256::digest(outbound_body.as_ref());
let body_sha256_hex = hex::encode(body_sha256_hex);
```

The hash must reflect the body that will be sent to the vendor.

- [ ] **Step 6: Invoke `patch_request` in `apply_outbound_adapter`**

Change `apply_outbound_adapter` so it receives a mutable outbound body buffer or returns the patched body. The call sequence must be:

```text
1. render or passthrough outbound body
2. compute initial body_sha256_hex
3. build AdapterCtx
4. parse outbound body into WireAnyRequest for upstream_protocol
5. call adapter.patch_request(&adapter_ctx, &mut wire)
6. render WireAnyRequest back to bytes when the wire value changed
7. recompute body_sha256_hex and content_len from the final body
8. rebuild or update AdapterCtx with final body_sha256_hex and content_len
9. call adapter.rewrite_auth(&mut adapter_ctx)
10. call adapter.rewrite_transport(&mut adapter_ctx)
11. set content-length from final body length
12. archive final body
```

Do not add a `patch_body` hook. Codex and Kiro body changes must use `patch_request`.

- [ ] **Step 7: Verify proxy hook execution**

Run:

```bash
cargo test -p gw-codec wire_any --locked
cargo test -p gw-proxy patch_request_updates_outbound_body_and_content_length --locked
cargo test -p gw-proxy direct_mode_outbound_adapter_headers --locked
```

Expected: PASS.

---

## Kiro Vendor Lane

### Task 4: Add Kiro SigV4 And EventStream Adapter

**Files:**
- Create: `crates/gw-adapter/src/kiro.rs`
- Modify: `crates/gw-adapter/src/lib.rs`
- Modify: `crates/gw-adapter/src/registry.rs`
- Modify: `crates/gw-adapter/Cargo.toml`
- Modify: `crates/gw-adapter/tests/registry_lookup.rs`
- Create: `crates/gw-adapter/tests/kiro.rs`

- [ ] **Step 1: Add failing adapter tests**

Create `crates/gw-adapter/tests/kiro.rs` with tests that assert:

```rust
#[tokio::test]
async fn kiro_adapter_sets_sigv4_headers_without_logging_secret() {
    let mut ctx = test_ctx_for_vendor("v_kiro", "vk_kiro", "kiro");
    let adapter = gw_adapter::KiroAdapter::new(gw_adapter::kiro::KiroSigV4Config {
        region: "us-east-1".into(),
        service: "bedrock".into(),
    });
    adapter.rewrite_auth(&mut ctx).await.expect("rewrite auth");
    assert!(ctx.headers_mut.get("authorization").unwrap().starts_with("AWS4-HMAC-SHA256 "));
    assert_eq!(ctx.headers_mut.get("x-amz-content-sha256").unwrap().len(), 64);
    assert!(ctx.headers_mut.get("x-amz-date").unwrap().ends_with('Z'));
}

#[test]
fn kiro_adapter_declares_standard_transport() {
    assert_eq!(gw_adapter::KiroAdapter::default().transport_profile(), None);
}
```

Implement the test helpers using the same in-memory `HeadersAccessor` and `UrlAccessor` pattern from `crates/gw-adapter/tests/noop_behavior.rs`.

- [ ] **Step 2: Run the failing Kiro adapter tests**

Run:

```bash
cargo test -p gw-adapter kiro --locked
```

Expected: FAIL because `KiroAdapter` does not exist.

- [ ] **Step 3: Add adapter module and dependencies**

In `crates/gw-adapter/Cargo.toml`, add workspace dependencies already declared in the root `Cargo.toml`:

```toml
hmac = { workspace = true }
sha2 = { workspace = true }
```

Use `time::OffsetDateTime` plus `format_description!("[year][month][day]T[hour][minute][second]Z")` for `x-amz-date`. Keep secret material in `secrecy::SecretString` and never include it in `Debug`.

- [ ] **Step 4: Implement `KiroAdapter`**

Create `crates/gw-adapter/src/kiro.rs` with:

```rust
pub struct KiroAdapter {
    config: KiroSigV4Config,
}

pub struct KiroSigV4Config {
    pub region: String,
    pub service: String,
}
```

`rewrite_auth` must:

```text
- remove inbound client auth headers
- read the final outbound body hash from AdapterCtx.body_sha256_hex
- set x-amz-date
- set x-amz-content-sha256
- set authorization = AWS4-HMAC-SHA256 Credential=..., SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=...
```

- [ ] **Step 5: Register Kiro by vendor id**

In `crates/gw-adapter/src/registry.rs`, add builtin lookup for vendor ids containing `kiro`:

```rust
if id.contains("kiro") {
    return Some(Arc::new(crate::kiro::KiroAdapter::default()));
}
```

Use an internal helper matching the existing `builtin_error_analyzer_for` pattern.

- [ ] **Step 6: Verify Kiro lane**

Run:

```bash
cargo test -p gw-codec kiro --locked
cargo test -p gw-adapter kiro --locked
cargo test -p gw-adapter registry_lookup --locked
cargo test -p gw-proxy direct_mode_outbound_adapter_headers --locked
```

Expected: PASS.

---

## Vertex Vendor Lane

### Task 5: Add Vertex Service-Account Transform

**Files:**
- Create: `crates/gw-adapter/src/vertex.rs`
- Modify: `crates/gw-adapter/src/lib.rs`
- Modify: `crates/gw-adapter/src/registry.rs`
- Modify: `crates/gw-adapter/Cargo.toml`
- Create: `crates/gw-adapter/tests/vertex.rs`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`

- [ ] **Step 1: Add failing Vertex token tests**

Create `crates/gw-adapter/tests/vertex.rs` with:

```rust
#[test]
fn vertex_jwt_claims_use_service_account_contract() {
    let claims = gw_adapter::vertex::VertexJwtClaims::new(
        "svc@example.iam.gserviceaccount.com",
        "https://oauth2.googleapis.com/token",
        1_800_000_000,
    );
    assert_eq!(claims.iss, "svc@example.iam.gserviceaccount.com");
    assert_eq!(claims.sub, "svc@example.iam.gserviceaccount.com");
    assert_eq!(claims.aud, "https://oauth2.googleapis.com/token");
    assert_eq!(claims.scope, "https://www.googleapis.com/auth/cloud-platform");
    assert_eq!(claims.iat, 1_800_000_000);
    assert_eq!(claims.exp, 1_800_003_600);
}

#[tokio::test]
async fn vertex_adapter_injects_bearer_token_and_quota_project() {
    let runtime = Arc::new(FakeOAuthRuntime::token("ya29.test-token"));
    let mut ctx = test_ctx_with_oauth_runtime_and_quota_project(
        "v_vertex",
        "vk_vertex",
        Some(&runtime),
        "quota-project",
    );
    gw_adapter::VertexAdapter::default()
        .rewrite_auth(&mut ctx)
        .await
        .expect("rewrite auth");
    assert_eq!(ctx.headers_mut.get("authorization"), Some("Bearer ya29.test-token"));
    assert_eq!(ctx.headers_mut.get("x-goog-user-project"), Some("quota-project"));
}
```

Use the existing `OAuthRuntime` trait in `AdapterCtx`; do not make Vertex call PgSQL or Redis directly from the adapter.

- [ ] **Step 2: Run the failing Vertex tests**

Run:

```bash
cargo test -p gw-adapter vertex --locked
```

Expected: FAIL because `VertexAdapter` and JWT claims do not exist.

- [ ] **Step 3: Add JWT dependencies**

Add:

```toml
jsonwebtoken = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

The root `Cargo.toml` already declares `jsonwebtoken = "9"` in `[workspace.dependencies]`; do not add a second version.

- [ ] **Step 4: Implement Vertex JWT and auth injection**

Create `crates/gw-adapter/src/vertex.rs` with:

```rust
#[derive(Debug, serde::Serialize)]
pub struct VertexJwtClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub scope: String,
    pub iat: i64,
    pub exp: i64,
}
```

`VertexAdapter::rewrite_auth` must:

```text
- strip inbound client auth headers
- first ask AdapterCtx.oauth_runtime for a refreshed access token when present
- inject authorization: Bearer <token>
- inject x-goog-user-project when vendor_key.oauth.quota_project_id is present
- never log the private key, JWT assertion, refresh token, or bearer token
```

Keep token exchange ownership in W6 `OAuthRuntime`; W7 adapter consumes the seam.

- [ ] **Step 5: Document service-account payload**

In `docs/go-contracts/parts/03-shared-schema.md`, extend `vendor_keys.payload.oauth` with Vertex service-account fields:

```json
{
  "token_endpoint": "https://oauth2.googleapis.com/token",
  "scope": ["https://www.googleapis.com/auth/cloud-platform"],
  "quota_project_id": "my-quota-project"
}
```

State that private key material remains encrypted in the existing OAuth credential shape and is consumed through Rust's W6 runtime refresher.

- [ ] **Step 6: Verify Vertex lane**

Run:

```bash
cargo test -p gw-adapter vertex --locked
cargo test -p gw-proxy direct_mode_outbound_adapter_headers --locked
bash scripts/check-spec.sh docs/go-contracts/parts/03-shared-schema.md
```

Expected: PASS.

---

## Codex Vendor Lane

### Task 6: Add Codex Request-Body Transform

**Files:**
- Create: `crates/gw-adapter/src/codex.rs`
- Modify: `crates/gw-adapter/src/lib.rs`
- Modify: `crates/gw-adapter/src/registry.rs`
- Modify: `crates/gw-codec/src/wire/any.rs`
- Modify: `crates/gw-core/src/impersonate/openai_codex.rs`
- Create: `crates/gw-adapter/tests/codex.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs` tests

- [ ] **Step 1: Add failing Codex transform tests**

Create `crates/gw-adapter/tests/codex.rs` with:

```rust
#[test]
fn codex_patch_request_uses_wire_any_openai() {
    let adapter = gw_adapter::CodexAdapter::default();
    let ctx = test_ctx_for_vendor("v_codex", "vk_codex", "openai");
    let mut req = gw_codec::wire::WireAnyRequest::OpenAi(Box::new(openai_request("gpt-5")));
    adapter.patch_request(&ctx, &mut req).expect("patch request");
    let gw_codec::wire::WireAnyRequest::OpenAi(req) = req else {
        panic!("expected openai request");
    };
    assert_eq!(req.model.as_ref(), "gpt-5");
    assert!(req.extra.contains_key("codex"));
}
```

The test must assert semantic fields on `OpenAiRequest`, not raw JSON substrings.

- [ ] **Step 2: Run the failing Codex test**

Run:

```bash
cargo test -p gw-adapter codex --locked
```

Expected: FAIL because `CodexAdapter` does not exist and `patch_request` is not wired through the proxy until Task 3.

- [ ] **Step 3: Implement Codex adapter**

Create `crates/gw-adapter/src/codex.rs`:

```rust
pub struct CodexAdapter;

impl Default for CodexAdapter {
    fn default() -> Self {
        Self
    }
}
```

`patch_request` must:

```text
- accept only WireAnyRequest::OpenAi
- leave non-OpenAI variants unchanged
- add `extra["codex"] = {"gateway":"ailinkmax","request_id":ctx.request_id}`
- preserve existing model, messages, stream flag, tools, and tool_choice
- return AdapterError::PatchRequest("codex requires openai wire request".into()) only if the vendor is explicitly Codex and the upstream protocol is not OpenAi
```

Do not edit raw JSON strings and do not add a `patch_body` hook.

- [ ] **Step 4: Keep Codex identity source explicit**

Use `ctx.request_id` for the body-level `extra["codex"].request_id` value. Keep `session-id`, `thread-id`, `x-client-request-id`, `x-codex-window-id`, and `x-codex-turn-state` as headers owned by `crates/gw-core/src/impersonate/openai_codex.rs`; do not duplicate those header values inside the request body in W7.

- [ ] **Step 5: Register Codex by vendor id**

In `crates/gw-adapter/src/registry.rs`, register `CodexAdapter` for ids containing `codex` before the generic OpenAI analyzer fallback:

```rust
if id.contains("codex") {
    return Some(Arc::new(crate::codex::CodexAdapter::default()));
}
```

- [ ] **Step 6: Verify Codex lane**

Run:

```bash
cargo test -p gw-adapter codex --locked
cargo test -p gw-proxy patch_request_updates_outbound_body_and_content_length --locked
cargo test -p gw-proxy --test cli_filter_chain --locked
```

Expected: PASS.

---

## Integrator And Documentation Lane

### Task 7: Registry, Capability, Specs, And Roadmap Closure

**Files:**
- Modify: `crates/gw-adapter/src/registry.rs`
- Modify: `crates/gw-adapter/tests/registry_lookup.rs`
- Modify: `crates/gw-capability/src/resolver.rs`
- Modify: `crates/gw-capability/tests/responses_capability.rs`
- Modify: `crates/gw-codec/tests/cli_variants.rs`
- Modify: `docs/spec/01-protocol-codec.md`
- Modify: `docs/spec/07-vendor-integration.md`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/go-contracts/parts/08-control-plane.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Add registry coverage for all heavy adapters**

Extend `crates/gw-adapter/tests/registry_lookup.rs`:

```rust
#[test]
fn builtin_registry_returns_heavy_vendor_adapters() {
    let registry = gw_adapter::AdapterRegistry::default();
    assert_eq!(registry.for_vendor(&VendorId::from("v_kiro")).name(), "kiro");
    assert_eq!(registry.for_vendor(&VendorId::from("v_vertex")).name(), "vertex");
    assert_eq!(registry.for_vendor(&VendorId::from("v_codex")).name(), "codex");
}
```

- [ ] **Step 2: Run the failing registry test**

Run:

```bash
cargo test -p gw-adapter builtin_registry_returns_heavy_vendor_adapters --locked
```

Expected: FAIL until Tasks 4-6 register builtin adapters.

- [ ] **Step 3: Update capability surfacing**

In `crates/gw-capability/tests/responses_capability.rs`, add:

```rust
#[tokio::test]
async fn kiro_eventstream_shape_does_not_gain_openai_responses_capability() {
    let resolver = CapabilityResolver::new(FakeResolveBackend::default());
    let resolved = resolver
        .resolve_for_shape(
            "kiro-model",
            ProtocolKind::Kiro,
            ApiShape::KiroEventStream,
            &[],
            &[],
        )
        .await;
    assert!(!resolved.has(&Capability::OpenAiResponsesShape));
}
```

Update `crates/gw-capability/src/resolver.rs` only if matching on `ApiShape` becomes exhaustive after Task 1. Keep `ApiShape::KiroEventStream` out of `cli_variants`; add only `ApiShape::KiroCli` / `kiro:cli` to CLI variant surfacing. W7 does not add new Prometheus metrics; do not modify `crates/gw-gateway/src/metrics_catalog.rs`.

- [ ] **Step 4: Update specs**

In `docs/spec/01-protocol-codec.md`, move Kiro from "Not built -> ROADMAP" into "As-built surface" with:

```text
Kiro EventStream is registered through `AnyCodec::Kiro`, `ProtocolKind::Kiro`, and `ApiShape::KiroEventStream`. The Aether `FORMAT_ID` is `kiro:eventstream`; the codec parses and renders AWS EventStream frames and embedded `<thinking>` payload sections.
Kiro CLI is registered separately as `ApiShape::KiroCli` with Aether `FORMAT_ID` `kiro:cli`. Runtime dispatch must not infer CLI behavior from `ProtocolKind::Kiro` alone.
```

In `docs/spec/07-vendor-integration.md`, replace the heavy-transform gap with:

```text
Kiro SigV4/EventStream, Vertex service-account auth, and Codex request mutation are implemented as `gw-adapter` outbound adapters. The proxy invokes `patch_request` after codec render and before archive/send; no task-runtime code calls adapters.
Kiro CLI request detection is a CLI shape concern and is intentionally separate from the Kiro EventStream adapter path.
```

In `docs/ROADMAP.md`, mark W7 items closed with the commit ids created by Tasks 1, 2, 2A, and 3-7.

- [ ] **Step 5: Run full W7 verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p gw-core wire_enum_lock --locked
cargo test -p gw-codec cli_variants --locked
cargo test -p gw-codec kiro --locked
cargo test -p gw-codec wire_any --locked
cargo test -p gw-adapter kiro --locked
cargo test -p gw-adapter vertex --locked
cargo test -p gw-adapter codex --locked
cargo test -p gw-adapter registry_lookup --locked
cargo test -p gw-proxy direct_mode_outbound_adapter_headers --locked
cargo test -p gw-proxy --test cli_filter_chain --locked
bash scripts/check-invariants.sh
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
bash scripts/check-spec.sh docs/go-contracts/parts/03-shared-schema.md
bash scripts/check-spec.sh docs/go-contracts/parts/08-control-plane.md
git diff --check
```

Expected: PASS.

## Commit Plan

- [ ] Commit Task 1 as `feat(codec): add kiro protocol shapes`.
- [ ] Commit Task 2 as `feat(codec): register kiro eventstream codec`.
- [ ] Commit Task 2A as `feat(codec): add kiro cli shape`.
- [ ] Commit Task 3 as `feat(adapter): execute request patch hook`.
- [ ] Commit Task 4 as `feat(adapter): add kiro outbound transform`.
- [ ] Commit Task 5 as `feat(adapter): add vertex service-account transform`.
- [ ] Commit Task 6 as `feat(adapter): add codex request transform`.
- [ ] Commit Task 7 as `docs(vendor): close heavy adapter transform roadmap entries`.

## Planner Verification

- [ ] `git diff --check`
