//! Wire form of OpenAI Responses API response.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesResponse {
    pub id: String,
    pub object: String,
    pub model: String,
    pub output: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(flatten)]
    pub extras: serde_json::Map<String, Value>,
}
