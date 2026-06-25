# OpenAI-Compatible Task Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement OpenAI-compatible image, video, and embedding APIs as first-class async-task template/runtime surfaces without touching `gw-codec`.

**Architecture:** `gw-proxy` admits OpenAI-compatible task endpoints before codec detection, resolves a published async-task profile/binding, and dispatches to `gw-task-runtime`. `gw-task-runtime` renders provider requests, executes sync or async paths, runs configured poll templates, and renders OpenAI-compatible client responses. Go owns template validation and published route/binding/profile projections.

**Tech Stack:** Rust workspace (`gw-proxy`, `gw-task-runtime`, `gw-task`, `gw-store`), Pingora, Redis task contract, serde_json templates, Go `modules/asynctask` validation/projection.

## Current Implementation Status (2026-06-25)

Completed in this pass:

- `gw-proxy` admits OpenAI-compatible image, embedding, and video task endpoints before codec detection, without adding image/video/embedding handling to `gw-codec`.
- OpenAI-compatible request bodies are parsed by task helpers: JSON for normal submit endpoints and bounded multipart parsing for image/video file surfaces. Multipart fields and file metadata/content are exposed to templates through `client_request` and `_multipart`.
- Published async-task binding/profile selection is wired for `<global_model_id>:<task_kind>`, with immutable profile/binding snapshots persisted on async task records.
- `gw-task-runtime` executes configured templates inline and in the worker, including configured `upstream_poll_template` request rendering and status mapping.
- `HttpTemplateTransport` supports JSON provider requests, relative paths via provider/base URL, and template-described multipart provider requests.
- Async submit responses are rendered through `client_async_submit_response_template`, not hard-coded internal task JSON.
- `GET /v1/videos/{video_id}` returns an OpenAI-compatible video object from task state.
- `GET /v1/videos/{video_id}/content` returns a `302 Location` redirect to the resolved final content URL and returns `GW_TASK_ARTIFACT_NOT_FOUND` when unavailable.
- Runtime route admission first consults the published async-task endpoint registry and falls back to the static OpenAI-compatible baseline routes.
- Endpoint-aware runtime selection chooses the profile whose published `client_request_template.endpoint.method/path` matches the incoming OpenAI-compatible route, so the same model/task kind can back multiple OpenAI-compatible surfaces.
- Endpoint projections support `default_model` for OpenAI SDK-style requests whose body does not carry a `model` field.
- Static fallback covers baseline image/embedding/video routes plus OpenAI video edit, extension, remix, and character creation POST surfaces; non-task video character GET is intentionally not intercepted by the static fallback.
- Poll template execution classifies configured statuses and records `openai-poll-after-ms` / configured header wait policy as `poll_wait` events.
- Go async-task template validation now requires endpoint metadata, compatible client modes, async submit template for async mode, route conflict checks, endpoint registry invalidation, and poll template for `async_lro`; schema and fixtures were updated.

Known residuals after this pass:

- Full OpenAI SDK smoke tests against a locally configured gateway were not run in this pass.

Verification completed:

- `cargo test -p gw-task-runtime -- --nocapture`
- `cargo test -p gw-proxy -- --nocapture`
- `cargo test -p gw-store async_task -- --nocapture`
- `cargo fmt --all -- --check`
- `go test ./modules/asynctask/... ./shared/rustpact`

## Global Constraints

- Do not add image, video, or embedding handling to `gw-codec`.
- Do not route OpenAI-compatible image/video/embedding through the chat/responses protocol IR path.
- All OpenAI-compatible media/embedding endpoints are owned by async-task template/runtime.
- Every phase in this plan is required for launch; phases are sequencing only.
- Use TDD: write a failing test, run it to verify the failure, then implement.
- Preserve unrelated dirty worktree changes.

---

## Phase 1: Runtime Route Admission And Endpoint Registry

### Task 1: Add Task Route Candidate Detection Before Codec Rejection

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Modify: `crates/gw-proxy/src/filters/early.rs`
- Test: `crates/gw-proxy/tests/task_detector.rs`

