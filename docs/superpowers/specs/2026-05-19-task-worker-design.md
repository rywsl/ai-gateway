# 异步任务 Worker — 架构设计文档（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅；含 v3 修订）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)
  - [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md)
  - 对标：`Aether/src/services/task/`
- **范围**：长任务专用 worker；状态机、Redis/PgSQL 持久化、与 Pingora 网关通过 Redis 的协作边界
- **不在范围**：具体 provider 协议解析、视频计费公式、UI 管理面

> **v3 修订（2026-05-19，撤回 v2）**：v2 的「gateway HTTP 反向代理到 worker」被撤回。理由：
> 1. 客户端 poll 频率会直接打到 worker，worker 被客户端节奏控制；
> 2. 长连接挂在 gateway+worker 两段，对 Pingora 与 axum 两边都不友好；
> 3. HTTP 反代链是新增攻击面，需要 HMAC + 私网 + 版本契约一整套防御；
> 4. 与「无状态 + 任意副本接管」语义有摩擦（HTTP 选了哪个副本，状态就和那个副本耦合一段时间）。
>
> v3 决定：**worker 不开任何业务 HTTP 端口**；客户端 task API 完全在 gateway 实现；gateway ↔ worker 仅通过 Redis 联系（Stream + Entity + Counter）。worker 主动入队消费、主动写终态、主动 settle。受影响：ADR-T2/T3/T7/T8/T10/T11/T12/T13 全部重写；ADR-T14（HMAC 内部安全契约）整体删除，因为不再有跨进程 HTTP；ADR-T15（gateway 反代链）改为「gateway task 路径实现」。
>
> **v3.1 微调（2026-05-19，撤回 `gw-proxy → gw-task` 反向依赖）**：v3 初稿允许 `gw-proxy` 依赖 `gw-task` 以复用 DTO/schema 工具，违反用户「不让 worker 出现在 proxy 的依赖」的原意。修订：`gw-task` 与 `gw-proxy` 并列、互不依赖；共享面下沉到 `gw-core`（task DTO、TaskKind/TaskStatus 枚举、task 错误码）、`gw-store`（entity:task / tasks:queue / tasks:cancel / tasks:leases 的读写 helper + schema_version 校验）、`gw-adapter`（新抽 crate，proxy 与 worker 都依赖）。受影响：ADR-T1 依赖图、ADR-T12、ADR-T15。`gw-task` 仍是 worker 自用的 library（任务执行循环、看门狗、settle 编排），不参与 gateway 侧实现。

---

## 1. 背景与问题

`gw-gateway` 走 Pingora，热路径 P50 200µs；image/video/audio/batch 等长任务上游本身就是异步，单任务数十秒到分钟级。把这类塞进 Pingora 会出现：

- 单连接挂十几分钟、TLS 半连接堆积；
- FreezeGuard 的请求级 RAII 在中断时回滚冻结，长任务里「已经发生了上游消耗」回滚 = 损失；
- ADR-009 4 类秒级超时与分钟级任务不在同一量纲。

## 2. 关键判断：worker 是 Redis 队列的消费者，不是 gateway 的 HTTP upstream

- **单一客户端入口**：客户端只看到 gateway 8000；task 的 submit / poll / cancel 全部在 gateway 上实现，由 gateway 转写到 Redis。
- **网关 ↔ worker 零 HTTP**：通信仅通过 Redis 三件套（Stream 队列、Entity HASH 状态、Counter 计数）。worker 不开业务 HTTP 端口。
- **客户端 poll 节奏与 worker 节奏解耦**：gateway poll 读 `entity:task:<id>` 即可，不触达 worker；worker 的 provider poll 节奏由 worker 自己控（backoff 表），不受客户端 poll 频率影响。
- **worker 自驱动**：消费 stream → 执行 → 终态写 Redis → 自己 settle 余额 → ack。gateway 不"调度"worker。
- **独立进程 + axum/tokio**：axum 仅用于 `:9091` `/metrics` 与 `:8081` `/livez/readyz`，**没有业务端口**。
- **无状态**：副本宕机后看门狗（任一存活副本）回收 lease，其他副本继续从 stream consume。
- **共享底座**：复用 `gw-core / gw-codec / gw-store / gw-routing / gw-auth(类型层) / gw-ledger`；**禁止**依赖 `gw-proxy`。

## 3. 设计决策（ADR 草案）

### ADR-T1 新增两个 crate：`gw-task` + `gw-task-worker`；并新增 `gw-adapter`

