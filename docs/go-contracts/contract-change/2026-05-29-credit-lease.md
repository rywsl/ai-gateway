# 2026-05-29 — Credit Lease 防透支硬保护契约

> **Status**: 契约变更记录(Awaiting Go-side implementation)
> **日期**: 2026-05-29
> **触发来源**: Rust 侧 spec [`docs/superpowers/specs/2026-05-29-credit-lease-design.md`](../../superpowers/specs/2026-05-29-credit-lease-design.md)
> **范围**: Rust 网关新增"积分租约"硬防层。Go 管理面 / 计费面 需要新增 outstanding ledger 维护、模式切换决策、价格表 → 积分系数表换算、两条新 ns 推流。
>
> **本文件作用**:把 Rust spec 中所有"Go 必须做的事"显式列出,定义契约 shape、Go 侧职责、与 Rust 证据的对应关系。Go 端实现完成后,在每个章节末尾把 status 从 `Awaiting Go-side implementation` 改为 `landed`。

---

## 0. 变更索引

| # | 项 | Go 侧职责 | 契约落点 | Rust 证据 |
|---|---|---|---|---|
| 1 | 模式切换 + lease 推送 | 维护 protected/unprotected 状态,周期性续推 lease | `events:counter` `ns=protected_lease` | `crates/gw-ledger/src/lease.rs`(待实现) |
| 2 | 系数表分发 | 价格表变更时推 `coef_table` + `coef_table_commit` | `events:counter` `ns=coef_table` / `ns=coef_table_commit` | `crates/gw-ledger/src/coef.rs`(待实现) |
| 3 | outstanding ledger | per-(user, pod) 跟踪已下发未兑现 credits | Redis HASH `credit:outstanding:<uid>` | 间接:Rust 推 `usage:records` 带 `pod_id` + `credits_consumed` |
| 4 | `usage:records` 字段扩展 | 消费新增字段,反算 credits 减 outstanding | `usage:records.payload.{pod_id, credits_consumed, coef_table_version}` | `crates/gw-ledger/src/usage.rs`(扩展) |
| 5 | 低余额阈值判定 | 监控用户余额,触发模式切换 | Go 内部事务,无新契约 | (无) |

---

## 1. 模式切换 + lease 推送

**Status: Awaiting Go-side implementation**

### 1.1 Contract summary

Go 在以下任一情况发生时,XADD 一条 `protected_lease` 事件到 `events:counter` stream:

- 用户余额首次跌破 Go 配置的低余额阈值 → 推 `mode=protected` + 初始 `lease_credits`
- 用户在 protected 模式下 lease 续约 → 推 `mode=protected` + 新 `lease_credits` + 新 `expires_at_ms`
- 用户余额回升至阈值之上(充值、退款) → 推 `mode=unprotected`
- Pod 重启 catchup 后,Go 主动重发该 Pod 当前服务的所有 protected 用户最新 lease(可选,实现简化时可不做,Rust 会在自然续约周期到达时收到)

### 1.2 Stream payload shape

```
XADD events:counter *
  ns protected_lease
  user_id <string>                # u_* 前缀
  mode <"unprotected" | "protected">
  lease_credits <i64>             # mode=unprotected 时此字段语义无效但仍需提供(填 0)
  lease_id <string>               # ULID 或类似单调递增 ID,Rust 仅作日志
  expires_at_ms <i64>             # 绝对时间戳 ms,UTC,mode=unprotected 时填 0
  coef_table_version <u64>        # lease_credits 计算所基于的系数表版本
  ts_ms <i64>
```

### 1.3 Go 侧职责

- **维护 per-user 模式状态**:推荐 Redis HASH `credit:user_mode:<uid>` 存 `{mode, since_ms}`,Go 内部决策依据。
- **决定切换时机**:Go 配置 `low_balance_threshold_micro`(USD micro)。监测路径:每次 `usage:records` 处理完后,检查最新 `balance_micro - deducted_micro` 与阈值的关系,跨阈值则推切换事件。
- **决定 lease 大小**:见 §3 outstanding ledger 约束。
- **续约调度**:推荐间隔 = `lease_ttl_ms / 2`,典型 `lease_ttl_ms = 300_000`(5 分钟)→ 每 150 秒一次。续约时 `expires_at_ms = now + lease_ttl_ms`。