**Interfaces:**
- Produces: `TaskEndpointCandidate { method, path, task_kind, model_path, content_type }`
- Produces: `detect_openai_compatible_task_endpoint(task_enabled, method, path, content_type) -> Option<TaskEndpointCandidate>`
- Produces: `ReqCtx.task_endpoint_candidate: Option<TaskEndpointCandidate>`

- [ ] Write a failing test proving `POST /v1/images/generations`, `POST /v1/embeddings`, and `POST /v1/videos` detect as OpenAI-compatible task endpoint candidates when task is enabled.
- [ ] Run `cargo test -p gw-proxy --test task_detector openai_compatible -- --nocapture` and confirm it fails because the detector does not exist or returns `None`.
- [ ] Implement the detector with static minimum registry entries for:
  - `POST /v1/images/generations`
  - `POST /v1/images/edits`
  - `POST /v1/images/variations`
  - `POST /v1/embeddings`
  - `POST /v1/videos`
  - `GET /v1/videos/{video_id}`
  - `GET /v1/videos/{video_id}/content`
- [ ] Store the candidate on `ReqCtx` and mark `ctx.is_task = true` without assigning legacy `/v1/tasks` action for OpenAI-compatible task endpoints.
- [ ] Update early filter to skip `AnyCodec::detect` only for task endpoint candidates while preserving current codec behavior for chat/responses/anthropic/gemini/kiro.
- [ ] Run `cargo test -p gw-proxy --test task_detector`.

### Task 2: Request Filter Task Dispatch Split

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-proxy/tests/task_detector.rs`

**Interfaces:**
- Produces: `TaskAction::OpenAiCompatibleSubmit`
- Produces: `TaskAction::OpenAiCompatibleLoadVideo`
- Produces: `TaskAction::OpenAiCompatibleVideoContent`

- [ ] Write failing tests proving OpenAI-compatible task candidates do not fall through to normal request body codec parsing.
- [ ] Run the focused failing test.
- [ ] Split task handling so legacy `/v1/tasks` actions still call `submit_endpoint/load/watch/cancel`, while OpenAI-compatible actions call new placeholder handlers returning `GW_TASK_PROFILE_NOT_CONFIGURED`.
- [ ] Ensure `[task].enabled=false` preserves current unknown protocol rejection.
- [ ] Run `cargo test -p gw-proxy --test task_detector`.

## Phase 2: Published Binding/Profile Selection

### Task 3: Extract Model And Task Kind From OpenAI-Compatible Requests

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-proxy/tests/task_detector.rs`

**Interfaces:**
- Produces: `resolve_task_request_key(candidate, body, query, headers) -> Result<TaskRequestKey, TaskHttpError>`
- `TaskRequestKey { global_model_id: String, task_kind: String }`

- [ ] Write failing tests for JSON body model extraction from image, embedding, and video requests.
- [ ] Write failing test for `GW_TASK_MODEL_REQUIRED` when model is absent on submit endpoints.
- [ ] Implement JSON model extraction using the candidate `model_path`.
- [ ] Keep multipart extraction behind a small interface so image edits/video file endpoints can use it in a later task.
- [ ] Run `cargo test -p gw-proxy --test task_detector`.

### Task 4: Load Binding Index, Binding Revisions, And Profile Revision

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Modify: `crates/gw-store/src/entity_store.rs` only if helper ergonomics require it
- Test: `crates/gw-proxy/tests/task_runtime_selection.rs`

**Interfaces:**
- Produces: `resolve_task_runtime(store, key, target_hint) -> Result<ResolvedTaskRuntime, TaskHttpError>`
- `ResolvedTaskRuntime { profile, binding, profile_revision_id, binding_revision_id, effective_template_hash }`

