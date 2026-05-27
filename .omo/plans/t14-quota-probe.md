# T14: Quota Probe（可选增强，依赖 P-D）

## TL;DR

> **Quick Summary**: P-D 已完成 retry-after header 消费 + cooldown 接线（主根治）。本 plan 实现可选的低成本探测请求，用于上游未返回 retry-after header 时的 quota 恢复检测。
>
> **Deliverables**: 可选的 quota probe 机制 + 配置 + tests
> **Estimated Effort**: Short
> **Depends On**: P-D (retry-after header 消费)

---

## Context

### P-D 已解决的问题

P-D 完成后：
- 429 的 Retry-After header 值被消费 → `apply_cooldown(retry_after_seconds)`
- Cooldown 到期后复用既有 half-open admission（`format_health.rs:66`）一个真实请求探测
- `adaptive_rpm` 精度改善（consume retry-after 而非盲猜）

### 剩余场景

部分上游不返回 Retry-After header。此时 cooldown 使用固定时间（P-D 的 default fallback）。探测可以在 cooldown 期间提早发现上游恢复。

### 设计决策

- **可选功能**：默认关闭，通过 config 启用
- **低成本**：发 `/v1/models` 或最小 token 请求
- **与 half-open 互补**：half-open 在 cooldown 到期后放行一个真实请求；probe 在 cooldown 期间主动探测
- **不计费**：probe 请求不产生 usage:records

---

## Work Objectives

### Must Have
- 配置：`[health.quota_probe]` section（enabled, interval_secs, probe_endpoint）
- Probe 逻辑：cooldown 期间按 interval 发探测请求
- 探测成功 → 清除 cooldown，key 恢复 available
- 探测失败 → 保持 cooldown，等待下次探测
- Probe 请求不产生 usage:records
- Prometheus metric `gw_quota_probe_total{key_id, result}` (success/failure)
- Boot-time only 配置
- 单元测试 + 集成测试

### Must NOT Have
- 不做探测请求的鉴权豁免（复用 vendor key 直接访问上游）
- 不做 probe 结果影响 adaptive_rpm（只影响 cooldown 状态）
- 不新增 Cargo 依赖
- 不改 half-open admission 逻辑
- 不在 probe 失败时延长 cooldown（保持原 TTL）

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Key assertion**: probe 默认关闭时零开销

---

## Execution Strategy

```
Wave 1 [单一 task]:
└── Task 1: Quota probe 实现 + tests [unspecified-high]
```

---

## TODOs

- [x] 1. Quota Probe 可选增强实现

  **What to do**:
  - 在 `crates/gw-gateway/src/config.rs` 新增 `[health.quota_probe]`：
    ```toml
    [health.quota_probe]
    enabled = false          # 默认关闭
    interval_secs = 30       # 探测间隔
    probe_endpoint = "/v1/models"  # 低成本端点
    ```
  - 在 `gw-health` crate 新增 `quota_probe.rs`：
    - 后台 tokio task，遍历处于 cooldown 状态的 key
    - 按 interval 向上游发 probe 请求（使用 vendor key 直接访问）
    - 成功（2xx）→ 调用 `apply_recovery(key_id)` 清除 cooldown
    - 失败 → 记录 metric，不操作
  - Probe 请求路径 **不经过** `upstream_request_filter`（避免 ledger/billing 记录）
  - Prometheus counter `gw_quota_probe_total{key_id, result}`
  - 单元测试：
    - enabled=false → 后台 task 不启动
    - cooldown 中的 key → 按 interval 探测
    - 探测成功 → cooldown 清除
    - 探测失败 → cooldown 保持
  - 更新 `config/default.toml` + `docs/architecture.md` §Configuration

  **Must NOT do**:
  - 不改 P-D 的 retry-after 接线逻辑
  - 不改 half-open admission
  - 不把 probe 结果写入 usage:records
  - 不在主热路径增加任何检查

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 后台 task + health store 交互 + 非热路径 HTTP 请求
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（依赖 P-D 完成）
  - **Parallel Group**: T=1 after P-D
  - **Blocks**: None
  - **Blocked By**: P-D

  **References**:

  **Pattern References**:
  - `crates/gw-health/src/format_health.rs:66-89` — half-open admission CAS 模式
  - `crates/gw-health/src/format_health.rs:132-140` — `apply_cooldown` 方法（P-D 接线后）
  - P-D 产出的 retry-after 消费逻辑

  **API/Type References**:
  - `crates/gw-health/src/format_health.rs` — `FormatHealth` struct + key 状态机
  - `crates/gw-gateway/src/config.rs` — 配置结构

  **WHY Each Reference Matters**:
  - `format_health.rs:66`: 理解 half-open 如何与 probe 互补
  - `apply_cooldown`: probe 成功需调用对应的恢复方法
  - config.rs: 遵循既有 figment 模式

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-health` 通过（含 probe tests）
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] `scripts/check-invariants.sh` exit 0
  - [ ] probe 默认关闭，不增加 soak 资源消耗

  **QA Scenarios**:

  ```
  Scenario: 默认关闭 — 零影响
    Tool: Bash (cargo test)
    Preconditions: config 未设 [health.quota_probe] 或 enabled=false
    Steps:
      1. cargo test -p gw-health --test quota_probe -- --exact test_disabled_no_task
    Expected Result: 后台 task 不启动，无探测请求
    Evidence: .omo/evidence/task-t14-1-disabled.txt

  Scenario: 启用后探测成功恢复
    Tool: Bash (cargo test)
    Preconditions: key 处于 cooldown 状态; mock upstream 返回 200
    Steps:
      1. cargo test -p gw-health --test quota_probe -- --exact test_probe_success_recovery
    Expected Result: key 从 cooldown 恢复到 available
    Evidence: .omo/evidence/task-t14-1-probe-success.txt

  Scenario: 探测失败保持 cooldown
    Tool: Bash (cargo test)
    Preconditions: key 处于 cooldown; mock upstream 返回 429
    Steps:
      1. cargo test -p gw-health --test quota_probe -- --exact test_probe_failure_keeps_cooldown
    Expected Result: key 保持 cooldown 状态
    Evidence: .omo/evidence/task-t14-1-probe-failure.txt
  ```

  **Commit**: YES
  - Message: `feat(health): optional quota probe for keys without retry-after header`
  - Files: `crates/gw-health/src/quota_probe.rs`, `crates/gw-gateway/src/config.rs`, `config/default.toml`, `docs/architecture.md`
  - Pre-commit: `cargo test -p gw-health && scripts/check-invariants.sh`

---

## Success Criteria

```bash
cargo test -p gw-health                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings      # Expected: exit 0
scripts/check-invariants.sh                                 # Expected: exit 0
```

### Final Checklist
- [ ] 默认关闭（enabled=false）
- [ ] 启用后按 interval 探测 cooldown keys
- [ ] 探测成功 → cooldown 清除
- [ ] 探测失败 → 保持
- [ ] 不产生 usage:records
- [ ] metric 正确
