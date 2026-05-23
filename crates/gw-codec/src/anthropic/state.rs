//! `AnthropicStreamState` — per-stream accumulator for Anthropic Messages SSE.
//!
//! Anthropic's streaming format is event-typed and block-keyed:
//!
//! 1. `content_block_start` opens a builder of the appropriate kind (text,
//!    thinking, tool_use) keyed by the upstream's `index` (`content_block_index`).
//! 2. `content_block_delta` carries one of `text_delta` / `thinking_delta` /
//!    `signature_delta` / `input_json_delta`, which the codec routes to the
//!    matching builder.
//! 3. `content_block_stop` flushes the builder into the appropriate IR event
//!    (`Delta` / `ThinkingDelta` / `ToolCallDelta`).
//! 4. `message_delta` carries the final `stop_reason` plus the cumulative
//!    `usage`; the codec emits `Usage` followed by `End`.
//! 5. `message_stop` is a no-op terminator.
//!
//! Tool-use blocks track an *additional* per-stream IR index because
//! Anthropic's `content_block_index` is a position-in-content index that
//! interleaves with text/thinking blocks; the IR contract emits sequential
//! integer indices on `ToolCallStart` so the downstream renderer can replay
//! them.

use std::any::Any;
use std::collections::HashMap;

use gw_core::ir::StreamState;

#[derive(Default)]
pub struct AnthropicStreamState {
    /// `content_block_index` → open builder.
    pub(crate) open_blocks: HashMap<u32, OpenBlock>,
    /// Captured from `message_start` for parity with the response id field.
    pub(crate) seen_message_id: Option<String>,
    /// Monotonic counter producing per-stream `ToolCallStart.index`. Anthropic
    /// does not number tool_use blocks separately on the wire; the IR
    /// contract requires a stable numeric index downstream.
    pub(crate) next_tool_call_index: u32,
}

pub(crate) enum OpenBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    ToolUse {
        /// Accumulated `partial_json` fragments. Held for the lifetime of the
        /// block so debug tooling can see the full reconstructed arguments
        /// JSON after `content_block_stop`; the per-fragment IR events have
        /// already been emitted by then.
        #[allow(dead_code)]
        input_json: String,
        /// IR-side numeric index emitted on `ToolCallStart` and reused on
        /// `ToolCallDelta`. Anthropic's `content_block_index` is a position
        /// index that may interleave with text/thinking blocks; the IR
        /// requires a stable per-tool-call numeric handle.
        ir_index: u32,
    },
}

impl StreamState for AnthropicStreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
