# Part 06 — Subscription / Video（订阅与视频任务契约）

> 本 part 描述 **Go 管理面 / task-worker 与 Rust 网关之间** 围绕订阅计划、订阅生效、视频任务状态推进的契约面。索引见 [main.md](../main.md)。

---

## 6.1 概览

| 接触点 | Writer | Reader | 说明 |
|---|---|---|---|
| PgSQL `subscription_plans` | Go | Go / 运维 | 订阅套餐权威定义；Rust 不直接热路径读取 |
| PgSQL `subscriptions` | Go | Go / 运维 | 用户订阅实例权威；Rust 不直接热路径读取 |
| PgSQL `video_tasks` | Go task-worker | Go / 运维 | 长视频任务状态机与输入输出快照 |
| Redis `entity:subscription_plan:<plan_id>` | Go（可选 write-through） | Rust（冷加载写回） | 订阅套餐缓存；遵循 ADR-013 entity envelope |
| Redis `entity:subscription:<subscription_id>` | Go（可选 write-through） | Rust（冷加载写回） | 订阅实例缓存；用于失效传播 |
| `events:entity` ns=`subscription_plan` / `subscription` | Go | Rust StreamConsumer（未来接入）/ Go 辅助 worker | **仅复用现有 `events:entity` stream**，禁止新开 stream |
| `usage:records.operation_id` | Rust | Go task-worker / billing | OpenAI `/v1/videos` 或 Gemini LRO 的关联键 |

## Redis Keys

### 6.2 entity cache / invalidation 约定

订阅相关缓存**必须**沿用 [main.md](../main.md) §3.2 的 entity envelope，**不得**自创 stream 或旁路失效协议。

| Key / Stream | Writer | Reader | 语义 |
|---|---|---|---|
| `entity:subscription_plan:<plan_id>` | Go | Rust / Go | 套餐定义缓存；Go write-through 时**不加 TTL** |
| `entity:subscription:<subscription_id>` | Go | Rust / Go | 用户订阅实例缓存；Go write-through 时**不加 TTL** |
| `events:entity` with `ns=subscription_plan` | Go | Rust / Go | 套餐变更通知；payload 不随事件发送 |
| `events:entity` with `ns=subscription` | Go | Rust / Go | 订阅实例变更通知；payload 不随事件发送 |

Go 写顺序：

1. `UPDATE/INSERT` PgSQL 行并使 `version = version + 1`；
2. 可选 `HSET entity:subscription_plan:<id>` 或 `entity:subscription:<id>`；
3. `XADD events:entity * ns <subscription_plan|subscription> id <id> v <new_version> op upsert|delete`。

Rust 侧消费语义与 [parts/01-go-as-producer.md](01-go-as-producer.md) §1.2 保持一致：以 invalidate-then-reload 为准，事件不携带权威 payload。

### 6.3 缓存 payload 示例 — `entity:subscription:<subscription_id>`

```json
{
  "subscription_id": "sub_20260526_001",
  "user_id": "u_pro_001",
  "plan_id": "plan_creator_pro",
  "status": "active",
  "billing_cycle": "monthly",
  "current_period_start_ts_ms": 1779753600000,
  "current_period_end_ts_ms": 1782432000000,
  "cancel_at_period_end": false,
  "entitlements": {
    "rpm_limit": 600,
    "daily_quota_tokens": 2500000,
    "max_concurrent_streams": 8
  },
  "updated_by": "billing_cron"
}
```

## PgSQL Tables

### 6.4 `subscription_plans`

套餐定义是 Go 权威。推荐控制列与其他 entity 表保持一致：

```sql
id          TEXT PRIMARY KEY,
payload     JSONB       NOT NULL,
version     BIGINT      NOT NULL DEFAULT 0,
updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
deleted_at  TIMESTAMPTZ NULL
```

`payload` 最低要求：

```json
{
  "plan_id": "plan_creator_pro",
  "display_name": "Creator Pro",
  "status": "active",
  "billing_cycle": "monthly",
  "price_micro": 29900000,
  "currency": "cny",
  "tier": "creator_pro",
  "entitlements": {
    "rpm_limit": 600,
    "tpm_limit": 0,
    "daily_quota_tokens": 2500000,
    "max_concurrent_streams": 8
  },
  "video": {
    "enabled": true,
    "max_duration_seconds": 300,
    "allowed_task_types": ["video_generation", "video_upscale"]
  }
}
```

