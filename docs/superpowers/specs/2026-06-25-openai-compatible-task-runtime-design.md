# OpenAI-Compatible Task Runtime Design

## Goal

Make OpenAI-compatible image, video, and embedding APIs first-class gateway surfaces, backed by async-task profiles and bindings rather than `gw-codec`.

The gateway must accept OpenAI-compatible client requests, select a configured async-task profile/binding, render the provider request, optionally poll provider LROs using configured poll templates, and render OpenAI-compatible client responses. This closes the known gaps for OpenAI image/video/embedding compatibility and for configurable async poll formats.

## Non-Negotiable Constraints

- Do not add image, video, or embedding handling to `gw-codec`.
- Do not route OpenAI-compatible image/video/embedding through the chat/responses protocol IR path.
- Keep `gw-codec` focused on high-frequency streaming and protocol transform surfaces.
- All OpenAI-compatible media/embedding endpoints are owned by async-task template/runtime.
- Every phase in this spec is required for launch; phases are sequencing only, not optional scope.
- The project is not live. Breaking schema/template changes are allowed when they remove incomplete legacy shape.

## External Compatibility Baseline

OpenAI-compatible means matching the current OpenAI API/SDK endpoint shape for the following surfaces:

- Images:
  - `POST /v1/images/generations`
  - `POST /v1/images/edits`
  - `POST /v1/images/variations`
  - The Python SDK posts image generation to `/images/generations`, edits to `/images/edits`, and variations to `/images/variations`.
- Embeddings:
  - `POST /v1/embeddings`
  - The Python SDK exposes `client.embeddings.create(...)` and posts to `/embeddings`.
- Videos:
  - `POST /v1/videos`
  - `GET /v1/videos/{video_id}`
  - `GET /v1/videos/{video_id}/content`
  - Optional configured pass-through surfaces for `/v1/videos/edits`, `/v1/videos/extensions`, `/v1/videos/{video_id}/remix`, `/v1/videos/characters`, and `/v1/videos/characters/{character_id}`.
  - The Python SDK `videos.create_and_poll` creates a video, repeatedly retrieves `/videos/{video_id}`, treats `queued` and `in_progress` as pending, and uses `openai-poll-after-ms` when present.

API易 and other third-party providers are provider targets, not a separate client protocol. They are mapped to OpenAI-compatible client shape through async-task profile/binding configuration.

## Current Gaps To Close

1. `early_request_filter` rejects unknown `/v1/*` paths before `request_filter` can mark task context.
2. `mark_task_context` only recognizes `/v1/tasks` management-style task routes.
3. `detect_published_task_endpoint` exists but is not wired into live request handling.
4. Binding indexes are keyed by `<global_model_id>:<task_kind>`, but live endpoint detection starts from method/path and a request body model.
5. `submit_endpoint_impl` hard-codes `client_mode=async` and `upstream_mode=async_lro`.
6. `TaskRecord` stores template metadata fields, but live submit does not populate profile/binding revision identity from published async-task configuration.
7. `ExecLoop::call_upstream` hard-codes `POST`, hard-codes body source, and has no template rendering.
8. Worker polling is hard-coded to `status_url` / `poll_url` and fixed status strings.
9. `upstream_poll_template` exists in Go schema/fixtures but is not enforced by Rust worker runtime.
10. `TaskKind::Embedding` exists, but task-kind inference and endpoint routing do not treat embeddings as first-class.
11. OpenAI video object responses are not produced; current async submit returns internal `{task_id,status,expires_at}`.
12. OpenAI image and embedding synchronous responses are not produced from configured templates on the business route.
13. Docs claim legacy `/v1/images/generations` and `/v1/responses background:true` task detection that current tests disprove.

## Architecture

### High-Level Flow

```text
Client OpenAI-compatible request
  -> early_request_filter task-route bypass for configured OpenAI-compatible task endpoints
  -> request_filter auth/quota/balance
  -> task endpoint resolver extracts method/path/content-type/body model/task kind
  -> load async_task_binding_index(<model>:<task_kind>)
  -> load candidate binding revisions + profile revisions
  -> select binding/profile
  -> select client mode
  -> execute sync inline or submit async task
  -> render OpenAI-compatible client response
```

### Ownership Boundaries

- `gw-proxy` owns HTTP endpoint detection, auth-integrated task dispatch, and writing HTTP responses.
- `gw-task-runtime` owns template execution, binding selection, client-mode selection, provider request rendering, provider response normalization, poll-template execution, and OpenAI-compatible response rendering.
- `gw-task` owns durable `TaskRecord`, modes, kinds, status, and Redis task contract.
- `gw-store` owns profile/binding/index cold-load and cache invalidation.
- Go owns drafting, validating, publishing, and projecting async-task profiles/bindings into PgSQL/Redis entities.