### 1.4 lease 大小计算公式

设:
- `B = real_balance_micro(user) = balance_micro - deducted_micro`(从 counter 读)
- `O_self = outstanding[user][pod_id_being_pushed]`(本 Pod 当前未兑现 credits)
- `O_others = Σ_pod≠self outstanding[user][pod]`(其他 Pod 当前未兑现 credits)
- `peak_concurrency` = 该用户近期(滚动 60s)在该 Pod 上的并发请求峰值,由 Go 从 `usage:records.ts_sent_ms` 推算
- `max_per_request_credits` = 该用户近期请求的预扣 credits P95,由 Go 从 `usage:records.credits_consumed` 推算
- `safety_factor = 2.0`(Go 配置,默认 2 倍冗余以覆盖突发)
- `min_floor` = Go 配置,典型对应 `peak_concurrency × max_per_request_credits` 的等价 micro

公式:
```
desired_lease_credits = max(min_floor_credits, peak_concurrency × max_per_request_credits × safety_factor)
allowed_total_credits = balance_to_credits(B × (1 + overcommit_ratio))
push_lease_credits = min(
    desired_lease_credits,
    allowed_total_credits − O_self − O_others
)
```

`balance_to_credits()` 使用当前生效 `coef_table_version` 按"该用户最常用模型"或"价格中位数"换算,Go 内部决定。

### 1.5 Cross-process touchpoints

- **Redis stream key**: `events:counter`(已存在)
  - 新增 ns: `protected_lease`
- **PgSQL column**: 无新增
- **Go 内部 Redis key**(非契约,Go 自行实现):
  - `credit:user_mode:<uid>` HASH — Go 内部模式状态
  - `credit:outstanding:<uid>` HASH — outstanding ledger(见 §3)

### 1.6 Rust evidence

- Spec: `docs/superpowers/specs/2026-05-29-credit-lease-design.md` §5.1.1, §6.1
- 消费点(待实现): `crates/gw-ledger/src/lease.rs::LeasePool::on_event`
- StreamConsumer 接入(已存在): `crates/gw-store/src/redis_client.rs` events:counter consumer

---

## 2. 系数表分发(`coef_table` + `coef_table_commit`)

**Status: Awaiting Go-side implementation**

### 2.1 Contract summary

价格表变更时,Go 推送一组 `coef_table` 条目(每个 `(model_id, dim)` 一条),最后推一条 `coef_table_commit` 标记本批结束。Rust 收到 commit 后原子切换生效版本。

**关键约束:仅在价格表实际变更时推送**,不做心跳推送。Rust 启动时若没有任何 `coef_table` 历史,会以 `version=0` 空表运行,所有 protected 模式请求 fail 到 `GW_QUOTA_LEASE_INSUFFICIENT`(因为 estimate_request_credits 拿不到系数)——所以 Go 必须确保 Rust 启动后**第一时间**(catchup 期间)能拿到至少一个完整版本。

### 2.2 Stream payload shape

**单条 entry**:

```
XADD events:counter *
  ns coef_table
  coef_table_version <u64>        # 单调递增
  model_id <string>
  dim <"input" | "output" | "cache_read" | "cache_creation" | "thinking" | "multimodal_input">
  coef_per_token <u64>            # credits / token,纯系数,无单位
  ts_ms <i64>
```

**commit 终结**:

```
XADD events:counter *
  ns coef_table_commit
  coef_table_version <u64>        # 与本批 entry 的 version 一致
  model_count <u32>               # 本版本包含的不同 model_id 数(用于 Rust 校验完整性)
  ts_ms <i64>
```

每个 `model_id` 在一个版本内通常有 6 条 dim entry(全部维度)。Rust 不强制要求 6 条都有——缺失的维度按 0 处理(等价于"该模型不计该维度")。

### 2.3 Go 侧职责

