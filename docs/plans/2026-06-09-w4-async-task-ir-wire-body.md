# W4 Async Task IR Wire Body Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make async task submission enqueue a complete protocol IR envelope plus codec-rendered request bytes in `input_payload`, make the worker send `input_payload.ir_wire_body` verbatim when present, and record a complete response IR envelope plus rendered response bytes without letting task workers call `gw-adapter`.

**Architecture:** The gateway submit path owns request parse/render because `crates/gw-proxy` already has the detected client codec and already depends on `gw-codec`. W4 starts with a feasibility spike that proves the existing protocol IR can be serialized into task payload JSON and restored for request/response round trips without broad contract or API changes. If the spike is feasible, W4 implements the complete IR envelope; if it exposes a need for large `TaskContract`, `gw-codec`, load/watch, or API response contract changes, W4 stops and returns a decision packet to the user instead of silently falling back to a reduced raw-plus-wire envelope. `gw-task-runtime` may use `gw-codec` for response parse/render, but it must not depend on or call `gw-adapter`; worker request sending is byte transport only. Existing task payloads remain valid: if `input_payload.ir_wire_body` is absent, the worker keeps the current `input_payload.body` / whole-payload JSON logic.

**Tech Stack:** Rust 1.88 workspace, `gw-proxy`, `gw-task`, `gw-task-runtime`, `gw-codec::AnyCodec`, `ProtocolCodec`, Redis `TaskRecord` hash serialization, `reqwest`.

---

## Current Evidence

- `crates/gw-proxy/src/filters/task.rs:152-222` parses submit JSON into `input_payload` and stores it directly on `TaskRecord.input_payload`.
- `crates/gw-proxy/src/filters/request.rs:1004-1027` calls `submit_endpoint` after auth/body buffering and has access to `ctx.client_codec` / `ctx.client_shape`.
- `crates/gw-task-runtime/src/exec_loop.rs:276-308` resolves `upstream_url`, derives `upstream_body`, sends `reqwest.post(...).json(&body)`, reads text, and parses raw JSON.
- `crates/gw-task-runtime/src/exec_loop.rs:465-481` implements the old body fallback as `input_payload.body` or the whole payload.
- `crates/gw-task/src/types.rs:98-129` keeps task schema as a `TaskRecord` with arbitrary JSON `input_payload` and `output_payload`.
- `crates/gw-task/src/contract.rs:49-96` passes `TaskRecord` through `submit`, `claim`, and `settle`; no new trait method is needed for W4.
- `crates/gw-task/src/redis_contract.rs:261-295` persists record fields during submit; `crates/gw-task/src/redis_contract.rs:462-531` persists terminal `output_payload` during settle.
- `crates/gw-task/src/redis_contract.rs:659-735` round-trips `input_payload` and `output_payload` through Redis hash fields, so nested JSON keys can be added without adding Redis fields.
- `crates/gw-codec/src/any.rs:55-69`, `97-118`, and `159-180` expose `AnyCodec::detect`, `parse_request`, `render_request`, `parse_response`, and `render_response`.
- `crates/gw-codec/src/trait_def.rs:16-109` defines `ProtocolCodec`; W4 should use these methods instead of a new codec abstraction.
- `crates/gw-proxy/src/filters/upstream.rs:899-945` is the sync request precedent: parse IR, optionally mutate, render bytes through the upstream codec.
- `crates/gw-proxy/src/filters/response.rs:839-882` is the sync non-stream response precedent: parse response IR, then render normalized bytes.
- `docs/spec/08-async-tasks.md:38-40` documents the current raw worker body behavior.
- `docs/spec/08-async-tasks.md:68-70` and `docs/ROADMAP.md:70-73` list codec render of `ir_wire_body` as the remaining async-task gap.
- F1 authority from `docs/plans/2026-06-08-roadmap-implementation-program.md:45-49` and `docs/design/2026-06-09-roadmap-authority-and-scope.md:18-22`: task workers do not call adapters; task submission pre-renders wire bodies; workers ship bytes verbatim.

## File Structure

- Modify: `crates/gw-task/src/types.rs`
  - Add string constants and small helper functions for `ir_request`, `ir_response`, `ir_wire_body`, `ir_wire_format`, `ir_wire_content_type`, and response envelope keys. Keep them JSON-key helpers, not new `TaskRecord` fields.
- Modify: `crates/gw-task/src/contract.rs`
  - Add contract comments that `input_payload.ir_wire_body` is optional and that absence means legacy body behavior.
- Modify: `crates/gw-task/src/redis_contract.rs`
  - Add focused tests or helper assertions proving nested payload keys survive Redis serialization; do not add top-level Redis hash fields.
