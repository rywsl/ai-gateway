# 异步任务 Worker — 架构设计文档（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


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
>
> **v3.2 微调（2026-05-19，业务/支撑分离）**：v3.1 把 task 业务的 Redis schema helper 下沉到 `gw-store` 是错的——`gw-store` 是通用 Entity/Counter/Append log 协议的支撑层，往里塞 `task::*` 命名空间会让支撑层耦合业务认知。原则升级：**业务化模块单独拆 crate，不寄生在功能支撑模块**。修订方案：把 v3.1 的 `gw-task` 一拆为二：
> - `gw-task`（业务**契约 + 通用工具**层）：DTO、Redis schema helper、settle lua 脚本字符串、TaskFreezeGuard 的 task 侧实现编排——纯 library，无 tokio runtime、无 axum、无后台 loop；**proxy 与 worker 都依赖**。"worker"在用户硬约束中特指**执行进程实体**，不是 task 业务本身，故 proxy 依赖 `gw-task`（业务）不违反"不让 worker 出现在 proxy 依赖"。
> - `gw-task-runtime`（worker **执行**层）：tasks:queue 消费循环、状态机驱动、看门狗 leader election、CancellationToken 管理、reqwest 调上游——仅 `gw-task-worker` binary 依赖；`gw-proxy` **禁止**依赖。
> - 同样原则向其他业务模块外推（见末尾"架构影响补注"）：`gw-billing`（复杂计费公式）、`gw-adapter`（上游差异化）都遵循"业务一 crate"原则。`gw-store::task::*` 撤回。受影响：ADR-T1 依赖图、ADR-T12、ADR-T15、未决问题 #8、架构影响补注。
>
> **v3.3 微调（2026-05-19，部署可分离性升级为硬约束）**：v3.2 的 Redis 通信形态已经天然支持 N gateway × M worker，但 spec 没把"部署组合"作为显式约束写下来。补：
> - 「一网关 × 多 worker」「多网关 × 一 worker」「多网关 × 多 worker」「网关与 worker 跨网络/跨可用区/跨集群部署」全部必须支持；
> - 唯一耦合面 = 同一个 Redis namespace（KMS KEK 同源）；PgSQL 可同实例可分库（只要 schema 一致），但 gateway 不强求与 worker 共库；
> - 任意一边水平伸缩不需要对端配置变更；
> - 任意一边滚动重启、版本升级，对端无感（schema 版本兼容窗口内）；
> - 不允许任何"网关知道某个具体 worker 实例"的硬编码或路由表，反之亦然。
>
> 新增 ADR-T16 固化此约束 + 验收条件。受影响：ADR-T13（worker 注册协议范围）、ADR-T11（部署形态扩展为矩阵）、未决问题新增 #10。

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

### ADR-T1 新增 crate：`gw-task` + `gw-task-runtime` + `gw-task-worker`；并新增 `gw-adapter`

业务/支撑分离 + 执行体分离，task 一族拆三层：

```
gw-core         ← 通用 ID/IR/错误
gw-codec        ← 协议 codec
gw-store        ← 通用 Entity/Counter/Append log 协议（**禁止业务命名空间**）
gw-routing      ← 选 key
gw-auth         ← 鉴权
gw-ledger       ← 余额账户、FreezeGuard trait（含 Task 实现）
gw-adapter      ← 上游差异化（业务 crate）
gw-billing      ← 复杂计费公式（业务 crate，video/阶梯/markup；详见 §架构补注）
gw-task         ← **task 业务契约层**：DTO、Redis schema helper、entity:task / tasks:queue 编解码、settle 编排（业务 crate；纯 library）
gw-task-runtime ← worker 执行层：tokio loop、看门狗、claim/lease、CancellationToken（仅 worker binary 用）
gw-proxy        ← Pingora 入口（gateway 业务）
gw-gateway      ← gateway binary
gw-task-worker  ← worker binary
```

**依赖方向**：

```
gw-core ◄── 全部
gw-codec ◄── gw-proxy, gw-task, gw-task-runtime
gw-store ◄── gw-auth, gw-routing, gw-ledger, gw-adapter, gw-task, gw-proxy
gw-auth ◄── gw-proxy, gw-task            （task 业务校验快照解码用）
gw-routing ◄── gw-proxy, gw-task-runtime  （worker 选 vendor key）
gw-ledger ◄── gw-proxy, gw-task           （TaskFreezeGuard::try_freeze 走 gw-task → gw-ledger）
gw-adapter ◄── gw-proxy, gw-task-runtime  （上游 adapter；gateway 同步与 worker 任务都用）
gw-billing ◄── gw-ledger, gw-task-runtime, gw-proxy  （复杂计费公式）
gw-task ◄── gw-proxy, gw-task-runtime    （task 业务契约，proxy 与 runtime 都需要）
gw-task-runtime ◄── gw-task-worker        （只有 worker binary 用）
gw-proxy ◄── gw-gateway
```

