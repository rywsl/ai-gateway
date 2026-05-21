//! Wire form of OpenAI Responses API SSE chunk.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesStreamChunk {
    pub r#type: String,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}