- Modify: `crates/gw-proxy/src/filters/task.rs`
  - Add submit-time request normalization using the already detected `AnyCodec`.
- Modify: `crates/gw-proxy/src/filters/request.rs`
  - Pass `ctx.client_codec.as_ref()` into `SubmitTaskRequest`.
- Modify: `crates/gw-task-runtime/Cargo.toml`
  - Add `gw-codec = { path = "../gw-codec" }`; do not add `gw-adapter`.
- Modify: `crates/gw-task-runtime/src/exec_loop.rs`
  - Send `input_payload.ir_wire_body` with `.body(bytes)` when present; otherwise keep current JSON fallback.
  - Parse/render response bytes with the recorded format and store a complete IR response envelope in `output_payload` when feasible.
- Modify: `crates/gw-proxy/tests/task_endpoints.rs`
  - Add submit-time render tests.
- Modify: `crates/gw-task/tests/types.rs`
  - Add payload helper tests.
- Modify: `crates/gw-task/tests/redis_contract.rs`
  - Extend ignored Redis round-trip coverage for nested payload keys.
- Modify: `crates/gw-task-runtime/tests/exec_loop.rs`
  - Add worker verbatim-body, complete IR response envelope, response parse/render, and legacy fallback tests.
- Modify or verify: task load/watch projection tests and docs
  - Confirm existing load/watch projections still return the expected raw payload shape, or update them explicitly if complete IR envelope projection is intentionally surfaced.
- Modify: `docs/spec/08-async-tasks.md`
  - Move W4 behavior from "Not built" to as-built once code is green.
- Modify: `docs/ROADMAP.md`
  - Remove the async `ir_wire_body` residual entry once code and spec are green.

## Contract Shape

Request payload keys:

```json
{
  "body": {"model": "gpt-4.1-mini", "messages": [{"role": "user", "content": "hi"}]},
  "ir_request": {"protocol": "openai:chat", "kind": "request", "value": {"model": "gpt-4.1-mini", "messages": [{"role": "user", "content": "hi"}]}},
  "ir_wire_format": "openai:chat",
  "ir_wire_content_type": "application/json",
  "ir_wire_body": "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
}
```

Worker request rule:

- If `input_payload.ir_wire_body` is a string, send its UTF-8 bytes exactly with `Content-Type: input_payload.ir_wire_content_type || application/json`.
- If `input_payload.ir_wire_body` is absent, use current behavior: send `input_payload.body` as JSON, or the whole `input_payload` as JSON.
- If `input_payload.ir_wire_body` is present but not a string, fail the task with `invalid task payload: ir_wire_body must be a string`; do not silently reinterpret malformed new records.
- If `input_payload.ir_request` is absent on an old record, do not fail the task; the worker's wire-send path only requires `ir_wire_body`.

Complete response IR envelope:

```json
{
  "body": {"id": "chatcmpl-task", "choices": [{"message": {"role": "assistant", "content": "done"}}]},
  "ir_response": {"protocol": "openai:chat", "kind": "response", "value": {"id": "chatcmpl-task", "choices": [{"message": {"role": "assistant", "content": "done"}}]}},
  "ir_wire_format": "openai:chat",
  "ir_wire_content_type": "application/json",
  "ir_wire_body": "{\"id\":\"chatcmpl-task\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"done\"}}]}"
}
```

- `body` keeps the current raw JSON payload semantics for backward-compatible operator visibility and provider id/status extraction.
- `ir_response` stores the complete protocol response IR envelope when `gw-codec` can parse the terminal response with the recorded format.
- `ir_wire_body` stores the response rendered from that IR with the recorded format.
- Response parse/render failure must not fail an otherwise successful upstream task; store raw `body` only and emit a debug or warn log with no key-level labels.

用户决策已记录：完整 IR 优先

The user direction is to prefer a complete IR envelope because missing complete IR makes future protocol conversion harder. W4 therefore starts with a feasibility spike and, if implementation is not large, implements complete request/response IR envelopes. If the spike shows complete IR requires broad contract/API changes, W4 must stop and return that finding to the user for a decision; it must not quietly implement only a reduced raw-plus-wire envelope.

## Boundary Rules

- Do not add `gw-adapter` to `crates/gw-task-runtime/Cargo.toml`.
- Do not call `AdapterRegistry`, `OutboundAdapter`, or any `gw-adapter` type from task worker code.
- Do not route or select vendor keys in W4. Current task submit has no live routing result; W4 renders through the submit path's detected client codec and stores a format string. W5/W7 adapter work can later decide how adapter-rendered bytes are produced for vendor-specific transforms.
- Do not remove `input_payload.body`; it is the backward-compatible fallback and an operator-visible raw payload.
- Do not change the `TaskContract` trait method set.
- Do not proceed with a reduced envelope if complete IR requires large-scale contract or API changes; stop after the feasibility spike and return a user decision packet.

