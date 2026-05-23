//! `OpenAiStreamState` — per-stream accumulator owned by the gateway's
//! stream framer and downcast inside `parse_stream_chunk`.
//!
//! Tracks open `tool_calls` keyed by OpenAI's per-chunk `index`. Each
//! incoming `data:` chunk for a given index appends to the builder's
//! `arguments` buffer; the codec emits a `ToolCallStart` exactly once
//! (first time `id` + `function.name` are seen) and a `ToolCallDelta`
//! per fragment.

use std::any::Any;
use std::collections::HashMap;

use gw_core::ir::StreamState;

#[derive(Default)]
pub struct OpenAiStreamState {
    /// Open tool_calls keyed by the per-chunk `tool_calls[].index` value.
    pub(crate) pending_tool_calls: HashMap<u32, OpenAiToolCallBuilder>,
}

#[derive(Default)]
pub(crate) struct OpenAiToolCallBuilder {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub started_emitted: bool,
}

impl StreamState for OpenAiStreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
