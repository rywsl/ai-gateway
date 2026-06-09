# 08 - Async Tasks (as-built)

- Family: T (task)
- Backing crates: `gw-task`, `gw-task-runtime`, `gw-task-worker`
- Source-of-truth: `crates/gw-task/src/{contract.rs,scripts.rs,redis_contract.rs}`, `crates/gw-task-runtime/src/{exec_loop.rs,heartbeat_loop.rs,watchdog_loop.rs,release.rs}`, `crates/gw-task-worker/src/lib.rs`, `crates/gw-proxy/src/filters/task.rs`
- Status: default-off (`[task].enabled = false`); submit, Redis queue, Redis task freeze, worker execution/polling, full IR request/response envelopes, atomic settle accounting, and watchdog leader election are built.

## Purpose

The task family handles long-running vendor operations (image / video / audio / batch / LRO-responses) that do not fit the synchronous streaming proxy path. A client submits a job over HTTP, the gateway freezes an estimated amount in Redis and enqueues the job onto a Redis stream, and a separate worker process (`gw-task-worker`) claims, executes, polls when the provider returns an LRO-style response, and settles it. Clients poll or watch for the terminal record.

Task settlement is the accounting point: it atomically releases the task freeze, increments the actual deduction counter, removes the lease, and emits a task usage payload onto `usage:records`.

## As-built surface

`detect_task_shape` (`crates/gw-proxy/src/filters/task.rs`) routes to `TaskAction::Submit` for `POST /v1/tasks`, `POST /v1/images/generations`, and `POST /v1/responses` when the JSON body has `background: true`; `GET /v1/tasks/{id}`, `GET /v1/tasks/{id}/watch`, and `POST /v1/tasks/{id}/cancel` map to `Load` / `Watch` / `Cancel`. The matcher returns `None` when `[task].enabled = false`.

`submit_endpoint_impl` parses the body, infers the kind from `task_type` or `kind`, computes `frozen_micro` with `gw_billing::formulas::calculate_freeze_amount`, generates the `TaskId`, and calls `TaskContract::freeze_task(user_id, task_id, frozen_micro, available_balance_micro)` before enqueue. When the request has a non-CLI detected client codec, submit also parses the task request body through `gw-codec`, stores `input_payload.ir_request = {protocol, kind:"request", value}`, renders the normalized request back to wire bytes, and stores `input_payload.ir_wire_format`, `input_payload.ir_wire_content_type`, and `input_payload.ir_wire_body`. Insufficiency maps to `GW_TASK_FREEZE_FAILED` (HTTP 402) and increments `gw_task_gw_freeze_total{result="insufficient"}`. Successful freeze increments `gw_task_gw_freeze_total{result="ok"}`. If enqueue fails after a freeze, `release_task_freeze` rolls the Redis task freeze back.

A successful submit writes a `TaskRecord` with `status = Queued`, `schema_version = 1`, `expires_at_ms = now + 86_400_000`, and the parsed `input_payload`, then returns HTTP 201 `{task_id, status:"queued", expires_at}`. Load/watch/cancel serialize the Redis task record back to the client.

## Redis Contract

`RedisTaskContract` loads `TASK_FREEZE_LUA`, `SUBMIT_LUA`, `SETTLE_LUA`, and `RECOVER_LEASE_LUA` with `SCRIPT LOAD` and falls back to `EVAL` on `NOSCRIPT`.

- `freeze_task` runs `TASK_FREEZE_LUA`: read scalar `counter:user:<uid>:task_frozen_micro`; reject when `available_balance_micro - current_task_frozen < amount`; otherwise `INCRBY` the task-frozen counter and `SADD` the task id into `counter:user:<uid>:task_frozen_tasks`.
- `release_task_freeze` is the submit rollback path: `INCRBY counter:user:<uid>:task_frozen_micro -amount` and `SREM counter:user:<uid>:task_frozen_tasks task_id`.
- `submit` runs `SUBMIT_LUA`: `HSET` all record fields on `entity:task:{id}`, `EXPIRE` the hash, `XADD` `{task_id, task_type, vendor_id, lease_seconds}` onto `tasks:queue`, and `ZADD lease_until_ms task_id` onto `tasks:leases`.
- `claim` ensures consumer group `tasks-workers`, `XREADGROUP`s one entry, loads the record, transitions `Queued -> Running`, stamps `worker_id` / `claimed_ts_ms`, recomputes the lease, and re-`ZADD`s it.
- `update_running` refreshes `lease_until_ms` and optional `provider_task_id`.
- `settle` validates the terminal transition, sets `billing_record_id = "task:<task_id>"`, builds a `task_usage_v1` JSON payload, and runs `SETTLE_LUA`.
- `recover_lease` runs `RECOVER_LEASE_LUA` to re-enqueue expired leases.