### Task 0: Complete IR Envelope Feasibility Spike

**Files:**
- Inspect: `crates/gw-codec/src`
- Inspect: `crates/gw-task/src/types.rs`
- Inspect: task load/watch endpoint code and tests
- Produce, if blocked: a decision packet in this plan's handoff notes instead of implementation code

- [ ] **Step 1: Prove request IR can be stored**

Confirm that the request IR returned by `AnyCodec::parse_request` can be converted into stable JSON with the existing Rust types or a small local envelope adapter. The spike must prove an `openai:chat` request can round-trip as:

```text
wire request bytes -> codec request IR -> input_payload.ir_request JSON -> restored IR -> codec render_request -> wire request bytes
```

- [ ] **Step 2: Prove response IR can be stored**

Confirm that the response IR returned by `AnyCodec::parse_response` can be converted into stable JSON with the existing Rust types or a small local envelope adapter. The spike must prove an `openai:chat` response can round-trip as:

```text
wire response bytes -> codec response IR -> output_payload.ir_response JSON -> restored IR -> codec render_response -> wire response bytes
```

- [ ] **Step 3: Check load/watch projection impact**

Inspect task load/watch response projection and tests. If current projection returns `output_payload` unchanged, add or update tests to prove the complete IR envelope does not break existing consumers that read `output_payload.body`. If current projection unwraps or reshapes payloads, make that planned projection change explicit before implementation.

- [ ] **Step 4: Decide execution path**

If Steps 1-3 require only local helper functions, serde derives already available on IR types, or small envelope adapters, continue with Tasks 1-7 and implement complete IR envelopes. If they require broad changes to `TaskContract`, public task APIs, codec trait signatures, or client-facing load/watch contracts, stop and return the exact blocker set to the user for decision.

### Task 1: Lock Payload Key Helpers

**Files:**
- Modify: `crates/gw-task/src/types.rs:98-129`
- Test: `crates/gw-task/tests/types.rs:34-73`

- [ ] **Step 1: Write failing helper tests**

Append these tests to `crates/gw-task/tests/types.rs`:

```rust
#[test]
fn test_ir_wire_body_helper_reads_optional_string() {
    let payload = json!({
        "body": {"model": "gpt-4.1-mini"},
        "ir_wire_format": "openai:chat",
        "ir_wire_content_type": "application/json",
        "ir_wire_body": "{\"model\":\"gpt-4.1-mini\"}"
    });

    assert_eq!(
        gw_task::task_ir_wire_body(&payload),
        Some("{\"model\":\"gpt-4.1-mini\"}")
    );
    assert_eq!(gw_task::task_ir_wire_format(&payload), Some("openai:chat"));
    assert_eq!(
        gw_task::task_ir_wire_content_type(&payload),
        Some("application/json")
    );
}

#[test]
fn test_ir_wire_body_helper_treats_absence_as_legacy_payload() {
    let payload = json!({"body": {"prompt": "legacy"}});

    assert_eq!(gw_task::task_ir_wire_body(&payload), None);
    assert_eq!(gw_task::task_ir_wire_format(&payload), None);
    assert_eq!(gw_task::task_ir_wire_content_type(&payload), None);
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p gw-task test_ir_wire_body_helper --locked
```

Expected: FAIL with unresolved functions `task_ir_wire_body`, `task_ir_wire_format`, and `task_ir_wire_content_type`.

- [ ] **Step 3: Add helper constants and readers**

Add to `crates/gw-task/src/types.rs` after `TaskRecord`:

```rust
pub const TASK_IR_REQUEST: &str = "ir_request";
pub const TASK_IR_RESPONSE: &str = "ir_response";
pub const TASK_IR_WIRE_BODY: &str = "ir_wire_body";
pub const TASK_IR_WIRE_FORMAT: &str = "ir_wire_format";
pub const TASK_IR_WIRE_CONTENT_TYPE: &str = "ir_wire_content_type";
pub const TASK_BODY: &str = "body";

#[must_use]
pub fn task_ir_wire_body(payload: &serde_json::Value) -> Option<&str> {
    payload.get(TASK_IR_WIRE_BODY).and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_wire_format(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get(TASK_IR_WIRE_FORMAT)
        .and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_wire_content_type(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get(TASK_IR_WIRE_CONTENT_TYPE)
        .and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_request(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload.get(TASK_IR_REQUEST)
}

#[must_use]
pub fn task_ir_response(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload.get(TASK_IR_RESPONSE)
}
```

Update `crates/gw-task/src/lib.rs` exports:

