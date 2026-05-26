# learnings

- IR foundation fields landed in `crates/gw-core/src/ir.rs`; `UsageIR` is no longer `Copy` because it now carries `Option<Cow<'static, str>>` service/geo fields. `cargo build -p gw-core` and `cargo test -p gw-core` pass. A workspace build now fails in `gw-codec` because existing exhaustive matches/struct literals need to learn `MessageIR::Developer`, `FinishReasonIR::Refusal`, new `StreamEventIR` variants, and new `UsageIR`/`ResponseIR` fields; codec edits were intentionally not made in this foundation-only task.
- Task 2 codec fixes use `..Default::default()` for new `UsageIR`/`ResponseIR` fields and minimal enum arms: `Developer` renders as OpenAI/Gemini system or Anthropic user; `Refusal` renders to vendor-safe stop/completed equivalents when no dedicated wire value exists; new stream-only rich deltas are silently skipped in legacy stream renderers until later tasks produce them.
- `cross_render.rs` now gates response-side `refusal`, `annotations`, `server_tool_blocks`, `redacted_thinking`, and response `service_tier`; `FinishReasonIR::Refusal` is lossy-degraded to `Stop` when the target lacks refusal support. Tests cover the new drop matrix and refusal finish-reason mapping.
- `crates/gw-proxy/src/filters/response.rs` needed two compile-only follow-ups after the IR extension: TTFT tracking now treats `RefusalDelta`, `CitationsDelta`, `ServerToolStart`, and `ServerToolDelta` like other first-content stream events, and non-stream usage extraction now borrows with `ir.usage.as_ref()` so `CrossRender { source: ir }` can still consume the full response IR.
- Task 3 changed `OpenAiMessage::User { content: Cow<str> }` to `OpenAiMessage::User { content: OpenAiUserContent<'a> }`. `OpenAiUserContent` is an enum with `Text(Cow)` and `Parts(Vec<OpenAiContentPart>)` with a hand-written `Deserialize` visitor (not `#[serde(untagged)]`) because the borrowing `'de: 'a` lifetime bound requires `deserialize_any` to dispatch string vs. seq. `OpenAiContentPart` uses `#[serde(tag = "type")]` for `text`, `image_url`, `input_audio`, `file` variants. Tests must use `serde_json::from_str` not `from_value` because the custom visitor borrows from the input buffer.
- Task 4 adds a distinct `OpenAiMessage::Developer { content: Cow<'a, str> }` wire variant. Same-protocol OpenAI parse/render must preserve `developer` exactly (not degrade to `system`), while Anthropic/Gemini cross-renders intentionally fold `MessageIR::Developer` into their system surfaces (`system` and `system_instruction.parts`).
- Task 5 models Anthropic response-side server tool blocks as typed content enum wrappers plus opaque `extra` maps, projecting them through `ResponseIR.server_tool_blocks`. Internally tagged serde enums cannot deserialize `Box<RawValue>` fields directly in this shape, so `server_tool_use.input` uses a small `with` adapter that buffers through `serde_json::Value` and reconstitutes `RawValue`.

## Task 6: Gemini GenerationConfig Extension (Gap #2)

- `GeminiGenerationConfig` was non-generic (no lifetime). Adding `<'a>` required updating `GeminiRequest`'s field to `Option<GeminiGenerationConfig<'a>>` with `#[serde(borrow)]`.
- `serde(borrow)` on the `generation_config` field is needed so serde can borrow `Cow<'a, str>` fields inside nested structs.
- `stop_sequences` uses `Vec<Cow<'a, str>>` (not `SmallVec`) to match Gemini's JSON array shape and keep serde derive simple.
- `response_schema` uses `serde_json::Value` (not `Box<RawValue>`) because the struct already uses `Value` for function call args (untagged enum limitation documented in wire.rs header).
- IR `ReasoningConfig.effort` maps to Gemini `thinkingConfig.thinkingLevel`; `budget_tokens` maps to `thinkingBudget`.
- `candidate_count`, `response_mime_type`, `response_schema` are wire-only fields — no IR slot, preserved on same-protocol passthrough only.
- Pre-existing compile errors in `openai/via.rs` (missing `annotations`/`refusal` fields) are from other in-flight tasks, not from this change.

## Task 7: OpenAI Chat refusal + annotations wire (Gap #6, #7)