```
gw-core    ◄── gw-task, gw-proxy, gw-adapter, ...    （task DTO / TaskKind / TaskStatus / 错误码 放 gw-core）
gw-codec   ◄── gw-task, gw-proxy
gw-store   ◄── gw-task, gw-proxy                     （entity:task / tasks:queue / tasks:cancel / tasks:leases 读写 helper 放 gw-store）
gw-routing ◄── gw-task, gw-proxy
gw-auth    ◄── gw-task, gw-proxy
gw-ledger  ◄── gw-task, gw-proxy                     （FreezeGuard trait + 两套实现）
gw-adapter ◄── gw-task, gw-proxy                     （上游 adapter trait 与实现，proxy 与 worker 共享）
gw-task    ◄── gw-task-worker （binary：tokio runtime + 后台 loop + 健康端口）
gw-proxy   ◄── gw-gateway     （binary：原 Pingora 入口）
```

**禁止边**：
- `gw-task → gw-proxy`：禁止（worker 不依赖 proxy）；
- `gw-proxy → gw-task`：**也禁止**（task crate 不出现在 proxy 的依赖里，这是用户硬约束）。

`gw-task` 与 `gw-proxy` 完全平行。两边的共享面通过更下层的 crate：

| 共享物 | 归属 crate | 用法 |
|---|---|---|
| TaskKind / TaskStatus / TaskId / 任务错误码 | `gw-core` | 两边 import 同一份枚举 |
| entity:task 的 HASH 字段集 + schema_version 检查 | `gw-store`（扩展 entity helper） | 写：gateway 调；读：gateway+worker 都调 |
| tasks:queue / tasks:cancel XADD/XREADGROUP wrapper | `gw-store` | 写：gateway；消费：worker |
| tasks:leases ZSET wrapper | `gw-store` | 看门狗（worker 侧） |
| task settle lua 脚本 | `gw-ledger` | 仅 worker 调；脚本字符串常量也在 gw-ledger |
| 上游 adapter（OAuth refresh、JWT、kiro EventStream） | `gw-adapter` | gateway 同步路径 + worker 任务路径都用 |

gateway 侧的 task-shape 检测器、long-poll 等业务逻辑放 `gw-proxy` 自己（业务规则属于业务层），不需要从 `gw-task` 借。

二进制名 `gw-tasks`。`cargo build -p gw-gateway` 与 `cargo build -p gw-task-worker` 分开。

### ADR-T2 职责边界（v3 重写）

```
Client → :8000 (gw-gateway / Pingora)
             │
             │  task 路径 request_filter：
             │    1. ApiKey + permission merge
             │    2. detect task-shape
             │    3. 估算冻结 → HINCRBY task_frozen_micro
             │    4. 写 entity:task:<id>     ← Redis
             │    5. XADD tasks:queue        ← Redis
             │    6. 返回客户端 { task_id, status=queued }
             │
             │  客户端 poll：
             │    1. GET /v1/tasks/:id
             │    2. HGETALL entity:task:<id>← Redis
             │    3. 直接返回；不触达 worker
             │
             │  客户端 cancel：
             │    1. POST /v1/tasks/:id/cancel
             │    2. HSET entity:task.status=cancelling
             │    3. XADD tasks:cancel       ← Redis
             │    4. 返回 { status=cancelling }

Redis
  │   tasks:queue   tasks:cancel   tasks:leases   entity:task:*   counter:user:*:task_frozen_micro
  ▲                                                                        ▲
  │                                                                        │
  │ XREADGROUP / XADD / HGETALL / HSET / HINCRBY                           │
  │                                                                        │
Worker (gw-task-worker)
  ─ tokio loop A：XREADGROUP tasks:queue → execute → write entity → settle
  ─ tokio loop B：XREADGROUP tasks:cancel → trigger CancellationToken
  ─ tokio loop C：lease watchdog（leader-elected）→ 回收过期 lease
  ─ tokio loop D：heartbeat → 更新 entity:worker:<id>
  ─ axum :9091 /metrics、:8081 /livez/readyz （仅本地观测）
  │
  ▼
Provider（reqwest 直发）
```

worker 没有业务 HTTP；gateway 没有任何到 worker 的 HTTP 调用；两边的"协议"就是上面那一行 Redis key 集合的语义。

