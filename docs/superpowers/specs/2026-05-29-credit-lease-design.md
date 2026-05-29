# 2026-05-29 — Credit Lease 防透支硬保护设计

> **Status**: design (awaiting implementation plan)
> **作者**: Rust 网关团队
> **范围**: Rust 网关新增"积分租约"层,作为现有软防(`frozen_micro`)之上的硬上界。
> **不在范围**: Go 价格表内部结构、Go 端并发采样具体实现、人民币/USD 汇率换算(全部 Go 内部事务)。
> **关联契约变更**: [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](../../go-contracts/contract-change/2026-05-29-credit-lease.md)

## 1. 动机

现有防透支机制是**软防**:Rust `request_filter` 在请求开始时读 `counter:user:<uid>:balance_micro` 快照、本地 `frozen_micro` 累加估算冻结、释放在请求结束时进行,真扣账由 Go 在收到 `usage:records` 后异步 `INCRBY deducted_micro`。

软防的问题:
- 多 Pod 同时读到同一个余额快照,各自冻结估算,**总和可能超额**。
- Go 结算异步,从"用户余额耗尽"到"所有 Pod 都看到"之间存在窗口,期间打入的请求继续放行。
- 无界:理论透支额度 = 窗口期内全部进入网关的合法请求量。

业务诉求:对**低余额用户**提供一道硬上界,使任意时刻"已发出可消费授权"不超过实际余额(允许极小可配冗余)。本 spec 即此机制。

## 2. 核心理念:credits 不是钱

**Rust 网关不感知货币、价格、汇率。** Rust 持有的是一个无量纲整数 `credits`(i64),Go 负责把"美元/人民币余额 + 各模型每 token 的价格"换算成"该用户当前 Pod 可消费的 credits 上限"并下发。

| 谁知道什么 | |
|---|---|
| **Go** | USD/CNY/汇率/价格表/`balance_micro`/低余额阈值/overcommit 策略 |
| **Rust** | `credits`(i64) / 模型 → 每 token 多少 credits 的系数表 / lease 状态机 |

这条边界是本 spec 不可妥协的不变量,违反任何一处都意味着重新打开"Rust 算钱"的口子,与现有 `no_cost_micro` lint 直接冲突。

## 3. 双模式

| 模式 | 触发 | 防护 | 路径 |
|---|---|---|---|
| `unprotected`(默认) | 用户余额 ≥ Go 配置阈值 | 现有 `frozen_micro` 软防 | 完全保留现有 `request_filter` 第 7-10 步 |
| `protected` | 用户余额 < Go 配置阈值 | 本地 `LeasePool` + lease TTL fail-closed | `request_filter` 增加分支 |

模式切换由 **Go 单方决定**,通过 `events:counter` 推流通知 Rust。Rust 不主动判定阈值——Rust 不知道阈值是 $5 还是 ¥20。

切换是 per-user、可双向(`unprotected → protected → unprotected`)的:用户充值后 Go 推 `mode=unprotected` 即恢复软防路径。

## 4. 安全不变量

**Go 端硬约束**(每次推 lease 前必须满足):

```
Σ_pod outstanding[user][pod] ≤ real_balance(user) × (1 + overcommit_ratio)
```

其中:
- `outstanding[user][pod]` = Go 已 push 给该 Pod 的 credits − 已通过 `usage:records` 兑现的 credits
- `overcommit_ratio` ∈ [0.0, 0.2],Go 配置项,默认 `0.0`(严格永不透支)。允许小冗余以缓解网络抖动 / 推送延迟导致用户在保护模式下被误杀

冗余推送的设计意图:在 `overcommit_ratio = 0` 时极少数请求可能因 lease 推送延迟而被错误拒绝。运维可调到 `0.05` 或 `0.1`,牺牲极小有界透支风险换 UX。透支上限恒为 `real_balance × overcommit_ratio`,远胜软防的无界。

## 5. 数据流

### 5.1 Go → Rust(两条独立 ns,均复用现有 `events:counter` Stream)

**5.1.1 模式切换 + lease 推送**:`ns=protected_lease`

