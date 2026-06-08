# 08 — Async Tasks (as-built)

- Family: T (task) · Backing crates: `gw-task`, `gw-task-runtime`, `gw-task-worker` · Source-of-truth: `crates/gw-task/src/{scripts.rs,redis_contract.rs}`, `crates/gw-task-runtime/src/{exec_loop.rs,heartbeat_loop.rs,watchdog_loop.rs,release.rs}`, `crates/gw-task-worker/src/lib.rs`, `crates/gw-proxy/src/filters/task.rs` · Status: default-off (`[task].enabled = false`); submit, Redis queue, Redis task freeze, worker execution, submit→poll, settle accounting, and watchdog leader election exist.

## Purpose

The task family handles long-running-ish vendor operations (image / video / audio / batch / LRO-responses) that do not fit the synchronous streaming proxy path. A client submits a job over HTTP, the gateway freezes an estimated cost in Redis and enqueues the job onto a Redis stream, and a separate worker process (`gw-task-worker`) claims, executes, polls providers when needed, and settles it. Clients poll or watch for the terminal record.

## As-built surface

### Submit endpoint and request-path freeze

`detect_task_shape` (`crates/gw-proxy/src/filters/task.rs:26`) routes to `TaskAction::Submit` for `POST /v1/tasks`, `POST /v1/images/generations`, and `POST /v1/responses` when the JSON body has `background: true` (`body_has_background_true`, `crates/gw-proxy/src/filters/task.rs:54`); `GET /v1/tasks/{id}`, `GET /v1/tasks/{id}/watch`, and `POST /v1/tasks/{id}/cancel` map to `Load` / `Watch` / `Cancel`. The whole matcher short-circuits to `None` when `task_enabled` is false (`crates/gw-proxy/src/filters/task.rs:32`). `mark_task_context` sets `ctx.is_task` / `ctx.task_action` (`crates/gw-proxy/src/filters/task.rs:67`).

`submit_endpoint_impl` (`crates/gw-proxy/src/filters/task.rs`) parses the body, infers the kind via `infer_task_kind` (reads `task_type` or `kind`), and computes `frozen_micro` with `gw_billing::formulas::calculate_freeze_amount`. When `frozen_micro > 0` it calls `TaskContract::freeze_task(user_id, task_id, amount, available_balance_micro)`, which is Redis-backed for `RedisTaskContract`. Insufficiency yields `GW_TASK_FREEZE_FAILED` (HTTP 402) and increments `gw_task_gw_freeze_total{result="insufficient"}`. If `contract.submit` subsequently errors, the gateway calls `TaskContract::release_task_freeze` for the same task id and amount.

A successful submit builds a `TaskRecord` with `status = Queued`, `schema_version = 1`, `expires_at_ms = now + 86_400_000`, and the parsed `input_payload`, then returns HTTP 201 `{task_id, status:"queued", expires_at}` (`crates/gw-proxy/src/filters/task.rs:228`). `load_endpoint` / `watch_endpoint` / `cancel_endpoint` serialize the record back; `watch_endpoint` emits `text/event-stream` framed by `write_task_response` (`crates/gw-proxy/src/filters/task.rs:343`). Error codes `GW_TASK_NOT_FOUND`, `GW_TASK_SUBMIT_FAILED`, `GW_TASK_FREEZE_FAILED`, `GW_TASK_CANCELLED` are defined in `crates/gw-core/src/error/codes.rs:142`.

### Redis contract (Lua + fred)

`RedisTaskContract` (`crates/gw-task/src/redis_contract.rs`) loads four scripts (`TASK_FREEZE_LUA`, `SUBMIT_LUA`, `SETTLE_LUA`, `RECOVER_LEASE_LUA`, `crates/gw-task/src/scripts.rs`) via `SCRIPT LOAD` and dispatches each with `evalsha_or_eval`, falling back to `EVAL` on `NOSCRIPT`. Keys are `entity:task:{id}` (`task_key`), `tasks:queue` (`queue_key`), `tasks:cancel` (`cancel_key`), `tasks:leases` (`leases_key`), `usage:records`, `counter:user:{uid}:task_frozen_micro`, `counter:user:{uid}:deducted_micro`, and `counter:user:{uid}:task_frozen_tasks`, each optionally prefixed.