| 形状 | gateway 行为 | worker 行为 |
|---|---|---|
| 短同步 chat/messages stream | 原 Pingora vendor upstream | 不参与 |
| OpenAI Responses `background=true` | task 路径，写 Redis | 消费 stream，执行 |
| `/v1/images/generations` | 同上 | 同上 |
| `/v1/messages/batches` | 同上 | 同上 |
| Gemini `predictLongRunning` | 同上 | 同上 |
| Veo/Sora/Kling video | 同上 | 同上 |
| 长音频 STT | 同上 | 同上 |

### ADR-T3 Redis 协议契约（v3 重写，取代旧 HTTP API）

worker 与 gateway 之间所有通信均通过下列 Redis key。**这是契约边界**——schema 变更要 bump `entity:task.schema_version` 字段，新旧 worker 一个版本周期内必须同时兼容。

#### 3.1 提交流（gateway → worker）

```
HSET entity:task:<task_id>
  task_id, schema_version=1, status=queued,
  user_id, child_id?, api_key_id,
  client_protocol, vendor_id, vendor_key_hint?,
  task_type ∈ {image, video, audio, batch, lro_responses, ...},
  ir_wire_body              # codec parse + render 后的 wire JSON
  permissions_msgpack       # gw-auth merge 结果快照
  frozen_micro              # gateway 已 HINCRBY 的额度
  freeze_kind=task
  submitted_ts_ms, expires_at_ms,
  trace_id, request_id,
  v=1                       # ADR-013 版本
EXPIRE entity:task:<task_id> 86400

XADD tasks:queue *
  task_id=<task_id>
  task_type=<...>
  vendor_id=<...>
  lease_seconds=<vendor_default>
```

**注意**：`ir_wire_body` 是 gateway 已经 codec 归一过的最终 wire 形态；worker 直接用 reqwest 发，不再 parse_request。这避免了 worker 重复做 codec 工作，也保证 IR 改动只动 gateway 一处。

#### 3.2 取消流（gateway → worker）

```
HSET entity:task:<task_id> status=cancelling
XADD tasks:cancel *
  task_id=<task_id>
  reason ∈ {client, timeout, admin}
```

#### 3.3 状态更新（worker → gateway 实际是单向写 Redis）

```
# claim 时
HSET entity:task:<task_id>
  status=running, lease_until_ms=<now+lease>, worker_id=<id>, claimed_ts_ms=<now>, v=2
ZADD tasks:leases <lease_until_ms> <task_id>

# 续租
HSET entity:task:<task_id> lease_until_ms=<new>, v=...
ZADD tasks:leases <new> <task_id>

# 进度（可选）
HINCRBY counter:task:<task_id>:progress <delta>

# provider_task_id 拿到后
HSET entity:task:<task_id> provider_task_id=<...>, provider_first_response_ms=<...>, v=...

# 终态（succeeded/failed/cancelled）— 单事务原子化
EVAL settle.lua KEYS=[entity:task:<id>, counter:user:<uid>:task_frozen_micro,
                     counter:user:<uid>:balance:deducted, billing:records,
                     tasks:queue (ack), tasks:leases]
  ARGV=[task_id, terminal_status, actual_micro, frozen_micro, ...]
# 脚本内：
#   HSET entity:task ... status=<terminal>, result=<...>, ended_ts_ms=<...>
#   HINCRBY counter:user:<uid>:task_frozen_micro -<frozen_micro>
#   HINCRBY counter:user:<uid>:balance:deducted +<actual_micro>
#   XADD billing:records ...
#   ZREM tasks:leases <task_id>
#   XACK tasks:queue tasks-workers <stream_id>
```

#### 3.4 客户端协议端点（**仅 gateway 实现**）

| 方法 | Path | gateway 行为 |
|---|---|---|
| POST | `/v1/tasks` | 见 §3.1；返回 { task_id, status, expires_at } |
| GET | `/v1/tasks/:id` | HGETALL entity:task:<id>；按客户端协议封装；不触达 worker |
| POST | `/v1/tasks/:id/cancel` | 见 §3.2 |
| GET | `/v1/tasks/:id` 的 `?wait=<seconds>` 形态 | gateway 内做 long-poll：订阅 `events:entity` 直到 status 变化或超时；仍不触达 worker |

错误码沿用 `gw-core::error::codes`：

- `GW_TASK_NOT_FOUND` (404)
- `GW_TASK_ALREADY_TERMINAL` (409)
- `GW_TASK_PROVIDER_UNAVAILABLE` (503，整个 worker 池子全死)
- `GW_TASK_QUOTA_EXCEEDED` (429，per-user 在跑任务并发上限)