约定：

- `price_micro` 使用 **micro** 单位；
- `billing_cycle` 使用 `snake_case` 枚举，推荐 `monthly | quarterly | yearly | lifetime`；
- `status` 推荐 `active | disabled | archived`；
- 套餐定义变更后，Go 必须同步重算受影响订阅的有效 entitlement，并刷新投影到 Rust 已消费的 entity（见 §6.6）。

### 6.5 `subscriptions`

`subscriptions` 记录用户当前或历史订阅实例。Rust 网关**不直接以此表做热路径权限判断**；Go 必须把订阅生效后的结果投影到 `users` / `user_groups` / `api_keys` 等现有 entity，再通过 `events:entity` 通知 Rust。

推荐控制列同上；`payload` 最低要求：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `subscription_id` | string | ✓ | 主键镜像 |
| `user_id` | string | ✓ | 订阅归属用户 |
| `plan_id` | string | ✓ | 对应 `subscription_plans.id` |
| `status` | enum | ✓ | `active \| expired \| cancelled \| suspended` |
| `billing_cycle` | enum | ✓ | `monthly \| quarterly \| yearly \| lifetime` |
| `started_ts_ms` | i64 | ✓ | 生效时间 |
| `current_period_start_ts_ms` | i64 | ✓ | 当前账期开始 |
| `current_period_end_ts_ms` | i64 | ✓ | 当前账期结束 |
| `cancel_at_period_end` | bool | ✓ | 到期取消标记 |
| `entitlements` | object | ✓ | 已解析的 quota / rpm / 并发额度 |
| `source` | string | 可选 | `manual \| stripe \| admin \| migration` 等 |

状态语义：

| 状态 | 含义 | Go 义务 | Rust 可见效果 |
|---|---|---|---|
| `active` | 当前有效 | 投影 entitlement 到用户侧 entity | 新限制立即生效 |
| `expired` | 账期结束且未续费 | 回退到基础 tier / 免费额度 | 配额收紧后按下一次请求判定 |
| `cancelled` | 人工或支付侧立即终止 | 立即撤销订阅 entitlement | 与 `active` 相比不再享有提额 |
| `suspended` | 风控/欠费/人工封禁 | 可设为 0 配额或禁视频 | Rust 按投影后的 permission fail-closed |

### 6.6 订阅 tier → Rust permission merge 投影规则

Rust 网关已实现 5-tier merge（global → user_group → user → sub-account → api-key），**并不知道 `subscriptions` 表**。因此 Go 必须先把订阅 tier 解析为 Rust 已有字段：

1. 将订阅 entitlement 写入 `users.payload` 的 `rpm_limit` / `tpm_limit` / `daily_quota_tokens` / `max_concurrent_streams`；或
2. 写入专门的 `user_groups.payload` 后把用户挂入该 group；或
3. 针对特定 key 写入 `api_keys.payload` 覆盖值。

投影原则：

- **订阅只提供上游业务语义，Rust 只消费投影后的数值权限**；
- `rpm_limit = 0` / `tpm_limit = 0` / `daily_quota_tokens = 0` 仍表示“不限”，与现有 Rust 语义一致；
- 当订阅升级产生更高额度时，Go 先写 PgSQL entity，再 `XADD events:entity` 通知 Rust；
- 当订阅降级 / 过期 / suspended 时，Go 必须在同一事务批次内更新投影，避免 Rust 持续使用旧 entitlement。

换言之：`subscriptions` 是**业务权威**，`users` / `user_groups` / `api_keys` 是 Rust 热路径的**执行权威投影**。

### 6.7 `video_tasks`

`video_tasks` 由 Go task-worker 维护，承接 `/v1/videos` / 长轮询任务的生命周期。`operation_id` 关联规则见 [parts/02-go-as-consumer.md](02-go-as-consumer.md) §2.1.2。