- `submit` (`crates/gw-task/src/redis_contract.rs:167`) drives `SUBMIT_LUA` (`crates/gw-task/src/scripts.rs:1`): `HSET` of all record fields on `KEYS[1]`, an `EXPIRE` (TTL from `expires_at_ms`, default 86 400 s), an `XADD` of `{task_id, task_type, vendor_id, lease_seconds}` onto the queue (`KEYS[2]`), and a `ZADD lease_until_ms task_id` onto the lease set (`KEYS[3]`).
- `claim` (`crates/gw-task/src/redis_contract.rs:273`) ensures the `tasks-workers` consumer group, then `XREADGROUP … COUNT 1 … >` for one entry; it loads the record, transitions `Queued → Running`, stamps `worker_id` / `claimed_ts_ms`, recomputes the lease, `HSET`s the record, and re-`ZADD`s the lease. Returns a `ClaimedTask { record, stream_id, lease_until_ms }`.
- `update_running` (`crates/gw-task/src/redis_contract.rs:344`) refreshes `lease_until_ms` and optional `provider_task_id` and re-`ZADD`s the lease.
- `freeze_task` drives `TASK_FREEZE_LUA`: validates the amount, checks `available_balance_micro` against the existing `counter:user:{uid}:task_frozen_micro` when supplied, `INCRBY`s the task frozen counter, and `SADD`s the task id to `counter:user:{uid}:task_frozen_tasks`.
- `settle` validates the terminal transition, sets `billing_record_id = task:{id}`, serializes a `task_usage_v1` payload, and runs `SETTLE_LUA`.
- `recover_lease` (`crates/gw-task/src/redis_contract.rs:428`) runs `RECOVER_LEASE_LUA` (limit 100) then reloads the record.

`SETTLE_LUA` takes `KEYS = [entity:task:{id}, tasks:leases, counter:user:{uid}:task_frozen_micro, counter:user:{uid}:deducted_micro, usage:records, counter:user:{uid}:task_frozen_tasks]` and `ARGV = [task_id, terminal_status, settled_at_ms, released_micro, deducted_micro, usage_payload, <field value>…]`. It `HSET`s terminal fields, removes the lease, releases the task frozen counter with `INCRBY -released_micro`, increments `deducted_micro`, removes the task id from the frozen-task set, and `XADD`s the usage payload.

`RECOVER_LEASE_LUA` (`crates/gw-task/src/scripts.rs:57`) `ZRANGEBYSCORE`s expired leases (`-inf … now_ms`, `LIMIT 0 limit`), and for each it successfully `ZREM`s it re-`XADD`s `{task_id, recovered:1, lease_seconds}` onto the queue so a worker re-claims it. Records are serialized field-by-field via `record_to_fields` / `fields_to_record` with `REQUIRED_FIELDS` (`crates/gw-task/src/redis_contract.rs:566`).

### Runtime loops (`gw-task-runtime`)

`ExecLoop` claims one task, acquires global/vendor/model/key concurrency permits, `update_running`s it, and runs `call_upstream` under a `tokio::time::timeout(total)`. `call_upstream` POSTs to `upstream_url` (from `input_payload.upstream_url` or `vendor_key_hint`) with `upstream_body` (`input_payload.body` or the whole payload). A terminal success settles as `Succeeded`; non-2xx, failed provider state, transport error, or timeout settles as `Failed`. When the provider returns a non-terminal state (`queued` / `pending` / `running` / `in_progress` / `processing`) with `status_url` / `poll_url`, the loop polls with exponential backoff (`poll_initial_backoff`, default 1 s; `poll_max_backoff`, default 30 s) until terminal or until the outer total timeout fires. `provider_task_id` is extracted from `provider_task_id`/`id` and written back via `update_running`.

`HeartbeatLoop` (`crates/gw-task-runtime/src/heartbeat_loop.rs:29`) wakes every `heartbeat_interval` (default 15 s) and, for each task in the `RunningTaskRegistry`, calls `update_running` with `now + lease_extension` (default 60 s), incrementing `TASK_HEARTBEAT_TOTAL`; `AlreadyTerminal` / `NotFound` evict the task from the registry.

`WatchdogLoop` wakes every `check_interval` (default 5 s), first tries to acquire `lock:tasks:watchdog` with `SET NX PX leader_ttl` (`leader_ttl` default 10 s), and skips the scan when another worker holds the lock. The leader calls `RedisLeaseRecoveryStore::recover_expired` to re-enqueue expired-lease tasks, then `recover_lease`s each.

### Worker process (`gw-task-worker`)

