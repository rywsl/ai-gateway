use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct TaskId(Uuid);

impl TaskId {
    #[must_use]
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TaskId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

impl From<Uuid> for TaskId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<TaskId> for Uuid {
    fn from(value: TaskId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Cancelling,
}

impl TaskStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Queued)
                | (Self::Queued, Self::Running)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
                | (Self::Running, Self::Cancelling)
                | (Self::Cancelling, Self::Cancelled)
                | (Self::Cancelling, Self::Succeeded)
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Image,
    Video,
    Audio,
    Batch,
    LroResponses,
    Generic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub schema_version: u64,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub user_id: String,
    pub child_id: Option<String>,
    pub api_key_id: String,
    pub client_protocol: String,
    pub vendor_id: String,
    pub vendor_key_hint: Option<String>,
    pub model: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub submitted_ts_ms: u64,
    pub expires_at_ms: u64,
    pub claimed_ts_ms: Option<u64>,
    pub ended_ts_ms: Option<u64>,
    pub lease_until_ms: Option<u64>,
    pub worker_id: Option<String>,
    pub trace_id: Option<String>,
    pub request_id: Option<String>,
    pub provider_task_id: Option<String>,
    pub provider_first_response_ms: Option<u64>,
    pub input_payload: serde_json::Value,
    pub output_payload: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub frozen_micro: i64,
    pub actual_micro: Option<i64>,
    pub duration_seconds: Option<u64>,
    pub billing_record_id: Option<String>,
}

pub const TASK_IR_REQUEST: &str = "ir_request";
pub const TASK_IR_RESPONSE: &str = "ir_response";
pub const TASK_IR_WIRE_BODY: &str = "ir_wire_body";
pub const TASK_IR_WIRE_FORMAT: &str = "ir_wire_format";
pub const TASK_IR_WIRE_CONTENT_TYPE: &str = "ir_wire_content_type";
pub const TASK_BODY: &str = "body";

#[must_use]
pub fn task_ir_wire_body(payload: &serde_json::Value) -> Option<&str> {
    payload.get(TASK_IR_WIRE_BODY).and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_wire_format(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get(TASK_IR_WIRE_FORMAT)
        .and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_wire_content_type(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get(TASK_IR_WIRE_CONTENT_TYPE)
        .and_then(serde_json::Value::as_str)
}

#[must_use]
pub fn task_ir_request(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload.get(TASK_IR_REQUEST)
}

#[must_use]
pub fn task_ir_response(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload.get(TASK_IR_RESPONSE)
}
