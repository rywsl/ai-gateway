# T5+T17: 多维度可组合调度 + Scheduling Presets

## TL;DR

> **Quick Summary**: 利用 P-B 的 scoring 管线（filter→extract→gate→score→pick），实现多维度权重组合策略 + 预设配置（如 health_first, latency_first, cost_first 等）。现有 8 策略作为管线预设自然表达。
>
> **Deliverables**: MultiDim scoring policy + presets 配置 + tests + docs
> **Estimated Effort**: Medium
> **Depends On**: P-B (路由 scoring 管线重构)

---

## Context

### 现状（P-B 完成后）

P-B 提供：
- `CandidateFeatures` struct（per-candidate 多维度特征快照）
- `ScoringPolicy` trait（`fn score(&self, features: &CandidateFeatures) -> f64`）
- Pipeline executor：`filter → extract → gate(health) → score(policy) → pick/tiebreak`
- 既有 8 策略已作为 `ScoringPolicy` 适配器实现

### 根治方案

- `MultiDimPolicy` 实现 `ScoringPolicy`，接受维度权重配置
- Presets 是预定义的权重组合（`health_first`, `latency_first`, `cost_first`, `balanced` 等）
- 配置在 `config/default.toml` 的 `[routing.presets]` section
- Boot-time only（不支持热更新）

---

## Work Objectives

### Must Have
- `MultiDimPolicy` 实现 `ScoringPolicy`，按维度权重加权评分
- 至少 4 个预设：`health_first`, `latency_first`, `cost_first`, `balanced`
- 预设可通过 `ModelGroup.strategy_kind` 引用（如 `MultiDim("health_first")`）
- 自定义权重可通过 config 定义
- 单元测试 + 集成测试
- Config 文档更新

### Must NOT Have
- 不新增 Cargo 依赖
- 不改 `ScoringPolicy` trait 定义（P-B 已定义好）
- 不做运行时动态切换预设
- 不做 per-request 权重覆盖
- 不做超过 `CandidateFeatures` 已有维度的新维度

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Bench gate**: `cargo bench -p gw-routing --bench strategy_pick` P99 ≤ 5% drift
- **Invariants**: `scripts/check-invariants.sh` exit 0

---

## Execution Strategy

```
Wave 1 [serial]:
├── Task 1: MultiDimPolicy 实现 [deep]
└── Task 2: Presets 配置 + tests [quick]
```

---

## TODOs

