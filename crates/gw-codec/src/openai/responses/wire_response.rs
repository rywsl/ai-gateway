//! Wire form of OpenAI Responses API non-stream response.
//!
//! Mirrors the shape documented at
//! <https://platform.openai.com/docs/api-reference/responses/object>:
//!
//! - top-level: `{ id, object: "response", created_at, model, status, output, usage }`
//! - `output` is an array of typed items: `message` / `reasoning` / `function_call`
//! - `usage.input_tokens_details.cached_tokens` and
//!   `usage.output_tokens_details.reasoning_tokens` carry the v2 IR
//!   `UsageIR.cached_tokens` / `UsageIR.reasoning_tokens` fields.
//!
//! All string fields use `Cow<'a, str>` so `parse_response` borrows from the
//! upstream body slice (matching the T4-A OpenAI Chat idiom). `render_response`
//! constructs `Cow::Owned` values by `clone()` from the IR.
//!
//! `incomplete_details` / `usage` / `created_at` default to absent / 0 so that
//! pre-existing tests (e.g. `tests/openai_responses_wire.rs::deserialize_response_object`)
//! exercising the minimal `{id, object, model, output}` payload continue to
//! deserialise cleanly.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::openai::wire::{OpenAiCompletionTokensDetails, OpenAiPromptTokensDetails};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiResponsesResponse<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub object: Cow<'a, str>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(borrow)]
    pub model: Cow<'a, str>,
    #[serde(borrow, default = "default_status")]
    pub status: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<IncompleteDetails<'a>>,
    #[serde(borrow)]
    pub output: Vec<ResponseOutputItem<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
}

fn default_status<'a>() -> Cow<'a, str> {
    Cow::Borrowed("completed")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IncompleteDetails<'a> {
    /// One of: `"max_output_tokens"`, `"content_filter"`, …
    #[serde(borrow)]
    pub reason: Cow<'a, str>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputItem<'a> {
    Message {
        #[serde(default, borrow)]
        id: Cow<'a, str>,
        #[serde(borrow)]
        content: Vec<MessageContent<'a>>,
        #[serde(borrow)]
        role: Cow<'a, str>,
        #[serde(default = "default_status", borrow)]
        status: Cow<'a, str>,
    },
    Reasoning {
        #[serde(default, borrow)]
        id: Cow<'a, str>,
        #[serde(default, borrow)]
        summary: Vec<ReasoningSummaryPart<'a>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<Cow<'a, str>>,
    },
    FunctionCall {
        #[serde(default, borrow)]
        id: Cow<'a, str>,
        #[serde(borrow)]
        name: Cow<'a, str>,
        #[serde(borrow)]
        arguments: Cow<'a, str>,
        #[serde(default, borrow)]
        call_id: Cow<'a, str>,
        #[serde(default = "default_status", borrow)]
        status: Cow<'a, str>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent<'a> {
    OutputText {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    // Refusal / OutputImage shapes exist in the API surface but are not
    // round-tripped through the v2 IR text channel; add when needed.
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummaryPart<'a> {
    SummaryText {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
}

/// OpenAI Responses uses `input_tokens` / `output_tokens` (different from
/// Chat's `prompt_tokens` / `completion_tokens`). Chat-style aliases are
/// accepted on parse to be tolerant of clients that mirror Chat shape, but
/// always written in the Responses canonical form.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct ResponsesUsage {
    #[serde(alias = "prompt_tokens", default)]
    pub input_tokens: u32,
    #[serde(alias = "completion_tokens", default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<OpenAiPromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<OpenAiCompletionTokensDetails>,
}