- **版本号单调递增**:`coef_table_version` 全局严格递增,典型实现为 PgSQL sequence 或 Redis `INCR`。
- **完整推送语义**:同一 `coef_table_version` 的所有 entry 必须在同一次"价格变更事件"内连续推完,中间不得插入其他 ns 的 stream 写入(StreamConsumer 内部按 stream 顺序处理,跨 ns 顺序不强制,但同 ns 同 version 必须连续)。
- **commit 时机**:所有 entry 推完后立即推 commit,不要拖延。
- **不做兜底心跳**:本 ns 仅在价格变更时推,变更不频繁(可能数月一次)。

### 2.4 启动期对齐

Pod 启动时,StreamConsumer 从该消费组的 last-id 重读。新部署的 Rust(没有持久化 last-id)会从 `0-0` 全量重读,捕获最新 `coef_table_version`。Go 不需要为 Rust 启动做任何特殊推送。

但 Go 应保证:**`events:counter` stream 中至少保留最新一个 commit 完成的 `coef_table_version` 的全部 entry + commit**,即 stream 截断策略不能截掉最新版本。推荐使用 `XADD MAXLEN ~ <N>` 或定期 `XTRIM` 时按 `coef_table_version` 边界保留。

### 2.5 Cross-process touchpoints

- **Redis stream key**: `events:counter`
  - 新增 ns: `coef_table`, `coef_table_commit`
- **PgSQL column**: Go 内部价格表 schema(本契约不规定)
- **Payload field shape**: 见 §2.2

### 2.6 Rust evidence

- Spec: `docs/superpowers/specs/2026-05-29-credit-lease-design.md` §5.1.2, §5.1.3, §6.2
- 消费点(待实现): `crates/gw-ledger/src/coef.rs::ModelCoefCache::on_event`

---

## 3. Outstanding Ledger

**Status: Awaiting Go-side implementation**

### 3.1 Contract summary

Go 维护 per-(user, pod) 的 "已下发但未通过 `usage:records` 兑现" credits 计数。每次 push lease 加,每次 `usage:records` 减。**这是永不透支的核心数据结构**。

本 ledger 完全是 **Go 内部数据**,Rust 不读、不写、不感知其存在。但 Rust 通过在 `usage:records` 上带 `pod_id` + `credits_consumed`(§4)给 Go 提供减计数所需信号。

### 3.2 Recommended Redis schema

```
credit:outstanding:<uid>          HASH    TTL = lease_ttl_ms × 3 (sliding,每次更新刷新)
  field <pod_id>:credits      → <i64 outstanding credits>
  field <pod_id>:expires_at   → <i64 ms>
  field <pod_id>:lease_id     → <string>
```

或者用 ZSET 按 `expires_at` 排序方便 GC,Go 自行决定。

### 3.3 Go 侧职责

**Push lease 时**(§1):
```
HINCRBY credit:outstanding:<uid> <pod_id>:credits +<delta>
HSET    credit:outstanding:<uid> <pod_id>:expires_at <expires_at_ms>
HSET    credit:outstanding:<uid> <pod_id>:lease_id <lease_id>
```

`<delta>` = 推送策略决定。**SET 语义还是 INCR 语义?** 推荐 **SET 语义**(每次推完整 lease 而不是增量):
```
HSET credit:outstanding:<uid> <pod_id>:credits <new_lease_credits + remaining_from_previous>
```
这要求 Go 知道上一次推送后还剩多少。最干净的做法:Go 在推 lease 前先看 `usage:records` 已经从上次以来兑现了多少,把"上次推送量 − 已兑现量"作为 `remaining_from_previous`,加上本次新分配量,即新 credits。

**消费 `usage:records` 时**(§4):
```
if record.pod_id present and record.credits_consumed > 0:
    HINCRBY credit:outstanding:<record.user_id> <record.pod_id>:credits -<record.credits_consumed>
    if value goes < 0:
        log warning, clamp to 0
```

**TTL GC**:周期任务(每 30s 或 lease_ttl_ms / 2)扫描:
```
for each user with outstanding entries:
    for each (pod_id, expires_at):
        if now_ms > expires_at + grace_ms:
            HDEL credit:outstanding:<uid> <pod_id>:credits
            HDEL credit:outstanding:<uid> <pod_id>:expires_at
            HDEL credit:outstanding:<uid> <pod_id>:lease_id
            metric: go_credit_outstanding_gc_total{reason="expired"}
```

