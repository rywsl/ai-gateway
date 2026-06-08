# 08 — Async Tasks (as-built)

- Family: T (task) · Backing crates: `gw-task`, `gw-task-runtime`, `gw-task-worker` · Source-of-truth: `crates/gw-task/src/{scripts.rs,redis_contract.rs}`, `crates/gw-task-runtime/src/{exec_loop.rs,heartbeat_loop.rs,watchdog_loop.rs,release.rs}`, `crates/gw-task-worker/src/lib.rs`, `crates/gw-proxy/src/filters/task.rs` · Status: default-off (`[task].enabled = false`); submit + Redis queue + single-shot worker execution exist, but settle neither releases the freeze, deducts balance, nor emits a usage record.

## Purpose

The task family handles long-running-ish vendor operations (image / video / audio / batch / LRO-responses) that do not fit the synchronous streaming proxy path. A client submits a job over HTTP, the gateway freezes an estimated cost and enqueues the job onto a Redis stream, and a separate worker process (`gw-task-worker`) claims, executes, and settles it. Clients poll or watch for the terminal record. The submit, queue, claim, lease, heartbeat, watchdog-recovery, and worker-registration mechanics are built; the settlement accounting (freeze release + balance deduction + billing emission) is not.

## As-built surface

### Submit endpoint and request-path freeze

`detect_task_shape` (`crates/gw-proxy/src/filters/task.rs:26`) routes to `TaskAction::Submit` for `POST /v1/tasks`, `POST /v1/images/generations`, and `POST /v1/responses` when the JSON body has `background: true` (`body_has_background_true`, `crates/gw-proxy/src/filters/task.rs:54`); `GET /v1/tasks/{id}`, `GET /v1/tasks/{id}/watch`, and `POST /v1/tasks/{id}/cancel` map to `Load` / `Watch` / `Cancel`. The whole matcher short-circuits to `None` when `task_enabled` is false (`crates/gw-proxy/src/filters/task.rs:32`). `mark_task_context` sets `ctx.is_task` / `ctx.task_action` (`crates/gw-proxy/src/filters/task.rs:67`).

`submit_endpoint_impl` (`crates/gw-proxy/src/filters/task.rs:151`) parses the body, infers the kind via `infer_task_kind` (`crates/gw-proxy/src/filters/task.rs:365`, reads `task_type` or `kind`), and computes `frozen_micro` with `gw_billing::formulas::calculate_freeze_amount`. When `frozen_micro > 0` it calls `try_freeze_task`.

`try_freeze_task` (`crates/gw-proxy/src/filters/task.rs:239`) performs the freeze **in process** against `gw_store::deduction::DEDUCT` (a `DashMap<UserId, Arc<DeductionState>>`, `crates/gw-store/src/deduction.rs:31`). It rejects up front when `available_balance_micro < amount`, then CAS-loops on `DeductionState::frozen_micro`, computing real-available as `base_micro − deducted_micro − frozen_micro` and bumping `frozen_micro` by the amount; insufficiency yields a `GW_TASK_FREEZE_FAILED` (HTTP 402, `crates/gw-proxy/src/filters/task.rs:114`). On success it increments `gw_task_gw_freeze_total{result="ok"}`. If `contract.submit` subsequently errors, `release_local_task_freeze` (`crates/gw-proxy/src/filters/task.rs:278`) decrements the in-process `frozen_micro` again.

A successful submit builds a `TaskRecord` with `status = Queued`, `schema_version = 1`, `expires_at_ms = now + 86_400_000`, and the parsed `input_payload`, then returns HTTP 201 `{task_id, status:"queued", expires_at}` (`crates/gw-proxy/src/filters/task.rs:228`). `load_endpoint` / `watch_endpoint` / `cancel_endpoint` serialize the record back; `watch_endpoint` emits `text/event-stream` framed by `write_task_response` (`crates/gw-proxy/src/filters/task.rs:343`). Error codes `GW_TASK_NOT_FOUND`, `GW_TASK_SUBMIT_FAILED`, `GW_TASK_FREEZE_FAILED`, `GW_TASK_CANCELLED` are defined in `crates/gw-core/src/error/codes.rs:142`.

### Redis contract (Lua + fred)