- [x] 1. MultiDimPolicy scoring 实现

  **What to do**:
  - 在 `gw-routing` crate 新增 `multi_dim.rs`（或 P-B 建立的 scoring 模块内）
  - 实现 `MultiDimPolicy` struct：
    ```rust
    pub struct MultiDimPolicy {
        weights: DimWeights, // { health: f64, latency: f64, cost: f64, inflight: f64, error_rate: f64 }
    }
    impl ScoringPolicy for MultiDimPolicy {
        fn score(&self, features: &CandidateFeatures) -> f64 { /* 加权求和 */ }
    }
    ```
  - 维度归一化：每个维度 0.0-1.0，health/latency/cost/inflight/error_rate
  - 权重归一化：权重和为 1.0（构造时 normalize）
  - 单元测试：
    - 各维度权重测试（单维度 1.0 退化为对应策略）
    - 多维度加权排序正确性
    - 权重归一化边界（全零权重 → 均匀）

  **Must NOT do**:
  - 不改 `ScoringPolicy` trait
  - 不改 `CandidateFeatures` struct（P-B 定义）
  - 不引入新的 dim 维度

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: scoring 算法实现需要理解多维度归一化和边界处理
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（Task 2 依赖 Task 1）
  - **Parallel Group**: Sequential
  - **Blocks**: Task 2
  - **Blocked By**: P-B

  **References**:

  **Pattern References**:
  - P-B 产出的 `ScoringPolicy` trait 定义和 `CandidateFeatures` struct
  - `crates/gw-routing/src/load_balance.rs:32` — 现有 LoadBalance 的评分模式参考
  - `crates/gw-routing/src/cost_first.rs:25-33` — 现有 CostFirst 的 scored.sort_by 模式

  **WHY Each Reference Matters**:
  - `ScoringPolicy` trait: 需实现的接口
  - `load_balance.rs`: 理解现有评分粒度（Vec alloc + sort 模式）
  - `cost_first.rs`: 理解 cost 维度数据来源

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-routing` 通过
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] MultiDimPolicy 单维度权重 1.0 行为等价于对应单策略

  **QA Scenarios**:

  ```
  Scenario: Happy path — 多维度加权选择
    Tool: Bash (cargo test)
    Preconditions: 3 个候选 key，各维度特征不同
    Steps:
      1. cargo test -p gw-routing --test multi_dim -- --exact test_weighted_scoring
      2. 断言测试通过
    Expected Result: 加权得分最高的 key 被选中
    Failure Indicators: 测试失败或选错 key
    Evidence: .omo/evidence/task-t5-1-multi-dim-scoring.txt

  Scenario: Edge case — 全零权重
    Tool: Bash (cargo test)
    Preconditions: DimWeights 全部为 0.0
    Steps:
      1. cargo test -p gw-routing --test multi_dim -- --exact test_zero_weights_uniform
    Expected Result: 归一化为均匀权重，不 panic
    Evidence: .omo/evidence/task-t5-1-zero-weights.txt
  ```

  **Commit**: YES (与 Task 2 合并)
  - Message: `feat(routing): multi-dim scoring policy + scheduling presets`
  - Files: `crates/gw-routing/src/multi_dim.rs`, config, docs
  - Pre-commit: `cargo test -p gw-routing && scripts/check-invariants.sh`

---

- [x] 2. Scheduling Presets 配置 + 集成

  **What to do**:
  - 在 `crates/gw-gateway/src/config.rs` 新增 `[routing.presets]` section：
    ```toml
    [routing.presets]
    health_first = { health = 0.6, latency = 0.2, cost = 0.1, inflight = 0.1, error_rate = 0.0 }
    latency_first = { health = 0.2, latency = 0.5, cost = 0.1, inflight = 0.2, error_rate = 0.0 }
    cost_first = { health = 0.2, latency = 0.1, cost = 0.5, inflight = 0.1, error_rate = 0.1 }
    balanced = { health = 0.25, latency = 0.25, cost = 0.25, inflight = 0.15, error_rate = 0.1 }
    ```
  - 扩展 `StrategyKind` 或 P-B 的管线 dispatch 逻辑，支持 `MultiDim("<preset_name>")` 或 `MultiDim(custom_weights)` 选择
  - Boot-time 加载，不支持热更新
  - 更新 `config/default.toml` 含默认 4 预设
  - 更新 `docs/architecture.md` §Configuration 新增 `[routing.presets]`
  - 集成测试：完整 pick 流程 + preset 选择

  **Must NOT do**:
  - 不做运行时动态切换
  - 不做 per-request preset override
  - 不改 existing 8 策略的行为

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 配置解析 + dispatch 接线，模式明确
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（依赖 Task 1）
  - **Parallel Group**: Sequential after Task 1
  - **Blocks**: None
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/config.rs` — 现有 config 解析模式（figment）
  - `config/default.toml` — 现有配置结构

  **WHY Each Reference Matters**:
  - `config.rs`: 遵循既有 figment 层级解析模式新增 section
  - `default.toml`: 确认命名约定和嵌套结构

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-routing` 通过（含 preset 集成测试）
  - [ ] `config/default.toml` 含 4 个默认预设
  - [ ] `docs/architecture.md` §Configuration 更新
  - [ ] `cargo bench -p gw-routing --bench strategy_pick` P99 ≤ 5% drift

  **QA Scenarios**:

  ```
  Scenario: Happy path — preset 选择
    Tool: Bash (cargo test)
    Preconditions: config 加载 "health_first" preset
    Steps:
      1. cargo test -p gw-routing --test presets -- --exact test_health_first_preset
    Expected Result: health_first 预设等价于 health=0.6 权重的 MultiDimPolicy
    Evidence: .omo/evidence/task-t5-2-preset-selection.txt

  Scenario: Error — 无效 preset 名称
    Tool: Bash (cargo test)
    Preconditions: config 引用不存在的 preset 名
    Steps:
      1. cargo test -p gw-routing --test presets -- --exact test_invalid_preset_name
    Expected Result: boot-time panic/error（fail-fast），不静默回退
    Evidence: .omo/evidence/task-t5-2-invalid-preset.txt
  ```

  **Commit**: YES (与 Task 1 合并)
  - Message: `feat(routing): multi-dim scoring policy + scheduling presets`
  - Files: `crates/gw-routing/src/multi_dim.rs`, `crates/gw-gateway/src/config.rs`, `config/default.toml`, `docs/architecture.md`
  - Pre-commit: `cargo test -p gw-routing && cargo bench -p gw-routing --bench strategy_pick --no-run && scripts/check-invariants.sh`

---

## Success Criteria

```bash
cargo test -p gw-routing                                   # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings      # Expected: exit 0
cargo bench -p gw-routing --bench strategy_pick            # Expected: P99 ≤ 5% drift
scripts/check-invariants.sh                                 # Expected: exit 0
```

### Final Checklist
- [ ] MultiDimPolicy 实现 ScoringPolicy
- [ ] 4 预设可用（health_first, latency_first, cost_first, balanced）
- [ ] 自定义权重可通过 config 定义
- [ ] StrategyKind 支持 MultiDim 选择
- [ ] Config/docs 更新
