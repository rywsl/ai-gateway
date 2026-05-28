# Systemic Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land 4 cross-cutting fixes (ErrorSink, RequestBodyView, ManagedLoop, wire-enum lock) that systemically resolve 5 high/medium priority gaps without point-patching.

**Architecture:** Per [`docs/superpowers/specs/2026-05-28-systemic-fixes-design.md`](../specs/2026-05-28-systemic-fixes-design.md): 3 horizontal abstractions + 1 CI lock test, organized into 7 independent phases (A: ErrorSink core, B: RequestBodyView, C: ManagedLoop + StreamConsumer, D: wire-enum lock, E: fail_to_proxy override, F: codec/worker observe migration, G: docs sync). Each phase is an independent PR.

**Tech Stack:** Rust 1.88.0, Pingora 0.8, fred (Redis), sqlx (Postgres), `metrics` crate, `tracing`, `once_cell`, `async-trait`. Tests use `cargo test` + Python e2e in `tests/e2e/client/scenarios/`.

**Spec deviation:** ErrorSink lives in `crates/gw-core/src/observability/` (not `gw-proxy/src/errors/` as spec drafted). Rationale: gw-core already has `metrics.workspace = true`; codec (gw-codec) and Supervisor (gw-store) must both call `observe()` but neither depends on gw-proxy. Putting it in gw-core respects the layering rule (gw-core is the lowest crate).

---

## File Structure

### New files

| Path | Purpose |
|------|---------|
| `crates/gw-core/src/observability/mod.rs` | Module declaration |
| `crates/gw-core/src/observability/sink.rs` | `ErrorObservation`, `ErrorKind`, `Stage`, `observe()` |
| `crates/gw-core/tests/wire_enum_lock.rs` | wire-form drift lock tests |
| `crates/gw-proxy/src/body/mod.rs` | Module declaration |
| `crates/gw-proxy/src/body/view.rs` | `RequestBodyView`, `DecodeErr` |
| `crates/gw-store/src/managed_loop.rs` | `ManagedLoop` trait, `Outcome`, `RecoverReason`, `Supervisor` |

### Modified files

| Path | Why |
|------|-----|
| `crates/gw-core/src/lib.rs` | Add `pub mod observability;` |
| `crates/gw-proxy/src/lib.rs` | Add `pub mod body;` |
| `crates/gw-proxy/src/ctx.rs:76-200` | Replace `cached_body: Option<Bytes>` with `body: Option<RequestBodyView>` |
| `crates/gw-proxy/src/filters/request.rs:480-595` | HMAC verify uses `body.decoded(...)`; codec parse reuses; remove cached_body write |
| `crates/gw-proxy/src/filters/upstream.rs:54-77,140-460,654-797` | `emit_envelope_and_bail` calls `observe`; `same_protocol_passthrough` & cross-protocol render use `body.outbound_or_raw()` / `set_outbound()` |
| `crates/gw-proxy/src/gateway.rs:445-1080` | Override `fail_to_proxy`; `fail_to_connect` calls `observe`; body chunk swap uses new view API |
| `crates/gw-proxy/src/filters/response.rs:365-712` | `parse_stream_chunk` / `cross-protocol render` warns → `observe(CodecParse/CodecRender, ...)` |
| `crates/gw-store/src/lib.rs` | Add `pub mod managed_loop;` |
| `crates/gw-store/src/stream_consumer.rs:25-156` | Refactor to `impl ManagedLoop`; `start()` switches to `Supervisor::spawn` |
| `crates/gw-codec/src/openai/codec.rs:96,216-228` | warns → `observe(CodecParse/CodecRender, ...)` |
| `crates/gw-codec/src/anthropic/codec.rs` (similar warns) | Same as openai |
| `crates/gw-codec/src/gemini/codec.rs` (similar warns) | Same as openai |
| `scripts/check-invariants.sh` | Add `no_cached_body` (error), `wire_enum_lock` (error), `managed_loop_advisory` (warn) |
| `docs/architecture.md` | §Deliberate behaviours +3 entries; §Known gaps trim §9.1/§10.5; §Invariants +3 |
| `docs/workflow.md` | §request_filter step 7/3b/4 reference `body.decoded()`; new §Error observability sink |

---

## Phase A — ErrorSink foundation

**Outcome:** `observe()` callable from any crate; `emit_envelope_and_bail` and `fail_to_connect` route through it. Metric `gw_errors_total{code,kind,stage}` populated on short-circuit and upstream-connect failures.

### Task A.1: Create observability module skeleton in gw-core

**Files:**
- Create: `crates/gw-core/src/observability/mod.rs`
- Create: `crates/gw-core/src/observability/sink.rs`
- Modify: `crates/gw-core/src/lib.rs` (add `pub mod observability;`)

- [ ] **Step 1: Inspect current `crates/gw-core/src/lib.rs` to know where to insert the new module declaration**

Run: `head -30 crates/gw-core/src/lib.rs`

Expected output: a list of `pub mod ...;` declarations.

- [ ] **Step 2: Create `crates/gw-core/src/observability/mod.rs`**

```rust
//! Observability primitives shared across crates.
//!
//! - [`sink`] — `observe()` unified error event sink + `ErrorObservation` type.
pub mod sink;

pub use sink::{observe, ErrorKind, ErrorObservation, Stage};
```

- [ ] **Step 3: Create `crates/gw-core/src/observability/sink.rs` with empty stub**

```rust
//! Unified error observability sink.
//!
//! All non-trivial error events (short-circuit bail, upstream connect failure,
//! codec parse warn, background worker recover/fatal) call [`observe`].
//! See `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §3.

use std::fmt;

#[derive(Copy, Clone, Debug)]
pub enum ErrorKind {
    ShortCircuit,
    UpstreamConnect,
    UpstreamMidStream,
    UpstreamProtocol,
    CodecParse,
    CodecRender,
    BackgroundWorker,
}

impl ErrorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortCircuit => "short_circuit",
            Self::UpstreamConnect => "upstream_connect",
            Self::UpstreamMidStream => "upstream_mid_stream",
            Self::UpstreamProtocol => "upstream_protocol",
            Self::CodecParse => "codec_parse",
            Self::CodecRender => "codec_render",
            Self::BackgroundWorker => "background_worker",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Stage {
    Request,
    Upstream,
    Response,
    Logging,
    Background,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Upstream => "upstream",
            Self::Response => "response",
            Self::Logging => "logging",
            Self::Background => "background",
        }
    }
}

pub struct ErrorObservation<'a> {
    pub code: &'a str,
    pub kind: ErrorKind,
    pub stage: Stage,
    pub status: Option<u16>,
    pub trace_id: Option<&'a str>,
    pub extra: Option<&'a (dyn fmt::Debug + Sync)>,
}

pub fn observe(o: ErrorObservation<'_>) {
    metrics::counter!(
        "gw_errors_total",
        "code" => o.code.to_string(),
        "kind" => o.kind.as_str(),
        "stage" => o.stage.as_str(),
    )
    .increment(1);

    tracing::warn!(
        target: "gw_errors",
        code = o.code,
        kind = o.kind.as_str(),
        stage = o.stage.as_str(),
        status = ?o.status,
        trace_id = o.trace_id,
        extra = ?o.extra,
        "error observed",
    );
}
```

- [ ] **Step 4: Add `pub mod observability;` to `crates/gw-core/src/lib.rs`**

Insert near other top-level `pub mod` declarations (alphabetical or grouped — match existing style).

```rust
pub mod observability;
```

- [ ] **Step 5: Build to verify the module compiles**

Run: `cargo build -p gw-core`
Expected: clean build, no warnings about unused items (the items are `pub`, exported).

- [ ] **Step 6: Commit**

```bash
git add crates/gw-core/src/observability/ crates/gw-core/src/lib.rs
git commit -m "feat(gw-core): observability::sink module skeleton (ErrorObservation, observe)"
```

### Task A.2: Unit test — observe() emits expected metric

**Files:**
- Modify: `crates/gw-core/src/observability/sink.rs` (add `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add a test module that uses a `metrics_util::debugging::DebuggingRecorder` to capture counter increments**

Append to `crates/gw-core/src/observability/sink.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

    fn install_recorder() -> Snapshotter {
        let r = DebuggingRecorder::new();
        let snap = r.snapshotter();
        let _ = r.install();
        snap
    }

    #[test]
    fn observe_increments_gw_errors_total_with_expected_labels() {
        let snap = install_recorder();
        observe(ErrorObservation {
            code: "test_code",
            kind: ErrorKind::ShortCircuit,
            stage: Stage::Request,
            status: Some(401),
            trace_id: Some("trace-abc"),
            extra: None,
        });
        let metrics = snap.snapshot().into_vec();
        let counter = metrics
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "gw_errors_total")
            .expect("gw_errors_total recorded");
        let labels: Vec<_> = counter
            .0
            .key()
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect();
        assert!(labels.contains(&("code".into(), "test_code".into())));
        assert!(labels.contains(&("kind".into(), "short_circuit".into())));
        assert!(labels.contains(&("stage".into(), "request".into())));
    }
}
```