### ADR-T4 任务状态机

```
            ┌──────────────┐
            │  pending     │  ← gateway HSET 写入瞬间
            └──────┬───────┘
                   │ XADD tasks:queue
                   ▼
            ┌──────────────┐
            │  queued      │
            └──────┬───────┘
                   │ worker XREADGROUP claim
                   ▼
            ┌──────────────┐
   ┌────────│  running     │── lease 过期/total_timeout ──► failed (reason=timeout)
   │        └──────┬───────┘
   │ cancel        │
   │ signal        │ provider terminal
   │ received      ▼
   │        ┌──────────────┐         ┌──────────────┐
   └───────►│  cancelling  │────────►│  cancelled   │
            └──────────────┘         └──────────────┘
                   │
                   ▼ provider 2xx + result
            ┌──────────────┐
            │  succeeded   │
            └──────────────┘
```

- 驱动方：
  - **gateway submit**：`(none) → pending → queued`（HSET + XADD 两步原子，落库时已是 `queued`）；
  - **worker exec loop A**：`queued → running`、`running → succeeded|failed`；
  - **worker exec loop B（cancel）**：收到 cancel 信号 → 设置 token → `running → cancelling → cancelled`；
  - **看门狗（worker exec loop C, leader-elected）**：lease 过期 → 回收。
- lease 续租：worker 每 (`lease_seconds`/3) 续一次；崩溃 → 至多 `lease_seconds` 后被回收。

### ADR-T5 持久化分层（按 ADR-013 四协议归约）

| 数据 | 协议 | Redis key | PgSQL 表 | 备注 |
|---|---|---|---|---|
| 任务元数据 + 实时状态 | Entity | `entity:task:<id>` (HASH, TTL=1d) | `tasks` | version 列单调；状态变化 XADD `events:entity` |
| 任务执行队列 | Append log | `tasks:queue` (Stream) | — | consumer group `tasks-workers` |
| 取消信号 | Append log | `tasks:cancel` (Stream) | — | 每个 worker 副本独立 consumer name（**fan-out**：所有 worker 都要看到信号，下面 ADR-T7 详述） |
| lease 看门狗 | sorted set | `tasks:leases` (ZSET) | — | leader-elected 看门狗扫 ZRANGEBYSCORE 0 now |
| 任务结果 | Entity 字段 / 对象存储 | `entity:task.result` | `tasks_results`（大字段） | 视频 URL 倾向只存 metadata + S3 link |
| 任务计费记录 | Append log | `billing:records`（复用 ADR-026） | 同 ADR-026 | `is_task=true`, `task_id`, `duration_seconds?` |
| Worker liveness（**ADR-T13 简化版**） | Entity | `entity:worker:<id>` (HASH, TTL=15s 续约) | — | **不参与路由**，仅供运维/观测 |
| 告警 | Alert | `alerts` | — | `type ∈ {task_timeout, provider_outage, video_duration_anomaly}` |

worker 进程内 dashmap/moka 仅做 ADR-006 的版本仲裁缓存，不持有 ground truth。

### ADR-T6 执行模型

- **claim**：worker N 个 tokio task 并发 `XREADGROUP GROUP tasks-workers <consumer> BLOCK 5000 COUNT 1`。
- **执行**：
  - 读 `entity:task:<id>.ir_wire_body`（gateway 已渲染好的 wire JSON）；
  - 用 `gw-routing` 选 vendor key（worker 端复用 8 策略）；
  - reqwest 直发上游 submit；
  - 写 `provider_task_id` → 进入 poll 循环，按 vendor backoff 表（指数，上限 30s 间隔）调上游；
  - 终态 → `gw-codec::parse_response` 解析（v1 此函数 `unimplemented!()`，是 v1.1 阻塞项）→ 走 §3.3 settle.lua 一次性原子结算。
- **超时**：独立 `TaskTimeouts { submit_timeout_s, single_poll_timeout_s, total_timeout_s }` 按 vendor+task_type 双键。默认 image=120s / audio=600s / video=1200s。
- **并发上限**：四层 Counter，全部在 **gateway submit 阶段**校验并 HINCRBY；worker 不再校验，只在终态 HINCRBY -1：
  - `counter:tasks:running:total / vendor:<vid> / user:<uid> / key:<kid>`。
  - 这把"校验时 to-be-running 数"放在 gateway，worker 不再有竞态决定权。
