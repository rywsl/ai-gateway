# T15: Adaptive Reservation

## TL;DR

> **Quick Summary**: 利用 P-F 的全局视图基础设施（local atomic + events:reservation 异步聚合），在 P-B scoring 管线的 gate 阶段实现 per-model 容量预留。多实例精度 5s 级最终一致。零 hot-path 退化。
>
> **Deliverables**: reservation gate policy + pipeline 集成 + tests + docs
> **Estimated Effort**: Medium
> **Depends On**: P-F (Reservation 全局视图基础设施), P-B (路由 scoring 管线)

---

## Context

### P-B + P-F 提供的基础

- **P-B**: `filter → extract → gate → score → pick` 管线，`gate` 阶段可注入过滤策略
- **P-F**: 
  - per-model 频率采样（复用 RoundRobin 的 `DashMap<ModelName, AtomicUsize>`）
  - `events:reservation` Redis stream 异步聚合
  - 全局频率视图（本地 DashMap，5s 最终一致）

### 设计方案

"Local-Read + Async-Aggregate" — hot path 仅读本地原子计数器，后台 task 异步聚合全局视图：
1. `gate` 阶段检查 model 的全局频率 vs 配置的 reservation 阈值
2. 超阈值的 key 被 gate 过滤（不参与 scoring）
3. 阈值以下的 key 正常进入 scoring

### v1 显式语义
- 多实例聚合 **5s 级延迟** — 非精确实时
- per-instance 本地计数仍是最低延迟信号（0ms）
- 这是**设计权衡**（零 hot-path Redis call），不是 v1 缺陷

---

## Work Objectives

### Must Have
- `ReservationGate` 实现 P-B 管线的 gate 接口
- 配置：`[routing.reservation]` section（thresholds per model pattern）
- Gate 逻辑：全局频率 > threshold → 过滤该 key
- 全局频率读取：P-F 的本地 DashMap（零 Redis RTT）
- 单元测试 + 集成测试
- 文档化"5s 最终一致"语义

### Must NOT Have
- 不在 hot path 调用 Redis
- 不在 hot path 加 Mutex
- 不做 per-request Redis 查询
- 不改 P-B 的 gate/ScoringPolicy trait
- 不做精确实时全局一致（违反零退化约束）
- 不新增 Cargo 依赖

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Bench gate**: `cargo bench -p gw-routing --bench strategy_pick` P99 ≤ 2µs drift
- **Key assertion**: reservation enabled vs disabled 时 strategy_pick P99 差异 ≤ 2µs

---

## Execution Strategy

```
Wave 1 [serial]:
├── Task 1: ReservationGate 实现 + config [deep]
└── Task 2: Pipeline 集成 + bench + docs [unspecified-high]
```

---

## TODOs

