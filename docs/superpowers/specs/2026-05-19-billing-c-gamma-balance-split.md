# C-γ — 余额三层读写分工（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 6 B12）
  - [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md)（UsageRecord 是 Go 算 cost 的来源）
  - [2026-05-19-billing-c-beta-estimate-pricing.md](./2026-05-19-billing-c-beta-estimate-pricing.md)（freeze 估算来源）
  - [2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)（ADR-004 + ADR-015 三层余额）
- **范围**：
  - `gw-store::deduction::DeductionState` 三层 (`base_micro / deducted_micro / frozen_micro`) 的写权语义变更：
    - **Rust 唯一写 `frozen_micro`**（HINCRBY +est on freeze, HINCRBY -est on release）；
    - **Go 唯一写 `base_micro` / `deducted_micro`**（充值时改 base，settle 时改 deducted）；
    - Rust 通过 `events:counter` 订阅 Go 的写入，失效本地缓存；
  - `FreezeGuard` API 改造：`settle(actual)` → `release()`，不再扣 deducted；
  - `gw-store::counter_store` 加 `events:counter` consumer，把 base / deducted 当只读缓存维护。
- **不在范围**：
  - cost 计算公式（→ Go 侧 + C-ε 文档）；
  - UsageRecord schema（→ C-α）；
  - DLQ（→ C-δ）。

> 用户决议：**Go 是 base/deducted 的唯一 writer，Rust 只读**。本 spec 把读写分工固化，并改 freeze 语义。

---

## 1. 背景

当前 `gw-store::deduction::DeductionState`（参 `crates/gw-store/src/deduction.rs`）：

```rust
pub struct DeductionState {
    pub base_micro: AtomicI64,
    pub deducted_micro: AtomicI64,
    pub frozen_micro: AtomicI64,
}

pub fn available(&self) -> i64 {
    self.base_micro.load(Relaxed)
        - self.deducted_micro.load(Relaxed)
        - self.frozen_micro.load(Relaxed)
}
```

`FreezeGuard::settle(actual)`（参 `gw-ledger::freeze`）做两件事：
1. `frozen_micro -= freeze_amount`（释放冻结）；
2. `deducted_micro += actual`（扣实际花费）。

第 2 件事**与新边界冲突**——deducted 应由 Go 写。

`gw-ledger::flusher` 同时做 `INCRBY counter:user:{uid}:balance_micro -cost_micro`，也是 Rust 写余额的痕迹，必须撤掉。

`gw-store::stream_consumer` 当前监听 `events:entity`；需要扩展监听 `events:counter` 以同步 Go 写入的 base/deducted。

## 2. 设计决策

### ADR-Cγ-1 三层余额读写分工

| 字段 | Rust 读 | Rust 写 | Go 读 | Go 写 |
|---|---|---|---|---|
| `base_micro` | ✓ | ✗ | ✓ | ✓ |
| `deducted_micro` | ✓ | ✗ | ✓ | ✓ |
| `frozen_micro` | ✓ | ✓ | ✓ | ✗ |

`available() = base - deducted - frozen` 在 Rust 端的语义保持，但 base/deducted 是"已同步过来的快照"，不是 Rust 写入的实时值。

代价：

- Go settle 完成到 Rust 看到 deducted 增长之间有窗口（典型几十 ms 到几秒，取决于 events:counter 传播 + Rust 缓存失效速度）；
- 这个窗口内同一用户的并发请求看到的 `available` 偏高（Go 还没扣完）；
- freeze 机制天然保护：上一个请求的 freeze 还在 `frozen_micro` 里 → `available = base - deducted(老) - frozen(新)` 仍然准确。所以这个窗口实际上**不导致透支**——freeze 把窗口期的并发请求挡住了。

### ADR-Cγ-2 FreezeGuard API 改造