### No Codec Path

For OpenAI-compatible image/video/embedding:

- `ctx.client_codec` may remain absent or may be set to OpenAI only for error-envelope fallback.
- The request body is parsed as JSON or multipart by task runtime helpers, not by `gw-codec`.
- No `AnyCodec` variant is added.
- No `ApiShape` is required for image/video/embedding client compatibility.
- OpenAI-compatible response JSON is rendered by `client_*_response_template`.

## Endpoint Resolution

### Route Index

Rust needs a method/path/content-type route index before model-specific binding lookup.

Implement a route-level projection:

```json
{
  "method": "POST",
  "path": "/v1/images/generations",
  "content_type": "application/json",
  "task_kind": "image",
  "model_path": "$.model"
}
```

Minimum supported route entries:

- `POST /v1/images/generations` -> `task_kind=image`, `model_path=$.model`
- `POST /v1/images/edits` -> `task_kind=image`, `model_path=$.model`
- `POST /v1/images/variations` -> `task_kind=image`, `model_path=$.model`
- `POST /v1/embeddings` -> `task_kind=embedding`, `model_path=$.model`
- `POST /v1/videos` -> `task_kind=video`, `model_path=$.model`
- `GET /v1/videos/{video_id}` -> `task_kind=video`, task lookup by OpenAI video id
- `GET /v1/videos/{video_id}/content` -> `task_kind=video`, artifact/content lookup by OpenAI video id

Phase 1 may use an in-process static OpenAI-compatible route registry for these known routes while Go projection catches up. Phase 2 must replace that with published `client_request_template` / endpoint projection from Go.

### Early Filter Behavior

`early_request_filter` must not reject a configured OpenAI-compatible task endpoint for `GW_REQUEST_PROTOCOL_UNKNOWN`.

Required behavior:

- If `[task].enabled=false`, current unknown protocol behavior remains.
- If task is enabled and method/path matches a configured OpenAI-compatible task endpoint, early filter marks a task candidate and skips codec detection.
- The request still goes through auth, rate limit, quota, balance, and task dispatch in `request_filter`.
- Existing chat/responses/anthropic/gemini/kiro codec behavior is unchanged.

## Binding And Profile Selection

### Model Extraction

For JSON bodies, extract `model` using the route's `model_path`.

For multipart image/video endpoints:

- Extract form field `model` when present.
- Preserve file parts for provider rendering.
- Reject requests that need model selection but do not carry a model with a stable OpenAI-shaped error.

### Binding Index Lookup

After extracting `global_model_id` and `task_kind`, load:

```text
async_task_binding_index:<global_model_id>:<task_kind>
```

Then load candidate binding revisions and profile revisions, and select the best match using existing `gw_task_runtime::binding_selector` semantics:

- exact provider key match wins when selected key is known
- higher specificity wins
- higher priority wins within same specificity
- OpenAI-compatible builtin fallback is allowed for image and embedding only when explicitly no published binding exists and the provider target is OpenAI-compatible

### Published Revision Immutability

Every runtime execution must persist enough immutable identity to make later polling/result rendering independent of draft edits:

- `profile_id`
- `profile_revision_id`
- `profile_version`
- `binding_id`
- `binding_revision_id`
- `binding_version`
- `effective_template_hash`
- effective profile snapshot
- effective binding snapshot

## Client Modes

Client mode is selected per request:

- `client_modes=["sync"]` always executes inline and returns final OpenAI-compatible response.
- `client_modes=["async"]` always returns a task/video reference without blocking on provider completion.
- `client_modes=["sync","async"]` uses selector rules, then `Prefer: respond-async`, then `default_client_mode`.

Required supported selector sources:

- header
- query
- body JSON path

Disabled selected mode returns `GW_TASK_CLIENT_MODE_UNSUPPORTED`.

## Upstream Modes

Supported combinations:

| Client mode | Upstream mode | Behavior |
|---|---|---|
| sync | sync | Render provider request, call provider, render final OpenAI-compatible response inline. |
| sync | async_lro | Submit provider job, poll by `upstream_poll_template` until terminal or sync timeout, render final OpenAI-compatible response or configured timeout response. |
| async | sync | Create task record, return client async submit response, worker calls provider once and settles. |
| async | async_lro | Create task record, return client async submit response, worker submits and polls provider to terminal. |

`sync + async_lro` must support:

