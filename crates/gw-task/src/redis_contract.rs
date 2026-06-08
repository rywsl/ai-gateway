use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use fred::clients::RedisPool;
use fred::error::RedisError;
use fred::interfaces::{
    HashesInterface, KeysInterface, LuaInterface, SetsInterface, SortedSetsInterface,
    StreamsInterface,
};
use fred::types::{MultipleZaddValues, RedisMap, RedisValue, XReadResponse};

use crate::contract::{CancelReason, ClaimedTask, SettleOutcome, TaskContract, TaskError};
use crate::scripts::{RECOVER_LEASE_LUA, SETTLE_LUA, SUBMIT_LUA, TASK_FREEZE_LUA};
use crate::types::{TaskId, TaskKind, TaskRecord, TaskStatus};

const DEFAULT_TASK_TTL_SECONDS: u64 = 86_400;
const DEFAULT_LEASE_SECONDS: u64 = 60;
const TASK_GROUP: &str = "tasks-workers";

#[derive(Clone, Debug)]
struct ScriptShas {
    task_freeze: String,
    submit: String,
    settle: String,
    recover_lease: String,
}

#[derive(Clone, Debug)]
pub struct RedisTaskContract {
    pool: RedisPool,
    prefix: String,
    scripts: ScriptShas,
    lease_seconds: u64,
    ttl_seconds: u64,
}

impl RedisTaskContract {
    pub async fn new(pool: RedisPool) -> Result<Self, TaskError> {
        Self::new_with_prefix(pool, "").await
    }

    pub async fn new_with_prefix(
        pool: RedisPool,
        prefix: impl Into<String>,
    ) -> Result<Self, TaskError> {
        let client = pool.next();
        let submit = client
            .script_load(SUBMIT_LUA)
            .await
            .map_storage("load submit lua")?;
        let task_freeze = client
            .script_load(TASK_FREEZE_LUA)
            .await
            .map_storage("load task freeze lua")?;
        let settle = client
            .script_load(SETTLE_LUA)
            .await
            .map_storage("load settle lua")?;
        let recover_lease = client
            .script_load(RECOVER_LEASE_LUA)
            .await
            .map_storage("load recover lease lua")?;
        Ok(Self {
            pool,
            prefix: prefix.into(),
            scripts: ScriptShas {
                task_freeze,
                submit,
                settle,
                recover_lease,
            },
            lease_seconds: DEFAULT_LEASE_SECONDS,
            ttl_seconds: DEFAULT_TASK_TTL_SECONDS,
        })
    }

    pub fn from_loaded_shas(
        pool: RedisPool,
        prefix: impl Into<String>,
        task_freeze_sha: impl Into<String>,
        submit_sha: impl Into<String>,
        settle_sha: impl Into<String>,
        recover_lease_sha: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            prefix: prefix.into(),
            scripts: ScriptShas {
                task_freeze: task_freeze_sha.into(),
                submit: submit_sha.into(),
                settle: settle_sha.into(),
                recover_lease: recover_lease_sha.into(),
            },
            lease_seconds: DEFAULT_LEASE_SECONDS,
            ttl_seconds: DEFAULT_TASK_TTL_SECONDS,
        }
    }

    fn key(&self, raw: impl AsRef<str>) -> String {
        let raw = raw.as_ref();
        if self.prefix.is_empty() {
            raw.to_string()
        } else {
            format!("{}{}", self.prefix, raw)
        }
    }

    fn task_key(&self, id: &TaskId) -> String {
        self.key(format!("entity:task:{id}"))
    }

    fn queue_key(&self) -> String {
        self.key("tasks:queue")
    }

    fn cancel_key(&self) -> String {
        self.key("tasks:cancel")
    }

    fn leases_key(&self) -> String {
        self.key("tasks:leases")
    }

    fn usage_records_key(&self) -> String {
        self.key("usage:records")
    }

    fn task_frozen_counter_key(&self, user_id: &str) -> String {
        self.key(format!("counter:user:{user_id}:task_frozen_micro"))
    }

    fn deducted_counter_key(&self, user_id: &str) -> String {
        self.key(format!("counter:user:{user_id}:deducted_micro"))
    }

    fn task_frozen_set_key(&self, user_id: &str) -> String {
        self.key(format!("counter:user:{user_id}:task_frozen_tasks"))
    }

    async fn evalsha_or_eval<T>(
        &self,
        sha: &str,
        script: &str,
        keys: Vec<String>,
        args: Vec<String>,
        ctx: &'static str,
    ) -> Result<T, TaskError>
    where
        T: fred::types::FromRedis,
    {
        let client = self.pool.next();
        let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        match client
            .evalsha(sha, key_refs.clone(), arg_refs.clone())
            .await
        {
            Ok(value) => Ok(value),
            Err(err) if is_no_script(&err) => client
                .eval(script, key_refs, arg_refs)
                .await
                .map_storage(ctx),
            Err(err) => Err(TaskError::Storage(format!("{ctx}: {err}"))),
        }
    }

    async fn save_record_fields(&self, record: &TaskRecord) -> Result<(), TaskError> {
        let map: RedisMap = record_to_fields(record)?.into_iter().collect();
        self.pool
            .next()
            .hset(self.task_key(&record.id), map)
            .await
            .map_storage("hset task")
    }

    async fn add_lease(&self, id: &TaskId, lease_until_ms: u64) -> Result<(), TaskError> {
        self.pool
            .next()
            .zadd(
                self.leases_key(),
                None,
                None,
                false,
                false,
                MultipleZaddValues::try_from((lease_until_ms as f64, id.to_string()))
                    .map_err(|err| TaskError::Storage(format!("build zadd lease: {err}")))?,
            )
            .await
            .map_storage("zadd task lease")
    }
}