```rust
// gw-ledger::freeze::trait_def

pub enum FreezeKind { Request, Task }

pub trait FreezeGuard: Send + Sync {
    fn kind(&self) -> FreezeKind;
    fn freeze_amount(&self) -> u64;
    /// 释放冻结——不再扣 deducted。
    /// 幂等：多次调用安全。Drop 也调用此方法（RAII）。
    fn release(&self);
}
```

旧 `FreezeGuard::settle(actual_micro)` 撤销。所有调用方（`gw-proxy::filters::logging`）改调 `release()`。

`RequestFreeze`（refactor plan 里规划的 trait 实现）的内部逻辑变化：

```rust
impl FreezeGuard for RequestFreeze {
    fn release(&self) {
        if self.released.compare_exchange(false, true, AcqRel, Acquire).is_err() {
            return;  // 已释放
        }
        self.state.frozen_micro.fetch_sub(self.freeze_amount as i64, AcqRel);
        // 注意：不再有 deducted_micro.fetch_add(actual, AcqRel)
    }
}

impl Drop for RequestFreeze {
    fn drop(&mut self) {
        if !self.released.load(Acquire) {
            self.release();
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}
```

### ADR-Cγ-3 frozen 同步到 Redis

当前 `frozen_micro` 只在 Rust 进程内（AtomicI64）。需要让 Go 也能看到：

```
Redis counter:
  counter:user:<uid>:frozen_micro     (HINCRBY by Rust on freeze +/-)
```

Rust freeze 时同时做两件事：
1. 本地 `state.frozen_micro.fetch_add(amount)`；
2. 异步 `HINCRBY counter:user:<uid>:frozen_micro +amount`（mpsc 队列；与 UsageRecord flush 同款）。

Rust release 时同样两步：
1. 本地 fetch_sub；
2. 异步 HINCRBY frozen -amount。

Go 端读 frozen 时直接 GET Redis；Rust 进程崩溃丢的本地 frozen 由 Redis 兜底（启动时从 Redis sync 回本地）。

**写顺序**：Rust 先写本地（请求路径 immediate）再异步写 Redis（事后 ~几十 ms 上窜）。本地一定先有；Redis 滞后可接受。

### ADR-Cγ-4 events:counter 订阅

`gw-store::stream_consumer` 当前只读 `events:entity`，加 `events:counter` 第二条 stream：

```rust
// events:counter payload 示例
{
  "ns": "user_balance",
  "uid": "u_xxx",
  "op": "incrby",
  "field": "deducted_micro",
  "delta": 1234,        // 可以为负
  "value_after": 9876,  // Go 写完后的当前值（让 Rust 直接覆盖本地缓存）
  "ts_ms": ...
}
```

Rust consumer 收到后：

1. 找到对应的 `DeductionState`（懒加载或现有缓存）；
2. **以 value_after 覆盖**（不用 delta 累加，避免与本地 frozen 写顺序竞争）；
3. 写入对应 atomic 字段（`deducted_micro` 或 `base_micro`，**不**写 frozen——frozen 是 Rust 自己写）。

为何用 value_after 而不是 delta：

- Go 是唯一 writer for base/deducted，value_after 是权威值；
- delta 累加在网络重传 / 跳跃场景下可能错乱；
- value_after 覆盖天然幂等。

### ADR-Cγ-5 启动期对齐

Rust 启动后第一次访问某 user 时：

1. 懒加载 `DeductionState`（与现行 ADR-005 全懒加载一致）；
2. `HGET counter:user:<uid>:base_micro` + `deducted_micro` + `frozen_micro` 取当前值；
3. 装填到本地 atomic；
4. 之后通过 events:counter 增量更新 base/deducted；frozen 由 Rust 自管 + 异步同步到 Redis。

### ADR-Cγ-6 `gw-ledger::flusher` 改造

撤销 `flusher::flush` 里的 `INCRBY counter:user:{uid}:balance_micro -cost_micro`（参 `crates/gw-ledger/src/flusher.rs:5-6`）。