- [ ] **Step 2: Add `metrics-util` as dev-dependency to gw-core**

Edit `crates/gw-core/Cargo.toml`, in `[dev-dependencies]` section (or create it):

```toml
[dev-dependencies]
metrics-util = "0.17"
```

If a different version is pinned in workspace, use `metrics-util.workspace = true` and add it to root `Cargo.toml` `[workspace.dependencies]`.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p gw-core observe_increments_gw_errors_total_with_expected_labels -- --nocapture`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-core/src/observability/sink.rs crates/gw-core/Cargo.toml Cargo.lock
git commit -m "test(gw-core): observe() emits gw_errors_total with code/kind/stage labels"
```

### Task A.3: Wire `observe()` into `emit_envelope_and_bail`

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs:77-110` (the `emit_envelope_and_bail` body)

- [ ] **Step 1: Read the existing `emit_envelope_and_bail` to understand its signature and call sites**

Run: `sed -n '77,135p' crates/gw-proxy/src/filters/upstream.rs`

Note the function signature (takes `session`, `ctx`, `envelope`) and that 66+ call sites pass `envelope_for(code, message, status)` as the 3rd arg.

- [ ] **Step 2: Modify `emit_envelope_and_bail` to call `observe()` before short-circuiting**

Add (near the top of `emit_envelope_and_bail`, after envelope binding):

```rust
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code: envelope.code.as_ref(),
    kind: gw_core::observability::ErrorKind::ShortCircuit,
    stage: gw_core::observability::Stage::Request,
    status: Some(envelope.status),
    trace_id: Some(ctx.trace_id.as_str()),
    extra: None,
});
```

Note: `envelope.code` is `Cow<'static, str>`; `.as_ref()` yields `&str` which fits `code: &'a str`.

- [ ] **Step 3: Re-run gw-core unit test to confirm no regression**

Run: `cargo test -p gw-core observe_increments_gw_errors_total_with_expected_labels`
Expected: 1 passed.

- [ ] **Step 4: Build gw-proxy to verify integration compiles**

Run: `cargo build -p gw-proxy`
Expected: clean build.

- [ ] **Step 5: Run gw-proxy unit + integration tests**

Run: `cargo test -p gw-proxy`
Expected: all existing tests still pass (we didn't change behavior, only added an observation call).

- [ ] **Step 6: Commit**

```bash
git add crates/gw-core/src/observability/sink.rs crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat(gw-proxy): emit_envelope_and_bail emits gw_errors_total via observe()"
```

### Task A.4: Wire `observe()` into `fail_to_connect`

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs:996-1080` (`fail_to_connect` body)

- [ ] **Step 1: Read existing `fail_to_connect` to understand the error classification logic**

Run: `sed -n '988,1080p' crates/gw-proxy/src/gateway.rs`

Identify where the error code is stamped onto `ctx.upstream_error_code` (or similar).

- [ ] **Step 2: Add an `observe()` call alongside the existing stamping**

Locate the place where the gateway error code is set (e.g., `ctx.upstream_error_code = Some(code)` or similar). Immediately after:

```rust
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code,  // the &'static str gateway error code being stamped
    kind: gw_core::observability::ErrorKind::UpstreamConnect,
    stage: gw_core::observability::Stage::Upstream,
    status: None,  // status will be 502 at fail_to_proxy time
    trace_id: Some(ctx.trace_id.as_str()),
    extra: None,
});
```

If there are multiple stamping branches (TLS error / connect refused / DNS / timeout), each gets its own `observe()` call with the matching `code`.

- [ ] **Step 3: Build + test**