**禁止边**（硬约束）：

- `gw-task-runtime → gw-proxy`：禁。
- `gw-proxy → gw-task-runtime`：**禁**（这是用户"worker 不出现在 proxy 依赖"的精确落地——指向**执行体**层 crate，不指向业务契约层）。
- `gw-store → gw-task`、`gw-store → gw-billing`、`gw-store → gw-adapter`：**禁**（支撑层不依赖任何业务 crate）。
- 任何业务 crate 之间互依需在 spec 内显式登记，避免业务之间隐式耦合。

`gw-task` 与 `gw-task-runtime` 拆分的关键点：

| 维度 | `gw-task`（业务契约层） | `gw-task-runtime`（执行层） |
|---|---|---|
| 内容 | DTO、Redis schema 编解码 helper、entity:task / tasks:queue / tasks:cancel / tasks:leases 的字段约定、settle lua 脚本字符串、TaskFreezeGuard 在 task 侧的接入方式 | tokio multi-thread runtime 装配、tasks:queue XREADGROUP 消费循环、状态机驱动、看门狗 leader election、CancellationToken 管理、reqwest 调上游、heartbeat loop |
| 形态 | 纯 library，无后台 task，可在同步上下文用 | 启动后产生持续运行的 tokio task |
| 依赖 | gw-core / gw-store / gw-codec / gw-ledger / gw-auth | gw-task / gw-codec / gw-routing / gw-adapter / gw-billing / gw-ledger |
| 谁用 | `gw-proxy`（写入 + 读取契约） / `gw-task-runtime`（消费 + 推进） | 仅 `gw-task-worker` binary |
| 测试 | 单测 schema 编解码、错误码、状态转移规则 | 集成测试 + 与 Redis testcontainer 配合 |

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
- **零执行体 crate 直接互依**：`gw-proxy ↔ gw-task-runtime` 两个方向都禁止。
- **业务契约层 `gw-task` 是 proxy 与 runtime 的共同依赖**：DTO、Redis schema 编解码、settle 脚本字符串全在 `gw-task`；proxy 与 runtime 通过 `gw-task` 看见同一份契约。
- **唯一运行时耦合面 = Redis 三件套**：
  - Stream：`tasks:queue` / `tasks:cancel`；
  - Entity：`entity:task:*` / `entity:worker:*`；
  - Counter：`counter:user:<uid>:task_frozen_micro`、四层并发 counter。
- **共享底座**：Redis 实例、PgSQL 实例、Entity namespace、`billing:records` stream、`alerts` stream、KMS KEK。
- **schema 协议契约**：`entity:task.schema_version` 字段由 `gw-task` 维护读写 helper；新增 field 视为兼容；改语义必须 bump 版本，新旧 worker 一个版本周期同时兼容。

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

### ADR-T16 部署可分离性（v3.3 新增）

形态矩阵：

| 拓扑 | 是否支持 | 备注 |
|---|---|---|
| 1 gateway × 1 worker（单机开发） | ✓ | docker compose 即可 |
| 1 gateway × N worker | ✓ | Redis Stream consumer group 自动分发；增加副本 = 起多一个 binary |
| N gateway × 1 worker | ✓ | 多个 gateway 写同一份 `tasks:queue` Stream；worker 单一 consumer 消费 |
| N gateway × M worker | ✓ | 默认形态 |
| 跨可用区 / 跨地域 / 跨集群 | ✓ | 只要 gateway 与 worker 能连到同一个 Redis（或 Redis cluster 同 namespace） |

**硬约束**：