- [x] 1. ReservationGate 实现 + 配置

  **What to do**:
  - 在 `gw-routing` crate（P-B 管线模块内）新增 `reservation_gate.rs`：
    ```rust
    pub struct ReservationGate {
        global_view: Arc<GlobalFrequencyView>,  // P-F 产出
        thresholds: HashMap<ModelPattern, u64>, // config 加载
    }
    impl Gate for ReservationGate {
        fn filter(&self, candidate: &CandidateFeatures) -> bool {
            // 查 global_view 的 model 频率 vs threshold
            // 超阈值返回 false（过滤掉）
        }
    }
    ```
  - 配置在 `crates/gw-gateway/src/config.rs`：
    ```toml
    [routing.reservation]
    enabled = false
    # model pattern → max requests/min
    # "gpt-4*" = 100
    # "claude-3*" = 200
    ```
  - Boot-time only，model pattern 支持 glob（复用既有 ReDoS-protected regex 或 glob crate）
  - 单元测试：
    - enabled=false → gate 始终返回 true（不过滤）
    - 全局频率 < threshold → pass
    - 全局频率 > threshold → filter
    - 无 threshold 配置的 model → pass（默认不限）

  **Must NOT do**:
  - 不在 filter() 中调用 Redis
  - 不加 Mutex
  - 不改 Gate trait 定义
  - 不新增依赖

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: gate 策略 + 全局视图集成需要理解多实例语义
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（Task 2 依赖 Task 1）
  - **Parallel Group**: Sequential
  - **Blocks**: Task 2
  - **Blocked By**: P-F

  **References**:

  **Pattern References**:
  - P-B 产出的 `Gate` trait 定义
  - P-F 产出的 `GlobalFrequencyView` struct + 本地 DashMap
  - `crates/gw-routing/src/round_robin.rs:11` — 复用的 per-model `DashMap<ModelName, AtomicUsize>`

  **API/Type References**:
  - P-B 的 `CandidateFeatures` struct
  - `crates/gw-gateway/src/config.rs` — 配置模式

  **WHY Each Reference Matters**:
  - Gate trait: 需实现的接口
  - GlobalFrequencyView: 全局频率数据源（零 RTT 读取）
  - RoundRobin DashMap: P-F 复用的底层数据结构

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-routing` 通过（含 reservation tests）
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] `scripts/check-invariants.sh` exit 0

  **QA Scenarios**:

  ```
  Scenario: 默认关闭 — 不过滤
    Tool: Bash (cargo test)
    Preconditions: reservation.enabled = false
    Steps:
      1. cargo test -p gw-routing --test reservation -- --exact test_disabled_passthrough
    Expected Result: 所有候选通过 gate
    Evidence: .omo/evidence/task-t15-1-disabled.txt

  Scenario: 超阈值过滤
    Tool: Bash (cargo test)
    Preconditions: threshold=100, global view 显示 model 频率=150
    Steps:
      1. cargo test -p gw-routing --test reservation -- --exact test_over_threshold_filtered
    Expected Result: 该 key 被过滤，不参与 scoring
    Evidence: .omo/evidence/task-t15-1-over-threshold.txt

  Scenario: 低于阈值通过
    Tool: Bash (cargo test)
    Preconditions: threshold=100, global view 显示 model 频率=50
    Steps:
      1. cargo test -p gw-routing --test reservation -- --exact test_under_threshold_pass
    Expected Result: 该 key 通过 gate
    Evidence: .omo/evidence/task-t15-1-under-threshold.txt
  ```

  **Commit**: YES (与 Task 2 合并)
  - Message: `feat(routing): adaptive reservation gate with global frequency view`
  - Files: `crates/gw-routing/src/reservation_gate.rs`, config, docs
  - Pre-commit: `cargo test -p gw-routing && scripts/check-invariants.sh`

---

- [x] 2. Pipeline 集成 + Bench + Docs

  **What to do**:
  - 在 P-B 管线中注册 `ReservationGate`（仅当 config 启用时）
  - Bench 验证：
    - reservation disabled: `strategy_pick` P99 与基线持平（±2µs）
    - reservation enabled: P99 增量 ≤ 2µs
  - 集成测试：完整 pick 流程 + reservation gate + scoring
  - 更新 `docs/architecture.md` §Configuration 新增 `[routing.reservation]`
  - 更新 `docs/architecture.md` 文档化"5s 最终一致"为 v1 显式语义

  **Must NOT do**:
  - 不改 P-B 管线结构
  - 不改既有 strategy 行为

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（依赖 Task 1）
  - **Blocks**: None
  - **Blocked By**: Task 1

  **References**:
  - P-B 管线 executor（gate 注册点）
  - `cargo bench -p gw-routing --bench strategy_pick` 现有基线

  **Acceptance Criteria**:

  - [ ] `cargo bench -p gw-routing --bench strategy_pick` P99 ≤ 2µs drift (disabled)
  - [ ] `cargo bench -p gw-routing --bench strategy_pick` P99 ≤ 2µs drift (enabled)
  - [ ] docs 更新含"5s 最终一致"描述
  - [ ] `make e2e-soak-smoke` 通过

  **QA Scenarios**:

  ```
  Scenario: Bench — 零退化验证
    Tool: Bash
    Steps:
      1. cargo bench -p gw-routing --bench strategy_pick -- --baseline before
      2. 对比 P99 数值
    Expected Result: ≤ 2µs drift
    Evidence: .omo/evidence/task-t15-2-bench.txt

  Scenario: 集成 — 完整 pick 含 reservation
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing --test integration -- --exact test_pipeline_with_reservation
    Expected Result: reservation gate 正确过滤 + scoring 选择剩余候选
    Evidence: .omo/evidence/task-t15-2-integration.txt
  ```

  **Commit**: YES (与 Task 1 合并)
  - Message: `feat(routing): adaptive reservation gate with global frequency view`
  - Pre-commit: `cargo bench -p gw-routing --bench strategy_pick --no-run && make e2e-soak-smoke`

---

## Success Criteria

```bash
cargo test -p gw-routing                                   # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings      # Expected: exit 0
cargo bench -p gw-routing --bench strategy_pick            # Expected: P99 ≤ 2µs drift
scripts/check-invariants.sh                                 # Expected: exit 0
make e2e-soak-smoke                                        # Expected: pass
```

### Final Checklist
- [ ] ReservationGate 实现 Gate trait
- [ ] 默认关闭
- [ ] 零 hot-path Redis call
- [ ] 零 hot-path Mutex
- [ ] Bench P99 ≤ 2µs drift
- [ ] "5s 最终一致" 文档化
- [ ] Config/docs 更新