`RedisTaskContract` (`crates/gw-task/src/redis_contract.rs:25`) loads three scripts (`SUBMIT_LUA`, `SETTLE_LUA`, `RECOVER_LEASE_LUA`, `crates/gw-task/src/scripts.rs`) via `SCRIPT LOAD` and dispatches each with `evalsha_or_eval` (`crates/gw-task/src/redis_contract.rs:113`), falling back to `EVAL` on `NOSCRIPT`. Keys are `entity:task:{id}` (`task_key`), `tasks:queue` (`queue_key`), `tasks:cancel` (`cancel_key`), and `tasks:leases` (`leases_key`), each optionally prefixed (`crates/gw-task/src/redis_contract.rs:88`).

- `submit` (`crates/gw-task/src/redis_contract.rs:167`) drives `SUBMIT_LUA` (`crates/gw-task/src/scripts.rs:1`): `HSET` of all record fields on `KEYS[1]`, an `EXPIRE` (TTL from `expires_at_ms`, default 86 400 s), an `XADD` of `{task_id, task_type, vendor_id, lease_seconds}` onto the queue (`KEYS[2]`), and a `ZADD lease_until_ms task_id` onto the lease set (`KEYS[3]`).
- `claim` (`crates/gw-task/src/redis_contract.rs:273`) ensures the `tasks-workers` consumer group, then `XREADGROUP … COUNT 1 … >` for one entry; it loads the record, transitions `Queued → Running`, stamps `worker_id` / `claimed_ts_ms`, recomputes the lease, `HSET`s the record, and re-`ZADD`s the lease. Returns a `ClaimedTask { record, stream_id, lease_until_ms }`.
- `update_running` (`crates/gw-task/src/redis_contract.rs:344`) refreshes `lease_until_ms` and optional `provider_task_id` and re-`ZADD`s the lease.
- `settle` (`crates/gw-task/src/redis_contract.rs:368`) validates the terminal transition, mutates the in-memory record, and runs `SETTLE_LUA`.
- `recover_lease` (`crates/gw-task/src/redis_contract.rs:428`) runs `RECOVER_LEASE_LUA` (limit 100) then reloads the record.

`SETTLE_LUA` (`crates/gw-task/src/scripts.rs:33`) takes `KEYS = [entity:task:{id}, tasks:leases]` and `ARGV = [task_id, terminal_status, settled_at_ms, <field value>…]`. It does exactly two things: `HSET` the status / `settled_at_ms` (plus any extra field pairs) on `KEYS[1]`, and `ZREM task_id` from `KEYS[2]`. It does **not** touch any balance counter, freeze counter, or usage stream.

`RECOVER_LEASE_LUA` (`crates/gw-task/src/scripts.rs:57`) `ZRANGEBYSCORE`s expired leases (`-inf … now_ms`, `LIMIT 0 limit`), and for each it successfully `ZREM`s it re-`XADD`s `{task_id, recovered:1, lease_seconds}` onto the queue so a worker re-claims it. Records are serialized field-by-field via `record_to_fields` / `fields_to_record` with `REQUIRED_FIELDS` (`crates/gw-task/src/redis_contract.rs:566`).

### Runtime loops (`gw-task-runtime`)

`ExecLoop` (`crates/gw-task-runtime/src/exec_loop.rs:88`) claims one task, acquires global/vendor/model/key concurrency permits (`ConcurrencyLimits`, `crates/gw-task-runtime/src/exec_loop.rs:96`), `update_running`s it, and runs `call_upstream` under a `tokio::time::timeout(total)`. `call_upstream` (`crates/gw-task-runtime/src/exec_loop.rs:272`) issues a **single synchronous `reqwest` POST** to `upstream_url` (from `input_payload.upstream_url` or `vendor_key_hint`, `crates/gw-task-runtime/src/exec_loop.rs:337`) with `upstream_body` (`input_payload.body` or the whole payload) and treats that **first HTTP response as terminal**: success → `settle(Succeeded, payload, settle_amount)`, non-2xx or error → `settle(Failed, …, 0)`, timeout → `settle(Failed, "task execution timeout…", 0)`. `settle_amount` (`crates/gw-task-runtime/src/exec_loop.rs:321`) calls `gw_billing::formulas::calculate_settle_amount`; the amount is passed into `settle` but, per `SETTLE_LUA`, is never applied to any balance. `provider_task_id` is extracted from `provider_task_id`/`id` in the response and written back via a second `update_running`.