```
XADD events:counter * 
  ns protected_lease 
  user_id <uid>
  mode <"unprotected" | "protected">
  lease_credits <i64>             # 仅 protected 模式语义上有意义
  lease_id <ULID>                 # 用于 Rust 端幂等覆盖
  expires_at_ms <i64>             # 绝对时间戳,Go 决定 TTL
  coef_table_version <u64>        # 引用当前生效的系数表版本
  ts_ms <now_ms>
```

Rust 收到后写入 `LeasePool`(per-user),覆盖语义:同 `user_id` 的最新一条为准,`lease_id` 仅作日志/调试,不参与冲突解决(Stream 顺序即冲突解决)。

**5.1.2 系数表推送**:`ns=coef_table`,**仅版本变更时推**

```
XADD events:counter * 
  ns coef_table
  coef_table_version <u64>
  model_id <string>
  dim <"input" | "output" | "cache_read" | "cache_creation" | "thinking" | "multimodal_input">
  coef_per_token <u64>            # 纯系数,无单位,credits / token
  ts_ms <now_ms>
```

Rust 端 `ModelCoefCache` 按 `coef_table_version` 分桶,新版本完整接收完成(`coef_table_commit` 触达,见 §5.1.3)前不切生效指针。

**lease 与系数表的版本对齐规则**:

- `LeaseState.coef_table_version` 字段是 lease 携带的版本号,**该 lease 在 settle 时一定按这个版本反算 `credits_consumed`**,与 Rust 当前生效版本无关。这保证 Go 推 lease 时与 Rust 上报 `credits_consumed` 用同一系数,outstanding 不会因版本错位而漂移。
- Rust 收到 lease 推送时,若 `lease.coef_table_version` 大于已 commit 的最高版本,Rust 接受 lease,但**预扣公式**(§6.3)在 acquire 时如果发现 staging 中已收到该版本足够 entry,可以使用 staging 版本计算预扣;否则降级用当前生效版本计算预扣(预扣只是放行决策,settle 才是兑现)。
- 若 `lease.coef_table_version` 小于当前生效版本,**仍然接受 lease**(Go 可能在新版本 commit 后还未来得及重推 lease),按 lease 携带的旧版本反算 settle。Rust 不丢弃任何 lease。

**5.1.3 系数表 commit**:`ns=coef_table_commit`

```
XADD events:counter *
  ns coef_table_commit
  coef_table_version <u64>
  model_count <u32>               # Rust 用于校验本版本是否完整接收
  ts_ms <now_ms>
```

收到 commit 且本地累计的该版本 entry 数 ≥ `model_count` 时,Rust 原子切换 `ModelCoefCache` 当前指针到新版本(RCU 风格),旧版本保留至无引用后释放。entry 数不足则记录 `gw_credit_coef_table_incomplete_total{version}` 并保持旧版本生效。

### 5.2 Rust → Go(完全复用现有路径)

`usage:records` 不变。Go 在消费 `usage:records` 时:
1. 算出真实 cost(USD micro,Go 内部事务)→ `INCRBY counter:user:<uid>:deducted_micro`(已有)
2. **新增**:用 `coef_table_version_at_request_time` × `usage` 反算消费的 credits → 减去对应 `outstanding[user][pod_id]`(见 §5.3)

为支持 (2),`UsageRecord` 新增可选字段(见 §10.1)。

### 5.3 Pod ID 来源

Rust 在每条 `UsageRecord` 上带 `pod_id`(等同于 `instance_id`,详见 [`docs/superpowers/specs/2026-05-24-billing-robustness-design.md`](2026-05-24-billing-robustness-design.md) §5.4.1 的 L0/L1/L2 三级解析)。Go 据此区分 outstanding 归属哪一个 Pod。

## 6. Rust 端组件

### 6.1 新增模块:`gw-ledger::lease`

