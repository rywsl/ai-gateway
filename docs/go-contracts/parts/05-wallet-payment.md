# Part 05 — Wallet / Payment（钱包 / 支付契约）

> 本 part 描述 Wallet / Payment 领域里 **Go 持有权威状态、Rust 只消费跨进程数据面** 的契约。重点覆盖余额计数器、钱包实体、支付状态、充值/提现结算，以及 `usage:records` 与冻结/解冻链路的衔接。索引见 [main.md](../main.md)。

---

## Redis Keys

### 5.1 Wallet balance counter family

Wallet 域余额计数器统一使用 `counter:balance:*` 前缀，Go 是唯一 writer；Rust 仅在鉴权 / 扣费 / 冻结路径读取派生后的可支付余额视图。

| Key | Writer | Reader | 含义 |
|---|---|---|---|
| `counter:balance:<wallet_id>:base_micro` | Go | Go + Rust | 钱包累计入账总额；充值、管理员调账、退款回补写这里 |
| `counter:balance:<wallet_id>:deducted_micro` | Go | Go + Rust | 已结算扣减总额；Go 消费 [`usage:records`](02-go-as-consumer.md#21-usagerecords-stream核心契约) 后递增 |
| `counter:balance:<wallet_id>:frozen_micro` | Rust | Go + Rust | Rust 预冻结中的金额；请求进入后 `+estimate`，请求结束 / 失败 / 取消后释放 |
| `counter:balance:<wallet_id>:available_micro` | Go（可选派生缓存） | Go | 可支付余额缓存；若存在，必须满足 `base_micro - deducted_micro - frozen_micro` |
| `counter:balance:<wallet_id>:last_settled_ts_ms` | Go（可选） | Go | 最近一次成功结算 `usage:records` 的 wall-clock 毫秒时间戳 |

**强不变量**：

```text
available_micro = base_micro - deducted_micro - frozen_micro
```

- 金额单位一律 **micro**；见 [main.md §3.1](../main.md#31-字段命名)。
- `base_micro` / `deducted_micro` / `available_micro` 仅 Go 写。
- `frozen_micro` 仅 Rust 写；写法与 [parts/01 §1.4](01-go-as-producer.md#14-redis-counter-keyscounter) / [parts/02 §2.5](02-go-as-consumer.md#25-counterfrozen_micro-rust-写) 保持一致。
- Go **不得**把 `frozen_micro` 当已花费金额；它只表示 in-flight 暂占。

### 5.2 Entity envelope for wallet mirrors

如 Go 需要把钱包实体 write-through 到 Redis，必须使用 [main.md §3.2](../main.md#32-entity-payload-信封adr-013-协议-1) 规定的统一 HASH 信封：

```text
entity:wallet:<wallet_id>     HASH
  v       u64
  enc     "json"
  d       <bytes>
  EXPIRE  Rust 写回时可带 TTL；Go 主动写时不带 TTL
```

推荐 JSON payload：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `wallet_id` | string | ✓ | 钱包主键，`snake_case` / 前缀化 ID |
| `owner_user_id` | string | ✓ | 钱包归属 user |
| `currency` | string | ✓ | 币种；若为法币微分单位也必须明确币种 |
| `base_micro` | i64 | ✓ | 权威累计入账 |
| `deducted_micro` | i64 | ✓ | 权威累计扣减 |
| `frozen_micro` | i64 | ✓ | 最近一次同步时的冻结快照 |
| `status` | `active` \| `suspended` \| `closed` | ✓ | 钱包状态 |
| `updated_ts_ms` | i64 | ✓ | Go 写入该镜像时的 wall-clock 毫秒 |

示例：

```json
{
  "wallet_id": "wal_01jv4n4ft0jz8s9v8m4q7y2n3p",
  "owner_user_id": "u_9fd2ab41",
  "currency": "CNY",
  "base_micro": 250000000,
  "deducted_micro": 84250000,
  "frozen_micro": 1250000,
  "status": "active",
  "updated_ts_ms": 1779456123456
}
```

### 5.3 Freeze / unfreeze interaction

充值 / 提现本身**不**直接改 Rust 侧冻结逻辑。Rust 冻结来自请求热路径：

1. Rust 按 `EstimatePricing` 估算本次请求最大可能支出；
2. `HINCRBY counter:balance:<wallet_id>:frozen_micro +<estimate_micro>`；
3. Rust 完成请求后释放同额度，或在崩溃恢复时通过 `usage:records` / reconciler 收敛；
4. Go 从 [`usage:records`](02-go-as-consumer.md#21-usagerecords-stream核心契约) 读取真实 usage 后，增加 `deducted_micro`，最终把冻结占用转成实际扣费结果。

这意味着：**充值 / 提现写 `base_micro`，推高 / 拉低 available；usage settle 写 `deducted_micro`；Rust 仅维护短生命周期 `frozen_micro`。**

## PgSQL Tables

### 5.4 `wallets`

`wallets` 是钱包权威表，Go 是唯一 writer；Rust 如需冷加载只读。推荐列：

| 列 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `id` | TEXT PK | ✓ | `wallet_id` |
| `payload` | JSONB | ✓ | 钱包业务字段；schema 见下表 |
| `version` | BIGINT | ✓ | 单调递增版本；与 [Part 03](03-shared-schema.md) 的 entity version 协议一致 |
| `created_at` | TIMESTAMPTZ | ✓ | 创建时间 |
| `updated_at` | TIMESTAMPTZ | ✓ | 更新时间 |
| `deleted_at` | TIMESTAMPTZ |  | soft-delete 标记 |

`payload` 推荐字段：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `wallet_id` | string | ✓ | 冗余主键，便于 JSONB 直读 |
| `owner_user_id` | string | ✓ | 钱包归属人 |
| `currency` | string | ✓ | 币种 |
| `base_micro` | i64 | ✓ | 累计入账 |
| `deducted_micro` | i64 | ✓ | 累计扣减 |
| `lifecycle_status` | `active` \| `suspended` \| `closed` | ✓ | 生命周期状态 |
| `last_payment_id` | string |  | 最近一次成功支付 / 退款 / 提现记录 |
| `updated_ts_ms` | i64 | ✓ | 与 Redis / webhook 对齐的 wall-clock 毫秒 |

### 5.5 `payments`

`payments` 保存每一笔支付 / 充值 / 提现的权威状态，Go 是唯一 writer。`status` 枚举固定为：

```text
pending | processing | completed | failed | refunded
```

推荐列：

| 列 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `id` | TEXT PK | ✓ | `payment_id` |
| `wallet_id` | TEXT | ✓ | 关联钱包 |
| `direction` | `deposit` \| `withdraw` | ✓ | 资金方向 |
| `provider` | TEXT | ✓ | 支付渠道标识 |
| `idempotency_key` | TEXT UNIQUE | ✓ | 业务幂等键；重复通知 / 重放必须复用 |
| `payload` | JSONB | ✓ | 支付详情与渠道回包快照 |
| `version` | BIGINT | ✓ | 单调递增版本 |
| `created_at` | TIMESTAMPTZ | ✓ | 创建时间 |
| `updated_at` | TIMESTAMPTZ | ✓ | 更新时间 |
| `deleted_at` | TIMESTAMPTZ |  | soft-delete 标记 |

`payload` 推荐字段：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `payment_id` | string | ✓ | 支付主键 |
| `wallet_id` | string | ✓ | 所属钱包 |
| `status` | `pending` \| `processing` \| `completed` \| `failed` \| `refunded` | ✓ | 当前状态 |
| `direction` | `deposit` \| `withdraw` | ✓ | 充值或提现 |
| `amount_micro` | i64 | ✓ | 申请金额 |
| `provider_trade_id` | string |  | 渠道侧流水号 |
| `idempotency_key` | string | ✓ | 幂等键 |
| `failure_code` | string |  | 渠道失败码 / 内部映射码 |
| `failure_reason` | string |  | 人类可读原因 |
| `requested_ts_ms` | i64 | ✓ | 发起时间 |
| `finalized_ts_ms` | i64 |  | 终态时间 |

### 5.6 State transition rules

| from | to | 允许 | 备注 |
|---|---|---|---|
| `pending` | `processing` | ✓ | 渠道已受理 |
| `pending` | `failed` | ✓ | 早期失败 |
| `processing` | `completed` | ✓ | 成功到账 / 成功出款 |
| `processing` | `failed` | ✓ | 渠道失败 |
| `completed` | `refunded` | ✓ | 仅充值允许退款回补 |
| `failed` | `pending` | ✗ | 必须新建 payment 并复用 / 轮换幂等键策略 |
| `refunded` | `completed` | ✗ | 终态不可回退 |

## Request/Response Payloads

### 5.7 Payment provider webhook payload

本节只定义 **Go 与 Rust 共识的数据形状**；不定义任何对外接入实现细节或公网暴露模式。

入站 webhook 解析后，Go 侧推荐归一化为：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `ts_ms` | i64 | ✓ | Go 收到并验签后的毫秒时间戳 |
| `payment_id` | string | ✓ | 内部支付 ID |
| `wallet_id` | string | ✓ | 钱包 ID |
| `provider` | string | ✓ | 渠道标识 |
| `provider_trade_id` | string |  | 渠道流水号 |
| `status` | `pending` \| `processing` \| `completed` \| `failed` \| `refunded` | ✓ | 归一化状态 |
| `direction` | `deposit` \| `withdraw` | ✓ | 资金方向 |
| `amount_micro` | i64 | ✓ | 本次金额 |
| `idempotency_key` | string | ✓ | 幂等键 |
| `signature_valid` | bool | ✓ | 渠道签名是否通过 |
| `failure_code` | string |  | 失败时填写 |
| `failure_reason` | string |  | 失败时填写 |

示例（充值成功 webhook）：

```json
{
  "ts_ms": 1779456189123,
  "payment_id": "pay_01jv4n8g33j1h7s9dr3w8gpk6q",
  "wallet_id": "wal_01jv4n4ft0jz8s9v8m4q7y2n3p",
  "provider": "wechat_pay",
  "provider_trade_id": "wx4200002184202605268842119901",
  "status": "completed",
  "direction": "deposit",
  "amount_micro": 50000000,
  "idempotency_key": "idem_dep_20260526_00017",
  "signature_valid": true
}
```

示例（提现失败 webhook）：

```json
{
  "ts_ms": 1779456220456,
  "payment_id": "pay_01jv4na4w7qkz3gkk1wjnf4g5m",
  "wallet_id": "wal_01jv4n4ft0jz8s9v8m4q7y2n3p",
  "provider": "stripe_payout",
  "provider_trade_id": "po_1QwxyZ2eZvKYlo2Cabc12345",
  "status": "failed",
  "direction": "withdraw",
  "amount_micro": 120000000,
  "idempotency_key": "idem_wd_20260526_00003",
  "signature_valid": true,
  "failure_code": "bank_account_closed",
  "failure_reason": "Receiving bank account is closed"
}
```

### 5.8 Wallet settlement input from `usage:records`

钱包扣费结算输入**不是** payment webhook，而是 Rust 发出的 [`usage:records`](02-go-as-consumer.md#21-usagerecords-stream核心契约)。Go settler 处理时需额外知道 `user_id → wallet_id` 的业务映射，但结算原始输入 schema 以 [Part 02 §2.1.2](02-go-as-consumer.md#212-payload-schema--usagerecord) 为准。

最小结算字段子集：

| 字段 | 来源 | 用途 |
|---|---|---|
| `ts_ms` | `usage:records` | 结算时间锚点 |
| `request_id` | `usage:records` | 幂等主键的一部分 |
| `attempt_index` | `usage:records` | 幂等去重 / provenance 升级 |
| `user_id` | `usage:records` | 找到 payer wallet |
| `freeze_amount_micro` | `usage:records` | 与 Rust 已冻结金额做核对 |
| `usage.*` | `usage:records` | Go 自算真实扣费输入 |
| `markup_chain_snapshot` | `usage:records` | Go 自算真实扣费输入 |

**结算顺序**：

1. Go 消费一条 `usage:records`；
2. 幂等检查按 `(request_id, attempt_index)` 与 provenance 优先级，详见 [Part 02 §2.1.8](02-go-as-consumer.md#218-去重契约新增billing-robustness-w4-t1)；
3. Go 计算本次真实应扣金额；
4. `INCRBY counter:balance:<wallet_id>:deducted_micro <actual_cost_micro>`；
5. Rust 侧在请求结束时释放同 request 对应 `frozen_micro`，使 available 回到真实余额；
6. 若真实扣费 < 已冻结金额，差额不会由 Go 回写退款事件，而是通过 Rust release 动作自然回补到 available；
7. 若真实扣费 > 已冻结金额，记为 under-freeze incident，Go 必须告警并保留 payment / wallet 审计记录。

### 5.9 Payment entity payload example

如 Go 选择对 `payments` 做 Redis write-through，可把 `payload` 直接序列化进 `entity:payment:<payment_id>` 的 `d` 字段：

```json
{
  "payment_id": "pay_01jv4n8g33j1h7s9dr3w8gpk6q",
  "wallet_id": "wal_01jv4n4ft0jz8s9v8m4q7y2n3p",
  "status": "completed",
  "direction": "deposit",
  "amount_micro": 50000000,
  "provider_trade_id": "wx4200002184202605268842119901",
  "idempotency_key": "idem_dep_20260526_00017",
  "requested_ts_ms": 1779456181000,
  "finalized_ts_ms": 1779456189123
}
```

## Failure Modes

### 5.10 Contract failure matrix

| 场景 | 后果 | Go 义务 | Rust 义务 |
|---|---|---|---|
| webhook 重放（同一 `idempotency_key`） | 重复充值 / 重复提现风险 | 以 `idempotency_key` / `payment_id` 去重；重复事件只做状态核对不得重复改余额 | 无直接写入；若读到最终余额变化按最新权威值服务 |
| `payments.status` 非法跳变（如 `failed -> completed` 同一版本） | 审计不一致 | 拒绝更新，保留原版本并告警 | 仅读权威终态，不猜测修复 |
| `usage:records` 重复消费 | `deducted_micro` 双扣风险 | 按 [Part 02 §2.1.8](02-go-as-consumer.md#218-去重契约新增billing-robustness-w4-t1) 去重 | Rust 可重复 emit，不负责幂等 |
| `frozen_micro` 未释放（进程崩溃 / 释放失败） | available 偏低 | Go 不应直接清零；等待 Rust reconciler / 启动恢复，并在长时间滞留时告警 | 通过 reconciler / guard drop / crash fallback 收敛 |
| `base_micro` 已入账但 `payments.status` 仍非 `completed` | 钱包与支付表不一致 | 以事务串联 `payments` + `wallets` 更新，失败则整笔重试 | 无 |
| 提现 `completed` 但未扣 `base_micro` | 可支付余额被高估 | Go 必须在同事务内落 payment 终态与 wallet 扣减 | Rust 只读结果 |
| provider 签名失败 | 伪造通知风险 | `signature_valid=false` 时不得改 `payments` / `wallets`；保留审计日志 | 无 |
| under-freeze（真实扣费 > `freeze_amount_micro`） | 可能透支 | 记录 incident、补充告警、继续按真实 cost 写 `deducted_micro` | 后续请求仍按当前 available 判定；freeze 公式由 Rust/EstimatePricing 改进 |

附加约束：

- **禁止**在本 part 内引入 `cost_micro` 字段或任何 Rust 侧成本计算逻辑；Go 自算、Rust 不感知。
- **禁止**把 webhook 原始 secret、签名密钥、银行卡号、证件号写进示例 payload。
- Payment / Wallet 相关 Redis / PgSQL 契约若新增 namespace、stream 或 entity schema，必须同步更新 [main.md §2](../main.md#2-契约文件组织) 与 [§7 Sync rule](../main.md#7-sync-rule与-claudemd-sync-rule-联动)。