Run: `cargo build -p gw-proxy && cargo test -p gw-proxy`
Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-proxy): fail_to_connect emits gw_errors_total{kind=upstream_connect}"
```

### Task A.5: E2E verify — short-circuit emits metric

**Files:**
- Modify: `tests/e2e/client/scenarios/test_metrics_smoke.py` (or create new)

- [ ] **Step 1: Find an existing e2e scenario that produces a short-circuit (e.g., invalid api key → 401)**

Run: `rg -l "expect.*401|GW_AUTH" tests/e2e/client/scenarios/*.py | head -3`

Pick one (e.g., `test_auth_errors.py` if it exists; else `test_non_stream.py`).

- [ ] **Step 2: Add a metric assertion after the failure assertion**

Pattern (adapt to existing test framework helpers in conftest):

```python
def test_short_circuit_emits_gw_errors_total(gateway, fake_upstream, metrics_client):
    pre = metrics_client.counter_value(
        "gw_errors_total", labels={"kind": "short_circuit"}
    )
    # ... make request with bad api key ...
    resp = gateway.post("/v1/chat/completions", headers={"x-api-key": "nope"}, json={...})
    assert resp.status_code == 401
    post = metrics_client.counter_value(
        "gw_errors_total", labels={"kind": "short_circuit"}
    )
    assert post == pre + 1
```

(If `metrics_client` helper doesn't exist, query Prometheus endpoint directly: `requests.get(gateway.metrics_url).text` then parse line containing `gw_errors_total{kind="short_circuit"`).

- [ ] **Step 3: Run the e2e smoke for that scenario**

Run: `cd tests/e2e/client && pytest scenarios/test_auth_errors.py -k short_circuit_emits -v`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/client/scenarios/
git commit -m "test(e2e): verify gw_errors_total{kind=short_circuit} on auth bail"
```

---

## Phase B — RequestBodyView migration

**Outcome:** `ReqCtx.cached_body` removed; `ReqCtx.body: Option<RequestBodyView>` is sole authority for raw/decoded/outbound body bytes. HMAC verify and codec parse share one decompression via `OnceCell`.

### Task B.1: Create body module skeleton

**Files:**
- Create: `crates/gw-proxy/src/body/mod.rs`
- Create: `crates/gw-proxy/src/body/view.rs`
- Modify: `crates/gw-proxy/src/lib.rs`

- [ ] **Step 1: Create `crates/gw-proxy/src/body/mod.rs`**

```rust
//! Request body lifecycle types.
pub mod view;
pub use view::{DecodeErr, RequestBodyView};
```

- [ ] **Step 2: Create `crates/gw-proxy/src/body/view.rs` with type stubs**

```rust
//! `RequestBodyView` — explicit raw/decoded/outbound state.
//!
//! See `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §4.

use bytes::Bytes;
use gw_codec::compression::Encoding;
use once_cell::unsync::OnceCell;

#[derive(Debug)]
pub enum DecodeErr {
    TooLarge { limit: usize, actual: usize },
    FormatError(&'static str),
}

pub struct RequestBodyView {
    raw: Bytes,
    req_encoding: Encoding,
    decoded: OnceCell<Result<Bytes, DecodeErr>>,
    outbound: Option<Bytes>,
}

impl RequestBodyView {
    pub fn from_raw(raw: Bytes, req_encoding: Encoding) -> Self {
        Self {
            raw,
            req_encoding,
            decoded: OnceCell::new(),
            outbound: None,
        }
    }

    pub fn raw(&self) -> &Bytes {
        &self.raw
    }

    pub fn req_encoding(&self) -> Encoding {
        self.req_encoding
    }

    /// Lazy decompress + cache. Returns the same `Result` on subsequent calls.
    pub fn decoded(&self, max_bytes: usize) -> Result<&Bytes, &DecodeErr> {
        self.decoded
            .get_or_init(|| decompress(&self.raw, self.req_encoding, max_bytes))
            .as_ref()
    }

    pub fn set_outbound(&mut self, body: Bytes) {
        self.outbound = Some(body);
    }

    pub fn outbound_or_raw(&self) -> &Bytes {
        self.outbound.as_ref().unwrap_or(&self.raw)
    }
}

fn decompress(raw: &Bytes, enc: Encoding, max_bytes: usize) -> Result<Bytes, DecodeErr> {
    match enc {
        Encoding::Identity => Ok(raw.clone()),
        other => {
            // Delegate to gw-codec's compression module.
            gw_codec::compression::decode(other, raw, max_bytes)
                .map(Bytes::from)
                .map_err(|e| match e {
                    gw_codec::compression::DecodeError::TooLarge { limit, actual } => {
                        DecodeErr::TooLarge { limit, actual }
                    }
                    gw_codec::compression::DecodeError::Format(msg) => {
                        DecodeErr::FormatError(msg)
                    }
                })
        }
    }
}
```

**Verify first:** Run `rg -n 'pub fn decode|pub enum DecodeError' crates/gw-codec/src/compression/` to confirm the function signatures match. If they differ, adapt the `decompress` helper to match.

- [ ] **Step 3: Add `pub mod body;` to `crates/gw-proxy/src/lib.rs`**

Insert near other top-level `pub mod` declarations.

- [ ] **Step 4: Build to verify the module compiles**

Run: `cargo build -p gw-proxy`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/body/ crates/gw-proxy/src/lib.rs
git commit -m "feat(gw-proxy): RequestBodyView skeleton (raw + OnceCell decoded + outbound)"
```

### Task B.2: Unit test — decoded() decompresses only once

**Files:**
- Modify: `crates/gw-proxy/src/body/view.rs` (add `#[cfg(test)]`)

- [ ] **Step 1: Add a unit test with a gzip-encoded body and assert decoded() is idempotent**

Append to `crates/gw-proxy/src/body/view.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip_encode(payload: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn decoded_returns_plaintext_for_gzip_body() {
        let plain = b"{\"hello\":\"world\"}";
        let body = Bytes::from(gzip_encode(plain));
        let v = RequestBodyView::from_raw(body, Encoding::Gzip);
        let d1 = v.decoded(64 * 1024).expect("decompresses");
        assert_eq!(d1.as_ref(), &plain[..]);
    }

    #[test]
    fn decoded_only_decompresses_once() {
        // Compress a payload, then call decoded() multiple times — the
        // underlying OnceCell should mean only one decompression occurs.
        let plain = b"hello once";
        let body = Bytes::from(gzip_encode(plain));
        let v = RequestBodyView::from_raw(body, Encoding::Gzip);

        let p1: *const Bytes = v.decoded(64 * 1024).expect("ok 1");
        let p2: *const Bytes = v.decoded(64 * 1024).expect("ok 2");
        let p3: *const Bytes = v.decoded(64 * 1024).expect("ok 3");
        assert_eq!(p1, p2);
        assert_eq!(p2, p3);  // same Bytes instance → OnceCell init only once.
    }

    #[test]
    fn identity_encoding_returns_raw_clone() {
        let plain = b"plain payload";
        let v = RequestBodyView::from_raw(Bytes::from(&plain[..]), Encoding::Identity);
        assert_eq!(v.decoded(64 * 1024).unwrap().as_ref(), &plain[..]);
    }

    #[test]
    fn outbound_overrides_raw() {
        let mut v = RequestBodyView::from_raw(Bytes::from_static(b"raw"), Encoding::Identity);
        assert_eq!(v.outbound_or_raw().as_ref(), b"raw");
        v.set_outbound(Bytes::from_static(b"rewritten"));
        assert_eq!(v.outbound_or_raw().as_ref(), b"rewritten");
    }
}
```

- [ ] **Step 2: Add `flate2` as dev-dependency to gw-proxy if not present**

Check `crates/gw-proxy/Cargo.toml` `[dev-dependencies]`. If `flate2` missing, add:

```toml
[dev-dependencies]
flate2 = "1"
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p gw-proxy --lib body::view::tests`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/body/view.rs crates/gw-proxy/Cargo.toml Cargo.lock
git commit -m "test(gw-proxy): RequestBodyView OnceCell decoded() decompresses once"
```

### Task B.3: Add `ReqCtx.body` field alongside existing `cached_body`

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs:76-200`

- [ ] **Step 1: Read current `ReqCtx` to find the position of `cached_body`**

Run: `sed -n '95,105p' crates/gw-proxy/src/ctx.rs`

Confirm line 98: `pub cached_body: Option<Bytes>,`

- [ ] **Step 2: Add `pub body: Option<RequestBodyView>` immediately below cached_body**

Edit `crates/gw-proxy/src/ctx.rs`:

```rust
    pub cached_body: Option<Bytes>,
    /// Phase B: replaces `cached_body`. Three-state explicit lifecycle:
    /// raw / decoded (OnceCell) / outbound. `cached_body` remains during
    /// migration; final task in Phase B removes it.
    pub body: Option<crate::body::RequestBodyView>,
```

- [ ] **Step 3: Initialize `body: None` in the `Default` impl (or `new()` constructor)**

Find the `ReqCtx::default()` impl or `new()` (around `crates/gw-proxy/src/ctx.rs:330`). Add:

```rust
            cached_body: None,
            body: None,
```

- [ ] **Step 4: Build to verify**

Run: `cargo build -p gw-proxy`
Expected: clean build with no warnings (since `body` is `pub`, used only by tests).

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-proxy): ReqCtx.body field (RequestBodyView) parallel to cached_body"
```

### Task B.4: Populate `ctx.body` at body-read time and migrate HMAC verify to use it

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs:480-595` (body buffer + HMAC verify block)
- Modify: `crates/gw-proxy/src/gateway.rs:620-650` (`request_body_filter` body swap)

- [ ] **Step 1: Read the request_body_filter to find where `cached_body` is set**

Run: `sed -n '620,660p' crates/gw-proxy/src/gateway.rs`

- [ ] **Step 2: Find the body-frozen-at-EOS site and ALSO populate `ctx.body`**

In `request_body_filter` (gateway.rs), at the point where `ctx.cached_body = Some(...)` is set (frozen at end-of-stream):

```rust
// Existing:
ctx.cached_body = Some(body_bytes.clone());
// New (Phase B intermediate state — populate both):
ctx.body = Some(crate::body::RequestBodyView::from_raw(
    body_bytes.clone(),
    ctx.req_content_encoding,
));
```

- [ ] **Step 3: Migrate HMAC verify path to use `body.decoded(...)`**

In `crates/gw-proxy/src/filters/request.rs:480-595` (the HMAC verify block):

Replace the existing `body_verify::verify_body(input)` call's signature OR adapt the call so it uses `ctx.body.as_ref().unwrap().decoded(32 * 1024 * 1024)?` directly.

**Concrete change**: Before this block, add:

```rust
let body_view = ctx.body.as_ref().expect("request_body_filter populated ctx.body");
let decoded_bytes = body_view
    .decoded(32 * 1024 * 1024)
    .map_err(|e| anyhow::anyhow!("decompress failed: {:?}", e))?;
// `decoded_bytes` is &Bytes pointing at the (potentially) decompressed plaintext.
```

Then in the existing `body_verify::verify_body(input)` flow, where it currently passes `body: body_bytes.clone()` and `content_encoding: ctx.req_content_encoding`, change the signature OR pass `decoded_bytes.clone()` with `content_encoding: Encoding::Identity` (since it's already decoded).

After the success branch, **do not** write `ctx.cached_body = Some(decoded.clone())` anymore (the view already has it via OnceCell).

```rust
// REMOVE:
//   if !matches!(ctx.req_content_encoding, gw_codec::compression::Encoding::Identity) {
//       ctx.cached_body = Some(decoded.clone());
//   }
// (decoded already cached in ctx.body via OnceCell)
```

- [ ] **Step 4: Build + run tests**

Run: `cargo build -p gw-proxy && cargo test -p gw-proxy`
Expected: clean build; existing tests pass (HMAC verify code path unchanged in behavior).

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/filters/request.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-proxy): HMAC verify uses ctx.body.decoded() (Phase B step 1)"
```

### Task B.5: Migrate codec parse + upstream_request_filter to use body view

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs:600-700` (codec parse step 4)
- Modify: `crates/gw-proxy/src/filters/upstream.rs:286,728-797` (passthrough + cross-protocol render)

- [ ] **Step 1: Codec parse in request_filter — use decoded body via view**

In `crates/gw-proxy/src/filters/request.rs:600-700` (step 4: `codec.parse_request(&body_bytes)`), replace `body_bytes` reference with:

```rust
let parse_bytes = ctx
    .body
    .as_ref()
    .expect("ctx.body populated")
    .decoded(32 * 1024 * 1024)
    .map_err(|e| /* existing decompress error envelope */)?;
let request_ir = codec.parse_request(parse_bytes.as_ref())?;
```

The OnceCell guarantees this reuses HMAC's decompression result.

- [ ] **Step 2: Upstream filter — same_protocol_passthrough takes outbound_or_raw**

In `crates/gw-proxy/src/filters/upstream.rs:728-797`, replace:

```rust
let cached_body = ctx
    .cached_body
    .as_ref()
    .map(|b| b.as_ref())
    .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_cached_body")))?;
```

With:

```rust
let view = ctx
    .body
    .as_ref()
    .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_body_view")))?;