**`LeasePool`** — per-user 状态:
```rust
pub struct LeaseState {
    credits: AtomicI64,           // 当前剩余 credits
    expires_at_ms: AtomicU64,
    lease_id: ArcSwap<String>,    // 仅日志
    mode: AtomicU8,               // Mode::Unprotected | Mode::Protected
    coef_table_version: AtomicU64,
}
pub static LEASE_POOL: Lazy<DashMap<UserId, Arc<LeaseState>>>;
```

**`LeaseGuard`** — RAII 风格,类比现有 `RequestFreeze`:
```rust
pub struct LeaseGuard {
    uid: UserId,
    reserved: i64,
    released: AtomicBool,
    state: Arc<LeaseState>,
}
impl LeaseGuard {
    pub fn try_acquire(uid: &UserId, credits: i64) -> Result<Self, GwError>;
    pub fn settle_actual(&self, actual_credits: i64);  // 流结束按真实 usage 补还
    pub fn release(&self);                              // idempotent
}
```

`try_acquire` 内部 CAS 减 `LeaseState.credits`,失败返回 `GW_QUOTA_LEASE_INSUFFICIENT`。`settle_actual` 与 `release` 的语义详见 §6.3 末段——`settle_actual` 在 actual ≤ reserved 时退还差额,actual > reserved 时不补扣;`release` 是 idempotent 的最终释放,只用 CAS `released` 标志保证只跑一次。

### 6.2 新增模块:`gw-ledger::coef`

**`ModelCoefCache`** — RCU 双 buffer:
```rust
pub struct CoefSnapshot {
    version: u64,
    by_model: HashMap<String, ModelCoefs>,
}
pub struct ModelCoefs {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
    thinking: u64,
    multimodal_input: u64,
}
pub static COEF_CACHE: Lazy<ArcSwap<CoefSnapshot>>;
pub static COEF_STAGING: Lazy<DashMap<u64, StagingState>>;  // 未 commit 的版本累积区
```

启动时 `COEF_CACHE` 指向 `version = 0` 的空 snapshot,此时 `try_acquire` 计算 credits 失败回退到 §7 fail-closed 行为。

### 6.3 新增模块:`gw-ledger::credit_estimator`

```rust
pub fn estimate_request_credits(
    coef_version: u64,            // lease.coef_table_version,settle 也用同一版本
    model_id: &str,
    est_input_tokens: u32,
    max_output_tokens: u32,
) -> Result<i64, EstimatorError>;

pub fn actual_usage_credits(
    coef_version: u64,            // 必须传入与 estimate 相同的 version
    model_id: &str,
    usage: &Usage,                // 6 维真实用量
) -> Result<i64, EstimatorError>;
```

**预扣公式**(只押 input + output 两维,因为流式输出未知,其他维度通常较小且仅在请求结束才能确定):
```
reserved_credits = est_input_tokens × coefs.input
                 + max_output_tokens × coefs.output
```

**真实兑现公式**(6 维全部计入):
```
actual_credits = Σ_dim usage.tokens[dim] × coefs[dim]
```

`max_output_tokens` 来源:
1. 请求显式 `max_tokens` 字段
2. 否则取**模型默认 max**(从 entity 配置读,Go 已下发)
3. 都没有 → 取一个保守上限常数(`DEFAULT_OUTPUT_CEILING_TOKENS`,默认 8192,可配),记录 `gw_credit_default_ceiling_used_total`

**两个公式都按 lease 携带的 `coef_version` 取系数**——这是 §5.1.2 版本对齐规则在估算层的体现。设计后果:对一次请求,reserved 和 actual 用同一系数表,Go 收到 `credits_consumed` 减 outstanding 时也用这个版本对应的"曾下发 credits",版本错位不会让 outstanding 失衡。

实际兑现可能高于预扣(`actual > reserved`):
- thinking / cache_creation / multimodal_input 这些维度未进入预扣公式,真实出现时会让 actual 抬高
- 实际 output tokens 罕见地略超 max_tokens(provider 边界)

`LeaseGuard.settle_actual(actual)` 实现:
```
if actual <= reserved:
    credits.fetch_add(reserved - actual);  // 退还多扣的部分
else:
    // 不再扣减(reserved 已经在 acquire 时扣了)
    metric: gw_credit_actual_exceeds_reserved_total{model_id}
```

