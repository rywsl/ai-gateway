//! `OpenAiResponsesStreamState` — per-stream accumulator for the OpenAI
//! Responses SSE event stream.
//!
//! Responses streaming differs from Chat in three load-bearing ways:
//!
//! 1. Items are addressable: every reasoning / function_call / message event
//!    arrives keyed by `item_id` (a stable string), not Chat's per-chunk
//!    integer `index`. We keep two maps so a stream can interleave open
//!    reasoning and open function_call items without index collisions.
//!
//! 2. Reasoning is summary-style and arrives across multiple
//!    `response.reasoning_summary_text.delta` events plus an
//!    `output_item.done` terminator. We buffer the summary fragments inside
//!    `ReasoningBuilder` and emit a single `StreamEventIR::ThinkingDelta`
//!    when the upstream marks the item done.
//!
//! 3. Function-call arguments arrive as `response.function_call_arguments.delta`
//!    events whose `delta` is a raw JSON fragment of the arguments string —
//!    same accumulator shape as Chat, just keyed by item_id.

use std::any::Any;
use std::collections::HashMap;

use gw_core::ir::StreamState;

#[derive(Default)]
pub struct OpenAiResponsesStreamState {
    pub(crate) pending_reasoning: HashMap<String, ReasoningBuilder>,
    pub(crate) pending_function_calls: HashMap<String, FunctionCallBuilder>,
    /// Captured from `response.created` so downstream re-render logic could
    /// echo it back if needed. Not used by v1 emit but recorded for symmetry
    /// with the non-stream path's `id` field.
    pub(crate) seen_response_id: Option<String>,
    /// Monotonic counter producing per-stream `StreamEventIR::ToolCallStart.index`.
    /// Responses' item ids are stable strings, but the IR contract requires a
    /// numeric index per call so the downstream Chat renderer can replay them.
    pub(crate) next_tool_call_index: u32,
}

#[derive(Default)]
pub(crate) struct ReasoningBuilder {
    pub summary: String,
    pub items_emitted: bool,
}

#[derive(Default)]
pub(crate) struct FunctionCallBuilder {
    pub index: u32,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub start_emitted: bool,
}

impl StreamState for OpenAiResponsesStreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