- `OpenAiMessage::Assistant` variant now carries `refusal: Option<Cow<'a, str>>` and `annotations: Option<Vec<OpenAiAnnotation<'a>>>` alongside `content` and `tool_calls`. Both use `skip_serializing_if = "Option::is_none"` so they're absent on request-side serialization.
- `OpenAiAnnotation<'a>` has `ty: Cow<'a, str>` (serde rename "type") + `url_citation: Option<OpenAiUrlCitation<'a>>`.
- `OpenAiUrlCitation<'a>` has `start_index: u32, end_index: u32, title: Cow<'a, str>, url: Cow<'a, str>`.
- `OpenAiStreamDelta` now has `refusal: Option<Cow<'a, str>>`.
- In `openai_msg_to_ir`, the new fields are destructured with `refusal: _, annotations: _` — they're not relevant at the message IR level; extraction happens at the response level in `From<OpenAiResponse> for ResponseIR`.
- `From<OpenAiResponse> for ResponseIR` extracts refusal + annotations from the first choice's assistant message and maps them to `ResponseIR.refusal` and `ResponseIR.annotations` (via `AnnotationIR`).
- `TryFrom<&ResponseIR> for OpenAiResponse` reverse-maps: `ir.refusal` → `message.refusal`, `ir.annotations` → `message.annotations` via `OpenAiAnnotation`. The url_citation mapping requires all four fields (url, title, start_index, end_index) to be present to produce an `OpenAiUrlCitation`; partial data → no url_citation.
- Stream parse: `delta.refusal` non-empty → `StreamEventIR::RefusalDelta { text }`.
- Stream render: `RefusalDelta` → `data: {"choices":[{"index":0,"delta":{"refusal":"..."}}]}`.
- Previously `RefusalDelta` was silently dropped in `render_stream_chunk`; now it's properly rendered as an SSE chunk.
- Tests added: `parse_response_extracts_refusal`, `parse_response_extracts_annotations`, `refusal_annotations_round_trip`, `parse_stream_chunk_refusal_delta`, `render_stream_chunk_refusal_delta`.

## Task 13: OpenAI Audio Modality Wire (Gap #16)