`actual > reserved` 不让请求失败,只是放过这次"小幅透支"——本质上预扣在保护"突然爆量",对小幅误差容忍是合理的。但这部分误差**仍然会通过 `usage:records.credits_consumed` 上报给 Go**,Go 减对应 outstanding 时按真实 actual 减,不按 reserved 减。Go 端 outstanding 会因此短暂出现负值,clamp 到 0(详见 contract-change §3.3)。

### 6.4 集成到 `request_filter` / `response_body_filter` / `logging`

新增决策点(伪代码):

```rust
// request_filter step 7 (在余额闸之后,日额度闸之前)
let lease_state = LEASE_POOL.get(&uid);
match lease_state.as_deref().map(|s| s.mode.load(Acquire)) {
    Some(Mode::Protected) => {
        // 进入硬保护路径
        if lease_expired(state) {
            return error(GW_QUOTA_LEASE_EXPIRED);
        }
        let credits = credit_estimator::estimate_request_credits(...)?;
        let guard = LeaseGuard::try_acquire(&uid, credits)?;
        ctx.lease_guard = Some(guard);
        // 软防 frozen_micro 路径不再执行
    }
    _ => {
        // 现有 frozen_micro 路径(完全不变)
    }
}
```

`response_body_filter` 在累积出真实 usage 后,若处于 protected 模式:
```rust
if let Some(guard) = ctx.lease_guard.as_ref() {
    let coef_version = guard.coef_version();  // lease 携带,settle 用同一版本
    let actual = credit_estimator::actual_usage_credits(coef_version, &model_id, &final_usage)?;
    guard.settle_actual(actual);
    ctx.credits_consumed = Some(actual);
    ctx.coef_table_version_at_settle = Some(coef_version);
}
```

`ctx.credits_consumed` 与 `ctx.coef_table_version_at_settle` 由 `billing_lifecycle` 注入到 `UsageRecord`(见 §10.2)。

`logging` 阶段 `guard.release()`(idempotent,与软防 `freeze_guard.release()` 同位置)。

## 7. lease TTL 与 fail-closed

**TTL 在协议负载内**(`expires_at_ms`),由 Go 决定。Rust 的唯一参与是:

```
now_ms() > expires_at_ms + lease_grace_ms  →  GW_QUOTA_LEASE_EXPIRED
```

`lease_grace_ms` 是 Rust 配置项(默认 5_000),仅用于容忍时钟漂移,不参与 TTL 长度协商。

Go 应该以 ≤ TTL/2 的间隔重推同 user 的 lease(典型:TTL = 5 min,推送间隔 1-2 min),确保任意时刻有效 lease 存在。Rust 不感知 Go 的推送间隔。

### 7.1 极端 case:Go 推流彻底中断

所有处于 protected 模式的用户在 `lease_grace_ms` 后开始全部命中 `GW_QUOTA_LEASE_EXPIRED`,业务失败。这是**主动设计选择**:fail-closed 优先于"误放任何透支请求"。运维侧通过 `gw_credit_lease_expired_total` 告警发现,处置路径:
- 修复 Go 端推流
- 或运维显式把 `[protection].enabled = false` 关掉这一层(Pod 滚动重启,需热更则后续优化)

### 7.2 极端 case:Pod 启动后还没收到任何 lease

Pod 启动 → 收到第一条 `usage:records` → ... 但 Pod 还没收到任何 `protected_lease` 事件。此时该用户的 `LeasePool` 没有 entry。

Rust 在 `request_filter` 看到"用户来了请求但没有 LeasePool entry"时:
- 默认走 `unprotected` 路径(软防)。**前提**是 Pod 启动时已从 `events:counter` last-id 重读至少一次完整 stream tail(由现有 StreamConsumer 启动逻辑保证)。
- 重读完成前,记录 `gw_credit_lease_warmup_pending_total`,继续走软防。

这意味着:**Pod 启动到 stream catchup 完成的窗口内,新进入的低余额用户只受软防保护,不受硬保护**。这是可接受的 trade-off,因为窗口是秒级,且 catchup 后立即转为硬保护。