`grace_ms` 推荐略大于 Rust 的 `lease_grace_ms`(典型 `10_000`),确保 Rust 已 fail-closed 后 Go 才回收 outstanding(避免回收时 Rust 还在消费已过期 lease)。

**Pod 永久消失**:GC 是唯一回收路径。Go 不需要监听 Pod 列表变化——Pod 死了 lease 不续约,outstanding 自动到期清理。

### 3.4 永不透支不变量(必须在 push lease 路径上断言)

```
Σ_pod (current_outstanding[user][pod] + delta_being_pushed_to_this_pod)
    ≤ balance_to_credits(real_balance_micro × (1 + overcommit_ratio))
```

违反时:Go 拒绝本次推送,记录 `go_credit_overcommit_blocked_total`,等待下一次 `usage:records` 兑现释放 outstanding 后再推。

### 3.5 Cross-process touchpoints

- **Redis key**: `credit:outstanding:<uid>` (Go 自有,非契约 — 此处仅作记录,Rust 不接触)
- **PgSQL column**: 无
- **Payload field shape**: 见 §4

---

## 4. `usage:records` 字段扩展

**Status: Awaiting Go-side implementation**(Rust 需先在 `crates/gw-ledger/src/usage.rs` 加字段,然后 Go 消费)

### 4.1 Contract summary

`UsageRecord` payload 新增三个 **additive + omitempty** 字段:

```json
{
  // ...既有字段不变...
  "pod_id": "<string>",            // 必填(从本 spec 起所有新发的 record 都带)
  "credits_consumed": <i64>,       // 可选,仅 protected 模式填,unprotected 缺失或 0
  "coef_table_version": <u64>      // 可选,与 credits_consumed 同时出现
}
```

### 4.2 字段语义

- `pod_id`:Rust 进程的 instance_id(L0 env / L1 OnceLock / L2 Redis,见 [`2026-05-24-billing-robustness-design.md`](../../superpowers/specs/2026-05-24-billing-robustness-design.md) §5.4.1)。**从本 spec 起所有 record 都必填**,旧 record 兼容性靠"缺失视为 unknown_pod"处理。
- `credits_consumed`:Rust 在 protected 模式下用 `coef_table_version` 对应的系数表把真实 6 维 usage 反算出的 credits 总和。仅 protected 模式有值。
- `coef_table_version`:`credits_consumed` 计算所基于的版本号。Go 验证版本与当前 outstanding 记录的版本一致——若不一致(理论上发生在 Rust 持久 lease 未续期 而 Go 已切版本),Go 仍按 `credits_consumed` 减 outstanding,但记 `go_credit_version_mismatch_total`。

### 4.3 Go 侧职责

**消费时新逻辑**(在现有计费流程之外**追加**):

```
on usage_records record:
    # 既有:计算 cost,INCRBY deducted_micro
    cost_micro = compute_cost(record.usage)
    INCRBY counter:user:<uid>:deducted_micro <cost_micro>
    
    # 新增:减 outstanding(若适用)
    if record.pod_id and record.credits_consumed > 0:
        HINCRBY credit:outstanding:<record.user_id> <record.pod_id>:credits -<record.credits_consumed>
        # 若结果 < 0 clamp 到 0,记 metric
    
    # 新增:模式切换检查
    new_balance = balance_micro - deducted_micro
    if user.mode == "protected" and new_balance >= low_balance_threshold_micro:
        push protected_lease event with mode="unprotected"
        delete credit:outstanding:<uid>:* (此 user 所有 pod outstanding)
        delete credit:user_mode:<uid>
```

### 4.4 兼容性

- **旧 record**(本 spec 上线前):`pod_id` 缺失。Go 容忍,跳过 outstanding 相关逻辑,仅做计费。
- **`credits_consumed = 0`** 或 缺失:说明该请求走的是软防路径(`unprotected` 模式),Go 跳过 outstanding 减计数,仅做计费。
- **`pod_id` 存在但 outstanding 中无对应 entry**:可能是 Pod 重启 / TTL GC 已清。Go 跳过减计数,记 `go_credit_outstanding_orphan_total`。

