# P-F: Reservation 全局视图基础设施

## TL;DR

> **Quick Summary**: 基于 "Local-Read + Async-Aggregate" 模式，复用 RoundRobin 已有的 per-model atomic counter，通过 `events:reservation` Redis stream 异步聚合多实例全局视图。零 hot-path Redis 调用。
>
> **Deliverables**: per-model 频率采样、events:reservation 异步聚合、全局视图 DashMap
> **Estimated Effort**: Medium
> **Depends On**: P-B (scoring pipeline)
> **Blocks**: T15 (adaptive reservation)

---

## Context

### 设计要点

- **Hot path 仅读本地原子计数器** — 复用 `InFlightStore::count`（`AtomicU32::load(Relaxed)`）
- **Per-model 频率窗口 = 复用 RoundRobin 已有的 per-model atomic** (`round_robin.rs:11 counters: DashMap<ModelName, AtomicUsize>`)
  - 该原子已在每次 pick 时 `fetch_add(1, Relaxed)`（`round_robin.rs:48`）
  - 增加采样时间戳即可衍生频率
- **异步聚合到 Redis** — 后台 task 每 5s 把本实例 model→count 增量 XADD 到 `events:reservation`
- **全局视图** — 所有实例订阅同 stream，本地聚合 DashMap

### 性能约束

- Hot path **零新增** — 复用既有 atomic + DashMap::get
- 5 秒级最终一致 — 文档化为显式 v1 语义
- 零 hot-path Redis 调用

---

## TODOs

- [x] 1. Per-model 频率采样基础设施

  **What to do**:
  - 升级 RoundRobin 的 `DashMap<ModelName, AtomicUsize>` 为 `DashMap<ModelName, (AtomicU64 count, AtomicU64 window_start_ms)>`
  - 或新建独立 `ModelFrequencyTracker` 结构复用同一 DashMap
  - 采样逻辑：每 5s 后台 task 读 `(count_now - count_prev) / 5.0` = requests/sec per model
  - Hot path 不变：仍只 `fetch_add(1, Relaxed)`

  **Must NOT do**:
  - 不改 RoundRobin pick 行为
  - 不在 hot path 新增写操作
  - 不引入 sliding window push（每请求）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocks**: Task 2
  - **Blocked By**: P-B 完成

  **References**:
  - `crates/gw-routing/src/round_robin.rs:11` — existing per-model DashMap
  - `crates/gw-routing/src/round_robin.rs:48` — fetch_add(1, Relaxed)

  **Acceptance Criteria**:
  - [ ] 频率采样测试：5s 内 100 次 fetch_add → rate ≈ 20/s
  - [ ] hot path bench 无退化

  **QA Scenarios**:

  ```
  Scenario: 频率计算准确
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing -- test_frequency_sampling
      2. 模拟 5s 内 100 次 pick
      3. 断言 sample_rate() ≈ 20.0 (±1.0)
    Expected Result: 频率准确
    Evidence: .omo/evidence/pf-1-frequency.txt
  ```

  **Commit**: YES (groups with 2, 3)

- [x] 2. events:reservation 异步聚合

  **What to do**:
  - 后台 tokio task 每 5s:
    1. 读本实例所有 model→rate 快照
    2. XADD `events:reservation` `{ instance_id, model, rate, ts }`
  - 同时 XREAD `events:reservation`，聚合其他实例的 rate
  - 本地 `DashMap<ModelName, GlobalModelView>` 存储全局视图
  - Go 侧可选消费 `events:reservation`（在 Go 契约中声明）
  - 5s 级延迟是显式 v1 语义

  **Must NOT do**:
  - 不在 hot path 调用 Redis
  - 不引入 Mutex（DashMap 已足够）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 1

  **References**:
  - 现有 `usage:records` XADD 模式（`crates/gw-ledger/`）
  - 现有 `events:entity` 消费模式

  **Acceptance Criteria**:
  - [ ] XADD 格式正确
  - [ ] 多实例聚合测试（模拟 2 实例 XADD → 读全局视图）
  - [ ] 5s 级延迟测试

  **QA Scenarios**:

  ```
  Scenario: 多实例聚合
    Tool: Bash (cargo test)
    Steps:
      1. 模拟 2 实例各 XADD events:reservation
      2. 聚合后断言 global_view 含两实例数据
      3. 断言 per-model rate 是两实例之和
    Expected Result: 全局视图正确
    Evidence: .omo/evidence/pf-2-aggregation.txt
  ```

  **Commit**: YES (groups with 1, 3)

- [x] 3. 管线集成 + bench

  **What to do**:
  - 在 P-B 的 scoring pipeline 中，reservation 作为 `gate` 阶段的可选 policy
  - `CandidateFeatures` 新增 `reservation_pressure: Option<f64>`（来自全局视图）
  - 当 `reservation_pressure > threshold` → gate 降低该 model 优先级
  - bench: strategy_pick P99 ≤ 基线 +2µs

  **Must NOT do**:
  - 不改管线核心接口
  - 不在管线内调用 Redis

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 2

  **Acceptance Criteria**:
  - [ ] reservation 集成到管线 gate 阶段
  - [ ] bench P99 ≤ 基线 +2µs
  - [ ] 未启用 reservation 时零开销

  **QA Scenarios**:

  ```
  Scenario: bench 无退化
    Tool: Bash
    Steps:
      1. cargo bench -p gw-routing --bench strategy_pick
      2. 断言 P99 ≤ 基线 +2µs
    Expected Result: 零退化
    Evidence: .omo/evidence/pf-3-bench.txt
  ```

  **Commit**: YES (groups with 1, 2)
  - Message: `feat(routing): reservation global view infrastructure [P-F]`
  - Pre-commit: `cargo test --workspace && cargo bench -p gw-routing --bench strategy_pick && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace
cargo bench -p gw-routing --bench strategy_pick  # P99 ≤ +2µs
bash scripts/check-invariants.sh
```