留下的只剩：
- `XADD usage:records * payload <json>`（参 C-α）。

flusher 不再负责余额扣减。balance 扣减由 Go 在消费 usage:records 时做。

### ADR-Cγ-7 frozen 流量

请求路径上的 frozen 写：

```
Request arrives:
  1. compute estimate (C-β)
  2. local: state.frozen_micro.fetch_add(estimate)  // 本地实时
  3. send to mpsc: HINCRBY frozen_micro +estimate   // 异步刷 Redis
  4. check: base - deducted - frozen ≥ 0  → 放行/402
  5. freeze guard registered

Response done:
  1. local: state.frozen_micro.fetch_sub(estimate)
  2. send to mpsc: HINCRBY frozen_micro -estimate
  3. release guard
```

mpsc 队列：复用 `gw-ledger::Ledger` 但加 enum：

```rust
enum LedgerMsg {
    Usage(UsageRecord),
    FrozenDelta { uid: UserId, delta: i64 },
}
```

flusher 内部 batch 处理两类 msg。

### ADR-Cγ-8 性能预算

| 操作 | 预算 |
|---|---|
| `DeductionState::available()` 读 | < 10 ns（三次 atomic load） |
| `RequestFreeze` 构造（freeze） | < 100 ns（本地 atomic）+ async send |
| `RequestFreeze::release()` | < 100 ns（本地 atomic）+ async send |
| events:counter consumer 处理 1 个 msg | < 5 µs |
| Redis HINCRBY frozen | 异步路径，不阻塞请求 |

mpsc 队列若积压（Redis 慢）→ Rust frozen 本地仍准确，但 Redis 滞后；监控 mpsc 长度。

### ADR-Cγ-9 可观测性

新增 metrics：

| Metric | Label | 说明 |
|---|---|---|
| `gw_balance_frozen_micro` | uid（高基数注意） | gauge：当前 frozen 值 |
| `gw_balance_available_micro` | uid | gauge：available = base - deducted - frozen |
| `gw_balance_event_received_total` | field, source | counter：base / deducted 事件接收 |
| `gw_balance_event_lag_ms` | - | histogram：Go 写 → Rust 收到的延迟（用 events 内 ts_ms 算） |
| `gw_balance_frozen_sync_pending` | - | gauge：mpsc 内待 flush 的 frozen delta 数量 |

uid 高基数 → 走 vendor / user_group bucketed 而非 per-uid（参考 K-D 的处理思路）。

### ADR-Cγ-10 失败模式

| 故障 | 后果 | 缓解 |
|---|---|---|
| Redis 不可用 | Rust 写 frozen 失败 → mpsc 队列积压 → 本地 frozen 仍准确；Go 看不到新 freeze | mpsc 满则 backoff，请求路径仍放行（freeze 已本地登记）；告警 |
| events:counter 失联 | Rust 看不到 Go 的 deducted 更新 → balance 偏高 → 用户可能透支 | Stream consumer 重连 + 启动期对齐（§ADR-Cγ-5）；告警 |
| Rust 进程崩溃 | 本地 frozen 丢，Redis 上的 frozen 仍有（因为已异步 flush） | 重启时启动期对齐拉回；崩溃前未来得及 flush 的 frozen delta 丢失（请求计数对得上即可） |
| Go 慢 settle | UsageRecord 积压 in stream；balance 短期偏高；但 frozen 仍挡 | metric `usage_records_consume_lag`；告警 |

## 3. 实施大纲

依赖：C-α（UsageRecord） + C-β（EstimatePricing）。

1. 改 `gw-ledger::freeze`：
   - 删 `FreezeGuard::settle(actual)`；
   - 加 `FreezeGuard::release()`；
   - 改 `RequestFreeze` 内部不再写 deducted；
   - Drop 行为不变；