推荐 `payload` 最低要求：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `task_id` | string | ✓ | 任务主键 |
| `request_id` | string | ✓ | 初始提交请求 ID |
| `operation_id` | string | 可选 | 上游 LRO 名称，如 `operations/...` |
| `user_id` | string | ✓ | 任务归属用户 |
| `api_key_id` | string | ✓ | 触发任务的 key |
| `task_type` | enum | ✓ | `video_generation \| video_upscale \| video_extend \| video_lipsync` |
| `status` | enum | ✓ | `pending \| queued \| running \| succeeded \| failed` |
| `input` | object | ✓ | 任务输入 |
| `output` | object/null | ✓ | 终态输出；失败时可为 `null` |
| `error` | object/null | ✓ | 失败快照；成功时 `null` |
| `submitted_ts_ms` | i64 | ✓ | 提交时间 |
| `started_ts_ms` | i64/null | ✓ | 开始执行时间 |
| `finished_ts_ms` | i64/null | ✓ | 终态时间 |
| `duration_seconds` | i64/null | 可选 | 视频时长，供 Go 侧计费 |

`task_type` 扩展必须保持 `snake_case`，并与 Go task-worker / 计费侧同 PR 落地。

### 6.8 `video_tasks.payload` 示例

```json
{
  "task_id": "vt_20260526_9f3d",
  "request_id": "req_01J0VIDEO9F3D",
  "operation_id": "operations/video-gen-9f3d",
  "user_id": "u_pro_001",
  "api_key_id": "ak_live_video",
  "vendor_id": "v_google",
  "task_type": "video_generation",
  "status": "running",
  "input": {
    "model": "veo-2.0-generate",
    "prompt": "a cinematic drone shot over a neon city in the rain",
    "duration_seconds": 8,
    "aspect_ratio": "16:9",
    "fps": 24,
    "seed": 424242
  },
  "output": null,
  "error": null,
  "submitted_ts_ms": 1779753600123,
  "started_ts_ms": 1779753602456,
  "finished_ts_ms": null,
  "duration_seconds": null
}
```

### 6.9 视频任务状态机

允许状态推进：

```
pending -> queued -> running -> succeeded
pending -> queued -> running -> failed
```

约束：

- `pending`：Go 接到提交请求、尚未完成入队；
- `queued`：已入任务队列，等待 worker claim；
- `running`：已拿到 provider 接受或轮询中的 operation；
- `succeeded`：拿到最终视频产物 / metadata；
- `failed`：provider 明确失败、超时或内部结算失败。

本 part **不**设计 Go HTTP API；只规定持久化与跨进程关联字段。

### 6.10 `video_tasks.output` 终态示例

```json
{
  "video_url": "https://cdn.example.com/video/vt_20260526_9f3d.mp4",
  "thumbnail_url": "https://cdn.example.com/video/vt_20260526_9f3d.jpg",
  "duration_seconds": 8,
  "width": 1280,
  "height": 720,
  "sha256": "76d9c4a0b7a4f2c945b11475a9d56ac8b944f3f6f44f2e8ecf7601ab2d8cc10e",
  "provider_metadata": {
    "finish_reason": "completed",
    "provider_task_id": "google-veo-job-9913"
  }
}
```

## Failure Modes

| 故障 | 结果 | Go / Rust 行为 |
|---|---|---|
| Go 更新 `subscriptions` 但忘记更新 `users` / `user_groups` / `api_keys` 投影 | Rust 继续使用旧 entitlement | 视为契约违背；必须以同事务批次或可靠补偿任务修复 |
| Go 更新 PgSQL 成功但漏发 `events:entity` | Rust 直到缓存 TTL 或冷加载前仍见旧值 | 与 [parts/01-go-as-producer.md](01-go-as-producer.md) §1.2 相同；下次 miss 用 `version` 追平 |
| `status=suspended` 但 entitlement 未清零 | Rust 可能继续放行请求 | Go 必须显式投影成 0 配额或禁止相关模型/能力 |
| `video_tasks.status=running` 后 worker 崩溃 | 任务卡住或被超时回收 | Go task-worker / watchdog 负责 lease 回收与重试；Rust 不参与轮询恢复 |
| `usage:records.operation_id` 缺失 | Go 无法把初始请求与 `video_tasks` 关联 | Go 应回退到 `request_id` 关联；如上游确为 LRO 仍缺失，记审计告警 |
| 终态成功但 `output` 未落库 | 计费可完成但用户无法拿到结果 | Go 先落 `video_tasks.output` 再标记 `succeeded`，避免“成功无产物” |
| 订阅过期与视频任务在途并发发生 | 新请求应按新额度拒绝；旧任务按既有 task 生命周期收尾 | Go 只影响后续 entitlement 投影；在途任务依其冻结/结算语义完成 |
