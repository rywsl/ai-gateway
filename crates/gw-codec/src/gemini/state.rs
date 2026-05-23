//! `GeminiStreamState` — per-stream accumulator for Gemini SSE.
//!
//! Gemini's stream emits one `data: <json>\r\n\r\n` frame per chunk, each
//! carrying a partial `GeminiResponse` (with `candidates[]`, optional
//! `usageMetadata`, optional `finishReason`). Function-call parts may arrive
//! in one shot or, rarely, dribble across multiple chunks; the IR contract
//! requires a stable `index` on `ToolCallStart` so the codec assigns one
//! sequentially and tracks the builder by that index.
//!
//! Gemini parts do not carry an upstream id (unlike Anthropic's `tool_use.id`
//! or OpenAI's `tool_calls[].id`); the IR-emitted `id` field is therefore
//! the empty string for Gemini-origin tool calls.

use std::any::Any;

use gw_core::ir::StreamState;

#[derive(Default)]
pub struct GeminiStreamState {
    /// Function-call builders in sequential arrival order. The slot's
    /// position is the `ToolCallStart.index` reported to the IR.
    pub(crate) pending_function_calls: Vec<GeminiFunctionCallBuilder>,
    /// Set after the first frame is observed; reserved for future
    /// session-id tracking. Currently informational.
    #[allow(dead_code)]
    pub(crate) seen_response: bool,
}

#[derive(Default)]
pub(crate) struct GeminiFunctionCallBuilder {
    pub name: String,
    /// Accumulated arguments JSON. Held for the lifetime of the stream so
    /// debug tooling can read the full reconstructed payload after the
    /// stream completes; per-fragment `ToolCallDelta` events are emitted
    /// inline as fragments arrive.
    #[allow(dead_code)]
    pub args_json: String,
    /// One-shot guard so `ToolCallStart` is emitted exactly once per builder.
    pub emitted_start: bool,
    /// IR-side `ToolCallStart.index`. Stable across deltas.
    pub ir_index: u32,
}

impl StreamState for GeminiStreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
