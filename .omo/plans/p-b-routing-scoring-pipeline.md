# P-B: 路由 Scoring 管线重构

## TL;DR

> **Quick Summary**: 将互斥的 `StrategyKind` enum dispatch 重构为 `filter→extract→gate→score→pick` 管线，使现有 8 策略成为管线预设权重，支撑多维度可组合调度 (T5) 和 scheduling presets (T17)。
>
> **Deliverables**: CandidateFeatures struct、ScoringPolicy trait、管线 executor、8 策略适配器
> **Estimated Effort**: Large
> **Blocks**: T5+T17 (multi-dim + presets), P-F (reservation 全局视图)

---

## Context

### 根因分析

`StrategyKind` 是 `ModelGroup` 上的 **单一 enum 字段**（`entity.rs:233`），`StrategyAny` enum dispatch 选 **恰好一个** 策略（`set.rs:93`）。候选过滤与 scoring/selection **没有分离** — 每个策略内部各自 score。5 个维度数据散落在 `VendorKey`/`KeyHealthStore`/`InFlightStore`/`LatencyTracker`/`PickCtx`，无统一的 `CandidateFeatures` 结构。

### 根治路径

引入显式管线: `filter(health/capability) → extract(features) → gate(hard limits) → score(weighted dims) → pick(tiebreak)`。现有 8 策略表达为该管线的预设权重/policy。新增 MultiDim 不再是第 9 个单体策略，而是自定义权重组合。

---

## Work Objectives

### Must Have
- `CandidateFeatures` struct：per-candidate 维度数据快照
- `ScoringPolicy` trait：定义评分权重组合
- 管线 executor：filter→extract→gate→score→pick 5 阶段
- 8 现有策略适配为 ScoringPolicy 预设
- 每策略 bench 不退化
- `snapshot(&self, key_ids)` 批量 API（一次 DashMap 遍历）

### Must NOT Have
- 不改 `Strategy` trait 公开接口（现有消费者不感知）
- 不改 `PickCtx` 生命周期
- 不引入新 Cargo 依赖
- 不碰 response/billing 路径
- 不在管线中引入 sync Redis 调用

---

## Verification Strategy

- **Bench gate**: `cargo bench -p gw-routing --bench strategy_pick` per-strategy P99 ≤ 100µs
- **Tests**: 每策略一个"管线模式 = 旧模式结果"对比测试

---

## Execution Strategy

```
Wave 1 [serial, deep]:
├── 1. CandidateFeatures + ScoringPolicy trait
├── 2. 管线 executor (filter→extract→gate→score→pick)
├── 3. 8 策略适配 + snapshot 批量 API
└── 4. Bench + 回归测试
```

---

## TODOs

- [x] 1. CandidateFeatures struct + ScoringPolicy trait

  **What to do**:
  - 新建 `crates/gw-routing/src/pipeline/` 模块
  - 定义 `CandidateFeatures`:
    ```rust
    struct CandidateFeatures {
        key_id: KeyId,
        health_score: f64,        // from KeyHealthStore
        latency_p50: Option<f64>, // from LatencyTracker
        in_flight: u32,           // from InFlightStore
        cost_weight: f64,         // from VendorKey.pricing
        error_rate_30s: f64,      // from LatencyTracker
        // extensible: 后续 P-F 可追加 reservation_count
    }
    ```
  - 定义 `ScoringPolicy` trait:
    ```rust
    trait ScoringPolicy: Send + Sync {
        fn needed_dims(&self) -> DimMask;  // bitmask: 按需查询
        fn score(&self, features: &CandidateFeatures) -> f64;
        fn tiebreak(&self) -> TiebreakStrategy; // Random / FixedOrder / RoundRobin
    }
    ```
  - 定义 `DimMask` bitmask（FixedOrder/RoundRobin 不需要 latency/cost dim，短路跳过）

  **Must NOT do**:
  - 不改 `Strategy` trait 签名
  - 不在此 task 实现具体策略适配

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Serial
  - **Blocks**: Task 2
  - **Blocked By**: None

  **References**:
  - `crates/gw-routing/src/set.rs:93` — StrategyAny enum dispatch
  - `crates/gw-routing/src/load_balance.rs:32` — 现有 Vec alloc + sort 模式
  - `crates/gw-routing/src/cost_first.rs:25-33` — scored.sort_by 模式
  - `crates/gw-health/src/format_health.rs:66-89` — is_available 无锁检查
  - `crates/gw-health/src/in_flight.rs:76,83` — relaxed atomic
  - `crates/gw-routing/src/latency.rs:111-127` — latency p50 + error_rate

  **Acceptance Criteria**:
  - [ ] `CandidateFeatures` 可 construct/access 测试
  - [ ] `ScoringPolicy` trait 可 impl 测试
  - [ ] `DimMask` bitmask 短路正确性测试

  **QA Scenarios**:

  ```
  Scenario: DimMask 短路
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing -- test_dim_mask_shortcircuit
      2. FixedOrder 策略 DimMask 仅含 HEALTH
      3. 断言 extract 阶段跳过 LATENCY/COST/INFLIGHT
    Expected Result: 按需查询，不查不需要的维度
    Evidence: .omo/evidence/pb-1-dim-mask.txt
  ```

  **Commit**: YES (groups with 2, 3, 4)
  - Message: `refactor(routing): add CandidateFeatures + ScoringPolicy [P-B]`

