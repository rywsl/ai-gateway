//! Wire form of OpenAI Responses API request.
//!
//! v1 keeps `input` as `Vec<Value>` so all ContentPart shapes round-trip
//! without crate-level enumeration; cross-shape conversion (T5-F) walks the
//! Values. Other shape-specific fields can be added later without breaking
//! callers (they live in `extras` until promoted).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: Value,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(flatten)]
    pub extras: serde_json::Map<String, Value>,
}