`HeartbeatLoop` (`crates/gw-task-runtime/src/heartbeat_loop.rs:29`) wakes every `heartbeat_interval` (default 15 s) and, for each task in the `RunningTaskRegistry`, calls `update_running` with `now + lease_extension` (default 60 s), incrementing `TASK_HEARTBEAT_TOTAL`; `AlreadyTerminal` / `NotFound` evict the task from the registry.

`WatchdogLoop` (`crates/gw-task-runtime/src/watchdog_loop.rs:101`) wakes every `check_interval` (default 5 s), calls `RedisLeaseRecoveryStore::recover_expired` (which runs `RECOVER_LEASE_LUA`, `crates/gw-task-runtime/src/watchdog_loop.rs:53`) to re-enqueue expired-lease tasks, then `recover_lease`s each. It runs with **no leader election** — every worker's watchdog scans the same `tasks:leases` set concurrently; the `ZREM`-guards inside `RECOVER_LEASE_LUA` are the only thing preventing duplicate re-enqueue.

### Worker process (`gw-task-worker`)

`run` (`crates/gw-task-worker/src/lib.rs:365`) loads `WorkerConfig` (from `config/task-default.toml` + `GW_TASK_*` env), connects Redis, generates a worker id, writes a registration, builds a `RedisTaskContract`, and spawns six tasks: exec, cancel, watchdog, heartbeat, worker-registration, and a hyper `/livez` `/readyz` health server (default ports 8081 health / 9091 metrics, `crates/gw-task-worker/src/lib.rs:301`). `heartbeat_worker_registration` (`crates/gw-task-worker/src/lib.rs:152`) `HSET`s `entity:worker:{id}` (`worker_id`, `start_time_ms`, `status="running"`, `hostname`, `last_heartbeat_ms`) with a 15 s `EXPIRE`; the registration loop refreshes it on its heartbeat interval so a dead worker's key TTL-expires.

### Configuration

`[task]` in `config/default.toml:52` — `enabled = false`, `queue_stream = "tasks:queue"`, `cancel_stream = "tasks:cancel"`, `billing_base_micro = 1000`, `billing_vendor_multiplier = 1.0`, `billing_estimated_duration_secs = 60`. The worker process has its own `config/task-default.toml` layer (`WorkerConfig::load`, `crates/gw-task-worker/src/lib.rs:242`) with `[server]` `[redis]` `[worker]` `[loops]` `[concurrency]` `[timeouts]` tables.

## Deliberate behaviours

- **Default-off.** `[task].enabled = false` (`config/default.toml:53`) makes `detect_task_shape` return `None` for every method/path, so the gateway never enters the task path unless explicitly enabled.
- **Submit-path freeze is fail-closed on insufficiency.** `try_freeze_task` rejects with `GW_TASK_FREEZE_FAILED` (HTTP 402) both on the up-front balance check and inside the CAS loop, and increments `gw_task_gw_freeze_total{result="insufficient"}` (`crates/gw-proxy/src/filters/task.rs:244`).
- **Settle is HSET + ZREM only.** `SETTLE_LUA` (`crates/gw-task/src/scripts.rs:33`, `KEYS=[entity:task:{id}, tasks:leases]`, `ARGV=[task_id, terminal_status, settled_at_ms, …]`) writes the terminal status and removes the lease — nothing else. The `released_micro` / `deducted_micro` / `billing_record_id` returned in `SettleOutcome` (`crates/gw-task/src/redis_contract.rs:420`) are computed from the in-memory record and have no Redis-side effect. See ROADMAP.
- **A standalone `release.rs` exists but is unused, and addresses a different key namespace.** `RedisTaskFreezeRelease` (`crates/gw-task-runtime/src/release.rs:16`) implements `gw_ledger::TaskFreezeRelease` via `TASK_FREEZE_RELEASE_LUA` and atomically removes a lease, mutates task status, and decrements a frozen counter under hash-tagged keys `{user:<uid>}:tasks:leases`, `{user:<uid>}:entity:task:{id}`, `{user:<uid>}:counter:task_frozen_micro` (`crates/gw-task-runtime/src/release.rs:47`). Those keys are **not** the keys the contract writes — `RedisTaskContract` writes un-hash-tagged `entity:task:{id}` / `tasks:leases` (`crates/gw-task/src/redis_contract.rs:97`), and the per-user `counter:task_frozen_micro` is never written anywhere in these crates. The only references to `RedisTaskFreezeRelease` outside its own module are its tests (`crates/gw-task-runtime/tests/task_freeze_release.rs`); no production code constructs or calls it. See ROADMAP.
- **Exec treats the first upstream response as terminal.** There is no submit→poll loop; `call_upstream` POSTs once and settles on that response (`crates/gw-task-runtime/src/exec_loop.rs:272`).
- **Worker liveness is TTL-based, not lease-arbitrated.** `entity:worker:{id}` carries a 15 s `EXPIRE` refreshed by the registration loop (`crates/gw-task-worker/src/lib.rs:166`); a crashed worker simply lets the key lapse.