impl TaskContract for RedisTaskContract {
    async fn freeze_task(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
        available_balance_micro: Option<i64>,
    ) -> Result<(), TaskError> {
        if amount < 0 {
            return Err(TaskError::InvalidPayload(
                "task freeze amount must be non-negative".to_string(),
            ));
        }
        if amount == 0 {
            return Ok(());
        }
        let keys = vec![
            self.task_frozen_counter_key(user_id),
            self.task_frozen_set_key(user_id),
        ];
        let args = vec![
            task_id.to_string(),
            amount.to_string(),
            available_balance_micro
                .map(|balance| balance.to_string())
                .unwrap_or_default(),
        ];
        let result: String = self
            .evalsha_or_eval(
                &self.scripts.task_freeze,
                TASK_FREEZE_LUA,
                keys,
                args,
                "eval task freeze lua",
            )
            .await?;
        match result.as_str() {
            "ok" => Ok(()),
            "insufficient" => Err(TaskError::QuotaExceeded),
            other => Err(TaskError::Storage(format!(
                "task freeze lua returned {other}"
            ))),
        }
    }

    async fn release_task_freeze(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
    ) -> Result<(), TaskError> {
        if amount <= 0 {
            return Ok(());
        }
        let client = self.pool.next();
        let counter_key = self.task_frozen_counter_key(user_id);
        let set_key = self.task_frozen_set_key(user_id);
        let _: i64 = client
            .incr_by(counter_key, -amount)
            .await
            .map_storage("release task freeze counter")?;
        let _: i64 = client
            .srem(set_key, task_id.to_string())
            .await
            .map_storage("release task freeze marker")?;
        Ok(())
    }

    async fn submit(&self, record: TaskRecord) -> Result<TaskId, TaskError> {
        let task_id = record.id.to_string();
        let lease_until_ms = record
            .lease_until_ms
            .unwrap_or_else(|| now_ms() + self.lease_seconds * 1_000);
        let mut args = vec![
            task_id.clone(),
            lease_until_ms.to_string(),
            lease_seconds_from(record.lease_until_ms).to_string(),
            kind_wire(record.kind)?,
            record.vendor_id.clone(),
            ttl_from_record(&record, self.ttl_seconds).to_string(),
        ];
        for (field, value) in record_to_fields(&record)? {
            args.push(field);
            args.push(value);
        }
        let keys = vec![
            self.task_key(&record.id),
            self.queue_key(),
            self.leases_key(),
        ];
        let result: String = self
            .evalsha_or_eval(
                &self.scripts.submit,
                SUBMIT_LUA,
                keys,
                args,
                "eval submit lua",
            )
            .await?;
        if result != "ok" {
            return Err(TaskError::Storage(format!("submit lua returned {result}")));
        }
        Ok(record.id)
    }

    async fn load(&self, id: &TaskId) -> Result<Option<TaskRecord>, TaskError> {
        let hash: HashMap<String, String> = self
            .pool
            .next()
            .hgetall(self.task_key(id))
            .await
            .map_storage("hgetall task")?;
        if hash.is_empty() {
            Ok(None)
        } else {
            fields_to_record(hash).map(Some)
        }
    }