let body_bytes = view.outbound_or_raw().as_ref();
```

And in branches that emit a rewritten body (after `same_protocol_passthrough` or cross-protocol render), instead of:

```rust
// OLD:
//   let rewritten: Bytes = ...;
//   ctx.cached_body = Some(rewritten);
```

write:

```rust
let rewritten: Bytes = /* serialize result */;
if let Some(view) = ctx.body.as_mut() {
    view.set_outbound(rewritten);
}
```

- [ ] **Step 3: request_body_filter — swap in outbound (gateway.rs:620-650)**

The chunked-body filter swaps `ctx.cached_body` at EOS. Change to read from `ctx.body.outbound_or_raw()`:

```rust
// Old: if let Some(rewritten) = ctx.cached_body.as_ref() { ... }
if let Some(view) = ctx.body.as_ref() {
    let body = view.outbound_or_raw();
    // existing logic that writes `body` into the upstream session goes here
}
```

- [ ] **Step 4: Strategy `message_hash` (`workflow.md:123` mentions `body_hash_u64` over `ctx.cached_body`)**

Find: `rg -n "body_hash_u64|cached_body" crates/gw-proxy/src/`

Update the strategy `PickCtx` build site to hash `ctx.body.as_ref().unwrap().raw()` instead of `ctx.cached_body`. The semantic is "hash the on-wire raw body" so use `raw()` not `outbound_or_raw()`.

- [ ] **Step 5: Build + run tests**

Run: `cargo build -p gw-proxy && cargo test -p gw-proxy`
Expected: clean build; all tests pass. If a test fails because it directly populated `ctx.cached_body`, update the test to populate `ctx.body` instead.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-proxy/src/filters/ crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-proxy): codec parse + upstream filter read body via ctx.body (Phase B step 2)"
```

### Task B.6: Delete `cached_body` field

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs` (remove field)
- Modify: any remaining file that references `cached_body`

- [ ] **Step 1: Verify all remaining `cached_body` references are removable**

Run: `rg -n 'cached_body' crates/gw-proxy/src/ docs/`

Expected list at this point: only the field definition + `Default::default()` init + possibly stale doc-comments.

- [ ] **Step 2: Remove `cached_body` field from `ReqCtx`**

In `crates/gw-proxy/src/ctx.rs:98`, delete the line:

```rust
pub cached_body: Option<Bytes>,
```

And in the `Default` / `new()` impl, delete:

```rust
cached_body: None,
```

- [ ] **Step 3: Delete any remaining doc references in code**

Run: `rg -n 'cached_body' crates/gw-proxy/src/` again. If any remain (e.g., comments mentioning "swap cached_body"), update to "body view".

- [ ] **Step 4: Build + test**

Run: `cargo build -p gw-proxy && cargo test -p gw-proxy && cargo clippy -p gw-proxy -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/
git commit -m "refactor(gw-proxy): remove cached_body field (Phase B final)"
```

### Task B.7: Add `no_cached_body` invariant to check-invariants.sh

**Files:**
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: Identify a similar existing lint block to copy style**

Run: `grep -nE '^echo "\[no_' scripts/check-invariants.sh | head -5`

- [ ] **Step 2: Add the `no_cached_body` lint after the last similar block**

Append:

```bash
echo "[no_cached_body] ctx.cached_body removed in Phase B (2026-05-28); use ctx.body (RequestBodyView) instead"
if rg -n 'cached_body' --type rust crates/ >/dev/null 2>&1; then
    echo "  FAIL: 'cached_body' references found:"
    rg -n 'cached_body' --type rust crates/ | sed 's/^/    /'
    fail=1
else
    echo "  ok"
fi
```

- [ ] **Step 3: Run the script to verify it passes**

Run: `bash scripts/check-invariants.sh 2>&1 | grep -E 'no_cached_body|^\[' | head`
Expected: `[no_cached_body] ...` followed by `  ok`.

- [ ] **Step 4: Commit**

```bash
git add scripts/check-invariants.sh
git commit -m "ci(invariants): add no_cached_body lint (Phase B)"
```

### Task B.8: E2E verify — gzip + HMAC request decompresses only once

**Files:**
- Modify or create: `tests/e2e/client/scenarios/test_hmac_signature.py`

- [ ] **Step 1: Find an existing HMAC signature scenario**

Run: `rg -l 'signature|hmac' tests/e2e/client/scenarios/ -i | head -3`

If none, create `test_hmac_signature.py` with a basic gzip + HMAC happy path.

- [ ] **Step 2: Add a test that asserts only one decompress event in tracing log**

If the gateway exports a `gw_codec_decompress_total` or similar metric, assert it increments by 1 per request. Otherwise rely on tracing output (capture via env var `RUST_LOG=trace`).

Pattern:

```python
def test_gzip_signed_request_decompresses_once(gateway, fake_upstream, metrics_client):
    pre = metrics_client.counter_value("gw_codec_decompress_total")  # if exists
    body = gzip(json.dumps({...}).encode())
    headers = sign_hmac(body, secret, ts=...)
    headers["Content-Encoding"] = "gzip"
    resp = gateway.post("/v1/...", headers=headers, data=body)
    assert resp.status_code == 200
    post = metrics_client.counter_value("gw_codec_decompress_total")
    assert post - pre == 1, "should decompress exactly once per gzip+signed request"
```

If no `gw_codec_decompress_total` metric exists, skip the metric check and rely on the unit test in B.2 + manual inspection.

- [ ] **Step 3: Run the e2e**

Run: `cd tests/e2e/client && pytest scenarios/test_hmac_signature.py -v`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/client/scenarios/test_hmac_signature.py
git commit -m "test(e2e): gzip + HMAC signed request decompresses once (Phase B end)"
```

---

## Phase C — ManagedLoop + StreamConsumer

**Outcome:** `ManagedLoop` trait + `Supervisor` lives in gw-store. `StreamConsumer` is the first impl; NOGROUP recovers automatically.

### Task C.1: Create `managed_loop.rs` skeleton

**Files:**
- Create: `crates/gw-store/src/managed_loop.rs`
- Modify: `crates/gw-store/src/lib.rs`

- [ ] **Step 1: Create the module file with the trait + types**

```rust
//! ManagedLoop — self-healing background-loop contract.
//!
//! See `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §5.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

#[async_trait]
pub trait ManagedLoop: Send + Sync + 'static {
    /// metric label + log target.
    fn name(&self) -> &'static str;

    /// Idempotent ensure call. Invoked once at start, and again whenever
    /// `tick()` returns `Outcome::Recover`. Default is a no-op for workers
    /// that have no startup-time resources.
    async fn ensure(&self) -> Result<(), anyhow::Error> {
        Ok(())
    }

    /// One business tick. `Outcome` directs the supervisor on what to do next.
    async fn tick(&self) -> Outcome;
}

pub enum Outcome {
    /// Loop immediately into next tick.
    Continue,
    /// Sleep, then loop.
    Sleep(Duration),
    /// External resource gone; supervisor will call `ensure()` then loop.
    Recover(RecoverReason),
    /// Unrecoverable; supervisor will observe + exit.
    Fatal(anyhow::Error),
}