- **崩溃恢复**：lease 过期 → 看门狗回滚 entity.status=queued、ZREM、XADD 回 `tasks:queue`；frozen 不解、运行计数器 -1。

### ADR-T7 取消语义（v3 调整：fan-out 消费）

三种来源：

1. **客户端取消**：gateway `POST /v1/tasks/:id/cancel` → `HSET status=cancelling` + `XADD tasks:cancel reason=client`。
2. **超时取消**：看门狗 `XADD tasks:cancel reason=timeout`，但语义化写 `failed(reason=timeout)`（cancelling 中间态只对 client/admin 来源使用）。
3. **行政取消**：管理面（Go）写 entity + `XADD events:entity` namespace=task op=update status=cancelling；worker 通过 events:entity 观察到。

**fan-out 难点**：tasks:cancel 是 Stream；如果用普通 consumer group，cancel 消息只会被一个 worker 副本看到，但那个副本不一定持有该 task 的 lease。两种处理方式：

- **方案 A（采纳）**：每个 worker 副本用 `<worker_id>` 作为独立 consumer name，配同一个 group `tasks-cancel-fanout` 但 PEL 各自维护，相当于 fan-out。worker 收到 cancel 消息后查本地 inflight 表：持有该 task 的 lease 就触发 CancellationToken；否则忽略。代价：N 副本 × 全量 cancel 流，量级小可接受。
- **方案 B**：cancel 信号写到 entity，worker exec 在每次 poll tick 之间 `HGET status` 自检（轮询式取消）。代价是 HGET 调用频繁，但实现最简单。

v3 选 A，cancel 是稀疏事件，fan-out 流量可忽略。

抢占：CancellationToken → 调上游 provider 的 cancel API（best-effort，失败仅 metrics）→ 写终态 + settle。

与计费交互：
- 视频/图像按产出付费：cancelling 前如有中间产物 → 按 partial usage 计费；否则全额解冻；
- 视频按 second 计费：以 cancel 时间为截断点（与 Aether `video/billing.py:finalize_video_task` 对齐）。

### ADR-T8 计费集成（v3 重写：双侧 + settle 归 worker）

**网关侧（gateway request_filter）**：
- 调 `gw-ledger::TaskFreezeGuard::try_freeze(uid, est_micro)`：
  - `HINCRBY counter:user:<uid>:task_frozen_micro <est>`
  - 校验 `base - deducted - balance_frozen - task_frozen ≥ est`，不足 → 402；
- 成功后把 `frozen_micro` 写入 `entity:task:<id>` 作为审计快照；
- **gateway 自身 RAII**：写 entity 成功但 XADD tasks:queue 失败（极端情况）→ 立即 unfreeze（HINCRBY -<est>）+ DEL entity。
- gateway 不调 settle；submit 这一刻 frozen 即被"传递"给 task 生命周期。

**Worker 侧 settle**（终态时）：
- 单事务 lua（见 §3.3）：
  - `HINCRBY counter:user:<uid>:task_frozen_micro -<frozen>`
  - `HINCRBY counter:user:<uid>:balance:deducted +<actual>`（actual 由 worker 从上游 usage 计算）
  - `XADD billing:records ...`（含 `is_task=true / task_id / duration_seconds?`）
  - `HSET entity:task status=<terminal>, result=<...>`
  - `ZREM tasks:leases`、`XACK tasks:queue`
- **崩溃恢复**：lease 过期 → 状态回 queued，**不**解冻（任务还会重跑）；只有终态才 settle 或全额解冻。

**`gw-ledger::FreezeGuard` trait 化**（必前置）：
- 共用 trait：`try_freeze / settle / unfreeze`，参数带 `FreezeKind { Request, Task }`；
- 两套实现：请求级 RAII（请求 drop 自动 unfreeze）、任务级显式 lua 脚本；
- 余额校验公式 `base - deducted - Σ(frozen_by_kind)` 收敛到一处。

video 时长计费 simple_log schema 新增 `task_id / duration_seconds / task_type`，沿用单条 `billing:records` stream，不引第二条。

### ADR-T9 观测

- worker `:9091` `/metrics`，namespace `gw_task_*`：
  - `gw_task_inflight{vendor,type}`
  - `gw_task_state_transitions_total{from,to}`
  - `gw_task_provider_latency_ms{vendor,phase}`（submit / poll / cancel）
  - `gw_task_lease_recovered_total`
  - `gw_task_freeze_leak_recovered_total`
  - `gw_task_cancel_signal_received_total{reason,was_owner}`