1. **零进程内调用、零 HTTP 调用**——gateway 与 worker 之间只通过 Redis 通信（参 ADR-T12），这条本身就支持任意拓扑。
2. **零硬编码对端地址**：gateway 配置里不出现 worker host/port；worker 配置里不出现 gateway host/port。两边各自只配 Redis/PgSQL DSN + KMS KEK。
3. **零成对启动顺序约束**：gateway 可以先于任何 worker 启动接客户端 submit（任务积在 `tasks:queue`）；worker 可以先于 gateway 启动空跑。
4. **水平伸缩零对端感知**：新增 1 个 worker = 起 1 个 `gw-task-worker` 进程，自动 join consumer group；新增 1 个 gateway = 起 1 个 `gw-gateway` 进程，自动写 Redis；两边都不需要对端配置变更或注册。
5. **滚动升级零对端感知**：gateway 滚动重启 = 客户端短暂连接漂移，worker 不感知；worker 滚动重启 = 在跑任务被 lease 回收（参 ADR-T6），gateway 不感知；只要 `entity:task.schema_version` 兼容窗口内即可（参 ADR-T12）。

**验收条件**（实施 plan 必须包含的测试场景）：

- 启 0 worker + 1 gateway，submit 100 个 task → 全部 queued → 再启 2 worker → 100 个全部 succeeded。
- 启 1 worker + 0 gateway，等 30s 验证 worker 空跑稳定 → 启 1 gateway → submit 1 个 → 1s 内 claim。
- 启 N gateway + M worker（N、M 各 ≥ 2），每 gateway 各 submit 50 → 全部 succeeded，每个 worker 至少 claim 过 1 个（验证负载分散）。
- gateway 副本 A 写 submit → worker B claim 执行 → 期间 gateway A 进程被 kill → 客户端通过 gateway B（轮询读 `entity:task`）继续看到终态。
- worker 副本 A 拿到 task → 进入 running → kill 副本 A → lease 在 `lease_seconds` 内被 worker B 回收 → 任务终态。

**与 ADR-T13 worker 注册协议的关系**：`entity:worker:<id>` 仅供 ops 观测（用户面板看"现在有几个 worker 在跑"），**不参与**业务路由。即使所有 worker 都没注册（极端情况），任务仍按 `tasks:queue` 顺序被任意可达 worker claim。注册协议是 _observability_，不是 _service discovery_。

### ADR-T15 Gateway 侧 task 路径实现（v3 替换原反代链）

gateway 改动落在 `gw-proxy`（业务编排）+ `gw-task`（业务契约 helper）+ `gw-ledger`（freeze）+ `gw-core`（基础类型）；**不依赖 `gw-task-runtime`**。

1. **task-shape 检测器**（在 `gw-proxy::filters::request`）：path 表 + body sniff 判定 → `RouteDecision::Vendor` 或 `RouteDecision::Task(TaskKind)`。
2. **task 路径 request_filter**（仍在 `gw-proxy`）：
   - auth + permission merge（与 vendor 路径共享）；
   - `gw-ledger::TaskFreezeGuard::try_freeze`（vendor 路径用 `RequestFreezeGuard`）；
   - codec parse_request → IR → render 为 wire JSON（worker 直接用）；
   - 选 vendor + 计算 vendor_key_hint（worker 仍可重选，hint 优先）；
   - 调 `gw-task::contract::submit(entity_fields, queue_payload)` — 这个 helper 在 `gw-task` 内做"HSET + XADD"的 lua 原子化，底层走 `gw-store` 的通用 Redis 客户端；
   - **short-circuit 返回**：跳过 upstream_peer / upstream_request_filter，gateway 直接构造 `{task_id, status=queued, expires_at}` 响应给客户端；用现有 `error_emit` 同款 short-circuit 路径，但语义是"已入队"不是"错误"。
3. **GET /v1/tasks/:id**：
   - 调 `gw-task::contract::load(task_id)` → 拿 `gw-task::dto::TaskRecord`；
   - 按客户端协议（OpenAI / Anthropic / Gemini）封装；
   - 不存在 → `GW_TASK_NOT_FOUND` 404；
   - 终态且过 TTL 进 PgSQL `tasks` 表回填（`gw-task::contract::load_persistent`）。
4. **GET /v1/tasks/:id?wait=N**：
   - 调 `gw-task::contract::watch(task_id, wait_ms)`，内部用 `XREAD BLOCK STREAMS events:entity`；
   - 命中状态变化 / 超时 → 返回当时 record；
   - 触达上限退出，不持续推。
5. **POST /v1/tasks/:id/cancel**：调 `gw-task::contract::request_cancel(task_id, reason)`，立即 200。

**`gw-task` crate 的 API 面**：