### 4.5 Cross-process touchpoints

- **Redis stream key**: `usage:records`(已存在)
- **PgSQL column**: 无新增
- **Payload field**: 三个新字段(§4.1)

### 4.6 Rust evidence

- Spec: `docs/superpowers/specs/2026-05-29-credit-lease-design.md` §10.2
- Schema(待扩展): `crates/gw-ledger/src/usage.rs`
- 填充点(待实现): `crates/gw-proxy/src/billing_lifecycle.rs`(在 protected 模式下注入 `credits_consumed`)

---

## 5. 低余额阈值与配置

**Status: Awaiting Go-side implementation**

### 5.1 Contract summary

低余额阈值、lease TTL、overcommit 比例、续约间隔**全部**是 Go 配置项,Rust 不感知。本节列出 Go 端需要的配置,作为 Rust spec 的逻辑前置条件,**不是契约字段**。

### 5.2 Go 推荐配置项

```yaml
credit_lease:
  enabled: false                       # 总开关
  low_balance_threshold_micro: 5000000 # USD micro,$5;跌破即切 protected
  lease_ttl_ms: 300000                 # 5 分钟
  lease_renew_interval_ms: 150000      # 续约间隔(应 ≤ lease_ttl_ms / 2)
  outstanding_grace_ms: 10000          # outstanding GC 宽限,应 > Rust lease_grace_ms
  overcommit_ratio: 0.0                # [0.0, 0.2],严格 0 = 永不透支
  safety_factor: 2.0                   # lease 大小 = peak_concurrency × max_per_req × safety_factor
  min_floor_credits: 0                 # lease 下限
```

低余额阈值的单位是 **USD micro**(与 `balance_micro` 对齐)。运营若按人民币定义阈值,Go 内部按汇率换算成 USD micro 后比对——汇率管理与 Rust 无关。

### 5.3 Rust 侧不感知什么

Rust 配置仅含三项(`[protection]` 表):
- `enabled`(总开关)
- `lease_grace_ms`(时钟漂移容忍)
- `default_output_ceiling_tokens`(请求未指定 max_tokens 的兜底)

Rust 看不到 `low_balance_threshold_micro` / `lease_ttl_ms` / `overcommit_ratio`。

---

## 6. 与既有契约的关系

| 既有契约 | 影响 |
|---|---|
| `counter:user:<uid>:balance_micro` / `deducted_micro` | 不变,Go 仍是唯一 writer |
| `counter:user:<uid>:frozen_micro` | 不变,服务于 unprotected 模式 |
| `usage:records` payload(既有字段) | 不变,新增字段全部 additive omitempty |
| `events:counter` 既有 ns(`user_balance` 等) | 不变,新增 3 个 ns 走同一 stream |
| `events:entity` | 无影响 |
| `usage:records:dlq` | 无影响,DLQ 处理逻辑相同 |
| KEK 轮换 | 无影响,本契约无加密 payload |

---

## 7. Go 侧验收标准

实现 landed 前,Go 团队需提供:

1. **outstanding ledger 一致性测试**:模拟并发推送 + 兑现,验证 `Σ outstanding ≤ balance × (1 + overcommit_ratio)` 全程成立。
2. **TTL GC 正确性测试**:Pod 永久消失场景,outstanding 在 `lease_ttl_ms + outstanding_grace_ms` 后清零,且不影响该用户其他 Pod 的 lease。
3. **模式切换边界测试**:余额跨阈值上下波动,推送事件序列正确,不丢不重。
4. **系数表完整性测试**:同一 version 的 entry 全部推完才推 commit;Rust 端 `gw_credit_coef_table_incomplete_total` 在异常场景下能被触发。
5. **联合 e2e**:配合 Rust 端 `tests/e2e/client/soak/ --protected-mode` 跑通 50k 请求,断言任意时刻无透支。

完成上述验收后,本文档每个 §X 标题下的 status 改为 `landed`,并在 [`docs/architecture.md`](../../architecture.md) §"Cross-process touchpoints" 索引中追加引用。