pub enum RecoverReason {
    UpstreamResourceGone(&'static str),
    Other(&'static str),
}

pub struct Supervisor;

impl Supervisor {
    pub fn spawn<L: ManagedLoop>(l: Arc<L>) -> JoinHandle<()> {
        let name = l.name();
        tokio::spawn(async move {
            if let Err(e) = l.ensure().await {
                gw_core::observability::observe(gw_core::observability::ErrorObservation {
                    code: "ensure_failed",
                    kind: gw_core::observability::ErrorKind::BackgroundWorker,
                    stage: gw_core::observability::Stage::Background,
                    status: None,
                    trace_id: None,
                    extra: Some(&(name, format!("{:?}", e))),
                });
                // Continue into tick loop — next Recover will re-ensure.
            }
            loop {
                match l.tick().await {
                    Outcome::Continue => continue,
                    Outcome::Sleep(d) => tokio::time::sleep(d).await,
                    Outcome::Recover(reason) => {
                        let detail: &dyn std::fmt::Debug = match &reason {
                            RecoverReason::UpstreamResourceGone(s) => s,
                            RecoverReason::Other(s) => s,
                        };
                        gw_core::observability::observe(
                            gw_core::observability::ErrorObservation {
                                code: "recover",
                                kind: gw_core::observability::ErrorKind::BackgroundWorker,
                                stage: gw_core::observability::Stage::Background,
                                status: None,
                                trace_id: None,
                                extra: Some(&(name, detail)),
                            },
                        );
                        // ensure() failure does NOT exit; next Recover will retry.
                        let _ = l.ensure().await;
                    }
                    Outcome::Fatal(e) => {
                        gw_core::observability::observe(
                            gw_core::observability::ErrorObservation {
                                code: "fatal",
                                kind: gw_core::observability::ErrorKind::BackgroundWorker,
                                stage: gw_core::observability::Stage::Background,
                                status: None,
                                trace_id: None,
                                extra: Some(&(name, format!("{:?}", e))),
                            },
                        );
                        break;
                    }
                }
            }
        })
    }
}
```

- [ ] **Step 2: Add `pub mod managed_loop;` to `crates/gw-store/src/lib.rs`**

- [ ] **Step 3: Build**

Run: `cargo build -p gw-store`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-store/src/managed_loop.rs crates/gw-store/src/lib.rs
git commit -m "feat(gw-store): ManagedLoop trait + Supervisor (NOGROUP fix scaffold)"
```

### Task C.2: Unit test — Supervisor handles Recover by calling ensure() again

**Files:**
- Modify: `crates/gw-store/src/managed_loop.rs` (add `#[cfg(test)]`)

- [ ] **Step 1: Write a mock ManagedLoop with controllable tick outcomes**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockLoop {
        ensure_calls: AtomicU32,
        tick_calls: AtomicU32,
        recover_after: u32,
    }

    #[async_trait]
    impl ManagedLoop for MockLoop {
        fn name(&self) -> &'static str { "mock" }

        async fn ensure(&self) -> Result<(), anyhow::Error> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn tick(&self) -> Outcome {
            let n = self.tick_calls.fetch_add(1, Ordering::SeqCst);
            if n == self.recover_after {
                Outcome::Recover(RecoverReason::UpstreamResourceGone("test_NOGROUP"))
            } else if n > self.recover_after + 1 {
                Outcome::Fatal(anyhow::anyhow!("test done"))
            } else {
                Outcome::Sleep(Duration::from_millis(1))
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_calls_ensure_on_recover() {
        let l = Arc::new(MockLoop {
            ensure_calls: AtomicU32::new(0),
            tick_calls: AtomicU32::new(0),
            recover_after: 2,
        });
        let handle = Supervisor::spawn(l.clone());
        // Wait for the supervisor's loop to exit via Fatal.
        let _ = handle.await;
        // ensure called once at start + once on recover = 2.
        assert_eq!(l.ensure_calls.load(Ordering::SeqCst), 2);
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p gw-store managed_loop::tests::supervisor_calls_ensure_on_recover`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/gw-store/src/managed_loop.rs
git commit -m "test(gw-store): Supervisor calls ensure() on Recover outcome"
```

### Task C.3: Refactor StreamConsumer to extract `ensure()` body

**Files:**
- Modify: `crates/gw-store/src/stream_consumer.rs:36-156`

- [ ] **Step 1: Move the existing `xgroup_create` calls from `start()` into a separate `ensure()`-shaped helper**

Refactor `crates/gw-store/src/stream_consumer.rs:36-51` so the body that creates consumer groups is in its own private method on `StreamConsumer`:

```rust
impl StreamConsumer {
    /// Idempotent: create both consumer groups, tolerating BUSYGROUP.
    async fn ensure_groups(&self) -> Result<(), anyhow::Error> {
        let client = self.redis.pool.next();
        let _: Result<(), _> = client
            .xgroup_create(ENTITY_STREAM, &*self.instance_id, "$", true)
            .await;
        let _: Result<(), _> = client
            .xgroup_create(COUNTER_STREAM, &*self.instance_id, "$", true)
            .await;
        Ok(())
    }

    pub async fn start(self: Arc<Self>) -> anyhow::Result<JoinHandle<()>> {
        // Will be replaced in Task C.5 with Supervisor::spawn(self).
        self.ensure_groups().await?;
        let me = self.clone();
        Ok(tokio::spawn(async move { me.consume_loop().await }))
    }
    // ... existing consume_loop unchanged for now ...
}
```

- [ ] **Step 2: Build + test**

Run: `cargo build -p gw-store && cargo test -p gw-store`
Expected: clean build, existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/gw-store/src/stream_consumer.rs
git commit -m "refactor(gw-store): extract StreamConsumer::ensure_groups()"
```

### Task C.4: Implement `ManagedLoop` for `StreamConsumer`

**Files:**
- Modify: `crates/gw-store/src/stream_consumer.rs`

- [ ] **Step 1: Add NOGROUP detection helper**

Above `impl StreamConsumer`:

```rust
fn is_nogroup_error(e: &fred::error::Error) -> bool {
    // fred surfaces NOGROUP as RedisError with "NOGROUP" prefix.
    let s = e.to_string();
    s.contains("NOGROUP")
}
```

(If `fred::error::Error` is a different type, adapt. Check by running `cargo doc -p fred` or by looking at the current error pattern in `consume_loop`.)

- [ ] **Step 2: Implement `ManagedLoop` for `StreamConsumer`**

Add at the bottom of the file (before the `#[cfg(test)]` mods):

```rust
use crate::managed_loop::{ManagedLoop, Outcome, RecoverReason};

#[async_trait::async_trait]
impl ManagedLoop for StreamConsumer {
    fn name(&self) -> &'static str { "stream_consumer" }

    async fn ensure(&self) -> Result<(), anyhow::Error> {
        self.ensure_groups().await
    }

    async fn tick(&self) -> Outcome {
        let consumer_name = format!("{}-consumer", self.instance_id);
        let client = self.redis.pool.next();
        let raw = client
            .xreadgroup(
                &*self.instance_id,
                &consumer_name,
                Some(COUNT_PER_POLL),
                Some(BLOCK_MS),
                false,
                vec![ENTITY_STREAM, COUNTER_STREAM],
                vec![">", ">"],
            )
            .await;

        let value = match raw {
            Ok(v) => v,
            Err(e) if is_nogroup_error(&e) => {
                return Outcome::Recover(RecoverReason::UpstreamResourceGone("NOGROUP"));
            }
            Err(_e) => {
                return Outcome::Sleep(Duration::from_millis(500));
            }
        };

        if value.is_null() {
            return Outcome::Continue;
        }
        if let fred::types::RedisValue::Array(ref a) = value {
            if a.is_empty() {
                return Outcome::Continue;
            }
        }

        let entries: fred::types::XReadResponse<String, String, String, String> =
            match value.into_xread_response() {
                Ok(m) => m,
                Err(_e) => {
                    return Outcome::Sleep(Duration::from_millis(500));
                }
            };

        for (stream, stream_entries) in entries {
            for (entry_id, fields) in stream_entries {
                self.apply(&stream, &fields).await;
                let _: Result<u64, _> = client
                    .xack(stream.as_str(), &*self.instance_id, entry_id.clone())
                    .await;
            }
        }
        Outcome::Continue
    }
}
```

- [ ] **Step 3: Build + run existing tests**

Run: `cargo build -p gw-store && cargo test -p gw-store`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-store/src/stream_consumer.rs
git commit -m "feat(gw-store): impl ManagedLoop for StreamConsumer (NOGROUP → Recover)"
```

### Task C.5: Switch StreamConsumer entry to Supervisor

**Files:**
- Modify: `crates/gw-store/src/stream_consumer.rs:36-51` (`start()`)
- Modify: any call site of `StreamConsumer::start()` (search for it)

- [ ] **Step 1: Find `StreamConsumer::start` call sites**

Run: `rg -n 'StreamConsumer::start|consumer\.start\(' crates/`

Typical site: `crates/gw-gateway/src/lib.rs` boot sequence.

- [ ] **Step 2: Replace `start()` body with Supervisor::spawn**

```rust
pub fn start(self: Arc<Self>) -> JoinHandle<()> {
    use crate::managed_loop::Supervisor;
    Supervisor::spawn(self)
}
```

Note: `start()` is no longer `async` and no longer returns `Result` — Supervisor handles ensure failures internally via observe(). Callers must drop the `?` and `.await`.

- [ ] **Step 3: Update call sites**

In `crates/gw-gateway/src/lib.rs` (boot path), change:

```rust
// Old: let handle = consumer.start().await?;
let handle = consumer.start();
```

- [ ] **Step 4: Delete the now-orphaned `consume_loop()` method**

```rust
// Remove the entire `async fn consume_loop(&self)` block.
```

- [ ] **Step 5: Build + test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-store/ crates/gw-gateway/
git commit -m "feat: switch StreamConsumer to Supervisor::spawn (NOGROUP self-heals)"
```

### Task C.6: E2E — Redis FLUSHALL triggers NOGROUP recovery

**Files:**
- Create or modify: `tests/e2e/client/scenarios/test_stream_consumer_recovery.py`

- [ ] **Step 1: Write a scenario that flushes Redis mid-test and re-publishes an entity event**

```python
import time

def test_stream_consumer_recovers_from_nogroup(
    gateway, redis_client, metrics_client, control_plane
):
    # Pre-condition: gateway running, StreamConsumer attached.
    pre_recover = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "background_worker", "code": "recover"},
    )

    # Step 1: Publish an entity event, verify it's consumed (cache invalidate).
    control_plane.invalidate_entity("apikey", "ak-test-1")
    time.sleep(0.5)
    # (assert cache invalidate succeeded — exact assertion depends on test fixtures)

    # Step 2: FLUSHALL Redis — this destroys both consumer groups.
    redis_client.flushall()
    time.sleep(0.2)

    # Step 3: Publish another entity event.
    control_plane.invalidate_entity("apikey", "ak-test-2")

    # Step 4: Within 2 seconds, recovery should occur (NOGROUP -> ensure
    # rebuilds groups -> next tick consumes the event).
    deadline = time.time() + 5
    while time.time() < deadline:
        post = metrics_client.counter_value(
            "gw_errors_total",
            labels={"kind": "background_worker", "code": "recover"},
        )
        if post > pre_recover:
            return  # PASS: recovery observed
        time.sleep(0.3)
    raise AssertionError("StreamConsumer did not recover after Redis FLUSHALL")
```

- [ ] **Step 2: Run the e2e**

Run: `cd tests/e2e/client && pytest scenarios/test_stream_consumer_recovery.py -v`
Expected: PASS within ~5 s.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e/client/scenarios/test_stream_consumer_recovery.py
git commit -m "test(e2e): StreamConsumer recovers from Redis FLUSHALL (NOGROUP)"
```

---

## Phase D — wire_enum_lock CI test

**Outcome:** `cargo test -p gw-core --test wire_enum_lock` locks both wire forms of `ProtocolKind` and `ApiShape`. Drift on either side fails CI.

### Task D.1: Create `wire_enum_lock.rs`

**Files:**
- Create: `crates/gw-core/tests/wire_enum_lock.rs`

- [ ] **Step 1: Create the test file with both ProtocolKind locks**

```rust
//! Locks ProtocolKind / ApiShape wire forms against silent drift.
//!
//! Two wire forms are intentional cross-process contract — see
//! `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §6.

use gw_core::protocol::{ApiShape, ProtocolKind};

#[test]
fn protocol_kind_serde_wire_form_locked() {
    // Go ↔ Rust entity payload JSONB contract.
    // DO NOT change these strings — Go writes them into PG.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi, "open_ai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini, "gemini"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        let got_str = got.as_str().expect("ProtocolKind serializes as string");
        assert_eq!(
            got_str, *expected,
            "{:?}: serde wire form drift; got '{}', contract baseline '{}'. \
             entity payload JSONB in PG depends on this — do NOT change.",
            v, got_str, expected,
        );
    }
}

#[test]
fn protocol_kind_as_str_wire_form_locked() {
    // events:entity stream + model_mapping.protocol TEXT column contract.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi, "openai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini, "gemini"),
    ];
    for (v, expected) in cases {
        assert_eq!(
            v.as_str(),
            *expected,
            "{:?}: as_str wire form drift; events stream + model_mapping.protocol depend on this.",
            v,
        );
    }
}

#[test]
fn api_shape_serde_wire_form_locked() {
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat, "open_ai_chat"),
        (OpenAiResponses, "open_ai_responses"),
        (AnthropicMessages, "anthropic_messages"),
        (GeminiGenerate, "gemini_generate"),
        (ClaudeCli, "claude_cli"),
        (OpenAiCli, "open_ai_cli"),
        (OpenAiCompact, "open_ai_compact"),
        (GeminiCli, "gemini_cli"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        assert_eq!(
            got.as_str().unwrap(),
            *expected,
            "{:?}: ApiShape serde wire drift",
            v,
        );
    }
}

#[test]
fn api_shape_format_id_wire_form_locked() {
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat, "openai:chat"),
        (OpenAiResponses, "openai:responses"),
        (AnthropicMessages, "anthropic:messages"),
        (GeminiGenerate, "gemini:generate"),
        (ClaudeCli, "claude:cli"),
        (OpenAiCli, "openai:cli"),
        (OpenAiCompact, "openai:compact"),
        (GeminiCli, "gemini:cli"),
    ];
    for (v, expected) in cases {
        assert_eq!(v.format_id(), *expected, "{:?}: format_id drift", v);
    }
}
```

- [ ] **Step 2: Run the lock test**

Run: `cargo test -p gw-core --test wire_enum_lock -- --nocapture`
Expected: **4 passed**. (If any fail, spec §6.1 baseline is out of date — verify against current codebase before editing the test.)

- [ ] **Step 3: Commit**

```bash
git add crates/gw-core/tests/wire_enum_lock.rs
git commit -m "test(gw-core): wire_enum_lock locks ProtocolKind/ApiShape baselines"
```

### Task D.2: Register `wire_enum_lock` in invariants script

**Files:**
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: Append the invariant block**

```bash
echo "[wire_enum_lock] ProtocolKind / ApiShape wire forms locked"
if cargo test -p gw-core --test wire_enum_lock --quiet >/dev/null 2>&1; then
    echo "  ok"
else
    echo "  FAIL: wire enum drift — see docs/superpowers/specs/2026-05-28-systemic-fixes-design.md §6"
    fail=1
fi
```

- [ ] **Step 2: Run the script**

Run: `bash scripts/check-invariants.sh 2>&1 | grep -A1 wire_enum_lock`
Expected: `[wire_enum_lock] ...` followed by `  ok`.

- [ ] **Step 3: Commit**

```bash
git add scripts/check-invariants.sh
git commit -m "ci(invariants): register wire_enum_lock"
```

---

## Phase E — fail_to_proxy override

**Outcome:** Pingora's `fail_to_proxy` is overridden. If `ctx.body_started == false` and the error is fatal, an `ErrorIR`-shaped JSON envelope is written to the session response. Mid-stream failures observe + truncate without injecting a synthetic SSE event.

### Task E.1: Override `fail_to_proxy` in `gateway.rs`

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs` (`ProxyHttp for Gateway` impl)

- [ ] **Step 1: Inspect the existing `ProxyHttp` impl to find where to add the override**

Run: `rg -n 'async fn (request_filter|fail_to_connect|logging)' crates/gw-proxy/src/gateway.rs | head`

Place the new method **between `response_body_filter` and `fail_to_connect`** to match Pingora's trait order convention.

- [ ] **Step 2: Add the method**

```rust
async fn fail_to_proxy(
    &self,
    session: &mut Session,
    e: &pingora::Error,
    ctx: &mut Self::CTX,
) -> pingora_proxy::FailToProxy {
    use pingora::ErrorType::*;
    use pingora::ErrorSource;
    use std::borrow::Cow;
    use gw_core::error::{codes, ErrorCategory, ErrorIR};

    // Classify the error -> (gateway code, http status).
    let (gw_code, http_status): (&'static str, u16) = match e.etype() {
        HTTPStatus(code) => (codes::GW_UPSTREAM_PROTOCOL, *code),
        _ => match e.esource() {
            ErrorSource::Upstream => {
                if ctx.body_started {
                    (codes::GW_UPSTREAM_TRUNCATED, 0)  // body already sent; cannot respond.
                } else {
                    (codes::GW_BACKEND_UNAVAILABLE, 502)
                }
            }
            ErrorSource::Downstream => match e.etype() {
                WriteError | ReadError | ConnectionClosed => (codes::GW_CLIENT_DISCONNECTED, 0),
                _ => (codes::GW_BAD_REQUEST, 400),
            },
            ErrorSource::Internal | ErrorSource::Unset => (codes::GW_GATEWAY_INTERNAL, 500),
        },
    };

    let kind = if ctx.body_started {
        gw_core::observability::ErrorKind::UpstreamMidStream
    } else {
        gw_core::observability::ErrorKind::UpstreamConnect
    };

    gw_core::observability::observe(gw_core::observability::ErrorObservation {
        code: gw_code,
        kind,
        stage: gw_core::observability::Stage::Upstream,
        status: if http_status > 0 { Some(http_status) } else { None },
        trace_id: Some(ctx.trace_id.as_str()),
        extra: None,
    });

    // Body not started + http_status > 0: render envelope.
    if http_status > 0 && !ctx.body_started {
        let envelope = ErrorIR {
            category: ErrorCategory::Upstream,
            status: http_status,
            upstream_status: None,
            code: Cow::Borrowed(gw_code),
            message: Cow::Borrowed("upstream unavailable"),
            retryable: false,
            upstream_raw: None,
            request_id: ctx.request_id.as_str(),
            retry_after_secs: None,
        };
        // Serialize and write to session (use existing helper if available).
        if let Ok(body) = serde_json::to_vec(&envelope) {
            let mut resp = pingora::http::ResponseHeader::build(http_status, None).unwrap();
            let _ = resp.insert_header("content-type", "application/json");
            let _ = resp.insert_header("content-length", body.len().to_string());
            let _ = session.write_response_header(Box::new(resp)).await;
            let _ = session
                .write_response_body(Some(bytes::Bytes::from(body)), true)
                .await;
        }
    }

    pingora_proxy::FailToProxy {
        error_code: http_status,
        can_reuse_downstream: false,
    }
}
```

**Verify first:** `codes::GW_UPSTREAM_TRUNCATED`, `GW_BACKEND_UNAVAILABLE`, `GW_CLIENT_DISCONNECTED`, `GW_BAD_REQUEST`, `GW_GATEWAY_INTERNAL` must exist in `crates/gw-core/src/error/codes.rs`. Check with:

Run: `rg -n 'pub const GW_(UPSTREAM_TRUNCATED|BACKEND_UNAVAILABLE|CLIENT_DISCONNECTED|BAD_REQUEST|GATEWAY_INTERNAL)' crates/gw-core/src/error/codes.rs`

If `GW_UPSTREAM_TRUNCATED` doesn't exist, add it as a `pub const ... = "gw_upstream_truncated";` line in codes.rs in the same commit.

- [ ] **Step 3: Build**

Run: `cargo build -p gw-proxy`
Expected: clean build. Address any Pingora signature mismatches (the trait method signature in Pingora 0.8 must match exactly).

- [ ] **Step 4: Run existing tests**

Run: `cargo test -p gw-proxy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-core/src/error/codes.rs
git commit -m "feat(gw-proxy): fail_to_proxy override emits ErrorIR envelope + observe()"
```

### Task E.2: E2E — upstream close before body → client gets 502 + JSON envelope

**Files:**
- Modify or create: `tests/e2e/client/scenarios/test_upstream_failure.py`

- [ ] **Step 1: Configure fake-upstream to abort the TCP connection at handshake**

In a new test:

```python
def test_upstream_connect_failure_returns_json_envelope(
    gateway, fake_upstream, metrics_client
):
    pre = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "upstream_connect"},
    )
    # Configure fake-upstream to refuse all connections for the next request.
    fake_upstream.set_mode("refuse")
    resp = gateway.post("/v1/chat/completions", json={"model":"x","messages":[]})
    assert resp.status_code == 502
    body = resp.json()
    assert body["code"].startswith("gw_"), f"expected gateway code, got: {body}"
    post = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "upstream_connect"},
    )
    assert post == pre + 1
```

- [ ] **Step 2: Run**

Run: `cd tests/e2e/client && pytest scenarios/test_upstream_failure.py::test_upstream_connect_failure_returns_json_envelope -v`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e/client/scenarios/test_upstream_failure.py
git commit -m "test(e2e): upstream connect failure renders JSON envelope (Phase E)"
```

### Task E.3: E2E — mid-stream upstream close → metric +1 + truncation, no SSE error event

- [ ] **Step 1: Add a mid-stream failure scenario**

```python
def test_upstream_mid_stream_failure_truncates(gateway, fake_upstream, metrics_client):
    pre_mid = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "upstream_mid_stream"},
    )
    # Configure fake-upstream to send 3 SSE events then close.
    fake_upstream.set_mode("partial_stream", events=3)
    chunks = []
    with gateway.stream("POST", "/v1/chat/completions",
                        json={"model":"x","messages":[],"stream":True}) as r:
        for ch in r.iter_lines():
            chunks.append(ch)
    # We should have received some SSE chunks but NOT a "data: {\"error\":..." line.
    assert chunks, "expected partial stream"
    assert not any(c.startswith('data: {"error"') for c in chunks), \
        "gateway must not inject SSE error events on mid-stream upstream failure"
    post_mid = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "upstream_mid_stream"},
    )
    assert post_mid == pre_mid + 1