- [ ] Write a failing in-memory test with fake `AsyncTaskBindingIndex`, `AsyncTaskBindingRevision`, and `AsyncTaskProfileRevision`.
- [ ] Run the focused failing test.
- [ ] Load binding index by `<global_model_id>:<task_kind>`.
- [ ] Load each binding revision id from the index.
- [ ] Load selected profile revision.
- [ ] Reuse `gw_task_runtime::binding_selector::select_binding`.
- [ ] Return `GW_TASK_PROFILE_NOT_CONFIGURED` when no binding/profile is usable.
- [ ] Run `cargo test -p gw-proxy task_runtime_selection`.

### Task 5: Select Client And Upstream Modes

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Modify: `crates/gw-task-runtime/src/client_mode.rs` only for missing selector coverage
- Test: `crates/gw-task-runtime/tests/client_mode.rs`
- Test: `crates/gw-proxy/tests/task_runtime_selection.rs`

**Interfaces:**
- Consumes: `select_client_mode(profile, method, path, headers, query, body)`
- Produces: `ResolvedExecutionModes { client_mode, upstream_mode }`

- [ ] Write failing tests proving `Prefer: respond-async`, query selector, and body selector work for OpenAI-compatible task requests.
- [ ] Write failing test for disabled selected mode returning `GW_TASK_CLIENT_MODE_UNSUPPORTED`.
- [ ] Parse `upstream_mode` from profile and reject unsupported/missing values.
- [ ] Run `cargo test -p gw-task-runtime --test client_mode` and proxy selection tests.

## Phase 3: Sync Inline Template Execution

### Task 6: Add HTTP Transport For Template Executor

**Files:**
- Create: `crates/gw-task-runtime/src/http_transport.rs`
- Modify: `crates/gw-task-runtime/src/lib.rs`
- Test: `crates/gw-task-runtime/tests/http_transport.rs`

**Interfaces:**
- Produces: `HttpTemplateTransport::new(reqwest::Client, base_url: Option<String>)`
- Consumes: `UpstreamRequest { method, path, headers, body }`

- [ ] Write failing tests using an in-process HTTP mock for POST JSON submit.
- [ ] Implement HTTP method/path/header/body rendering.
- [ ] Preserve non-2xx response body in `TransportError::Failed`.
- [ ] Run `cargo test -p gw-task-runtime --test http_transport`.

### Task 7: Execute Sync Image And Embedding Inline From Request Filter

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_runtime.rs`

**Interfaces:**
- Consumes: `TemplateExecutor::execute_inline`
- Produces: `handle_openai_compatible_submit(...) -> TaskHttpResponse`

- [ ] Write failing tests for image generation sync returning OpenAI-compatible image JSON.
- [ ] Write failing tests for embeddings sync returning OpenAI-compatible embeddings JSON.
- [ ] Build `ExecutionRequest` with identity, profile, binding, client request, task context, client mode, upstream mode.
- [ ] Execute `sync + sync` inline and write the rendered JSON response.
- [ ] Confirm no `gw-codec` image/embedding path is added.
- [ ] Run `cargo test -p gw-proxy openai_compatible_runtime`.

### Task 8: Multipart Input Support For Image Edits, Variations, And Video Creates

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Modify: `crates/gw-task-runtime/src/template_executor.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_multipart.rs`

**Interfaces:**
- Produces: `ClientRequestBody::Json(Value) | ClientRequestBody::Multipart(TaskMultipart)`

- [ ] Write failing tests for extracting `model` and preserving file fields from multipart image edit and video create requests.
- [ ] Implement bounded multipart parsing for task endpoints only.
- [ ] Expose multipart fields/files to templates without codec.
- [ ] Run `cargo test -p gw-proxy openai_compatible_multipart`.

## Phase 4: Async Submit And Worker Template Execution

### Task 9: Persist Template Snapshots In TaskRecord

**Files:**
- Modify: `crates/gw-task/src/types.rs`
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-task/tests/types.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_runtime.rs`

**Interfaces:**
- Produces `TaskRecord` fields or `input_payload` keys for profile/binding revision identity and effective snapshots.

- [ ] Write failing serialization round-trip tests for profile/binding revision identity and snapshots.
- [ ] Add durable fields or namespaced `input_payload` keys.
- [ ] Populate them during async OpenAI-compatible submit.
- [ ] Run `cargo test -p gw-task --test types` and proxy runtime tests.