- gateway 新增 task 指标（命名空间 `gw_task_gw_*`）：
  - `gw_task_gw_submit_total{result}`
  - `gw_task_gw_poll_total{cache_hit}`（poll 是否命中 events:entity long-poll）
  - `gw_task_gw_freeze_attempts_total{result}`
- detailed_log 复用 ADR-026 sink，新增 `task_phases`：`submit_ms / first_poll_ms / total_polls / last_poll_ms / cancel_ms`。

### ADR-T10 端口与配置（v3 重写）

- **worker 端口**：
  - **没有业务 HTTP 端口**；
  - `:9091` `/metrics`；
  - `:8081` `/livez` `/readyz`；
  - 这两个端口都建议绑私网，但即使绑公网也不影响业务（只有观测数据）。
- gateway 仍占 `:8000 / :9090 / :8080`，无冲突。
- **配置**：figment 同款分层，独立文件 `config/task-default.toml` + `config/task-local.toml`；环境变量沿用 `GW_*`：
  - 共享：`GW_KMS__KEK_HEX` / `GW_REDIS__URL` / `GW_PGSQL__URL`（worker 必须能解密 KMS 写余额扣减；与 gateway 共享 KEK）；
  - 任务专属：`GW_TASK__*`（`MAX_GLOBAL_RUNNING / TIMEOUTS__VIDEO__TOTAL_SECONDS / CONSUMER_NAME / LEASE_SECONDS__VIDEO`）。

### ADR-T11 部署形态（v3 重写）

- **多副本 N×worker**：Redis Stream consumer group 自动分发任务；副本之间无亲和性、无路由表。
- **看门狗 leader election**：所有副本可竞选，`SETNX lock:tasks:watchdog` TTL=10s；leader 扫描 `tasks:leases` ZRANGEBYSCORE 0 now。
- **平滑下线**：
  - SIGTERM → 停止 claim（XREADGROUP loop 退出）；
  - inflight 任务的 `CancellationToken` **不**触发，等 lease 自然过期让别的副本接管；
  - heartbeat loop 写 `entity:worker.state=draining`（仅供运维看，无业务效应）；
  - 等 inflight 减为 0 或 grace 超时 → 进程退出。
- **冷启动**：worker 池子全死时 gateway 仍能接 submit（写 Redis），任务积在 queue；新 worker 起来后自动消费。**但**这会让 ADR-T7 的 cancel fan-out 在 worker 全死期间无消费方——可接受，cancel 信号留在 Stream，新 worker 起来后会收到（worker 收到时如已是终态忽略即可）。
- **横向扩展**：增加副本 = 多一个 Stream consumer；理论上线性扩展直到 Redis stream 单分片瓶颈（远超业务量）。

### ADR-T12 与 gateway 的协作面（v3 重写）

- **零 HTTP**。
- **零 crate 直接互依**：`gw-proxy ↔ gw-task` 两个方向都禁止。
- **唯一耦合面 = Redis 三件套 + 下沉到底层 crate 的共享类型**：
  - Stream：`tasks:queue` / `tasks:cancel`；
  - Entity：`entity:task:*` / `entity:worker:*`；
  - Counter：`counter:user:<uid>:task_frozen_micro`、四层并发 counter；
  - 共享 DTO 与错误码：`gw-core`；
  - 共享 Redis schema helper：`gw-store`；
  - 共享 settle lua 脚本：`gw-ledger`；
  - 共享上游 adapter：`gw-adapter`。
- **共享底座**：Redis 实例、PgSQL 实例、Entity namespace、`billing:records` stream、`alerts` stream、KMS KEK。
- **schema 协议契约**：`entity:task.schema_version` 字段，`gw-store` 的 helper 在读写时强制校验；新增 field 视为兼容；改语义必须 bump 版本，新旧 worker 一个版本周期同时兼容（蓝绿/滚动升级）。

### ADR-T13 Worker 注册协议（v3 大幅简化）

worker 启动 → 写 `entity:worker:<worker_id>` HASH：

| field | 类型 | 说明 |
|---|---|---|
| `worker_id` | string | 启动时生成 UUIDv7 |
| `zone` | string | 可用区/机房标签（可选） |
| `version` | string | binary 版本，用于灰度观测 |
| `inflight` | u32 | 当前 running 任务数（worker 周期更新） |
| `state` | enum | `ready` / `draining` |
| `started_ts_ms` | u64 | 启动时间 |
| `last_heartbeat_ms` | u64 | 自续约时间戳 |
| `v` | u64 | ADR-013 版本号 |