- [x] 2. 管线 Executor

  **What to do**:
  - 在 `crates/gw-routing/src/pipeline/executor.rs` 实现 5 阶段管线:
    1. **filter**: 健康/能力过滤（复用现有 `candidates.rs` 逻辑）
    2. **extract**: 按 `needed_dims()` 收集 features（使用 snapshot 批量 API）
    3. **gate**: 硬限制检查（in-flight cap, cooldown）
    4. **score**: `policy.score(features)` 评分
    5. **pick**: 最高分 + tiebreak
  - 管线作为 `StrategyAny::pick` 的内部实现，不改外部接口
  - `snapshot()` 批量 API: 一次 DashMap 遍历收集多 key 的 features

  **Must NOT do**:
  - 不改 `Strategy` trait 外部接口
  - 不在管线中引入 async/await（pick 必须 sync）
  - 不引入 hot-path Mutex（复用现有 DashMap + atomic）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocks**: Task 3
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-routing/src/candidates.rs:19` — 现有候选过滤
  - `crates/gw-health/src/format_health.rs` — batch health check 可能
  - `crates/gw-health/src/in_flight.rs` — per-key atomic load

  **Acceptance Criteria**:
  - [ ] 管线 5 阶段按序执行测试
  - [ ] filter 后无候选时 → 返回 NoCandidates 错误
  - [ ] gate 后全被拦截 → 返回 AllGated 错误

  **QA Scenarios**:

  ```
  Scenario: 管线端到端
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing -- test_pipeline_e2e
      2. 构造 5 candidates + LatencyFirst policy
      3. 断言选中 latency 最低的 key
    Expected Result: 管线正确选择
    Evidence: .omo/evidence/pb-2-pipeline-e2e.txt
  ```

  **Commit**: YES (groups with 1, 3, 4)

- [x] 3. 8 策略适配 + snapshot API

  **What to do**:
  - 为 FixedOrder, RoundRobin, LoadBalance, WeightedRr, CostFirst, LatencyFirst, CacheAffinity, Sticky 各 impl `ScoringPolicy`
  - 每策略的 `needed_dims()` 返回最小 DimMask:
    - FixedOrder: HEALTH_ONLY
    - RoundRobin: HEALTH_ONLY
    - CostFirst: HEALTH | COST
    - LatencyFirst: HEALTH | LATENCY
    - LoadBalance: HEALTH | INFLIGHT
    - 等
  - 实现 `KeyHealthStore::snapshot(key_ids)` / `InFlightStore::snapshot(key_ids)` / `LatencyTracker::snapshot(key_ids)` — 一次遍历
  - **回归测试**: 每策略一个"管线选择 == 旧逻辑选择"的对比测试

  **Must NOT do**:
  - 不改策略的外部行为语义
  - 不删除旧策略代码（保留为参考直到 bench 验证管线等价）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocks**: Task 4, T5+T17, P-F
  - **Blocked By**: Task 2

  **References**:
  - `crates/gw-routing/src/fixed_order.rs` — FixedOrder 实现
  - `crates/gw-routing/src/round_robin.rs:11,48` — 已有 per-model DashMap + fetch_add
  - `crates/gw-routing/src/load_balance.rs` — Vec alloc + sort
  - `crates/gw-routing/src/cost_first.rs` — scored.sort_by
  - `crates/gw-routing/src/latency.rs` — latency p50 cache
  - `crates/gw-routing/src/sticky.rs` — moka LRU cache

  **Acceptance Criteria**:
  - [ ] 8 策略各有 `ScoringPolicy` impl
  - [ ] 8 策略各有"管线结果 == 旧结果"对比测试
  - [ ] snapshot 批量 API 一次遍历验证

  **QA Scenarios**:

  ```
  Scenario: CostFirst 管线等价
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing -- test_cost_first_pipeline_equivalent
      2. 相同 5 candidates + cost 数据
      3. 断言管线选择 == 旧 CostFirst::pick 选择
    Expected Result: 行为等价
    Evidence: .omo/evidence/pb-3-cost-first-equiv.txt

  Scenario: snapshot 批量 API
    Tool: Bash (cargo test)
    Steps:
      1. 创建 10 keys 的 health data
      2. 调用 snapshot([key1..key10])
      3. 断言一次调用返回 10 个 HealthSnapshot
    Expected Result: 批量读取
    Evidence: .omo/evidence/pb-3-snapshot-batch.txt
  ```

  **Commit**: YES (groups with 1, 2, 4)

- [x] 4. Bench + 回归集成测试

  **What to do**:
  - 运行 `cargo bench -p gw-routing --bench strategy_pick` 对比 before/after
  - 每策略 P99 ≤ 100µs
  - 8 策略在 10/50/100 candidates 下 bench
  - `make e2e-soak-smoke` 通过
  - 集成测试：端到端请求使用每种策略

  **Must NOT do**:
  - bench 退化 > 5% → 不合并

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 3

  **References**:
  - `benches/` — 现有 bench 文件

  **Acceptance Criteria**:
  - [ ] 8 策略 bench P99 ≤ 基线 +5%
  - [ ] `make e2e-soak-smoke` PASS

  **QA Scenarios**:

  ```
  Scenario: bench 无退化
    Tool: Bash
    Steps:
      1. cargo bench -p gw-routing --bench strategy_pick -- --save-baseline after
      2. 比较 before vs after
    Expected Result: P99 ≤ 5% drift
    Evidence: .omo/evidence/pb-4-bench.txt
  ```

  **Commit**: YES (groups with 1, 2, 3)
  - Message: `refactor(routing): scoring pipeline with 8 strategy adapters [P-B]`
  - Pre-commit: `cargo test --workspace && cargo bench -p gw-routing --bench strategy_pick && make e2e-soak-smoke && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace
cargo bench -p gw-routing --bench strategy_pick  # P99 ≤ 100µs per strategy
make e2e-soak-smoke
bash scripts/check-invariants.sh
```