```rust
pub use types::{
    task_ir_request, task_ir_response, task_ir_wire_body, task_ir_wire_content_type,
    task_ir_wire_format, TaskId, TaskKind, TaskRecord, TaskStatus, TASK_BODY, TASK_IR_REQUEST,
    TASK_IR_RESPONSE, TASK_IR_WIRE_BODY, TASK_IR_WIRE_CONTENT_TYPE, TASK_IR_WIRE_FORMAT,
};
```

- [ ] **Step 4: Verify helper tests pass**

Run:

```bash
cargo test -p gw-task test_ir_wire_body_helper --locked
```

Expected: PASS.

- [ ] **Step 5: Commit payload helper work**

Run:

```bash
git add crates/gw-task/src/types.rs crates/gw-task/src/lib.rs crates/gw-task/tests/types.rs
git commit -m "feat(task): add async wire-body payload helpers"
```

### Task 2: Preserve Nested Payload Keys Through Task Contracts

**Files:**
- Modify: `crates/gw-task/src/contract.rs:49-96`
- Modify: `crates/gw-task/tests/redis_contract.rs:32-105`
- Reference: `crates/gw-task/src/redis_contract.rs:659-735`

- [ ] **Step 1: Add Redis round-trip coverage for nested keys**

Change `sample_record()` in `crates/gw-task/tests/redis_contract.rs` so `input_payload` includes the W4 keys:

```rust
input_payload: serde_json::json!({
    "body": {"model": "gpt-4.1-mini", "messages": [{"role": "user", "content": "hi"}]},
    "ir_wire_format": "openai:chat",
    "ir_wire_content_type": "application/json",
    "ir_wire_body": "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
}),
```

Extend `test_submit_stores_and_queues` with:

```rust
let stored_payload: serde_json::Value =
    serde_json::from_str(hash.get("input_payload").expect("input_payload hash field"))
        .expect("input_payload decodes");
assert_eq!(
    gw_task::task_ir_wire_body(&stored_payload),
    Some("{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}")
);
assert_eq!(gw_task::task_ir_wire_format(&stored_payload), Some("openai:chat"));
```

- [ ] **Step 2: Run the ignored Redis test explicitly**

Run:

```bash
cargo test -p gw-task test_submit_stores_and_queues --locked -- --ignored
```

Expected before implementation changes: PASS if Docker Redis is available; if Docker is not available, record the infrastructure error and continue after the non-Docker tests are green.

- [ ] **Step 3: Document trait compatibility**

Add this comment above `TaskContract::submit` in `crates/gw-task/src/contract.rs`:

```rust
    /// Submit a full task record. `TaskRecord.input_payload.ir_wire_body` is
    /// optional for backward compatibility; workers must fall back to legacy
    /// `input_payload.body` when it is absent.
```

- [ ] **Step 4: Verify gw-task**

Run:

```bash
cargo test -p gw-task --locked
```

Expected: PASS.

- [ ] **Step 5: Commit contract compatibility work**

Run:

```bash
git add crates/gw-task/src/contract.rs crates/gw-task/tests/redis_contract.rs
git commit -m "test(task): preserve async wire-body payload contract"
```

