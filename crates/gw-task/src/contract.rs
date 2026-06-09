use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{TaskId, TaskRecord, TaskStatus};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    Client,
    Timeout,
    Admin,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClaimedTask {
    pub record: TaskRecord,
    pub stream_id: String,
    pub lease_until_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettleOutcome {
    pub record: TaskRecord,
    pub released_micro: i64,
    pub deducted_micro: i64,
    pub billing_record_id: Option<String>,
}

#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskError {
    #[error("task not found: {0}")]
    NotFound(TaskId),
    #[error("task is already terminal: {0:?}")]
    AlreadyTerminal(TaskStatus),
    #[error("invalid task status transition from {from:?} to {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },
    #[error("task provider unavailable")]
    ProviderUnavailable,
    #[error("task quota exceeded")]
    QuotaExceeded,
    #[error("task contract conflict: {0}")]
    Conflict(String),
    #[error("task contract storage error: {0}")]
    Storage(String),
    #[error("invalid task payload: {0}")]
    InvalidPayload(String),
}

#[allow(async_fn_in_trait)]
pub trait TaskContract {
    async fn freeze_task(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
        available_balance_micro: Option<i64>,
    ) -> Result<(), TaskError>;

    async fn release_task_freeze(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
    ) -> Result<(), TaskError>;

    /// Submit a full task record. `TaskRecord.input_payload.ir_wire_body` is
    /// optional for backward compatibility; workers must fall back to legacy
    /// `input_payload.body` when it is absent.
    async fn submit(&self, record: TaskRecord) -> Result<TaskId, TaskError>;

    async fn load(&self, id: &TaskId) -> Result<Option<TaskRecord>, TaskError>;

    async fn watch(&self, id: &TaskId, wait_ms: u64) -> Result<Option<TaskRecord>, TaskError>;

    async fn request_cancel(
        &self,
        id: &TaskId,
        reason: CancelReason,
    ) -> Result<TaskRecord, TaskError>;

    async fn claim(&self, consumer: &str) -> Result<Option<ClaimedTask>, TaskError>;

    async fn update_running(
        &self,
        id: &TaskId,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) -> Result<TaskRecord, TaskError>;

    async fn settle(
        &self,
        id: &TaskId,
        terminal_status: TaskStatus,
        output_payload: Option<serde_json::Value>,
        error_message: Option<String>,
        actual_micro: i64,
    ) -> Result<SettleOutcome, TaskError>;

    async fn recover_lease(&self, id: &TaskId, now_ms: u64) -> Result<TaskRecord, TaskError>;
}