```rust
// gw-task::dto
pub struct TaskId(/* UUIDv7 */);
pub enum TaskStatus { Pending, Queued, Running, Cancelling, Succeeded, Failed, Cancelled }
pub enum TaskKind   { Image, Video, Audio, Batch, LroResponses, ... }
pub struct TaskRecord { /* entity:task 解码后形态 */ }

// gw-task::codes  （task 业务错误码，不放 gw-core——gw-core 只放跨业务通用的）
pub const GW_TASK_NOT_FOUND: &str = "GW_TASK_NOT_FOUND";
pub const GW_TASK_ALREADY_TERMINAL: &str = "GW_TASK_ALREADY_TERMINAL";
pub const GW_TASK_PROVIDER_UNAVAILABLE: &str = "GW_TASK_PROVIDER_UNAVAILABLE";
pub const GW_TASK_QUOTA_EXCEEDED: &str = "GW_TASK_QUOTA_EXCEEDED";

// gw-task::contract  （Redis schema helper；gateway 与 runtime 共用）
pub async fn submit(redis: &RedisHandle, fields: TaskFields, queue: QueuePayload) -> Result<TaskId, ...>;
pub async fn load(redis: &RedisHandle, id: &TaskId) -> Result<Option<TaskRecord>, ...>;
pub async fn watch(redis: &RedisHandle, id: &TaskId, wait_ms: u64) -> Result<Option<TaskRecord>, ...>;
pub async fn request_cancel(redis: &RedisHandle, id: &TaskId, reason: CancelReason) -> Result<(), ...>;
// runtime 侧调：
pub async fn claim(redis: &RedisHandle, consumer: &str) -> Result<Option<Claimed>, ...>;
pub async fn update_running(...) / settle(...) / recover_lease(...);

// gw-task::scripts  （lua 脚本字符串常量）
pub const SUBMIT_LUA: &str = "...";
pub const SETTLE_LUA: &str = "...";
pub const RECOVER_LEASE_LUA: &str = "...";
```

gateway 与 worker（through `gw-task-runtime`）通过同一份 `gw-task` 看见同一套契约，但两边没有直接 crate 互依——它们的共同点是依赖 `gw-task`，而 `gw-task` 不依赖任何执行体 crate。

## 4. 未决问题

1. **task-shape 检测器可扩展性**：写死 path 表 vs 配置驱动 vs adapter 自声明？倾向 vendor + path 元数据由 `gw-store` entity 提供，与 vendor 注册同源。
2. **`?wait` long-poll 的超时与连接复用**：Pingora 上挂一个 N 秒等待会占用 worker 线程；要不要给 task long-poll 一个独立的 Pingora service（不同端口）来隔离？v1 倾向直接共用 8000，超时上限设 25s。
3. **video 计费规则放哪个 crate**：Aether `video/billing.py` 公式复杂（duration / quality / aspect-ratio）；倾向新增 `gw-billing` crate 单收复杂计费公式，与 ADR-T8 的 TaskFreezeGuard trait 化配套。
4. **provider webhook 回调**：部分 provider 用 webhook 而非轮询返回结果。这会重新引入「公网入站」需求，需要一个 webhook gateway（**仍是 Pingora gw-gateway 实现**，不是 worker）：gateway 收到 webhook → 鉴 webhook 签名 → HSET entity:task + XADD events:entity（无 cancel 语义，仅状态推进）。worker 不需要变。这条建议在另一份 spec 详述。
5. **取消 vs 计费的精确语义**：cancel 时已渲染 4s 视频，按 4s 还是按 0？需业务侧拍板，决定 worker 是否调 provider `query_partial_usage`。
6. **跨版本灰度**：worker v1 与 v2 同池消费 `tasks:queue` 时，schema_version 不一致如何处理？v3 默认"新 worker 必须能读旧 schema_version"，反向不支持。需要在 `gw-task` 里固化 schema_version compat matrix。
7. **adapter 层位置**：原 spec ADR-A1 把 adapter trait 挂 `gw-proxy`，worker 调上游也要用 adapter。**已采纳**：把 adapter 上提到独立 `gw-adapter` crate（依赖 gw-core + gw-store），proxy 与 worker 都引用。这是用户明确允许的共享（"worker 复用 adapter 的 crate 是没有问题的"）。
8. **业务/支撑分离的边界守护**：v3.2 已撤回 `gw-store::task::*`。新原则："`gw-store` 只放通用 Entity/Counter/Append log 协议工具，不承载任何业务 namespace"。task 业务 Redis schema 编解码归 `gw-task`，billing 公式归 `gw-billing`，adapter 归 `gw-adapter`。审查 PR 时凡是给 `gw-store` 加业务命名空间的，一律拒。
9. **`gw-task` 与 `gw-task-runtime` 拆分边界**：tokio runtime、后台 task、看门狗 leader election 这些"执行体"特征必须留在 `gw-task-runtime`；`gw-task` 必须可以在 gateway 的同步 filter 上下文中调用而不引入 tokio task 泄漏。审查标准：在 `gw-task` 里出现 `tokio::spawn`、`tokio::task::JoinHandle` 长期持有、`tokio::time::sleep` 在循环里、或任何"启动后才能用"的状态，立即视为破窗。
10. **部署可分离性的回归保护**：ADR-T16 列的 5 个验收场景必须进 e2e 测试套件（即便慢、用 testcontainers），否则隐式假设很容易在重构中悄悄破窗。如果验收测试本身被 ignore，至少要有 lint：grep `entity:worker` / `tasks:queue` / `tasks:cancel` 在 gateway 与 worker binary 配置里出现的硬编码地址，确保两边只配 Redis DSN。