## 8. Pod 退出 / 崩溃

Pod 收到 SIGTERM 或意外崩溃:
- 本地 `LeasePool` 中未消耗的 credits **不需要返还**——credits 不是钱,是授权阈值。
- Go 端 `outstanding[user][pod_id]` 在 lease TTL 过期后自动 GC(Go 责任,见 contract-change)。
- 该 Pod 重启后用同一 `instance_id`(L0 env 或 L2 Redis 持久键)恢复,但**不**继承崩溃前的 LeasePool——状态从 stream 重读+下次 Go 推送恢复。

## 9. 配置(Rust 侧)

`config/default.toml` 新增 `[protection]`,**所有字段都可被环境变量 `GW_PROTECTION__*` 覆盖**:

```toml
[protection]
enabled = false                  # 总开关,kill switch;关闭时全走软防,Rust 不消费 protected_lease ns
lease_grace_ms = 5_000           # 时钟漂移容忍窗口
default_output_ceiling_tokens = 8192  # 请求未指定 max_tokens 且模型默认 max 缺失时的兜底上限
```

**只有这三项**。lease TTL、阈值、overcommit 比例、推送间隔全部是 Go 配置项,Rust 不感知。这是 §2 边界的直接体现。

## 10. 跨进程契约改动总览

详见 [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](../../go-contracts/contract-change/2026-05-29-credit-lease.md)。本节仅列要点。

### 10.1 新增 `events:counter` ns

| ns | 方向 | 触发 | Rust 订阅点 |
|---|---|---|---|
| `protected_lease` | Go → Rust | 模式切换 / 续约 | `gw-ledger::lease` consumer |
| `coef_table` | Go → Rust | 价格表版本变更(仅变更时) | `gw-ledger::coef` consumer |
| `coef_table_commit` | Go → Rust | 价格表新版本完整推送结束 | 同上 |

### 10.2 `usage:records` 新增字段

```json
{
  "pod_id": "<instance_id>",       // 必填,Rust 已有 L0/L1/L2 instance_id 直接复用
  "credits_consumed": <i64>,       // 可选,protected 模式下 Rust 计算的实际兑现 credits
  "coef_table_version": <u64>      // 可选,protected 模式下 Rust 兑现时使用的系数表版本
}
```

后两者在 `unprotected` 模式下省略。Go 据 `pod_id` + `credits_consumed` 减 outstanding;若 `credits_consumed` 缺失或为 0,Go 仅做计费,outstanding 不动(说明该请求走的是软防路径)。

### 10.3 不破坏的现有契约

- `counter:user:<uid>:balance_micro` / `deducted_micro` 命名空间不变,Go 仍是唯一 writer。
- `frozen_micro` 命名空间不变,继续服务于软防路径。
- `usage:records` 既有字段全部保留,新增字段都是 additive + omitempty。
- 现有 `events:counter` 已订阅的 ns 不变,新增 ns 走同一 StreamConsumer。

## 11. 错误码

新增,登记到 `crates/gw-core/src/error/codes.rs`:

| Code | HTTP | 触发 | 用户可见消息 |
|---|---|---|---|
| `GW_QUOTA_LEASE_INSUFFICIENT` | 402 | protected 模式下 lease credits 不足以覆盖本次请求预扣 | "余额不足以发起此请求" |
| `GW_QUOTA_LEASE_EXPIRED` | 503 | protected 模式且 `now > expires_at_ms + lease_grace_ms` | 通用错误(运维信号,非用户错误) |

### 11.1 选用 402 vs 复用现有 `GW_QUOTA_BALANCE_ZERO`

`GW_QUOTA_BALANCE_ZERO` 语义是"余额已彻底归零",与 lease 不足不等价(用户余额可能 > 0 但当前 Pod 这一份 lease 已耗尽)。**用新错误码** 让用户/前端可区分两种状态:`GW_QUOTA_BALANCE_ZERO` 提示充值,`GW_QUOTA_LEASE_INSUFFICIENT` 提示稍后重试(系统可在下一次 Go 推送后恢复)。