`SETTLE_LUA` takes `KEYS = [entity:task:{id}, tasks:leases, counter:user:<uid>:task_frozen_micro, counter:user:<uid>:deducted_micro, usage:records, counter:user:<uid>:task_frozen_tasks]` and `ARGV = [task_id, terminal_status, settled_at_ms, released_micro, deducted_micro, usage_payload, <field value>...]`. In one Redis script it `HSET`s terminal task fields, `ZREM`s the lease, `INCRBY`s task frozen by `-released_micro`, `INCRBY`s deducted by `deducted_micro`, `SREM`s the task freeze marker, and `XADD`s `{payload: task_usage_v1 JSON}` to `usage:records`.

## Runtime Loops

`ExecLoop` claims one task, acquires global/vendor/model/key concurrency permits, updates it to running, and runs `call_upstream` under the task total timeout. `call_upstream` sends a `reqwest` `POST` to `input_payload.upstream_url` or `vendor_key_hint`. If `input_payload.ir_wire_body` is present, the worker sends those UTF-8 bytes verbatim with `Content-Type = input_payload.ir_wire_content_type || application/json`; old records without `ir_wire_body` keep the legacy fallback of JSON-serializing `input_payload.body` or the whole payload.

Immediate terminal responses settle once. When `input_payload.ir_wire_format` maps to a supported codec and the upstream response parses successfully, terminal `output_payload` stores a normalized full IR response envelope: `body` preserves the raw upstream JSON, `ir_response = {protocol, kind:"response", value}` stores the parsed response IR, and `ir_wire_body` stores the response rendered back from that IR with `ir_wire_content_type = application/json`. Response codec parse failures do not fail an otherwise successful task; the worker stores the raw JSON payload only.

LRO-style responses with `status`/`state` in `queued|pending|running|in_progress|processing` plus `status_url`/`poll_url` enter a generic JSON poll loop. The worker `GET`s the poll URL with exponential backoff (`poll_initial_backoff` default 1 s, capped by `poll_max_backoff` default 30 s) until terminal success (`succeeded|completed|complete|done|success`) or failure (`failed|cancelled|canceled|error`). `provider_task_id` is extracted from `provider_task_id` or `id`; for normalized response envelopes the worker reads those fields through `output_payload.body` / `output_payload.ir_response.value` as needed, and writes the provider id back before terminal settle.

`HeartbeatLoop` refreshes `lease_until_ms` for running tasks and evicts registry entries when the task is already terminal or missing.

`WatchdogLoop` wakes every `check_interval` (default 5 s), first attempts `SET lock:tasks:watchdog 1 NX PX <leader_ttl>` (`leader_ttl` default 10 s), and skips the scan unless it acquired the lock. The leader re-enqueues expired leases through `RECOVER_LEASE_LUA`.

`RedisTaskFreezeRelease` is namespace-compatible with the Redis task contract (`tasks:leases`, `entity:task:<id>`, scalar `counter:user:<uid>:task_frozen_micro`) but is not the production settle path; production settlement uses `RedisTaskContract::settle`.

## Cross-process Touchpoints

- `tasks:queue` - Redis stream for task submission and watchdog recovery.
- `tasks:cancel` - Redis stream for cancellation fan-out.
- `tasks:leases` - sorted set keyed by `lease_until_ms`.
- `entity:task:{id}` - Redis hash holding the serialized `TaskRecord`.
- `entity:worker:{id}` - worker heartbeat hash with a 15 s TTL.
- `counter:user:<uid>:task_frozen_micro` - scalar task-freeze counter.
- `counter:user:<uid>:deducted_micro` - scalar actual deduction counter incremented by task settle.
- `counter:user:<uid>:task_frozen_tasks` - set of task ids with active task freeze markers.
- `usage:records` - task settle appends `payload = task_usage_v1 JSON`.
- `lock:tasks:watchdog` - Redis leader lock for lease recovery scans.

## Deliberate Behaviours

- `[task].enabled = false` is the default and the only gateway route gate.
- Task freeze is Redis-backed, not process-local `DEDUCT`.
- Task actual billing amounts are task-domain formula outputs from `gw-billing`; synchronous request token cost computation remains outside Rust.
- Task workers do not call `gw-adapter`; request wire bytes are pre-rendered by submit, and response normalization uses `gw-codec` only.
- Worker polling is generic JSON-status polling. Codec response normalization is best-effort for terminal payload persistence and does not replace the JSON status/poll contract.

## Not built -> ROADMAP

No open ROADMAP items after W4 full IR request/response persistence.