**关键差异**（vs v2）：
- **没有 `bind_addr`、没有 `capacity` 字段**：因为 gateway 不需要选 worker；
- **gateway 不订阅 `events:entity` namespace=worker**：worker liveness 不参与业务路由；
- 仅供 ops dashboard / Alert（如所有 worker 全 draining → 报警）；
- TTL 15s + 5s 续约。

### ADR-T15 Gateway 侧 task 路径实现（v3 替换原反代链）

gateway 改动落在 `gw-proxy`（业务逻辑）+ `gw-store`（共享 schema helper）+ `gw-ledger`（freeze）+ `gw-core`（DTO）；**不依赖 `gw-task`**。

1. **task-shape 检测器**（在 `gw-proxy` request_filter 内）：path 表 + body sniff 判定 → `RouteDecision::Vendor` 或 `RouteDecision::Task(TaskKind)`。检测器实现属于 gateway 业务，放 `gw-proxy::filters::request`。
2. **task 路径 request_filter**（仍在 `gw-proxy`）：
   - auth + permission merge（与 vendor 路径共享）；
   - `gw-ledger::TaskFreezeGuard::try_freeze`（vendor 路径用 `RequestFreezeGuard`）；
   - codec parse_request → IR → render 为 wire JSON（worker 直接用）；
   - 选 vendor + 计算 vendor_key_hint（worker 仍可重选，hint 优先）；
   - 调 `gw-store::task::submit(entity_fields, queue_payload)` — 这个 helper 在 `gw-store` 内做"HSET + XADD"的 lua 原子化；
   - **short-circuit 返回**：跳过 upstream_peer / upstream_request_filter，gateway 直接构造 `{task_id, status=queued, expires_at}` 响应给客户端；用现有 `error_emit` 同款 short-circuit 路径，但语义是"已入队"不是"错误"。
3. **GET /v1/tasks/:id**：
   - 调 `gw-store::task::load(task_id)` → 拿 `gw-core::TaskRecord`；
   - 按客户端协议（OpenAI / Anthropic / Gemini）封装；
   - 不存在 → `GW_TASK_NOT_FOUND` 404；
   - 终态且过 TTL 进 PgSQL `tasks` 表回填（`gw-store::task::load_persistent`）。
4. **GET /v1/tasks/:id?wait=N**：
   - 调 `gw-store::task::watch(task_id, wait_ms)`，内部用 `XREAD BLOCK STREAMS events:entity`；
   - 命中状态变化 / 超时 → 返回当时 record；
   - 触达上限退出，不持续推。
5. **POST /v1/tasks/:id/cancel**：调 `gw-store::task::request_cancel(task_id, reason)`，立即 200。

**共享类型放 `gw-core`**：

```rust
// gw-core::task
pub struct TaskId(/* UUIDv7 */);
pub enum TaskStatus { Pending, Queued, Running, Cancelling, Succeeded, Failed, Cancelled }
pub enum TaskKind   { Image, Video, Audio, Batch, LroResponses, ... }
pub struct TaskRecord { /* entity:task 解码后形态 */ }
pub mod codes {
    pub const GW_TASK_NOT_FOUND: &str = "...";
    pub const GW_TASK_ALREADY_TERMINAL: &str = "...";
    pub const GW_TASK_PROVIDER_UNAVAILABLE: &str = "...";
    pub const GW_TASK_QUOTA_EXCEEDED: &str = "...";
}
```

**共享 Redis schema helper 放 `gw-store::task`**：

- `submit(fields, queue_payload) -> TaskId`：HSET + XADD 原子；
- `load(id) -> Option<TaskRecord>`：HGETALL + schema_version 校验；
- `request_cancel(id, reason)`：HSET status=cancelling + XADD tasks:cancel；
- `watch(id, wait_ms) -> Option<TaskRecord>`：XREAD BLOCK；
- `update_running(id, lease_ms, worker_id)` / `settle(id, terminal, actual)` / `recover_lease(id)`：worker 侧调。

gateway 与 worker 在 helper 层面是同一组函数,但调用集合不同（gateway 用前 4 个,worker 用后 3 个）。两边都通过 `gw-store` 这个共同下层依赖,而不互相 import。

## 4. 未决问题