```

- [ ] **Step 2: Run**

Run: `cd tests/e2e/client && pytest scenarios/test_upstream_failure.py::test_upstream_mid_stream_failure_truncates -v`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e/client/scenarios/test_upstream_failure.py
git commit -m "test(e2e): mid-stream upstream close — metric +1, no synthetic SSE event"
```

---

## Phase F — codec / worker batch warn → observe migration

**Outcome:** All `tracing::warn!`/`tracing::error!` in codec parse paths and response_filter chunk handling route through `observe()`; `gw_errors_total{kind=codec_parse|codec_render}` populated.

### Task F.1: Migrate `crates/gw-codec/src/openai/codec.rs` warns

**Files:**
- Modify: `crates/gw-codec/src/openai/codec.rs:96,216-228`

- [ ] **Step 1: List the warn sites**

Run: `rg -nF 'tracing::warn!' crates/gw-codec/src/openai/codec.rs`

Each line is a warn → replace it with an `observe(...)` call that preserves the structured context.

- [ ] **Step 2: Replace each warn**

For the `parse_stream_chunk` malformed data warn (line 96):

```rust
// OLD: tracing::warn!(error = %e, "openai stream: malformed data payload");
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code: "openai_stream_malformed_data",
    kind: gw_core::observability::ErrorKind::CodecParse,
    stage: gw_core::observability::Stage::Response,
    status: None,
    trace_id: None,
    extra: Some(&e),
});
```