### Task 3: Render Request Wire Body At Submit Time

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs:134-222`
- Modify: `crates/gw-proxy/src/filters/request.rs:1004-1027`
- Test: `crates/gw-proxy/tests/task_endpoints.rs:173-198`
- Reference: `crates/gw-codec/src/any.rs:97-118`

- [ ] **Step 1: Add a failing submit render test**

Add imports to `crates/gw-proxy/tests/task_endpoints.rs`:

```rust
use gw_codec::AnyCodec;
```

Append this test:

```rust
#[tokio::test]
async fn test_submit_endpoint_stores_codec_rendered_ir_wire_body() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"upstream_url":"https://example.invalid/v1/chat/completions","body":{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hi"}]}}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:chat",
            client_codec: Some(&AnyCodec::OpenAi(gw_codec::openai::OpenAi)),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    let payload = &submitted[0].input_payload;
    assert_eq!(payload["ir_wire_format"], "openai:chat");
    assert_eq!(payload["ir_request"]["protocol"], "openai:chat");
    assert_eq!(payload["ir_request"]["kind"], "request");
    assert_eq!(payload["ir_request"]["value"]["messages"][0]["content"], "hi");
    assert_eq!(payload["ir_wire_content_type"], "application/json");
    let rendered = payload["ir_wire_body"].as_str().expect("rendered body");
    let rendered_json: serde_json::Value =
        serde_json::from_str(rendered).expect("rendered body is json");
    assert_eq!(rendered_json["model"], "gpt-4.1-mini");
    assert_eq!(rendered_json["messages"][0]["content"], "hi");
    assert_eq!(payload["body"]["messages"][0]["content"], "hi");
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p gw-proxy test_submit_endpoint_stores_codec_rendered_ir_wire_body --locked
```

Expected: FAIL because `SubmitTaskRequest` has no `client_codec` field and submit does not populate `ir_wire_body`.

- [ ] **Step 3: Extend `SubmitTaskRequest`**

Add the field in `crates/gw-proxy/src/filters/task.rs`:

```rust
    pub client_codec: Option<&'a gw_codec::AnyCodec>,
```

Update all existing `SubmitTaskRequest` literals in `crates/gw-proxy/tests/task_endpoints.rs` to pass:

```rust
client_codec: Some(&AnyCodec::OpenAi(gw_codec::openai::OpenAi)),
```

For tests where codec rendering is irrelevant, passing `None` is also valid and should preserve legacy behavior.

- [ ] **Step 4: Add submit-time render helper**

Add this helper in `crates/gw-proxy/src/filters/task.rs` near `infer_task_kind`:

```rust
fn attach_ir_wire_body(
    payload: &mut serde_json::Value,
    client_protocol: &str,
    codec: Option<&gw_codec::AnyCodec>,
) -> std::result::Result<(), TaskHttpError> {
    let Some(codec) = codec else {
        return Ok(());
    };
    if codec.api_shape().is_cli() {
        return Ok(());
    }
    let source = payload
        .get(gw_task::TASK_BODY)
        .unwrap_or(payload);
    let source_bytes = serde_json::to_vec(source)
        .map_err(|err| TaskHttpError::submit_failed(format!("serialize task body: {err}")))?;
    let ir = codec.parse_request(&source_bytes).map_err(|err| {
        TaskHttpError::submit_failed(format!("codec parse task request: {}", err.message))
    })?;
    let mut out = bytes::BytesMut::with_capacity(source_bytes.len());
    codec.render_request(&ir, &mut out).map_err(|err| {
        TaskHttpError::submit_failed(format!("codec render task request: {}", err.message))
    })?;
    let rendered = String::from_utf8(out.to_vec())
        .map_err(|err| TaskHttpError::submit_failed(format!("rendered task body utf8: {err}")))?;
    let Some(obj) = payload.as_object_mut() else {
        return Err(TaskHttpError::submit_failed("task payload must be a JSON object"));
    };
    obj.insert(
        gw_task::TASK_IR_WIRE_FORMAT.to_string(),
        serde_json::Value::String(client_protocol.to_string()),
    );
    obj.insert(
        gw_task::TASK_IR_WIRE_CONTENT_TYPE.to_string(),
        serde_json::Value::String("application/json".to_string()),
    );
    obj.insert(
        gw_task::TASK_IR_WIRE_BODY.to_string(),
        serde_json::Value::String(rendered),
    );
    Ok(())
}
```

Call it immediately after parsing `input_payload` in `submit_endpoint_impl`:

```rust
    let mut input_payload: serde_json::Value = serde_json::from_slice(request.body)
        .map_err(|err| TaskHttpError::submit_failed(format!("invalid task payload: {err}")))?;
    attach_ir_wire_body(&mut input_payload, request.client_protocol, request.client_codec)?;
```

- [ ] **Step 5: Pass the detected codec from request filtering**

In `crates/gw-proxy/src/filters/request.rs:1008-1025`, add:

```rust
                        client_codec: ctx.client_codec.as_ref(),
```

- [ ] **Step 6: Verify submit render behavior**

Run:

```bash
cargo test -p gw-proxy task_endpoints --locked
```

Expected: PASS.

- [ ] **Step 7: Commit submit render work**

Run:

```bash
git add crates/gw-proxy/src/filters/task.rs crates/gw-proxy/src/filters/request.rs crates/gw-proxy/tests/task_endpoints.rs
git commit -m "feat(task): enqueue codec-rendered wire body"
```

### Task 4: Make Worker Send `ir_wire_body` Verbatim

**Files:**
- Modify: `crates/gw-task-runtime/src/exec_loop.rs:276-308`
- Modify: `crates/gw-task-runtime/src/exec_loop.rs:465-481`
- Test: `crates/gw-task-runtime/tests/exec_loop.rs:184-238`

- [ ] **Step 1: Add a failing verbatim-body test**

Add this helper to `crates/gw-task-runtime/tests/exec_loop.rs`:

```rust
async fn capture_body_http_server(response_body: &'static str) -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind capture server");
    let addr = listener.local_addr().expect("local addr");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_task = captured.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = [0_u8; 4096];
        let n = socket.read(&mut buf).await.expect("read request");
        let request = &buf[..n];
        let split = request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|idx| idx + 4)
            .expect("http header/body split");
        captured_for_task
            .lock()
            .await
            .extend_from_slice(&request[split..]);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    (addr, captured)
}
```

Append this test:

```rust
#[tokio::test]
async fn test_exec_loop_sends_ir_wire_body_verbatim() {
    let (addr, captured) =
        capture_body_http_server(r#"{"provider_task_id":"provider-wire","ok":true}"#).await;
    let mut record = sample_record(format!("http://{addr}/submit"));
    record.input_payload["ir_wire_format"] = serde_json::json!("openai:chat");
    record.input_payload["ir_wire_content_type"] = serde_json::json!("application/json");
    record.input_payload["ir_wire_body"] = serde_json::json!(
        "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
    );
    record.input_payload["body"] = serde_json::json!({"legacy": true});
    let contract = MemoryContract::enqueue(record).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    let body = String::from_utf8(captured.lock().await.clone()).expect("utf8 body");
    assert_eq!(
        body,
        "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
    );
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p gw-task-runtime test_exec_loop_sends_ir_wire_body_verbatim --locked
```

Expected: FAIL because the worker currently sends `input_payload.body` via `.json(&body)`.

- [ ] **Step 3: Replace the body selection helper**

In `crates/gw-task-runtime/src/exec_loop.rs`, replace `upstream_body` with:

```rust
enum UpstreamTaskBody {
    Wire { body: String, content_type: String },
    Json(serde_json::Value),
}

fn upstream_body(record: &TaskRecord) -> anyhow::Result<UpstreamTaskBody> {
    if let Some(value) = record.input_payload.get(gw_task::TASK_IR_WIRE_BODY) {
        let body = value
            .as_str()
            .ok_or_else(|| anyhow!("task {} ir_wire_body must be a string", record.id))?
            .to_string();
        let content_type = gw_task::task_ir_wire_content_type(&record.input_payload)
            .unwrap_or("application/json")
            .to_string();
        return Ok(UpstreamTaskBody::Wire { body, content_type });
    }
    Ok(UpstreamTaskBody::Json(
        record
            .input_payload
            .get(gw_task::TASK_BODY)
            .cloned()
            .unwrap_or_else(|| record.input_payload.clone()),
    ))
}
```

Update `call_upstream`:

```rust
        let body = upstream_body(record)?;
        let request = self.http.post(&url).timeout(submit_timeout);
        let response = match body {
            UpstreamTaskBody::Wire { body, content_type } => {
                request
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(body)
                    .send()
                    .await
            }
            UpstreamTaskBody::Json(body) => request.json(&body).send().await,
        }
        .map_err(|err| {
            if err.is_timeout() {
                anyhow!(
                    "upstream request timeout after {} ms",
                    submit_timeout.as_millis()
                )
            } else {
                anyhow!(err).context("send upstream task request")
            }
        })?;
```

- [ ] **Step 4: Verify worker fallback still works**

Run:

```bash
cargo test -p gw-task-runtime test_exec_loop_claims_and_executes --locked
cargo test -p gw-task-runtime test_exec_loop_sends_ir_wire_body_verbatim --locked
```

Expected: both PASS. The first test proves old payloads without `ir_wire_body` still use legacy JSON logic.

- [ ] **Step 5: Commit worker send work**

Run:

```bash
git add crates/gw-task-runtime/src/exec_loop.rs crates/gw-task-runtime/tests/exec_loop.rs
git commit -m "feat(task): send rendered async body verbatim"
```

### Task 5: Add Complete IR Response Envelope

**Files:**
- Modify: `crates/gw-task-runtime/Cargo.toml:8-23`
- Modify: `crates/gw-task-runtime/src/exec_loop.rs:308-390`
- Test: `crates/gw-task-runtime/tests/exec_loop.rs`
- Reference: `crates/gw-codec/src/any.rs:159-180`

- [ ] **Step 1: Add a failing complete-IR response test**

Append this test to `crates/gw-task-runtime/tests/exec_loop.rs`:

```rust
#[tokio::test]
async fn test_exec_loop_stores_complete_ir_response_envelope_when_codec_parses() {
    let addr = http_server(
        200,
        r#"{"id":"chatcmpl-task","choices":[{"message":{"role":"assistant","content":"done"}}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
        Duration::ZERO,
    )
    .await;
    let mut record = sample_record(format!("http://{addr}/submit"));
    record.input_payload["ir_wire_format"] = serde_json::json!("openai:chat");
    record.input_payload["ir_wire_content_type"] = serde_json::json!("application/json");
    record.input_payload["ir_wire_body"] = serde_json::json!(
        "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
    );
    let contract = MemoryContract::enqueue(record).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    let output = settlements[0].1.as_ref().expect("output payload");
    assert_eq!(output["ir_wire_format"], "openai:chat");
    assert_eq!(output["ir_response"]["protocol"], "openai:chat");
    assert_eq!(output["ir_response"]["kind"], "response");
    assert_eq!(
        output["ir_response"]["value"]["choices"][0]["message"]["content"],
        "done"
    );
    let rendered = output["ir_wire_body"].as_str().expect("normalized body");
    let rendered_json: serde_json::Value =
        serde_json::from_str(rendered).expect("rendered response is json");
    assert_eq!(rendered_json["choices"][0]["message"]["content"], "done");
    assert_eq!(output["body"]["id"], "chatcmpl-task");
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p gw-task-runtime test_exec_loop_stores_complete_ir_response_envelope_when_codec_parses --locked
```

Expected: FAIL because `gw-task-runtime` does not depend on `gw-codec` and stores raw response JSON directly.

- [ ] **Step 3: Add the codec dependency without adding adapters**

Modify `crates/gw-task-runtime/Cargo.toml`:

```toml
gw-codec = { path = "../gw-codec" }
```

Do not add `gw-adapter`.

- [ ] **Step 4: Add format-to-codec and complete response envelope helpers**

Add these helpers near `parse_response_payload` in `crates/gw-task-runtime/src/exec_loop.rs`:

```rust
fn codec_for_task_format(format: &str) -> Option<gw_codec::AnyCodec> {
    match format {
        "openai:chat" => Some(gw_codec::AnyCodec::OpenAi(gw_codec::openai::OpenAi)),
        "openai:responses" => Some(gw_codec::AnyCodec::OpenAiResponses(
            gw_codec::openai::responses::OpenAiResponsesCodec,
        )),
        "anthropic:messages" => Some(gw_codec::AnyCodec::Anthropic(
            gw_codec::anthropic::Anthropic,
        )),
        "gemini:generate" => Some(gw_codec::AnyCodec::Gemini(gw_codec::gemini::Gemini)),
        _ => None,
    }
}

fn complete_ir_output_payload(record: &TaskRecord, text: &str) -> anyhow::Result<serde_json::Value> {
    let raw = parse_response_payload(text)?;
    let Some(format) = gw_task::task_ir_wire_format(&record.input_payload) else {
        return Ok(raw);
    };
    let Some(codec) = codec_for_task_format(format) else {
        return Ok(raw);
    };
    let Ok(ir) = codec.parse_response(text.as_bytes()) else {
        return Ok(raw);
    };
    let mut out = bytes::BytesMut::with_capacity(text.len());
    if codec.render_response(&ir, &mut out).is_err() {
        return Ok(raw);
    }
    let ir_value = serde_json::to_value(&ir).context("serialize response ir")?;
    let rendered = String::from_utf8(out.to_vec()).context("normalized response utf8")?;
    Ok(serde_json::json!({
        "body": raw,
        "ir_response": {
            "protocol": format,
            "kind": "response",
            "value": ir_value
        },
        gw_task::TASK_IR_WIRE_FORMAT: format,
        gw_task::TASK_IR_WIRE_CONTENT_TYPE: "application/json",
        gw_task::TASK_IR_WIRE_BODY: rendered,
    }))
}
```

If `bytes` is not already available to this crate through an existing dependency, add `bytes.workspace = true` to `crates/gw-task-runtime/Cargo.toml`.

- [ ] **Step 5: Use the complete IR output helper**

In `call_upstream`, replace:

```rust
        let mut payload = parse_response_payload(&text)?;
```

with:

```rust
        let mut payload = complete_ir_output_payload(record, &text)?;
```

In the poll loop, replace:

```rust
            payload = parse_response_payload(&text)?;
```

with:

```rust
            payload = complete_ir_output_payload(record, &text)?;
```

Adjust `provider_task_id`, `provider_task_state`, and `poll_url` helpers so they look at `payload["body"]` first when the normalized envelope is present:

```rust
fn task_provider_payload(payload: &serde_json::Value) -> &serde_json::Value {
    payload.get(gw_task::TASK_BODY).unwrap_or(payload)
}
```

Then call `task_provider_payload(payload)` inside `provider_task_id`, `provider_task_state`, and `poll_url`.

- [ ] **Step 6: Verify response IR envelope and raw fallback**

Run:

```bash
cargo test -p gw-task-runtime test_exec_loop_stores_complete_ir_response_envelope_when_codec_parses --locked
cargo test -p gw-task-runtime exec_loop --locked
```

Expected: PASS. Existing LRO and raw JSON tests must still pass because `task_provider_payload` preserves the old status/id lookup behavior.

- [ ] **Step 7: Commit response normalization work**

Run:

```bash
git add crates/gw-task-runtime/Cargo.toml crates/gw-task-runtime/src/exec_loop.rs crates/gw-task-runtime/tests/exec_loop.rs
git commit -m "feat(task): store async response ir envelope"
```

### Task 6: Update Specs And Roadmap

**Files:**
- Modify: `docs/spec/08-async-tasks.md:16-70`
- Modify: `docs/ROADMAP.md:68-73`

- [ ] **Step 1: Update async task as-built spec**

Edit `docs/spec/08-async-tasks.md`:

- In the submit section, state that the gateway attempts codec parse/render at submit and stores `input_payload.ir_request`, `ir_wire_body`, `ir_wire_format`, and `ir_wire_content_type` when the detected client codec supports the body.
- In the runtime loop section, state that `ExecLoop` sends `ir_wire_body` verbatim when present and falls back to `input_payload.body` / whole payload for older records.
- In deliberate behaviours, add: `Task workers do not call gw-adapter; adapter-specific request transforms remain proxy-owned per F1.`
- Replace the "Not built" bullet with an as-built compatibility note:

```markdown
- **Async IR compatibility.** New IR-capable submissions carry `input_payload.ir_request` and `input_payload.ir_wire_body`; old records without those keys remain executable through the legacy `input_payload.body` fallback. Terminal responses keep raw `output_payload.body` and add `output_payload.ir_response` plus `output_payload.ir_wire_body` when response codec parse/render succeeds. Task load/watch projection must either preserve this envelope without breaking existing `output_payload.body` consumers or be explicitly updated with matching tests.
```

- [ ] **Step 2: Close the ROADMAP entry**

Remove `docs/ROADMAP.md:68-73` after the spec update is accurate. Do not remove unrelated tunnel entries.

- [ ] **Step 3: Run spec checks**

Run:

```bash
bash scripts/check-spec.sh docs/spec/08-async-tasks.md
rg -n "Codec render of `ir_wire_body`|worker still forwards" docs/ROADMAP.md docs/spec/08-async-tasks.md
```

Expected: `check-spec` exits 0. The `rg` command should find no stale "worker still forwards" language; it may find the new compatibility language.

- [ ] **Step 4: Commit docs closure**

Run:

```bash
git add docs/spec/08-async-tasks.md docs/ROADMAP.md
git commit -m "docs(task): close async wire-body roadmap entry"
```

### Task 7: Final Verification

**Files:**
- Verify all modified files from Tasks 1-6.

- [ ] **Step 1: Run focused tests**

Run:

```bash
cargo test -p gw-task --locked
cargo test -p gw-proxy task_endpoints --locked
cargo test -p gw-task-runtime exec_loop --locked
cargo test -p gw-proxy task_load --locked
cargo test -p gw-proxy task_watch --locked
```

Expected: PASS, or replace the load/watch test names with the exact local test filters after inspecting the endpoint tests. The load/watch verification must prove `output_payload.body` consumers are not broken by the added complete IR envelope, or that any planned projection update has matching assertions.

- [ ] **Step 2: Run workspace gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f"; done
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 3: Confirm adapter boundary**

Run:

```bash
rg -n "gw-adapter|AdapterRegistry|OutboundAdapter" crates/gw-task-runtime crates/gw-task-worker
```

Expected: no matches in task runtime or task worker code.

- [ ] **Step 4: Confirm commit history**

Run:

```bash
git log --oneline -5
```

Expected: recent commits include:

```text
docs(task): close async wire-body roadmap entry
feat(task): store async response ir envelope
feat(task): send rendered async body verbatim
feat(task): enqueue codec-rendered wire body
```

The exact helper/test commits may appear between those depending on review adjustments.

## Suggested Commit Granularity

- `feat(task): enqueue codec-rendered wire body`
- `feat(task): send rendered async body verbatim`
- `feat(task): store async response ir envelope`
- `docs(task): close async wire-body roadmap entry`

The user-requested headline commits are:

- `feat(task): enqueue codec-rendered wire body`
- `docs(task): close async wire-body roadmap entry`

## Handoff Notes

- Backward compatibility is mandatory: records without `input_payload.ir_wire_body` must continue using legacy `input_payload.body` / whole-payload JSON behavior.
- W4 should not make task submission perform vendor routing or call adapters. If implementation discovers a need for vendor-specific transform output before enqueue, stop and return a decision packet because that crosses into W5/W7 adapter ownership.
- 用户决策已记录：完整 IR 优先。Implement complete request/response IR envelopes if the feasibility spike shows this is local and tractable. If complete IR requires broad task contract, public API, codec trait, or load/watch projection redesign, stop and return a user decision packet; do not silently downgrade to a reduced envelope.