## Cross-process touchpoints

- **`tasks:queue`** — Redis stream, consumer group `tasks-workers` (`crates/gw-task/src/redis_contract.rs:15`). Producer: `SUBMIT_LUA` `XADD` `{task_id, task_type, vendor_id, lease_seconds}`; watchdog re-enqueues `{task_id, recovered:1, lease_seconds}`. Consumer: `gw-task-worker`'s `XREADGROUP`.
- **`tasks:cancel`** — Redis stream; `request_cancel` `XADD`s `{task_id, reason}` (`crates/gw-task/src/redis_contract.rs:258`), consumed by the worker's cancel loop.
- **`tasks:leases`** — Redis sorted set keyed by `lease_until_ms`; shared by submit/claim/update/settle/watchdog.
- **`entity:task:{id}`** — Redis hash holding the full serialized `TaskRecord` (`REQUIRED_FIELDS`, `crates/gw-task/src/redis_contract.rs:566`); read by load/watch and by the Go side per the task entity contract.
- **`entity:worker:{id}`** — Redis hash heartbeat written by the worker (`crates/gw-task-worker/src/lib.rs:152`), TTL 15 s.
- **In-process `DEDUCT`** — the submit-path freeze mutates `gw_store::deduction::DEDUCT` local to the gateway process only; it is not a cross-process touchpoint and is invisible to the worker and to Go.

## Invariants & lints

- `[task].enabled = false` by default; the matcher in `crates/gw-proxy/src/filters/task.rs:32` is the single gate.
- Cost is not computed in Rust for the synchronous path; the task path computes freeze/settle micros via `gw-billing::formulas` but (per `SETTLE_LUA`) does not bill, consistent with the workspace `no_cost_micro` / `no_billing_crate` posture in `scripts/check-invariants.sh`.
- Spec lint: `bash scripts/check-spec.sh docs/spec/08-async-tasks.md` → `ok`.

## Not built → ROADMAP

- **RUNTIME-RISK — settle leaks the freeze and bills nothing.** `SETTLE_LUA` only `HSET`s status and `ZREM`s the lease (`crates/gw-task/src/scripts.rs:33`); it does not release the freeze, deduct the actual balance, or `XADD` a usage record. A submitted-and-settled task therefore permanently holds its `frozen_micro` (in the gateway's in-process `DEDUCT`) and produces zero billing. Settle must release the freeze, deduct `actual_micro`, and emit a usage record on `usage:records`.
- **RUNTIME-RISK — task freeze is in-process, not Redis-based.** `try_freeze_task` (`crates/gw-proxy/src/filters/task.rs:239`) freezes against the per-process `DEDUCT` map; with multiple gateway Pods the freeze is per-Pod and breaks multi-gateway accounting. The freeze should be Redis-based (the unused `RedisTaskFreezeRelease` in `crates/gw-task-runtime/src/release.rs:16` gestures at the intended `{user:<uid>}:counter:task_frozen_micro` model, but it is unwired and its key namespace does not match what the contract writes — both the freeze write side and the release call site are missing).
- **Async submit→poll loop + vendor backoff.** `call_upstream` (`crates/gw-task-runtime/src/exec_loop.rs:272`) does one POST and treats the first response as terminal; there is no provider-side polling loop or backoff for genuinely long-running vendor jobs.
- **Codec render of `ir_wire_body`.** The worker forwards `input_payload.body` raw and stores the raw upstream JSON; there is no codec normalization/re-render of the request or response through the IR.
- **RUNTIME-RISK — watchdog leader election.** `WatchdogLoop` (`crates/gw-task-runtime/src/watchdog_loop.rs:101`) runs on every worker with no coordination; a SETNX-based leader lock would prevent every worker scanning `tasks:leases` simultaneously.