For the StreamEventIR dropped warns (lines 216, 219, 222, 225, 228):

```rust
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code: "openai_render_drop_unsupported_event",
    kind: gw_core::observability::ErrorKind::CodecRender,
    stage: gw_core::observability::Stage::Response,
    status: None,
    trace_id: None,
    extra: Some(&"Start"),  // or "Usage", "ThinkingDelta", etc. — one per call site
});
```

- [ ] **Step 3: Build + test**

Run: `cargo build -p gw-codec && cargo test -p gw-codec`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-codec/src/openai/codec.rs
git commit -m "refactor(gw-codec): openai codec warns → observe(CodecParse/CodecRender)"
```

### Task F.2: Migrate anthropic codec warns

**Files:**
- Modify: `crates/gw-codec/src/anthropic/codec.rs`

- [ ] **Step 1: List warns**

Run: `rg -n 'tracing::warn!|tracing::error!' crates/gw-codec/src/anthropic/codec.rs`

- [ ] **Step 2: Replace each with observe() using kind=CodecParse or CodecRender appropriately**

Use unique `code` strings like `"anthropic_stream_malformed_data"`, `"anthropic_render_drop_..."`.

- [ ] **Step 3: Build + test**

Run: `cargo build -p gw-codec && cargo test -p gw-codec`

- [ ] **Step 4: Commit**

```bash
git add crates/gw-codec/src/anthropic/codec.rs
git commit -m "refactor(gw-codec): anthropic codec warns → observe()"
```

### Task F.3: Migrate gemini codec warns

**Files:**
- Modify: `crates/gw-codec/src/gemini/codec.rs`

- [ ] **Step 1-4: Same pattern as F.1 / F.2**

```bash
git commit -m "refactor(gw-codec): gemini codec warns → observe()"
```

### Task F.4: Migrate `response_filter` chunk handling warns

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs:377-712`

- [ ] **Step 1: List warns in response.rs**

Run: `rg -n 'tracing::warn!|tracing::error!' crates/gw-proxy/src/filters/response.rs`

Major sites: line 380 (`stream parse error`), 431 (`cross-protocol stream render error`), 692, 712.

- [ ] **Step 2: Replace each**

```rust
// 380
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code: e.code.as_ref(),
    kind: gw_core::observability::ErrorKind::CodecParse,
    stage: gw_core::observability::Stage::Response,
    status: None,
    trace_id: Some(ctx.trace_id.as_str()),
    extra: None,
});

// 431
gw_core::observability::observe(gw_core::observability::ErrorObservation {
    code: e.code.as_ref(),
    kind: gw_core::observability::ErrorKind::CodecRender,
    stage: gw_core::observability::Stage::Response,
    status: None,
    trace_id: Some(ctx.trace_id.as_str()),
    extra: None,
});
```

Lines 692, 712: adapt similarly based on context.

- [ ] **Step 3: Build + test**

Run: `cargo build -p gw-proxy && cargo test -p gw-proxy`

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs
git commit -m "refactor(gw-proxy): response.rs chunk warns → observe(CodecParse/CodecRender)"
```

### Task F.5: E2E — inject malformed SSE chunk, verify metric

- [ ] **Step 1: Add e2e scenario**

```python
def test_malformed_sse_emits_metric(gateway, fake_upstream, metrics_client):
    pre = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "codec_parse"},
    )
    fake_upstream.set_mode("malformed_sse")  # send "data: not-json\n\n"
    with gateway.stream("POST", "/v1/chat/completions",
                        json={"model":"x","messages":[],"stream":True}) as r:
        list(r.iter_lines())  # drain
    post = metrics_client.counter_value(
        "gw_errors_total",
        labels={"kind": "codec_parse"},
    )
    assert post > pre, "malformed SSE should increment gw_errors_total{kind=codec_parse}"
