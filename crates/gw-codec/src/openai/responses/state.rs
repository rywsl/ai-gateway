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

use crate::overflow::{
    CodecOverflowError, DEFAULT_MAX_ACCUMULATED_BYTES, DEFAULT_MAX_PENDING_FUNCTION_CALLS,
};

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
    /// T-W3-08: hard cap on `pending_function_calls.len()`. A new builder
    /// past this limit is rejected and
    /// `gw_codec_overflow_total{kind="too_many_function_calls"}` is
    /// incremented. Default: [`DEFAULT_MAX_PENDING_FUNCTION_CALLS`].
    pub(crate) max_pending_function_calls: usize,
    /// T-W3-08: hard cap on any single function-call builder's
    /// `arguments` String length. A delta that would push past this is
    /// dropped + counted. Default: [`DEFAULT_MAX_ACCUMULATED_BYTES`].
    pub(crate) max_accumulated_bytes: usize,
}

impl Default for OpenAiResponsesStreamState {
    fn default() -> Self {
        Self {
            pending_reasoning: HashMap::new(),
            pending_function_calls: HashMap::new(),
            seen_response_id: None,
            next_tool_call_index: 0,
            max_pending_function_calls: DEFAULT_MAX_PENDING_FUNCTION_CALLS,
            max_accumulated_bytes: DEFAULT_MAX_ACCUMULATED_BYTES,
        }
    }
}

impl OpenAiResponsesStreamState {
    /// T-W3-08: register or reuse the [`FunctionCallBuilder`] keyed by
    /// `item_id`. When `item_id` is new and the map is already at cap,
    /// emits `gw_codec_overflow_total{kind="too_many_function_calls"}`
    /// and returns Err; the caller skips the event so the IR stays
    /// consistent.
    pub(crate) fn try_get_or_insert_function_call(
        &mut self,
        item_id: &str,
    ) -> Result<&mut FunctionCallBuilder, CodecOverflowError> {
        if !self.pending_function_calls.contains_key(item_id)
            && self.pending_function_calls.len() >= self.max_pending_function_calls
        {
            let err = CodecOverflowError::TooManyFunctionCalls {
                cap: self.max_pending_function_calls,
            };
            err.record_metric();
            return Err(err);
        }
        Ok(self
            .pending_function_calls
            .entry(item_id.to_owned())
            .or_default())
    }

    /// T-W3-08: append `delta` to the named builder's `arguments`. Returns
    /// Err when the resulting length would exceed `max_accumulated_bytes`;
    /// existing bytes are left intact.
    pub(crate) fn try_extend_function_arguments(
        &mut self,
        item_id: &str,
        delta: &str,
    ) -> Result<(), CodecOverflowError> {
        let cap = self.max_accumulated_bytes;
        let Some(builder) = self.pending_function_calls.get_mut(item_id) else {
            return Ok(());
        };
        if builder.arguments.len().saturating_add(delta.len()) > cap {
            let err = CodecOverflowError::ArgumentsBufferCap { cap };
            err.record_metric();
            return Err(err);
        }
        builder.arguments.push_str(delta);
        Ok(())
    }
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