`run` (`crates/gw-task-worker/src/lib.rs:365`) loads `WorkerConfig` (from `config/task-default.toml` + `GW_TASK_*` env), connects Redis, generates a worker id, writes a registration, builds a `RedisTaskContract`, and spawns six tasks: exec, cancel, watchdog, heartbeat, worker-registration, and a hyper `/livez` `/readyz` health server (default ports 8081 health / 9091 metrics, `crates/gw-task-worker/src/lib.rs:301`). `heartbeat_worker_registration` (`crates/gw-task-worker/src/lib.rs:152`) `HSET`s `entity:worker:{id}` (`worker_id`, `start_time_ms`, `status="running"`, `hostname`, `last_heartbeat_ms`) with a 15 s `EXPIRE`; the registration loop refreshes it on its heartbeat interval so a dead worker's key TTL-expires.

### Configuration

`[task]` in `config/default.toml:52` — `enabled = false`, `queue_stream = "tasks:queue"`, `cancel_stream = "tasks:cancel"`, `billing_base_micro = 1000`, `billing_vendor_multiplier = 1.0`, `billing_estimated_duration_secs = 60`. The worker process has its own `config/task-default.toml` layer (`WorkerConfig::load`, `crates/gw-task-worker/src/lib.rs:242`) with `[server]` `[redis]` `[worker]` `[loops]` `[concurrency]` `[timeouts]` tables.

## Deliberate behaviours

- **Default-off.** `[task].enabled = false` (`config/default.toml:53`) makes `detect_task_shape` return `None` for every method/path, so the gateway never enters the task path unless explicitly enabled.
- **Submit-path freeze is fail-closed on insufficiency.** `TaskContract::freeze_task` rejects with `GW_TASK_FREEZE_FAILED` (HTTP 402) when Redis reports insufficient available balance, and increments `gw_task_gw_freeze_total{result="insufficient"}`.
- **Settle mutates accounting atomically.** `SETTLE_LUA` updates terminal task state, releases frozen task funds, increments `deducted_micro`, and emits `usage:records` in one script call.
- **Exec polls non-terminal provider jobs.** Non-terminal provider statuses enter the poll loop; unknown or absent status remains treated as terminal success for backward compatibility with synchronous providers.
- **Worker liveness is TTL-based, not lease-arbitrated.** `entity:worker:{id}` carries a 15 s `EXPIRE` refreshed by the registration loop (`crates/gw-task-worker/src/lib.rs:166`); a crashed worker simply lets the key lapse.

## Cross-process touchpoints

- **`tasks:queue`** — Redis stream, consumer group `tasks-workers` (`crates/gw-task/src/redis_contract.rs:15`). Producer: `SUBMIT_LUA` `XADD` `{task_id, task_type, vendor_id, lease_seconds}`; watchdog re-enqueues `{task_id, recovered:1, lease_seconds}`. Consumer: `gw-task-worker`'s `XREADGROUP`.
- **`tasks:cancel`** — Redis stream; `request_cancel` `XADD`s `{task_id, reason}` (`crates/gw-task/src/redis_contract.rs:258`), consumed by the worker's cancel loop.
- **`tasks:leases`** — Redis sorted set keyed by `lease_until_ms`; shared by submit/claim/update/settle/watchdog.
- **`lock:tasks:watchdog`** — Redis leader lock for the watchdog recovery scan (`SET NX PX`, default TTL 10 s).
- **`entity:task:{id}`** — Redis hash holding the full serialized `TaskRecord` (`REQUIRED_FIELDS`, `crates/gw-task/src/redis_contract.rs:566`); read by load/watch and by the Go side per the task entity contract.
- **`entity:worker:{id}`** — Redis hash heartbeat written by the worker (`crates/gw-task-worker/src/lib.rs:152`), TTL 15 s.
- **`counter:user:{uid}:task_frozen_micro` / `counter:user:{uid}:task_frozen_tasks`** — task freeze counter and marker set written by submit and released by settle or submit rollback.
- **`counter:user:{uid}:deducted_micro`** — incremented by task settlement with the actual task charge.
- **`usage:records`** — task settlement emits a `task_usage_v1` payload for billing consumption.

## Invariants & lints

- `[task].enabled = false` by default; the matcher in `crates/gw-proxy/src/filters/task.rs:32` is the single gate.
- Cost is not computed in Rust for the synchronous path; the task path computes freeze/settle micros via `gw-billing::formulas` because async tasks have a separate billing contract and Redis settlement path.
- Spec lint: `bash scripts/check-spec.sh docs/spec/08-async-tasks.md` → `ok`.

## Not built → ROADMAP

- **Codec render of `ir_wire_body`.** The worker forwards `input_payload.body` raw and stores the raw upstream JSON; there is no codec normalization/re-render of the request or response through the IR.