### Task 10: Worker Template Execution

**Files:**
- Modify: `crates/gw-task-runtime/src/exec_loop.rs`
- Modify: `crates/gw-task-runtime/src/template_executor.rs`
- Test: `crates/gw-task-runtime/tests/template_executor.rs`
- Test: `crates/gw-task-runtime/tests/exec_loop.rs`

**Interfaces:**
- Consumes persisted profile/binding/client request/task context.
- Produces terminal `output_payload.client_final_response`.

- [ ] Write failing test proving worker uses templates instead of hard-coded `POST input_payload`.
- [ ] Implement execution request reconstruction from `TaskRecord`.
- [ ] Use `TemplateExecutor::execute_background`.
- [ ] Persist final rendered client response in task output payload before settle.
- [ ] Keep legacy direct `/v1/tasks` worker behavior for records without template snapshots.
- [ ] Run `cargo test -p gw-task-runtime`.

### Task 11: Async Client Submit Response Templates

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_runtime.rs`

**Interfaces:**
- Consumes: `client_async_submit_response_template`

- [ ] Write failing tests that async image/video submit returns template-rendered OpenAI-compatible response, not hard-coded `{task_id,status,expires_at}`.
- [ ] Render submit response with task id/status/model/prompt context.
- [ ] Run focused proxy runtime tests.

## Phase 5: Poll Template Runtime

### Task 12: Implement Poll Template Rendering

**Files:**
- Modify: `crates/gw-task-runtime/src/template_executor.rs`
- Test: `crates/gw-task-runtime/tests/template_executor.rs`

**Interfaces:**
- Produces: `render_poll_request(profile, binding, task, submit_response, poll_response) -> UpstreamRequest`
- Produces: `classify_poll_response(profile, response) -> PollState`

- [ ] Write failing tests for `upstream_poll_template` method/path/header/body rendering.
- [ ] Write failing tests for status mapping pending/succeeded/failed.
- [ ] Write failing test for missing poll template with `async_lro` returning `GW_TASK_POLL_TEMPLATE_REQUIRED`.
- [ ] Implement poll request rendering and response classification.
- [ ] Run `cargo test -p gw-task-runtime --test template_executor`.

### Task 13: Use Poll Template In Inline And Worker Execution

**Files:**
- Modify: `crates/gw-task-runtime/src/template_executor.rs`
- Modify: `crates/gw-task-runtime/src/exec_loop.rs`
- Test: `crates/gw-task-runtime/tests/template_executor.rs`
- Test: `crates/gw-task-runtime/tests/exec_loop.rs`

**Interfaces:**
- Consumes: `upstream_poll_template`
- Produces: provider-specific polling loop with configured backoff/header wait.

- [ ] Write failing test for OpenAI video poll: queued -> in_progress -> completed.
- [ ] Write failing test for third-party LRO poll URL extraction.
- [ ] Replace generic hard-coded poll in template path with configured poll template.
- [ ] Keep legacy generic poll only for old task records without template snapshots.
- [ ] Run `cargo test -p gw-task-runtime`.

## Phase 6: OpenAI Video Retrieve And Content

### Task 14: Implement `GET /v1/videos/{video_id}`

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_video.rs`

**Interfaces:**
- Produces: `load_openai_video_endpoint(contract, video_id) -> TaskHttpResponse`

- [ ] Write failing tests for queued, running, succeeded, failed, timed_out, and cancelled task states mapping to OpenAI video statuses.
- [ ] Resolve OpenAI video id to internal task id. Phase 6 may use identical ids generated at submit; if distinct ids are configured, persist a reverse lookup key.
- [ ] Render response through `client_async_result_response_template` when present, with default OpenAI video fallback.
- [ ] Run `cargo test -p gw-proxy openai_compatible_video`.

### Task 15: Implement `GET /v1/videos/{video_id}/content`

**Files:**
- Modify: `crates/gw-proxy/src/filters/task.rs`
- Test: `crates/gw-proxy/tests/openai_compatible_video.rs`

