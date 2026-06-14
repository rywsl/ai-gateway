# P0 #13 — Codec / Adapter Contract Defects (implementation spec)

Source of truth: Codex analysis (`work/claude-superpowers-router/p0-13-codec-adapter/codex-analysis.md`).
Method: reproduce each sub-finding first, fix only confirmed failures, document the rest with proof.
Scope: `gw-adapter`, `gw-codec`, `gw-capability` + tests/docs. No Go edits. No commit/push.

## Reproduction summary (as-built, current tree)

| # | Sub-finding | Verdict | Action |
|---|-------------|---------|--------|
| 1 | Kiro SigV4 hard-codes `POST` in canonical request | **Real** — `kiro.rs:151` hardcodes `"POST\n…"`; `AdapterCtx` carries no HTTP method | Thread method through `AdapterCtx`, sign the real method |
| 2 | Gemini `safetySettings` / `toolConfig` dropped on parse→render | **Real** — `GeminiRequest` wire struct has no such fields; serde silently drops them; `From<GeminiRequest>` never captures them. Lost whenever a Gemini request is re-rendered (forced-params / model-mapping / body-rules mutation forces the non-zero-copy parse→render path) | Add typed wire fields, round-trip via `RequestIR.extensions` (Gemini-only read-back) |
| 3 | Vertex adapter missing `x-goog-api-key` | **Not applicable** — Vertex AI (`aiplatform.googleapis.com`) authenticates with OAuth Bearer **only**; `x-goog-api-key` belongs to the Gemini *Developer* API (`generativelanguage.googleapis.com`), which is served by `NoopAdapter` + `auth_template = "x-goog-api-key: {api_key}"`. Setting it on Vertex would be wrong | Lock the OAuth-only contract with a test; document |
| 4 | Capability parser "dual-track" risk | **Not a bug** — every string→`Capability` conversion funnels through the single `resolver::parse_capability` (`backend.rs`, `filter.rs::parse_capabilities`, resolver header path). No second mapping table. `Tagged` is intentional. Required-side typed variants are only ever produced by the same parser, so both sides agree even for aliases | Add regression test locking all known-name→typed mappings; document |
| 5 | Streaming thinking/refusal silent drop | **Partly real** — thinking drops are already `observe()`d (openai chat, gemini) or rendered (anthropic, openai-responses, kiro). **Refusal** is the gap: OpenAI *Responses* parses `response.refusal.delta` into `RefusalDelta` but its `render_stream_chunk` silently drops it (functional loss); Anthropic + Gemini drop `RefusalDelta` with no observability | Render refusal in OpenAI Responses; add `observe()` drop markers for Anthropic + Gemini |

## Fix detail

### #1 Kiro method (gw-adapter)
- `AdapterCtx`: add `pub http_method: &'a str`.
- `kiro.rs::build_authorization`: replace the hardcoded `"POST"` line of the canonical request with `ctx.http_method`.
- Producer: `gw-proxy/src/filters/upstream.rs` construction site sets `http_method: upstream_request.method.as_str()`.
- All test constructions of `AdapterCtx` (kiro.rs, vertex.rs) add the field.
- Tests (kiro.rs): canonical request signs the actual method — a non-POST method changes the signature vs POST; POST path unchanged (existing tests stay green).

### #2 Gemini safetySettings / toolConfig (gw-codec)
- `gemini/wire.rs::GeminiRequest`: add
  `safety_settings: Option<Box<RawValue>>` and `tool_config: Option<Box<RawValue>>`
  (camelCase → `safetySettings` / `toolConfig`, `skip_serializing_if = Option::is_none`).
- `gemini/via.rs`:
  - `From<GeminiRequest>`: stash present values into `RequestIR.extensions` under keys `"safetySettings"` / `"toolConfig"`.
  - `TryFrom<&RequestIR>`: read back **only** those two keys from `extensions` into the wire fields.
- Read-back is restricted to the two Gemini-native keys so foreign extensions (e.g. OpenAI extras) are not leaked into a Gemini body on cross-protocol render.
- Residual risk (pre-existing, out of scope): generic `extensions` round-trips through any codec's render, so an exotic Gemini→OpenAI conversion could surface these as extras — same property OpenAI extras already have. Not introduced here.
- Tests: native Gemini body with `safetySettings` + `toolConfig` survives `parse_request` → `render_request`.

### #3 Vertex (gw-adapter)
- No code change to `vertex.rs`. Add a test asserting `rewrite_auth` does **not** set `x-goog-api-key` and uses `Authorization: Bearer`.
- Note in module/doc: Vertex = OAuth Bearer; Gemini Developer API key = NoopAdapter + `auth_template`.

### #4 Capability (gw-capability)
- No production change. Add a regression test asserting each known name (`vision`, `audio`, `video`, `thinking`, `tool_use`, `streaming_only`, `openai_responses_shape`, `vertex_ai_service_account`) parses to its typed variant (not `Tagged`), and that case-folding holds. Locks against accidental alias drift / a future second table.

### #5 Streaming refusal (gw-codec)
- `openai/responses/codec.rs::render_stream_chunk`: render `RefusalDelta` as `response.refusal.delta` (mirrors the parser).
- `anthropic/codec.rs` + `gemini/codec.rs`: split `RefusalDelta` out of the silent-drop arm and `observe()` a `*_render_drop_refusal_delta` marker (consistent with their existing `*_render_drop_*` markers). Drop remains correct (no streaming refusal shape in those wires); only observability is added.
- Tests: OpenAI Responses render emits `response.refusal.delta`; Anthropic + Gemini increment `gw_errors_total{code=*_render_drop_refusal_delta}` (via `DebuggingRecorder`).

## Verification
```
cargo fmt --all -- --check
cargo test -p gw-adapter
cargo test -p gw-codec
cargo test -p gw-capability
cargo clippy -p gw-adapter -p gw-codec -p gw-capability --all-targets -- -D warnings
```
Docs to sync: `docs/spec/01-protocol-codec.md` (Gemini passthrough fields), `docs/spec/07-vendor-integration.md` (Kiro method, Vertex auth contract) if those areas are described there.