- `sync_wait_ms`
- `poll_initial_interval_ms`
- `poll_max_interval_ms`
- `timeout_behavior=return_error`
- `timeout_behavior=return_task_reference`
- `client_timeout_response_template`

## Poll Template

`upstream_poll_template` is required for `upstream_mode=async_lro`.

Shape:

```json
{
  "method": "GET",
  "path": "/v1/videos/{{result.provider_task_id}}",
  "headers": {
    "Authorization": "Bearer {{binding.provider_api_key}}"
  },
  "body": null,
  "status": {
    "path": "$.status",
    "pending": ["queued", "in_progress", "pending", "running", "processing"],
    "succeeded": ["completed", "succeeded", "complete", "done", "success"],
    "failed": ["failed", "cancelled", "canceled", "error"]
  },
  "provider_task_id": "$.id",
  "next_poll_after_ms": {
    "header": "openai-poll-after-ms",
    "default": 1000
  },
  "next_poll_url": "$.poll_url"
}
```

Runtime requirements:

- Supports GET and POST poll requests.
- Supports templated path, headers, and optional body.
- Supports status extraction from JSON path.
- Supports provider task id extraction from submit response and poll response.
- Supports next poll URL extraction from JSON path.
- Supports next wait extraction from response header or configured default.
- Persists poll context in `TaskRecord.poll_context`.
- Stores final provider response before rendering client response.

## OpenAI-Compatible Response Requirements

### Images

`POST /v1/images/generations`, `/edits`, and `/variations` must return OpenAI-compatible image response JSON for sync mode:

```json
{
  "created": 1710000000,
  "data": [
    {"url": "https://..."}
  ]
}
```

or:

```json
{
  "created": 1710000000,
  "data": [
    {"b64_json": "..."}
  ]
}
```

depending on configured `response_format` and provider result.

Async image mode must return a configured OpenAI-compatible task reference, not the legacy internal submit shape unless the template explicitly chooses that shape.

### Embeddings

`POST /v1/embeddings` must return:

```json
{
  "object": "list",
  "data": [
    {
      "object": "embedding",
      "embedding": [0.1, 0.2],
      "index": 0
    }
  ],
  "model": "text-embedding-3-large",
  "usage": {
    "prompt_tokens": 8,
    "total_tokens": 8
  }
}
```

Embeddings default to sync mode. Async embedding is allowed only if a profile explicitly enables it.

### Videos

`POST /v1/videos` must return an OpenAI-compatible video object:

```json
{
  "id": "video_123",
  "object": "video",
  "created_at": 1710000000,
  "status": "queued",
  "model": "sora-2",
  "progress": 0,
  "prompt": "..."
}
```

`GET /v1/videos/{video_id}` must return the current OpenAI-compatible video object. Status mapping:

- internal queued/submitted -> `queued`
- internal running/processing -> `in_progress`
- internal succeeded -> `completed`
- internal failed/timed_out/cancelled -> `failed`

`GET /v1/videos/{video_id}/content` must return the configured artifact bytes or proxy URL behavior. If bytes are not stored locally, runtime may redirect to a signed/proxied artifact URL only when the template declares that behavior.

## Error Behavior

OpenAI-compatible task endpoints should emit OpenAI-shaped JSON errors:

```json
{
  "error": {
    "message": "...",
    "type": "invalid_request_error",
    "code": "GW_TASK_PROFILE_NOT_CONFIGURED"
  }
}
```

Required error codes:

- `GW_TASK_ENDPOINT_NOT_CONFIGURED`
- `GW_TASK_MODEL_REQUIRED`
- `GW_TASK_PROFILE_NOT_CONFIGURED`
- `GW_TASK_CLIENT_MODE_UNSUPPORTED`
- `GW_TASK_POLL_TEMPLATE_REQUIRED`
- `GW_TASK_POLL_STATUS_UNKNOWN`
- `GW_TASK_TEMPLATE_INVALID`
- `GW_TASK_UPSTREAM_FAILED`
- `GW_TASK_ARTIFACT_NOT_FOUND`

## Go Contract Requirements

Go must publish runtime-projection data sufficient for Rust to route by method/path before codec detection:

- route registry entries derived from profile `client_request_template` endpoint metadata
- binding index entries keyed by `<global_model_id>:<task_kind>`
- immutable profile revision payloads
- immutable binding revision payloads
- effective profile/binding template snapshots

Template validation must reject:

- `upstream_mode=async_lro` without `upstream_poll_template`
- OpenAI-compatible endpoint profile without method/path/task kind/model extractor
- `client_modes` that omit `default_client_mode`
- `client_async_submit_response_template` missing when async client mode is enabled
- `client_timeout_response_template` missing when sync LRO timeout may return a task reference or error
- duplicate endpoint registry entries that cannot disambiguate by model/task kind