    async fn watch(&self, id: &TaskId, wait_ms: u64) -> Result<Option<TaskRecord>, TaskError> {
        let deadline = now_ms().saturating_add(wait_ms);
        loop {
            if let Some(record) = self.load(id).await? {
                if record.status.is_terminal() {
                    return Ok(Some(record));
                }
            }
            if now_ms() >= deadline {
                return self.load(id).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn request_cancel(
        &self,
        id: &TaskId,
        reason: CancelReason,
    ) -> Result<TaskRecord, TaskError> {
        let mut record = self.load(id).await?.ok_or(TaskError::NotFound(*id))?;
        if record.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal(record.status));
        }
        let next = if record.status.can_transition_to(TaskStatus::Cancelling) {
            TaskStatus::Cancelling
        } else {
            TaskStatus::Cancelled
        };
        record.status = next;
        record.updated_at_ms = now_ms();
        if next.is_terminal() {
            record.ended_ts_ms = Some(record.updated_at_ms);
        }
        self.save_record_fields(&record).await?;
        let reason = serde_json::to_string(&reason)
            .map_err(|err| TaskError::InvalidPayload(format!("serialize cancel reason: {err}")))?;
        let _: String = self
            .pool
            .next()
            .xadd(
                self.cancel_key(),
                false,
                None::<()>,
                "*",
                vec![
                    ("task_id".to_string(), id.to_string()),
                    ("reason".to_string(), reason.trim_matches('"').to_string()),
                ],
            )
            .await
            .map_storage("xadd task cancel")?;
        Ok(record)
    }

    async fn claim(&self, consumer: &str) -> Result<Option<ClaimedTask>, TaskError> {
        let queue = self.queue_key();
        let client = self.pool.next();
        let _: Result<(), _> = client
            .xgroup_create(queue.clone(), TASK_GROUP, "0", true)
            .await;
        let raw: RedisValue = client
            .xreadgroup(
                TASK_GROUP,
                consumer,
                Some(1),
                Some(1),
                false,
                vec![queue.as_str()],
                vec![">"],
            )
            .await
            .map_storage("xreadgroup task queue")?;
        if raw.is_null() || matches!(&raw, RedisValue::Array(values) if values.is_empty()) {
            return Ok(None);
        }
        let response: XReadResponse<String, String, String, String> = raw
            .into_xread_response()
            .map_err(|err| TaskError::Storage(format!("decode xreadgroup task queue: {err}")))?;
        let Some((_, entries)) = response.into_iter().next() else {
            return Ok(None);
        };
        let Some((stream_id, fields)) = entries.into_iter().next() else {
            return Ok(None);
        };
        let task_id = fields
            .get("task_id")
            .ok_or_else(|| {
                TaskError::InvalidPayload("tasks:queue entry missing task_id".to_string())
            })?
            .parse::<TaskId>()
            .map_err(|err| TaskError::InvalidPayload(format!("invalid task_id in queue: {err}")))?;
        let mut record = self
            .load(&task_id)
            .await?
            .ok_or(TaskError::NotFound(task_id))?;
        if record.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal(record.status));
        }
        if !record.status.can_transition_to(TaskStatus::Running)
            && record.status != TaskStatus::Running
        {
            return Err(TaskError::InvalidTransition {
                from: record.status,
                to: TaskStatus::Running,
            });
        }
        record.status = TaskStatus::Running;
        record.worker_id = Some(consumer.to_string());
        record.claimed_ts_ms = Some(now_ms());
        record.updated_at_ms = record.claimed_ts_ms.unwrap_or(record.updated_at_ms);
        let lease_seconds = fields
            .get("lease_seconds")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(self.lease_seconds);
        let lease_until_ms = now_ms().saturating_add(lease_seconds.saturating_mul(1_000));
        record.lease_until_ms = Some(lease_until_ms);
        self.save_record_fields(&record).await?;
        self.add_lease(&record.id, lease_until_ms).await?;
        Ok(Some(ClaimedTask {
            record,
            stream_id,
            lease_until_ms,
        }))
    }

    async fn update_running(
        &self,
        id: &TaskId,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) -> Result<TaskRecord, TaskError> {
        let mut record = self.load(id).await?.ok_or(TaskError::NotFound(*id))?;
        if record.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal(record.status));
        }
        if record.status != TaskStatus::Running && record.status != TaskStatus::Cancelling {
            return Err(TaskError::InvalidTransition {
                from: record.status,
                to: TaskStatus::Running,
            });
        }
        record.lease_until_ms = Some(lease_until_ms);
        record.provider_task_id = provider_task_id;
        record.updated_at_ms = now_ms();
        self.save_record_fields(&record).await?;
        self.add_lease(id, lease_until_ms).await?;
        Ok(record)
    }

    async fn settle(
        &self,
        id: &TaskId,
        terminal_status: TaskStatus,
        output_payload: Option<serde_json::Value>,
        error_message: Option<String>,
        actual_micro: i64,
    ) -> Result<SettleOutcome, TaskError> {
        if !terminal_status.is_terminal() {
            return Err(TaskError::InvalidTransition {
                from: terminal_status,
                to: terminal_status,
            });
        }
        let mut record = self.load(id).await?.ok_or(TaskError::NotFound(*id))?;
        if record.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal(record.status));
        }
        if !record.status.can_transition_to(terminal_status) {
            return Err(TaskError::InvalidTransition {
                from: record.status,
                to: terminal_status,
            });
        }
        record.status = terminal_status;
        record.output_payload = output_payload;
        record.error_message = error_message;
        record.actual_micro = Some(actual_micro);
        record.billing_record_id = Some(format!("task:{}", record.id));
        record.ended_ts_ms = Some(now_ms());
        record.updated_at_ms = record.ended_ts_ms.unwrap_or(record.updated_at_ms);
        let usage_payload = task_usage_payload(&record, terminal_status, actual_micro)?;
        let mut args = vec![
            id.to_string(),
            status_wire(terminal_status)?,
            record.updated_at_ms.to_string(),
            record.frozen_micro.to_string(),
            actual_micro.to_string(),
            usage_payload,
        ];
        for (field, value) in record_to_fields(&record)? {
            args.push(field);
            args.push(value);
        }
        let keys = vec![
            self.task_key(id),
            self.leases_key(),
            self.task_frozen_counter_key(&record.user_id),
            self.deducted_counter_key(&record.user_id),
            self.usage_records_key(),
            self.task_frozen_set_key(&record.user_id),
        ];
        let result: String = self
            .evalsha_or_eval(
                &self.scripts.settle,
                SETTLE_LUA,
                keys,
                args,
                "eval settle lua",
            )
            .await?;
        if result != "ok" {
            return Err(TaskError::Storage(format!("settle lua returned {result}")));
        }
        Ok(SettleOutcome {
            released_micro: record.frozen_micro,
            deducted_micro: actual_micro,
            billing_record_id: record.billing_record_id.clone(),
            record,
        })
    }

    async fn recover_lease(&self, id: &TaskId, now_ms: u64) -> Result<TaskRecord, TaskError> {
        let keys = vec![self.leases_key(), self.queue_key()];
        let args = vec![
            now_ms.to_string(),
            "100".to_string(),
            self.lease_seconds.to_string(),
        ];
        let _: Vec<String> = self
            .evalsha_or_eval(
                &self.scripts.recover_lease,
                RECOVER_LEASE_LUA,
                keys,
                args,
                "eval recover lease lua",
            )
            .await?;
        self.load(id).await?.ok_or(TaskError::NotFound(*id))
    }
}