## 12. 可观测性

新增 metrics(全部 `gw_credit_*` 前缀):

| Metric | 类型 | 标签 | 含义 |
|---|---|---|---|
| `gw_credit_lease_active_users` | Gauge | (none) | 当前 protected 模式用户数 |
| `gw_credit_lease_credits_remaining` | Histogram | `user_bucket` | lease 剩余 credits 分布(bucket 分组以避免高基数) |
| `gw_credit_lease_acquire_total` | Counter | `result=ok\|insufficient` | LeaseGuard 获取尝试 |
| `gw_credit_lease_expired_total` | Counter | (none) | 因 TTL 过期被拒的请求数 |
| `gw_credit_actual_exceeds_reserved_total` | Counter | `model_id` | 实际用量超出预扣的次数(预扣公式偏差信号) |
| `gw_credit_coef_table_version_active` | Gauge | (none) | 当前生效系数表版本 |
| `gw_credit_coef_table_incomplete_total` | Counter | `version` | 系数表版本 commit 后 entry 不全的次数 |
| `gw_credit_default_ceiling_used_total` | Counter | `model_id` | 兜底 output ceiling 触发次数 |
| `gw_credit_lease_warmup_pending_total` | Counter | (none) | Pod 启动 stream catchup 期间走软防的请求数 |

Admin 端点新增(token 鉴权):

- `GET /healthz/credit/lease/<user_id>` — 当前 LeasePool 状态
- `GET /healthz/credit/coef-table` — 当前生效系数表 version + 入口 model 数

## 13. 部署 / 灰度策略

1. **第一阶段**:Rust 侧上线全部代码,但 `[protection].enabled = false`。Go 侧实现 outstanding ledger + 推流逻辑,但仅在 staging 环境推 `protected_lease` 事件验证 Rust 消费。
2. **第二阶段**:staging 打开 `enabled = true`,跑 e2e-soak 验证三种 fail-closed 路径(lease 过期、credits 不足、Pod 重启窗口)。
3. **第三阶段**:生产开启 `enabled = true`,Go 端先用 `overcommit_ratio = 0.1` 软启动,观察 `gw_credit_lease_expired_total` 与误杀率,稳定后逐步降到 `0.0`。

## 14. 已知 trade-off

- **Pod 启动窗口空隙**(§7.2):catchup 期间软防兜底,有秒级真空。可接受。
- **预扣偏差**(§6.3):max_tokens 上界把利用率压低;对低余额用户而言可接受(他们本来就接近余额尽头)。
- **系数表抖动**:Go 频繁版本切换会让 RCU 切换产生短暂多版本共存,内存占用上升。Go 端责任是版本变更**仅在价格真变时推**,本 spec 不限制频率。

## 15. 不做的事

- 不实现"Rust 主动拉 lease"(违背 Go 主动推决策)。
- 不实现"per-user 阈值"(全局单一阈值已够用,后续按需独立 spec)。
- 不实现 lease 跨 Pod 迁移(Pod 间不共享 lease,均分 + outstanding ledger 兜底)。
- 不实现 `frozen_micro` 路径的废弃(高余额用户仍走软防)。
- **不**让 Rust 知道任何货币符号、汇率、价格表(刚性边界)。

## 16. 验证

- 单元:`gw-ledger::lease`、`coef`、`credit_estimator` 各自的 CAS 边界、版本 RCU、TTL 判定。
- 集成:`tests/integration/credit_lease_*.rs`,模拟 Go 推流时序、TTL 过期、Pod 重启 catchup。
- 端到端:在 `tests/e2e/client/soak/` 加 `--protected-mode` 子模式,跑 50k 请求验证"任意时刻 `usage:records.credits_consumed` 累计 ≤ 总下发 credits + overcommit"。
- 不变量:`scripts/check-invariants.sh` 增加 `no_currency_in_lease`(grep `usd|cny|micro|cost` 在 `gw-ledger/src/lease.rs` / `coef.rs` / `credit_estimator.rs` 内禁止出现)。