## Documentation Updates Required With Implementation

Implementation PRs must update:

- `docs/spec/08-async-tasks.md`
- `docs/workflow.md` for early filter and request filter ordering
- `docs/go-contracts/main.md` and the relevant parts for new route projection and template payload fields
- `docs/ROADMAP.md` to remove the closed async-task divergence and add no residual for this feature
- Go contract fixtures under `docs/contracts/async-task-template/v1/fixtures`

## Required Phases

### Phase 1: Runtime Route Admission And Endpoint Registry

Deliverables:

- Early filter bypass for configured OpenAI-compatible task endpoints.
- Request filter task endpoint detection before codec parse requirement.
- Static minimum route registry for known OpenAI-compatible image/embedding/video endpoints.
- Tests proving `/v1/images/generations`, `/v1/embeddings`, and `/v1/videos` are admitted as task candidates while `/v1/chat/completions` still uses codec path.

### Phase 2: Published Binding/Profile Selection

Deliverables:

- Extract model and task kind from OpenAI-compatible requests.
- Load binding index by `<model>:<task_kind>`.
- Load binding/profile revisions.
- Build `ExecutionRequest` with profile, binding, identity, task, client request, selected client mode, and upstream mode.
- Tests for image, embedding, and video binding selection.

### Phase 3: Sync Inline Template Execution

Deliverables:

- Inline execution for `sync + sync`.
- HTTP transport for template executor.
- OpenAI-compatible image and embedding response rendering.
- No codec usage for image/embedding requests.
- Tests for `/v1/images/generations` and `/v1/embeddings` sync responses.

### Phase 4: Async Submit And Worker Template Execution

Deliverables:

- Async client mode creates durable task records with profile/binding snapshots.
- Worker renders provider requests from templates.
- Worker renders final client response from templates before settle.
- Internal `/v1/tasks` routes continue to work.
- Tests for async image/video submit and worker completion.

### Phase 5: Poll Template Runtime

Deliverables:

- `upstream_poll_template` execution in inline and worker runtime.
- Provider-specific status mapping.
- Header/query/body-based next poll handling.
- OpenAI video `create_and_poll` compatible behavior.
- Tests for OpenAI-style video poll and third-party LRO poll.

### Phase 6: OpenAI Video Retrieve And Content

Deliverables:

- `GET /v1/videos/{video_id}` returns OpenAI-compatible video object from task state.
- `GET /v1/videos/{video_id}/content` returns artifact or configured redirect/proxy response.
- Tests for queued, in-progress, completed, failed, and artifact-not-found states.

### Phase 7: Go Validation, Projection, Fixtures, And Docs

Deliverables:

- Go validates the new endpoint/poll template rules.
- Go publishes route registry and binding/profile revisions for Rust.
- Fixture coverage for OpenAI image generation/edit/variation, embedding, video create/retrieve/content, and at least one third-party LRO mapping.
- As-built docs updated and roadmap divergence removed.

## No-Omission Acceptance Matrix

| Gap | Required phase |
|---|---|
| Early filter rejects OpenAI media/embedding paths | Phase 1 |
| Published endpoint registry not wired | Phase 1 |
| Model/task-kind extraction missing | Phase 2 |
| Binding/profile selection not live | Phase 2 |
| Hard-coded async client mode | Phase 2 / Phase 3 |
| Hard-coded upstream mode | Phase 2 / Phase 3 |
| Sync image response missing | Phase 3 |
| Sync embedding response missing | Phase 3 |
| Async task snapshots missing | Phase 4 |
| Worker template execution missing | Phase 4 |
| Poll template missing | Phase 5 |
| OpenAI video poll shape missing | Phase 5 |
| OpenAI video retrieve missing | Phase 6 |
| OpenAI video content missing | Phase 6 |
| Go template validation incomplete | Phase 7 |
| Go projection incomplete | Phase 7 |
| Docs stale vs code | Phase 7 |

## Verification Gates

Required local verification before claiming complete:

```bash
cargo test -p gw-proxy task_detector
cargo test -p gw-task-runtime
cargo test -p gw-task
cargo test -p gw-store async_task
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Required Go-side verification after Go changes:

```bash
go test ./modules/asynctask/...
go test ./modules/provider/...
```

Contract-level smoke tests must include OpenAI Python SDK-style requests for:

- `client.images.generate(...)`
- `client.embeddings.create(...)`
- `client.videos.create(...)`
- `client.videos.retrieve(...)`
- `client.videos.download_content(...)`