- `OpenAiRequest` now has `modalities: Option<Vec<Cow<'a, str>>>` and `audio: Option<OpenAiAudioConfig<'a>>` fields. Both use `skip_serializing_if = "Option::is_none"` so they're absent when not set (no wire bloat for non-audio requests).
- `OpenAiAudioConfig<'a>` is a simple struct: `{ voice: Cow<'a, str>, format: Cow<'a, str> }`.
- `OpenAiAudioResponse<'a>` is: `{ id: Cow, data: Cow, expires_at: u64, transcript: Cow }` — added to the `OpenAiMessage::Assistant` variant as `audio: Option<OpenAiAudioResponse<'a>>`.
- Request parse: `OpenAiRequest.audio` → `RequestIR.audio_config` (mapping `voice`/`format` to `Option` fields); `OpenAiRequest.modalities` → `AudioConfig.modalities`.
- Request render: `RequestIR.audio_config` → `OpenAiRequest.audio` + `modalities`. When `audio_config` has voice/format but no explicit modalities, render auto-inserts `["text", "audio"]`.
- Response parse: `message.audio` (OpenAI-specific) → `MetadataIR.extensions["openai_audio"]` as a JSON object with all four fields. This avoids adding OpenAI-specific fields to the protocol-neutral `ResponseIR`.
- Response render: `metadata.extensions["openai_audio"]` → `OpenAiMessage::Assistant.audio` via field-by-field extraction from `serde_json::Value`.
- `openai_msg_to_ir` destructures `audio: _` (audio is not relevant at the message IR level; it's extracted at the response level).
- Tests: 5 tests covering request config round-trip, absent-when-not-set, IR-initiated modalities auto-fill, response audio parse+round-trip, response without audio omits field.

## Task 15: OpenAI structured output / response_format (Gap #13)

- Before this task, `ResponseFormatIR` existed in `gw-core`, but no codec actually mapped request-side `response_format`; OpenAI `response_format` fell through as an unknown field into `extra`, so same-protocol passthrough worked but cross-protocol render had no IR signal.
- `ResponseFormatIR::JsonSchema(Box<RawValue>)` is sufficient for the shared IR shape. The gateway only needs one opaque JSON payload that can carry OpenAI's `json_schema { name, description, schema, strict }` envelope losslessly; strong typing was added only at the OpenAI wire boundary so those subfields can parse/render cleanly.
- `crates/gw-codec/src/openai/wire.rs` now has `OpenAiResponseFormat::{Text, JsonObject, JsonSchema { json_schema: OpenAiJsonSchema }}` plus `OpenAiJsonSchema { name, description, schema, strict }`.
- `crates/gw-codec/src/openai/via.rs` now maps OpenAI request `response_format` ↔ `RequestIR.response_format`. For `json_schema`, the IR stores the inner envelope object (`name` / `description` / `schema` / `strict`) inside `ResponseFormatIR::JsonSchema(Box<RawValue>)`, then rehydrates that into OpenAI's wire shape on render.
- `crates/gw-codec/src/gemini/via.rs` now maps Gemini `generationConfig.responseMimeType` + `responseSchema` ↔ `RequestIR.response_format`: `application/json` + schema => `JsonSchema`, `application/json` without schema => `JsonObject`, `text/plain` => `Text`.
- Anthropic request wire currently has no `output_config.format.json_schema` surface in this repo, so there was nothing to wire here for Task 15.
- Cross-render consequence: OpenAI `json_schema.name` / `description` / `strict` survive OpenAI same-protocol round-trip, but Gemini can only carry the schema body plus JSON mime type, so OpenAI ← Gemini re-render intentionally loses those OpenAI-only envelope fields.
- Tests added in `crates/gw-codec/tests/openai_roundtrip.rs` and `crates/gw-codec/tests/cross_protocol.rs` to prove OpenAI `response_format.json_schema` parse/render and OpenAI → Gemini → OpenAI schema-body preservation.

## Task 8: Anthropic redacted_thinking + new stop reasons (Gap #8, #9)

- `AnthropicContent::RedactedThinking { data: Cow<'a, str> }` added to wire.rs before `Unknown`. Uses `#[serde(rename = "redacted_thinking")]` (explicit rename, though `rename_all = "snake_case"` would produce the same).
- `map_stop_reason_to_finish` now handles: `"refusal"` → `FinishReasonIR::Refusal`, `"pause_turn"` / `"model_context_window_exceeded"` → `FinishReasonIR::Other(Cow::Owned(...))`.
- `map_finish_to_stop_reason` return type changed from `&'static str` to `Cow<'static, str>`. This allows `Other(s)` to clone the original string for same-protocol round-trip fidelity (e.g. `pause_turn` → IR → `pause_turn`). The `Cow<'static, str>` in `FinishReasonIR::Other` means `.clone()` is cheap for borrowed statics and correct for owned strings.
- Call site in `TryFrom<&ResponseIR> for AnthropicResponse` updated: `.map(map_finish_to_stop_reason)` now directly returns `Option<Cow<'static, str>>`, removing the previous `.map(Cow::Borrowed)` chain.
- Stream parse: `redacted_thinking` content blocks in `ContentBlockStart` are silently skipped (no `OpenBlock` state, no IR events). They don't carry incremental deltas in Anthropic's stream protocol.
- Response parse: `RedactedThinking { data }` blocks → `redacted_thinking_blocks: Vec<RedactedThinkingIR>` → `ResponseIR.redacted_thinking`.
- Response render: `ir.redacted_thinking` blocks emitted after thinking blocks and before text/tool_use content.
- `RedactedThinking` added to all exhaustive match arms: `lower_user_blocks`, `lower_assistant_blocks`, and `ContentBlockStart` dispatch in codec.rs.

## Task 12: OpenAI reasoning_effort + service_tier (Gap #14, #21)

- `OpenAiRequest` now has explicit `reasoning_effort: Option<Cow<'a, str>>` and `service_tier: Option<Cow<'a, str>>` fields (both `#[serde(default, borrow, skip_serializing_if = "Option::is_none")]`), placed before `#[serde(flatten)] extra` so serde picks them up before the catch-all.
- `OpenAiResponse` now has explicit `service_tier: Option<Cow<'a, str>>` between `usage` and `extra`.
- Parse request: `reasoning_effort` → `RequestIR.reasoning_config = Some(ReasoningConfig { effort: Some(val), budget_tokens: None })`. When absent, `reasoning_config` stays `None`.
- Render request: `RequestIR.reasoning_config.effort` → `reasoning_effort`. `budget_tokens` is OpenAI Chat-irrelevant (it's a Gemini/Anthropic concept) and silently dropped.
- Parse/render response: `service_tier` ↔ `ResponseIR.service_tier` direct Cow clone.
- Responses API: `parse_request` is not wired (returns error); the proxy layer uses `responses_to_chat` which works at `Value` level — `reasoning` and `service_tier` flow through `extras` → Chat shape `extras` naturally. No Responses-specific changes needed.
- Responses API response wire does not include `service_tier` (not part of the OpenAI Responses API response object); the IR's `service_tier` defaults to `None`.
- 6 round-trip tests in `reasoning_service_tier_tests`: reasoning_effort request round-trip, service_tier request round-trip, service_tier response round-trip, absent fields omission, combined fields, IR with budget_tokens renders only effort.

## Task 9: Gemini thinking parts + thoughtsTokenCount (Gap #10)

- `GeminiPart::Text` variant extended with `thought: Option<bool>` and `thought_signature: Option<Cow<'a, str>>`. Both use `#[serde(default, skip_serializing_if = "Option::is_none")]` so they're absent when None, preserving backward compat.
- `thought_signature` requires explicit `#[serde(rename = "thoughtSignature")]` because `GeminiPart` is `#[serde(untagged)]` — untagged enums don't propagate `rename_all` to individual variant fields. Without the rename, the field silently fails to parse from camelCase JSON.
- `GeminiUsage` gained `thoughts_token_count: Option<u32>` — it remains `Copy` since `Option<u32>` is `Copy`.
- Parse response: `thought == Some(true)` Text parts → `ThinkingBlockIR` in `ResponseIR.thinking.blocks`; `thought_signature` → `ThinkingBlockIR.signature`. Non-thought Text parts still accumulate into `assistant_text` as before.
- Render response: `ResponseIR.thinking` blocks → Text parts with `thought: Some(true)` + `thought_signature` from `ThinkingBlockIR.signature`, emitted before content parts.
- Stream parse (codec.rs): `thought == Some(true)` Text → `StreamEventIR::Delta { thinking: Some(text), text: None }`.
- Usage mapping: `thoughts_token_count` ↔ `UsageIR.thoughts_tokens` in both parse and render directions.
- `WireCapabilities` for `GeminiGenerate` updated: `supports_thinking: true`. Cross-render tests updated to reflect Gemini no longer dropping thinking blocks.
- All existing match arms on `GeminiPart::Text { text }` updated: constructions add `thought: None, thought_signature: None`; pattern matches use `{ text, .. }`.
- Tests: `parse_response_thinking_parts`, `thinking_response_roundtrip`, `parse_stream_thinking_delta`.

## Task 11: Anthropic Document + SearchResult input blocks (Gap #12)

- `AnthropicCacheControl<'a>` added to wire.rs: `{ kind: Cow<'a, str> }` with `#[serde(rename = "type")]` — matches Anthropic's `{"type":"ephemeral"}` shape.
- `DocumentSource<'a>` is a `#[serde(tag = "type")]` enum with `Base64 { media_type, data }`, `Text { text }`, `Url { url }`, `Content { content: Vec<AnthropicContent> }`.
- `AnthropicContent::Document` carries `source: DocumentSource`, plus optional `title`, `context`, `cache_control`.
- `AnthropicContent::SearchResult` carries `source: Cow` (URL string), `title: Cow`, `content: Vec<AnthropicContent>` (nested text blocks), optional `cache_control`. Uses `#[serde(rename = "search_result")]` since the type tag is `search_result`.
- Parse (lower_user_blocks): Document with `Base64`/`Url` source → `ContentPart::File { source: MediaSource::B64/Url }`. Document with `Text`/`Content` source → `ContentPart::Text` (title + context prepended). SearchResult → `ContentPart::Text` (title + body text + source URL appended).
- Render (render_user_blocks): `ContentPart::File` now renders to `AnthropicContent::Document` (was previously dropped). B64 → `DocumentSource::Base64`, Url → `DocumentSource::Url`.
- Response side: Document and SearchResult added to defensive ignore arms in response parse (`From<AnthropicResponse>`) since they're request-side blocks not valid in assistant responses.
- Streaming: Document and SearchResult added to ignore arms in `codec.rs` `ContentBlockStart` match.
- `MediaSource` import added to via.rs alongside existing `ImageSource`.
- Pre-existing compile errors from other in-flight tasks (RedactedThinking variant, ReasoningConfig, OpenAi audio/response_format, Gemini thought fields) are unrelated to this task.

## Task 17: Anthropic cache TTL + inference_geo + service_tier (Gap #18, #26)

- `AnthropicUsage` extended with `cache_creation: Option<AnthropicCacheCreationDetails>` (nested struct with `ephemeral_1h_input_tokens` and `ephemeral_5m_input_tokens`) and `server_tool_use: Option<AnthropicServerToolUsage>` (nested struct with `web_search_requests` and `web_fetch_requests`). Both nested structs are `Copy` + `Default`.
- `AnthropicUsage` lost `Copy` derive (changed to `Clone, Debug, Default`) because the struct now contains `Option<AnthropicCacheCreationDetails>` and `Option<AnthropicServerToolUsage>`. No call site relied on `Copy`.
- `service_tier` and `inference_geo` are Anthropic response-level fields (not inside `usage`), so they live on `AnthropicResponse<'a>` and `AnthropicRequest<'a>` as `Option<Cow<'a, str>>`. The IR carries them on both `ResponseIR.service_tier` and `UsageIR.service_tier`/`UsageIR.inference_geo`.
- Response parse merges response-level `inference_geo`/`service_tier` into `UsageIR` (since that's where the IR stores them) and also sets `ResponseIR.service_tier` for the response-level slot.
- Response render sources `service_tier` from `ResponseIR.service_tier` first, falling back to `UsageIR.service_tier`; `inference_geo` comes from `UsageIR.inference_geo`.
- `cache_creation_input_tokens` (the flat field already present) now maps to `UsageIR.cache_creation_tokens` — previously it was accepted but ignored during render (always emitted as `None`).
- Streaming usage extraction in codec.rs updated in both `MessageStart` and `MessageDelta` handlers to map `cache_creation` and `server_tool_use` nested fields.
- Fixed two pre-existing compile errors from other in-flight tasks: missing brace in `openai/via.rs` (`user_multimodal_image_url_parses` test), and missing `audio_tokens`/`accepted_prediction_tokens`/`rejected_prediction_tokens` fields in `openai/responses/via.rs` struct constructors.
- 4 new tests in `anthropic_roundtrip.rs`: `cache_ttl_usage_round_trips_through_ir`, `inference_geo_service_tier_round_trips_through_ir`, `request_service_tier_round_trips`, `absent_usage_details_omitted_from_wire`.

## Task 14: Anthropic thinking config options (Gap #15)

- `AnthropicRequest<'a>` now carries optional `thinking` and `output_config` fields; `thinking` is a tagged enum with `enabled { budget_tokens, display }`, `adaptive { display }`, and `disabled`, while `output_config` currently maps `effort` plus opaque `format` passthrough on same-protocol bodies.
- Anthropic request parse maps `thinking.display` and adaptive/disabled/enabled state into `MetadataIR.anthropic_thinking`, preserving `kind`, `display`, `budget_tokens`, and `adaptive`; `enabled.budget_tokens` also seeds `RequestIR.reasoning_config.budget_tokens`.
- `output_config.effort` maps to `RequestIR.reasoning_config.effort`; if both thinking and output_config are present, reasoning config is merged rather than overwritten.
- Request render prefers `MetadataIR.anthropic_thinking` as the source of truth: `adaptive=true` or `kind="adaptive"` renders `{type:"adaptive"}`, `kind="disabled"` renders `{type:"disabled"}`, and enabled rendering requires a budget from either `AnthropicThinking.budget_tokens` or `ReasoningConfig.budget_tokens` so legacy `{type:"enabled",budget_tokens:N}` behavior stays unchanged.
- Coverage added in `crates/gw-codec/tests/anthropic_roundtrip.rs` for parsing adaptive thinking with display + effort and round-tripping enabled thinking with `display:"omitted"` plus `output_config.effort`.

## Task 10: OpenAI Responses output item expansion (Gap #11)

- OpenAI Responses output items now model server-tool variants as typed enum variants with shared structs and flattened extra maps; this keeps known identifiers/status/name fields typed while preserving opaque vendor payload keys.
- Response parsing maps new server-tool output items into `ResponseIR.server_tool_blocks`; any `input` key is lifted into the IR `RawValue` input slot, while `call_id` remains in `extra` because the existing IR shape has typed `id`, `name`, `input`, and `extra` fields rather than a dedicated call-id field.
- Rendering reverses `server_tool_blocks` for known OpenAI Responses kinds. Unknown kinds are skipped with a warning instead of inventing invalid wire item types.
- `message.content` accepts `refusal` parts and stores the first refusal text in `ResponseIR.refusal`; existing output_text, function_call, and reasoning summary behavior remains unchanged.
- Responses stream output item announcements for the new server-tool item types are explicitly skipped with debug logging; text/function-call/reasoning deltas keep their previous behavior.

## Task 19: OpenAI web_search_options, prediction, custom tool (Gap #22, #23, #24)

- `OpenAiRequest` now has `web_search_options: Option<Box<RawValue>>` and `prediction: Option<Box<RawValue>>` fields, placed before `#[serde(flatten)] extra` so serde picks them up before the catch-all. Both use `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Parse: `web_search_options` → `RequestIR.web_search`, `prediction` → `RequestIR.prediction` (direct opaque `Box<RawValue>` passthrough).
- Render: reverse mapping `RequestIR.web_search` → `web_search_options`, `RequestIR.prediction` → `prediction` via `.clone()`.
- `OpenAiTool` changed from requiring `function: OpenAiToolFunction` to `function: Option<OpenAiToolFunction>` plus `#[serde(flatten)] extra: HashMap<Cow<'a, str>, serde_json::Value>`. This supports custom/server tools like `{"type":"web_search_preview","search_context_size":"medium"}` alongside function tools.
- The `extra` flatten map in `OpenAiTool` uses `serde_json::Value` (not `Box<RawValue>`) because `#[serde(flatten)]` with `Box<RawValue>` values fails at deserialization time — serde's flatten machinery uses `serde_json::Value` as an intermediate, and `Box<RawValue>` cannot round-trip through that intermediate. This is the same limitation documented in Task 5 for `AnthropicContent`.
- Parse direction: `filter_map` skips custom tools (where `function` is `None`) and only maps function tools to `ToolIR`. Custom tools pass through opaquely at the wire level but don't have an IR representation.
- Render direction: all `ToolIR` entries render as `OpenAiTool { ty: "function", function: Some(...), extra: HashMap::new() }`.
- 7 tests added: `web_search_options_round_trip`, `web_search_options_absent_when_not_set`, `prediction_round_trip`, `prediction_absent_when_not_set`, `custom_tool_parses_without_panic`, `custom_tool_skipped_in_ir_function_tools_preserved`, `custom_tool_wire_round_trip`.

## Task 20: Usage detail fields full-protocol extension (Gap #27)

- `OpenAiCompletionTokensDetails` extended with `audio_tokens: Option<u32>`, `accepted_prediction_tokens: Option<u32>`, `rejected_prediction_tokens: Option<u32>`. All use `skip_serializing_if = "Option::is_none"` to avoid wire bloat.
- `OpenAiPromptTokensDetails` extended with `audio_tokens: Option<u32>`.
- `OpenAiStreamUsage` extended with `completion_tokens_details: Option<OpenAiCompletionTokensDetails>` — previously stream frames only carried `prompt_tokens_details`.
- `GeminiUsage` extended with `tool_use_prompt_token_count: Option<u32>` — camelCase rename handled by `#[serde(rename_all = "camelCase")]` on the struct. Remains `Copy` since all fields are `Option<u32>` or `u32`.
- OpenAI response parse now maps: `prompt_details.audio_tokens` → `UsageIR.audio_tokens_in`, `completion_details.audio_tokens` → `UsageIR.audio_tokens_out`, `accepted_prediction_tokens` → `UsageIR.accepted_prediction_tokens`, `rejected_prediction_tokens` → `UsageIR.rejected_prediction_tokens`.
- OpenAI response render: `prompt_tokens_details` is emitted when either `cached_tokens` or `audio_tokens_in` is present. `completion_tokens_details` is emitted when any of `reasoning_tokens`, `audio_tokens_out`, `accepted_prediction_tokens`, or `rejected_prediction_tokens` is present (was previously hardcoded to `None`).
- Stream usage (codec.rs) now maps both `prompt_tokens_details` and `completion_tokens_details` including all new sub-fields.
- Gemini `tool_use_prompt_token_count` has no corresponding IR slot — it's a Gemini-specific prompt-side breakdown. The wire field parses/serializes correctly for same-protocol passthrough, but cross-protocol render sets it to `None` since the IR doesn't carry it.
- Pre-existing compile error in `crates/gw-codec/tests/openai_responses_wire.rs` (type mismatch on `OpenAiResponsesStreamEventType`) is unrelated to this task.

## Task 21: Anthropic citations_delta + MCP connector blocks (Gap #25, #30)

- `AnthropicStreamDelta::CitationsDelta { citation: serde_json::Value }` added to wire.rs. Uses `Value` (not `Box<RawValue>`) because the `#[serde(tag = "type")]` internally-tagged enum machinery cannot directly deserialize `Box<RawValue>` — same constraint as `ToolUse.input`.
- In codec.rs `dispatch_payload`, `citations_delta` is handled as a special case in `ContentBlockDelta`: it emits `StreamEventIR::CitationsDelta { data }` immediately by serializing the opaque `Value` to a JSON string. Unlike text/thinking/tool deltas, citations_delta does not need a corresponding `OpenBlock` builder — it's a standalone event.
- `AnthropicContent::McpToolUse(ServerToolUseBlock)` and `AnthropicContent::McpToolResult(ServerToolResultBlock)` added with `#[serde(rename = "mcp_tool_use")]` / `#[serde(rename = "mcp_tool_result")]`. They reuse the exact same typed wrapper structs as the existing server tool blocks.
- `McpToolUse` maps through `server_tool_use_to_ir("mcp_tool_use", block)` and `McpToolResult` through `server_tool_result_to_ir("mcp_tool_result", block)` in the response parse direction, placing them in `ResponseIR.server_tool_blocks`.
- `render_server_tool_block` gained `"mcp_tool_use"` and `"mcp_tool_result"` match arms that reconstruct the wire variants.
- Both MCP variants added to all exhaustive match arms: `lower_user_blocks` (ignore), `lower_assistant_blocks` (ignore), `ContentBlockStart` dispatch in codec.rs (ignore with log), and the `renders_each_server_tool_block_kind_to_matching_anthropic_variant` coverage test.
- Tests: `parse_stream_chunk_citations_delta` (SSE parse → IR), `parse_stream_chunk_mcp_block_start_is_skipped` (stream skip), `mcp_tool_use_response_block_round_trips_through_ir` (non-stream parse + render round-trip with `server_name` extra field preservation).
- Pre-existing compile errors in `openai/via.rs` (orphaned code fragment, duplicate test, brace mismatch in `user_multimodal_tests`) were fixed as collateral to enable compilation.

## Task 18: Gemini executableCode + codeExecutionResult + grounding (Gap #19, #20)

- `GeminiPart` gained two new variants before `Text`: `ExecutableCode { executable_code: GeminiExecutableCode }` and `CodeExecutionResult { code_execution_result: GeminiCodeExecutionResult }`. Both use `#[serde(rename = "...")]` on their field to match Gemini's camelCase wire keys (`executableCode`, `codeExecutionResult`).
- `GeminiExecutableCode<'a>` has `code: Cow<'a, str>` and `language: Cow<'a, str>`. `GeminiCodeExecutionResult<'a>` has `outcome: Cow<'a, str>` and `output: Cow<'a, str>`.
- `GeminiCandidate` gained `grounding_metadata: Option<serde_json::Value>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Used `serde_json::Value` instead of `Box<RawValue>` because `RawValue` inside `#[serde(untagged)]` / struct-level deserialization can trigger buffering issues (same rationale as FunctionCall args documented in wire.rs header). Since `GeminiCandidate` isn't untagged, `Box<RawValue>` would technically work, but `Value` keeps consistency with the opaque-passthrough pattern used elsewhere in the Gemini wire module.
- Parse: `ExecutableCode` → `ServerToolBlockIR { kind: "executable_code", input: RawValue(code as JSON string), extra: {language} }`. `CodeExecutionResult` → `ServerToolBlockIR { kind: "code_execution_result", input: RawValue(output as JSON string), extra: {outcome} }`.
- Parse: `grounding_metadata` → `ResponseIR.metadata.extensions["grounding_metadata"]` as opaque `Value`.
- Render: `ServerToolBlockIR` with kind `executable_code` → `GeminiPart::ExecutableCode`; kind `code_execution_result` → `GeminiPart::CodeExecutionResult`. Unknown kinds silently skipped.
- Render: `metadata.extensions["grounding_metadata"]` → `GeminiCandidate.grounding_metadata`.
- All exhaustive match arms updated: `lower_content` model/assistant arm, `lower_content` user arm, codec.rs streaming parse — new variants added to ignore arms alongside `InlineData`/`FileData`.
- `GeminiUsage` render needed `tool_use_prompt_token_count: None` — that field was added by another in-flight task.
- Pre-existing compile error in `openai/via.rs` (unclosed delimiter in `user_multimodal_tests` module) was blocking all gw-codec compilation; fixed with a missing `}` brace.
- Tests: `parse_response_executable_code_and_result`, `executable_code_roundtrip`, `grounding_metadata_roundtrip` — all pass.

## Task 22: OpenAI Responses streaming event taxonomy (Gap #29)

- `wire_stream_chunk.rs` now parses the SSE JSON `type` field into `OpenAiResponsesStreamEventType` with an `Unknown(String)` fallback; unknown future Responses events therefore deserialize and are skipped by the codec instead of becoming protocol errors.
- The stream parser now consumes `OpenAiResponsesStreamChunk` rather than raw `Value` dispatch. `response.output_text.delta` and legacy `response.text.delta` both still map to `StreamEventIR::Delta`; `response.function_call_arguments.delta` still maps to `ToolCallDelta` via the existing function-call accumulator.
- New key stream mappings: `response.refusal.delta` → `RefusalDelta`; `response.reasoning_summary_text.delta` and `response.reasoning_text.delta` → immediate `ThinkingDelta` while preserving the existing reasoning accumulator used by `response.output_item.done`; server-tool output-item announcements emit `ServerToolStart`; tool progress/code/image/custom-input deltas emit `ServerToolDelta` when a data-like field is present.
- Coverage added for refusal delta, direct reasoning-text delta, and unknown event skip. Unknown/unhandled known events log at debug and return no IR events.

## Task 16: cross-render lossy matrix + Gemini finish reasons

- `cross_render.rs` owns response-side lossy gating only; request-side reasoning config mapping is handled in per-protocol `via.rs`, and `ResponseIR` currently has no response reasoning-config field to adapt here.
- OpenAI response audio is represented as `MetadataIR.extensions["openai_audio"]`, not a dedicated `ResponseIR.audio` field. Cross-render audio gating therefore removes that extension and drops `UsageIR.audio_tokens_in/out` when the target lacks `supports_audio`.
- Response-level `service_tier` and `UsageIR.service_tier` both need capability checks; Gemini lacks service-tier support while OpenAI Chat/Responses and Anthropic keep it.
- Gemini-specific finish reasons from `FinishReasonIR::Other` map in cross-render as: `BLOCKLIST` / `PROHIBITED_CONTENT` / `SPII` / `MODEL_ARMOR` → `ContentFilter`; `MALFORMED_FUNCTION_CALL` still degrades to `Stop` with the normal `finish_reason.other` lossy metric.
- The cross-render tests now use a 4×4 `all_wire_pairs()` matrix for each new response field, deriving expected keep/drop from `WireCapabilities` rather than hand-maintained partial case lists.

## Task 24: docs/architecture.md + docs/go-contracts/ sync rule update

- Updated `docs/architecture.md` §2.1 gw-core crate description to document full v2 IR surface: ResponseIR.{refusal, annotations, server_tool_blocks, redacted_thinking, service_tier}, MessageIR::Developer, FinishReasonIR::Refusal, UsageIR 14 optional detail fields, 7 new StreamEventIR variants, 5 new supporting structs.
- Added 4 new deliberate behaviour entries to §8: ServerToolBlockIR opaque extra Map, Anthropic Unknown block skip-with-warn, Gemini grounding_metadata opaque passthrough, OpenAI audio response in metadata.extensions.
- Added new §11 documenting the WireCapabilities cross-render matrix (17 flags × 4 wire formats) with notable drop paths.
- `docs/go-contracts/` did NOT need updating: `UsageSnapshot` (the billing record struct) is unchanged at 6 dimensions. The new `UsageIR` fields exist only at codec/IR level for cross-render gating and don't flow into any Redis stream. `usage:detailed` stores raw wire bytes, not parsed IR fields.

## Task 23: full round-trip coverage audit

- Baseline `cargo test --workspace` passed before edits; final `cargo test --workspace 2>&1 | tee /tmp/task-23-output.txt` also passed with 0 failed. Evidence saved to `.omo/evidence/task-23-full-test.txt`.
- Audit found T1-T22 already covered the 30 gaps across codec unit tests and existing integration tests. Task 23 therefore avoided duplicate narrow tests and added consolidation round-trip tests for combined OpenAI Chat request/response surfaces, Anthropic document/search/server-tool/redacted-thinking/MCP surfaces, Gemini finish-reason/usage detail surfaces, Responses item/stream taxonomy, and wire-visible lossy drops.
- Files touched are tests/evidence/notepad only; no production source or dependencies changed.

## Clippy/fmt cleanup follow-up

- `cargo fmt --all` made the workspace formatting-clean before the lint pass; final `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` all exited successfully.
- The gw-codec cleanup stayed behavior-preserving: `single_match` → `if let` for Anthropic citations delta, `question_mark` in Anthropic output-config render, redundant Gemini thought guard simplified into the pattern, and needless explicit lifetimes removed in helper fns / impls.
- `openai/responses/wire_response.rs` helper `cow_is_empty` can take `&str`; serde's `skip_serializing_if` still works for `Cow<str>` via deref coercion, removing the `ptr_arg` lint without changing serialization behavior.
- `WireAnyRequest::OpenAi` is boxed now to satisfy `large_enum_variant`; only the constructor call in `crates/gw-codec/tests/wire_any.rs` needed adjustment, and protocol dispatch remains unchanged because `kind()` only matches on the variant tag.