```

- [ ] **Step 2: Run**

Run: `cd tests/e2e/client && pytest scenarios/test_upstream_failure.py::test_malformed_sse_emits_metric -v`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e/client/scenarios/test_upstream_failure.py
git commit -m "test(e2e): malformed SSE chunk increments gw_errors_total{kind=codec_parse}"
```

---

## Phase G — Documentation sync + final invariants

**Outcome:** `docs/architecture.md` and `docs/workflow.md` reflect the new state. `scripts/check-invariants.sh` clean. soak smoke green.

### Task G.1: Update `docs/architecture.md`

**Files:**
- Modify: `docs/architecture.md`

- [ ] **Step 1: Read architecture.md §Deliberate behaviours, §Known gaps, §Invariants**

Run: `rg -n '^## |^### ' docs/architecture.md | head -40`

- [ ] **Step 2: §Deliberate behaviours — add three new entries**

Append (or insert in appropriate position):

```markdown
- **Mid-stream upstream failure does not emit SSE error event.** `fail_to_proxy` (gateway.rs) observes via `gw_errors_total{kind=upstream_mid_stream}` and truncates the connection. Synthetic `data: {"error":...}` injection is intentionally avoided (each SDK's stream parser behaves differently). Spec: 2026-05-28-systemic-fixes-design.md §3.4.
- **`RequestBodyView` (`crates/gw-proxy/src/body/view.rs`) is the sole authority on request body bytes.** Three explicit states: `raw` (immutable on-wire), `decoded()` (`OnceCell`-cached lazy decompress), `outbound_or_raw()` (post-mutation outbound). `cached_body` field was deleted in Phase B of 2026-05-28-systemic-fixes. CI lint `no_cached_body` enforces.
- **`ProtocolKind` maintains two wire forms by design.** `serde` snake_case (`OpenAi → "open_ai"`) for entity payload JSONB (PG `vendor_keys.payload` etc, Go writes). `as_str()` (`OpenAi → "openai"`) for events stream payload and `model_mapping.protocol` TEXT column. Both forms locked in `crates/gw-core/tests/wire_enum_lock.rs`.
```

- [ ] **Step 3: §Known gaps — trim closed items**

Remove from §9.1: G7 follow-up (HMAC body reuse) line.
Remove from §10.5: "no fail_to_proxy envelope emitter", "malformed SSE silent drop", "NOGROUP recovery missing" entries. ProtocolKind item: change to "ProtocolKind dual wire form is by-design; locked by `wire_enum_lock` invariant".

- [ ] **Step 4: §Invariants — add three lint entries**

Append the matching invariants:

```markdown
- **`no_cached_body`** — `ctx.cached_body` was removed; use `ctx.body` (`RequestBodyView`). Phase B of 2026-05-28-systemic-fixes.
- **`wire_enum_lock`** — `ProtocolKind` / `ApiShape` serde + as_str + format_id wire forms locked against drift. See `crates/gw-core/tests/wire_enum_lock.rs`.
- **`managed_loop_advisory`** (warn) — long-loop `tokio::spawn` in gw-store/gw-ledger should use `ManagedLoop` + `Supervisor::spawn` (see `crates/gw-store/src/managed_loop.rs`). Warn-only during migration.
```

- [ ] **Step 5: Commit**

```bash
git add docs/architecture.md
git commit -m "docs(architecture): sync deliberate behaviours + known gaps for systemic fixes"
```

### Task G.2: Update `docs/workflow.md`

**Files:**
- Modify: `docs/workflow.md`

- [ ] **Step 1: Replace `cached_body` references with `body view` language**

Find and update lines 52, 53, 89, 96, 97, 123, 137, 143:

```diff
- | 4 | `upstream_request_filter` | `filters::upstream` | Codec rewrite → `ctx.cached_body`; vendor auth header injection. |
+ | 4 | `upstream_request_filter` | `filters::upstream` | Codec rewrite → `ctx.body.set_outbound(...)`; vendor auth header injection. |
```

Apply analogous edits to other lines mentioning `cached_body`.

- [ ] **Step 2: Add a new section "Error observability sink" describing the 4 paths**

```markdown
## Error observability sink

All non-trivial error events route through `gw_core::observability::observe()`:

| Path | When | Source |
|------|------|--------|
| Short-circuit | `emit_envelope_and_bail` in any filter | `kind=short_circuit`, `stage=request|upstream`|
| Upstream connect failure | `fail_to_connect` (gateway.rs) | `kind=upstream_connect` |
| Upstream proxy failure | `fail_to_proxy` (gateway.rs) | `kind=upstream_connect|upstream_mid_stream` |
| Codec parse / render error | gw-codec internal warns | `kind=codec_parse|codec_render` |
| Background worker recover/fatal | Supervisor in `gw-store/managed_loop.rs` | `kind=background_worker` |

`gw_errors_total{code,kind,stage}` counter populated on every observation. Mid-stream upstream failure deliberately does **not** inject a synthetic SSE error event (see `docs/architecture.md` §Deliberate behaviours).
```

- [ ] **Step 3: Commit**

```bash
git add docs/workflow.md
git commit -m "docs(workflow): replace cached_body references; add error observability sink section"
```

### Task G.3: Add `managed_loop_advisory` invariant (warn-mode)

**Files:**
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: Append the advisory lint**

```bash
echo "[managed_loop_advisory] long-loop tokio::spawn in gw-store/gw-ledger should use ManagedLoop"
# Whitelist: paths that are already-migrated or intentionally raw spawn.
WHITELIST_REGEX='managed_loop\.rs|stream_consumer\.rs.*Supervisor::spawn|identity_persist\.rs'
hits=$(rg -n 'tokio::spawn\(' crates/gw-store/src crates/gw-ledger/src 2>/dev/null \
       | grep -vE "$WHITELIST_REGEX" || true)
if [ -n "$hits" ]; then
    note "  WARN: non-ManagedLoop tokio::spawn in worker crates:"
    note "$hits" | sed 's/^/    /'
    note "  (advisory; not a hard failure during Phase C migration)"
else
    echo "  ok"
fi
```

- [ ] **Step 2: Run the script**

Run: `bash scripts/check-invariants.sh 2>&1 | tail -30`
Expected: All `[name] ok` lines, advisory may print warnings but won't increment `fail`.

- [ ] **Step 3: Commit**

```bash
git add scripts/check-invariants.sh
git commit -m "ci(invariants): managed_loop_advisory (warn-mode) for unmigrated long-loop workers"
```

### Task G.4: Soak smoke + final verification

- [ ] **Step 1: Run the full invariants script**

Run: `bash scripts/check-invariants.sh`
Expected: exit 0; no `FAIL:` lines.

- [ ] **Step 2: Run the full workspace test**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Run e2e smoke**

Run: `make e2e-soak-smoke`
Expected: clean exit; soak report shows `gw_errors_total{kind=*}` distribution; no new invariant violations.

- [ ] **Step 5: Final amend/squash if needed and push to PR branch**

The 7 phases are each independent PRs. Use `git log --oneline -20` to review commit boundaries before pushing.

```bash
git log --oneline -20
# Confirm phases are clean. Push the branch.
git push origin <branch>
```

---

## Self-Review Checklist (run before opening PRs)

- [ ] **Spec §3.2 (ErrorObservation/observe()):** implemented in Task A.1-A.4
- [ ] **Spec §3.3 integration points:** emit_envelope_and_bail (A.3), fail_to_connect (A.4), fail_to_proxy (E.1), codec warns (F.1-F.4), background worker via Supervisor (C.2)
- [ ] **Spec §3.4 mid-stream policy:** Task E.1 branches on `ctx.body_started`; E.3 e2e asserts no synthetic SSE event
- [ ] **Spec §4.2 RequestBodyView types:** Task B.1-B.2
- [ ] **Spec §4.3 migration path:** B.3-B.6 step-by-step
- [ ] **Spec §4.4 no_cached_body CI lint:** Task B.7
- [ ] **Spec §5.2 ManagedLoop types:** Task C.1-C.2
- [ ] **Spec §5.3 StreamConsumer client:** Task C.3-C.5
- [ ] **Spec §5.5 managed_loop_advisory lint (warn):** Task G.3
- [ ] **Spec §6.3 wire_enum_lock test:** Task D.1
- [ ] **Spec §6.4 invariants register:** Task D.2
- [ ] **Spec §10 doc sync:** Task G.1-G.2
- [ ] **No `cached_body` reference in code or comments:** invariant runs in G.4
- [ ] **No `unreachable!()` / `todo!()` / placeholder in PR diff:** clippy + invariants in G.4

If any line above is unchecked when reviewing the final PRs, go back and address it before merging.