trait RedisTaskResultExt<T> {
    fn map_storage(self, context: &'static str) -> Result<T, TaskError>;
}

impl<T> RedisTaskResultExt<T> for Result<T, RedisError> {
    fn map_storage(self, context: &'static str) -> Result<T, TaskError> {
        self.map_err(|err| TaskError::Storage(format!("{context}: {err}")))
    }
}

fn is_no_script(err: &RedisError) -> bool {
    err.to_string().contains("NOSCRIPT")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn ttl_from_record(record: &TaskRecord, default: u64) -> u64 {
    let now = now_ms();
    if record.expires_at_ms > now {
        ((record.expires_at_ms - now) / 1_000).max(1)
    } else {
        default
    }
}

fn lease_seconds_from(lease_until_ms: Option<u64>) -> u64 {
    lease_until_ms
        .and_then(|until| until.checked_sub(now_ms()))
        .map(|ms| (ms / 1_000).max(1))
        .unwrap_or(DEFAULT_LEASE_SECONDS)
}

fn status_wire(status: TaskStatus) -> Result<String, TaskError> {
    json_string(status)
}

fn kind_wire(kind: TaskKind) -> Result<String, TaskError> {
    json_string(kind)
}

fn json_string<T: serde::Serialize>(value: T) -> Result<String, TaskError> {
    match serde_json::to_value(value).map_err(|err| TaskError::InvalidPayload(err.to_string()))? {
        serde_json::Value::String(value) => Ok(value),
        other => Err(TaskError::InvalidPayload(format!(
            "expected string json value, got {other}"
        ))),
    }
}

fn record_to_fields(record: &TaskRecord) -> Result<Vec<(String, String)>, TaskError> {
    let value = serde_json::to_value(record)
        .map_err(|err| TaskError::InvalidPayload(format!("serialize task record: {err}")))?;
    let object = value.as_object().ok_or_else(|| {
        TaskError::InvalidPayload("task record did not serialize to object".to_string())
    })?;
    Ok(object
        .iter()
        .map(|(key, value)| {
            let encoded = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Null => "null".to_string(),
                other => other.to_string(),
            };
            (key.clone(), encoded)
        })
        .collect())
}

fn task_usage_payload(
    record: &TaskRecord,
    terminal_status: TaskStatus,
    actual_micro: i64,
) -> Result<String, TaskError> {
    let payload = serde_json::json!({
        "schema": "task_usage_v1",
        "ts_ms": record.updated_at_ms,
        "task_id": record.id.to_string(),
        "request_id": record.request_id.clone().unwrap_or_default(),
        "trace_id": record.trace_id.clone().unwrap_or_default(),
        "user_id": record.user_id.clone(),
        "child_id": record.child_id.clone(),
        "api_key_id": record.api_key_id.clone(),
        "client_protocol": record.client_protocol.clone(),
        "model": record.model.clone(),
        "vendor_id": record.vendor_id.clone(),
        "vendor_key_id": record.vendor_key_hint.clone(),
        "status": status_wire(terminal_status)?,
        "task_kind": kind_wire(record.kind)?,
        "provider_task_id": record.provider_task_id.clone(),
        "frozen_micro": record.frozen_micro,
        "released_micro": record.frozen_micro,
        "actual_micro": actual_micro,
        "billing_record_id": record.billing_record_id.clone(),
        "duration_seconds": record.duration_seconds,
        "source": "gw-task-worker",
    });
    serde_json::to_string(&payload)
        .map_err(|err| TaskError::InvalidPayload(format!("serialize task usage: {err}")))
}

fn fields_to_record(fields: HashMap<String, String>) -> Result<TaskRecord, TaskError> {
    let mut object = serde_json::Map::new();
    for field in REQUIRED_FIELDS {
        let value = fields
            .get(*field)
            .ok_or_else(|| TaskError::InvalidPayload(format!("task hash missing {field}")))?;
        object.insert((*field).to_string(), decode_field(field, value)?);
    }
    serde_json::from_value(serde_json::Value::Object(object))
        .map_err(|err| TaskError::InvalidPayload(format!("deserialize task record: {err}")))
}

fn decode_field(field: &str, value: &str) -> Result<serde_json::Value, TaskError> {
    if matches!(
        field,
        "id" | "kind" | "status" | "user_id" | "api_key_id" | "client_protocol" | "vendor_id"
    ) {
        return Ok(serde_json::Value::String(value.to_string()));
    }
    if matches!(
        field,
        "child_id"
            | "vendor_key_hint"
            | "model"
            | "worker_id"
            | "trace_id"
            | "request_id"
            | "provider_task_id"
            | "error_message"
            | "billing_record_id"
    ) {
        return Ok(optional_string(value));
    }
    serde_json::from_str(value)
        .map_err(|err| TaskError::InvalidPayload(format!("decode field {field}: {err}")))
}

fn optional_string(value: &str) -> serde_json::Value {
    if value == "null" {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(value.to_string())
    }
}

const REQUIRED_FIELDS: &[&str] = &[
    "id",
    "schema_version",
    "kind",
    "status",
    "user_id",
    "child_id",
    "api_key_id",
    "client_protocol",
    "vendor_id",
    "vendor_key_hint",
    "model",
    "created_at_ms",
    "updated_at_ms",
    "submitted_ts_ms",
    "expires_at_ms",
    "claimed_ts_ms",
    "ended_ts_ms",
    "lease_until_ms",
    "worker_id",
    "trace_id",
    "request_id",
    "provider_task_id",
    "provider_first_response_ms",
    "input_payload",
    "output_payload",
    "error_message",
    "frozen_micro",
    "actual_micro",
    "duration_seconds",
    "billing_record_id",
];