2. `gw-ledger::flusher` 改造：
   - LedgerMsg 改 enum，含 Usage + FrozenDelta；
   - flush 时 Usage 走 XADD usage:records；FrozenDelta 走 HINCRBY；
   - 撤销原 `INCRBY balance_micro` 路径；
3. `gw-store::deduction::DeductionState` 不变，但语义注释更新（base/deducted 只读）；
4. `gw-store::stream_consumer` 加 events:counter 订阅 + handler；
5. `gw-store::counter_store` 加启动期对齐函数；
6. `gw-proxy::filters::request` 改用 `RequestFreeze::new(uid, estimate)` + 发送 FrozenDelta msg；
7. `gw-proxy::filters::logging` 改用 `guard.release()`；删除 cost 计算（参 C-β §ADR-Cβ-5）；
8. 测试：
   - freeze → release roundtrip，frozen 归零；
   - events:counter handler 用 value_after 覆盖；
   - 启动期对齐：Rust 启动 → 从 Redis 读到现有 base/deducted；
   - mpsc 满时 backoff 行为；
   - Drop 自动 release（panic 路径）。

## 4. 未决问题

1. **frozen 写 Redis 失败的本地 vs Redis 不一致**：本地写入成功 + Redis 写入失败 = Rust 看到 frozen 高 + Go 看到 frozen 低 = Go 可能误以为该用户有 budget 而允许过度 settle。倾向：mpsc flush 失败的 FrozenDelta 进 DLQ（详见 C-δ），重试到底；告警如果累积超 N。
2. **events:counter 重复事件（Go XADD 后重试)**：value_after 覆盖天然幂等，重复无害。但 value_after 必须**单调递增**（Go 不能用旧值覆盖新值）；用 ts_ms 字段排序确保。
3. **events:counter 与 events:entity 是否复用 stream**：当前 ADR-013 已经规划 events:entity；events:counter 应分离 stream 还是同 stream 不同 namespace？倾向**分离 stream**：counter 流量与 entity 流量频率差异大（counter 远高），分流便于 consumer 各自调参。需要在 ADR-013 加 protocol 5（Counter Event）。
4. **frozen 在多副本 Rust 网关下的并发**：副本 A 在 user X 上 +est；副本 B 同时也 +est。两者各自异步 HINCRBY → Redis 上是 +est1+est2。这是正确的（并发请求各自占冻结额）。但 Rust 本地的 frozen 是副本各自的——副本 A 不知道副本 B 的 frozen。所以 `available()` 在副本 A 视角下偏高。**这是 v1 接受的不一致**：依赖 Redis frozen 的全局视图做最终保护——由 Go 在 settle 时做最终 balance check（基于 Redis 全局 frozen），失败时拒绝 settle 走错误回路。
5. **跨副本 frozen 全局视图**：上一条引申——Rust 可以定期 HGETALL `counter:user:<uid>:frozen_micro` 校准本地（自己写的 delta 已经在 Redis 上）。但 v1 不做（接受不一致 + Go 兜底）。
6. **DeductionState dead entries**：DashMap 持每个用户的 state，不删。10000+ 活跃用户没问题；100 万 + 长尾用户内存膨胀。需要 LRU eviction，留 follow-up。
7. **frozen 状态在 Rust 网关重启时的归零策略**：进程重启 = 本地 frozen 归零 + Redis frozen 仍存。如果 Rust 在重启前还有 in-flight 请求未 release frozen，那些 frozen 永远留在 Redis（"幽灵冻结"）。倾向加重启对齐时调一次 `HSET counter:user:<uid>:frozen_micro 0`——但这与多副本冲突（其他副本的 frozen 也被清零）。需要**副本级 frozen 子键**（如 `frozen_micro:<replica_id>`），重启时只清自己的子键。留 follow-up。

---

> C-γ 完成后，Rust 完全脱离余额结算，仅守 freeze 保护门。Go 是 base/deducted 唯一权威。