---

## 架构影响补注（v3.2 同步升级）

v3.2 引入"业务化模块单独拆 crate"原则后，对整体架构判断的影响：

1. **业务/支撑分离原则**升级为 workspace 级规则：
   - **支撑层**（gw-core / gw-codec / gw-codec-macros / gw-store / gw-routing / gw-auth / gw-ledger）：**不允许**含业务 namespace；提供通用机制（IR、Redis 客户端、JWT 验签、计费 trait 等），不知道"任务""视频""图像生成"等业务概念。
   - **业务层**（gw-adapter / gw-billing / gw-task / 未来新增）：每个业务一个 crate；可以依赖任意支撑层；业务之间互依需 spec 显式登记。
   - **执行体层**（gw-proxy / gw-task-runtime / 未来新增）：承载 runtime/server/binary 入口；可以依赖支撑层 + 业务层；执行体之间禁止互依（gw-proxy 与 gw-task-runtime 互不依赖）。
   - **binary 层**（gw-gateway / gw-task-worker）：仅做装配，依赖一个执行体 + 必要业务。

2. **预计新增的业务 crate**（除 gw-task / gw-adapter）：
   - `gw-billing`：复杂计费公式（video duration / quality / 阶梯 / markup），从 `gw-ledger` 中剥出——`gw-ledger` 仅保留余额账户、Freeze trait、settle 原语等支撑能力；
   - 后续若 quota 规则、health policy 等成为复杂业务，同样独立成 crate。

3. **`gw-adapter` 抽出**：硬依赖未变；归类为业务 crate。

4. **`gw-ledger` FreezeGuard trait 化**：硬依赖未变；`gw-ledger` 自己仍属支撑层，只暴露 trait + 余额校验原语；具体的"任务级 settle 编排"放 `gw-task`，"video 时长 actual 计算"放 `gw-billing`。

5. **`gw-engine` 抽出**：仍是建议而非硬依赖。gateway 的 task 路径与 vendor 路径在 request_filter 阶段共享前半段，目前可让 `gw-proxy` 内部模块化处理；engine 抽出可以晚一步做。

6. **ADR-018 crate 分层规则** 需要登记以下三条：
   - "支撑层 crate 禁止引入业务 namespace"；
   - "执行体之间禁止互依（如 `gw-proxy` 与 `gw-task-runtime`）"；
   - "业务 crate 与其执行层 crate 应成对存在（如 `gw-task` + `gw-task-runtime`），业务层保持纯 library 形态可在任何上下文用"。

7. **内部安全契约 ADR-T14 整体删除**——没有跨进程 HTTP，HMAC/版本化 header/私网监听这套防御都不需要。Redis schema_version 替代了"协议版本"职责，仅在 schema 演进时需要。

实施次序：

1. **重构 plan**：`gw-adapter` 抽出 + `gw-ledger` FreezeGuard trait 化 + 从 `gw-ledger` 剥出 `gw-billing`（如果时间允许）+ ADR-018 三条新规则进 CLAUDE.md；
2. **`gw-task` 业务契约 crate** 落地（DTO + Redis schema helper + settle 脚本）；
3. **`gw-task-runtime` 执行层 crate** 落地（tokio loop + 看门狗）；
4. **`gw-task-worker` binary** 装配。

`gw-engine` 抽出可与上述任一步并行做。

---

> 本文仅记录边界与协议契约。crate 骨架、状态机实现、video 计费公式、entity:task schema DDL、lua 脚本各自独立 plan。