1. **task-shape 检测器可扩展性**：写死 path 表 vs 配置驱动 vs adapter 自声明？倾向 vendor + path 元数据由 `gw-store` entity 提供，与 vendor 注册同源。
2. **`?wait` long-poll 的超时与连接复用**：Pingora 上挂一个 N 秒等待会占用 worker 线程；要不要给 task long-poll 一个独立的 Pingora service（不同端口）来隔离？v1 倾向直接共用 8000，超时上限设 25s。
3. **video 计费规则放哪个 crate**：Aether `video/billing.py` 公式复杂（duration / quality / aspect-ratio）；倾向新增 `gw-billing` crate 单收复杂计费公式，与 ADR-T8 的 TaskFreezeGuard trait 化配套。
4. **provider webhook 回调**：部分 provider 用 webhook 而非轮询返回结果。这会重新引入「公网入站」需求，需要一个 webhook gateway（**仍是 Pingora gw-gateway 实现**，不是 worker）：gateway 收到 webhook → 鉴 webhook 签名 → HSET entity:task + XADD events:entity（无 cancel 语义，仅状态推进）。worker 不需要变。这条建议在另一份 spec 详述。
5. **取消 vs 计费的精确语义**：cancel 时已渲染 4s 视频，按 4s 还是按 0？需业务侧拍板，决定 worker 是否调 provider `query_partial_usage`。
6. **跨版本灰度**：worker v1 与 v2 同池消费 `tasks:queue` 时，schema_version 不一致如何处理？v3 默认"新 worker 必须能读旧 schema_version"，反向不支持。需要在 `gw-task` 里固化 schema_version compat matrix。
7. **adapter 层位置**：原 spec ADR-A1 把 adapter trait 挂 `gw-proxy`，worker 调上游也要用 adapter。**已采纳**：把 adapter 上提到独立 `gw-adapter` crate（依赖 gw-core + gw-store），proxy 与 worker 都引用。这是用户明确允许的共享（"worker 复用 adapter 的 crate 是没有问题的"）。
8. **`gw-store::task::*` 是否会让 gw-store 变得太"业务"**：原 gw-store 关注通用 Entity / Counter / Append log 协议；新增的 task helper 引入了对具体业务表的认知。可接受的边界：helper 仅做 Redis 字段编解码与 schema_version 校验，不涉及任务状态机决策（决策在 gateway 业务层与 worker 执行层各自实现）。如果发现 gw-store 里出现"任务什么时候该转到 running"这类决策代码，立即视为破窗。

---

## 架构影响补注（v3.1 同步升级）

v3 撤回反向代理 + v3.1 撤回 `gw-proxy → gw-task` 依赖后，对整体架构判断的影响：

1. **`gw-adapter` 抽出**：**硬依赖**——worker 调上游 provider 要用 adapter（OAuth refresh、JWT 签名、kiro EventStream 等）。用户已明确同意此共享：proxy 与 worker 都从 `gw-adapter` import，互不依赖。
2. **`gw-engine` 抽出**：仍是**建议**而非硬依赖。gateway 的 task 路径与 vendor 路径在 request_filter 阶段共享前半段，目前可以让 gw-proxy 内部把这段抽成 module 处理；engine 抽出可以晚一步做。
3. **`gw-ledger` FreezeGuard trait 化**：**ADR-T8 的硬依赖**——两套 guard 必须共存共享余额校验。
4. **`gw-store` 新增 task 命名空间**：把 `gw-task` 原本想做的"Redis schema helper"职责留在 `gw-store`，让 `gw-task` 完全归属 worker 自用。代价是 `gw-store` 多一个业务表的认知；约束见未决问题 #8。
5. **ADR-018 crate 分层规则** 需要登记一条："`gw-task` 是 worker 私有 library；`gw-proxy` 禁止 import `gw-task`"——这是用户的硬约束，应进 CLAUDE.md 的"things that are intentional"或 ADR-018 表格。
6. **内部安全契约 ADR-T14 整体删除**——没有跨进程 HTTP，HMAC/版本化 header/私网监听这套防御都不需要。Redis schema_version 替代了"协议版本"职责，仅在 schema 演进时需要。

实施次序：先做 `gw-adapter` 抽出 + `gw-ledger` FreezeGuard trait 化 + `gw-store::task` schema helper（重构 plan）→ 再落地 worker（实施 plan）。

---

> 本文仅记录边界与协议契约。crate 骨架、状态机实现、video 计费公式、entity:task schema DDL、lua 脚本各自独立 plan。