**Interfaces:**
- Produces: `load_openai_video_content_endpoint(contract, video_id) -> TaskHttpResponse`

- [ ] Write failing tests for completed task content redirect/proxy and artifact not found.
- [ ] Support configured artifact URL redirect when final response carries provider content URL.
- [ ] Return `GW_TASK_ARTIFACT_NOT_FOUND` for non-terminal or missing content.
- [ ] Run video endpoint tests.

## Phase 7: Go Validation, Projection, Fixtures, And Docs

### Task 16: Go Template Validation

**Files:**
- Modify: `AILinkMax-Go/modules/asynctask/business/template/types.go`
- Modify: `AILinkMax-Go/modules/asynctask/business/template/validate.go`
- Test: `AILinkMax-Go/modules/asynctask/business/profile_service_test.go`

**Interfaces:**
- Produces validation diagnostics for OpenAI-compatible endpoint and poll template requirements.

- [ ] Write failing Go tests for missing poll template, missing async submit response template, missing endpoint method/path/model extractor, and ambiguous duplicate route.
- [ ] Implement validation.
- [ ] Run `go test ./modules/asynctask/...`.

### Task 17: Go Runtime Projection

**Files:**
- Modify: `AILinkMax-Go/modules/asynctask/business/entity_projection.go`
- Modify: `AILinkMax-Go/modules/asynctask/business/binding_service.go`
- Modify: `ailinkmax-gateway/crates/gw-store/src/loader.rs`
- Test: `AILinkMax-Go/modules/asynctask/business/binding_service_test.go`
- Test: `ailinkmax-gateway/crates/gw-store/src/loader.rs`

**Interfaces:**
- Produces route registry entries from published profile endpoints.
- Produces binding index entries with route registry and binding revision ids.

- [ ] Write failing Go projection tests for image, embedding, and video route registry entries.
- [ ] Write failing Rust loader test decoding the new projection.
- [ ] Implement projection and loader decode.
- [ ] Run Go tests and `cargo test -p gw-store async_task`.

### Task 18: Fixtures And Contract Docs

**Files:**
- Modify: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/openai-image-default.json`
- Modify: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/openai-embedding-default.json`
- Modify: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/openai-video-default.json`
- Add: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/openai-image-edit-default.json`
- Add: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/openai-image-variation-default.json`
- Add: `AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/third-party-lro-video.json`
- Modify: `ailinkmax-gateway/docs/spec/08-async-tasks.md`
- Modify: `ailinkmax-gateway/docs/workflow.md`
- Modify: `ailinkmax-gateway/docs/go-contracts/main.md`
- Modify: `ailinkmax-gateway/docs/go-contracts/parts/01-go-as-producer.md`
- Modify: `ailinkmax-gateway/docs/go-contracts/parts/03-shared-schema.md`
- Modify: `ailinkmax-gateway/docs/ROADMAP.md`

- [ ] Update fixtures to cover all required OpenAI-compatible endpoints and one third-party LRO mapping.
- [ ] Update as-built async-task spec to remove stale claims and describe template runtime.
- [ ] Update workflow filter order.
- [ ] Update Go contract docs for route registry and async-task profile/binding projection.
- [ ] Remove closed async-task divergence from ROADMAP or add replacement only if a real residual remains.
- [ ] Run docs grep checks for stale statements:
  - `rg -n "POST /v1/images/generations.*detect_task_shape|background:true|generic JSON poll" docs`

## Final Verification

- [ ] Run `cargo test -p gw-proxy task_detector`.
- [ ] Run `cargo test -p gw-task-runtime`.
- [ ] Run `cargo test -p gw-task`.
- [ ] Run `cargo test -p gw-store async_task`.
- [ ] Run `cargo test --workspace`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `go test ./modules/asynctask/...`.
- [ ] Run `go test ./modules/provider/...`.
- [ ] Run OpenAI SDK-style smoke tests against a local gateway with configured fixtures:
  - images generate
  - embeddings create
  - videos create
  - videos retrieve
  - videos content
