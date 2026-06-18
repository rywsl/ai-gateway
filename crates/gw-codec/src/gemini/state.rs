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

use crate::overflow::{
    CodecOverflowError, DEFAULT_MAX_ACCUMULATED_BYTES, DEFAULT_MAX_PENDING_FUNCTION_CALLS,
};

pub struct GeminiStreamState {
    /// Function-call builders in sequential arrival order. The slot's
    /// position is the `ToolCallStart.index` reported to the IR.
    pub(crate) pending_function_calls: Vec<GeminiFunctionCallBuilder>,
    /// Set after the first frame is observed; reserved for future
    /// session-id tracking. Currently informational.
    #[allow(dead_code)]
    pub(crate) seen_response: bool,
    /// T-W3-08: hard cap on `pending_function_calls.len()`. A new call
    /// past this limit is dropped (the codec emits no ToolCall events for
    /// it) and `gw_codec_overflow_total{kind="too_many_function_calls"}`
    /// is incremented. Default: [`DEFAULT_MAX_PENDING_FUNCTION_CALLS`].
    pub(crate) max_pending_function_calls: usize,
    /// T-W3-08: hard cap on a single builder's `args_json` string length.
    /// A delta that would push past this is dropped + counted; the
    /// existing buffer stays intact so the call may still complete
    /// (typically with a truncated tail). Default:
    /// [`DEFAULT_MAX_ACCUMULATED_BYTES`].
    pub(crate) max_accumulated_bytes: usize,
}

impl Default for GeminiStreamState {
    fn default() -> Self {
        Self {
            pending_function_calls: Vec::new(),
            seen_response: false,
            max_pending_function_calls: DEFAULT_MAX_PENDING_FUNCTION_CALLS,
            max_accumulated_bytes: DEFAULT_MAX_ACCUMULATED_BYTES,
        }
    }
}

impl GeminiStreamState {
    /// T-W3-08: reserve a fresh slot in `pending_function_calls`. Returns
    /// the new index on success, or
    /// [`CodecOverflowError::TooManyFunctionCalls`] when the cap is
    /// reached. The metric is emitted on the error path so the caller can
    /// stay focused on stream-emit decisions.
    pub(crate) fn try_push_function_call(
        &mut self,
        builder: GeminiFunctionCallBuilder,
    ) -> Result<usize, CodecOverflowError> {
        if self.pending_function_calls.len() >= self.max_pending_function_calls {
            let err = CodecOverflowError::TooManyFunctionCalls {
                cap: self.max_pending_function_calls,
            };
            err.record_metric();
            return Err(err);
        }
        self.pending_function_calls.push(builder);
        Ok(self.pending_function_calls.len() - 1)
    }

    /// T-W3-08: append `frag` to the builder at `idx`. Returns Err when
    /// the resulting `args_json` would exceed `max_accumulated_bytes`;
    /// the existing buffer is left untouched on overflow.
    pub(crate) fn try_extend_args(
        &mut self,
        idx: usize,
        frag: &str,
    ) -> Result<(), CodecOverflowError> {
        let cap = self.max_accumulated_bytes;
        let Some(builder) = self.pending_function_calls.get_mut(idx) else {
            return Ok(());
        };
        if builder.args_json.len().saturating_add(frag.len()) > cap {
            let err = CodecOverflowError::ArgumentsBufferCap { cap };
            err.record_metric();
            return Err(err);
        }
        builder.args_json.push_str(frag);
        Ok(())
    }
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
