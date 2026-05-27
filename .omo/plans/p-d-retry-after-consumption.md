# P-D: Retry-After Header 消费 + Cooldown 接线

## TL;DR

> **Quick Summary**: 接线 429 Retry-After header 到 `apply_cooldown`，替代固定 60s cooldown。根治 T14 的主要问题（quota 恢复时机未知），将探测降级为可选增强。
>
> **Deliverables**: retry-after → apply_cooldown 接线、KeyHealthListener 从 telemetry-only 改为 actionable
> **Estimated Effort**: Short
> **包含**: T14-part1 (retry-after 消费)

---

## Context

### 根因分析

429 Retry-After **已被解析但被丢弃**:
- `classify_default.rs:46-118`: DefaultClassifier 解析 Retry-After header
- `store.rs:70`: `record()` 注释 "v1 ignores retry_after"
- `KeyHealthListener`（`key_health.rs`）: 只发 metric 不 `apply_cooldown`

即 "quota 恢复需要探测" 的真正原因是 **网关没消费上游已给出的 retry-after header**。

### 根治路径

接线 retry-after → `apply_cooldown`（cooldown 用 header 值而非固定 60s），让 cooldown 到期复用既有 half-open admission。探测仅在 header 缺失时考虑（T14-part2，可选）。

---

## Work Objectives

### Must Have
- `KeyHealthListener` 从 telemetry-only 改为 actionable（消费 retry-after）
- retry-after 值驱动 `apply_cooldown` 的持续时间
- 无 retry-after header 时保持现有默认 cooldown（向后兼容）
- 删除 `store.rs:70` 的 "v1 ignores" 注释

### Must NOT Have
- 不引入主动探测（T14-part2 scope）
- 不改 `apply_cooldown` 函数签名（仅传入不同的 duration）
- 不碰成功路径

---

## TODOs

- [x] 1. KeyHealthListener 消费 retry-after

  **What to do**:
  - 修改 `crates/gw-health/src/key_health.rs` 的 `KeyHealthListener`
  - 从 `ClassifierOutcome` 中提取 `retry_after_secs: Option<u64>`
  - 有值时: `apply_cooldown(key_id, Duration::from_secs(retry_after_secs))`
  - 无值时: `apply_cooldown(key_id, default_cooldown)`（保持现有行为）
  - 删除 `store.rs:70` 的 "v1 ignores retry_after" 注释
  - 新增 metric: `gw_retry_after_consumed_total{source="header"}`

  **Must NOT do**:
  - 不改 `apply_cooldown` 签名
  - 不改 cooldown 状态机（只传入更准确的 duration）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (单独执行)
  - **Blocks**: T14-part2
  - **Blocked By**: None

  **References**:
  - `crates/gw-health/src/key_health.rs` — KeyHealthListener
  - `crates/gw-health/src/format_health.rs:132-140` — apply_cooldown
  - `crates/gw-core/src/error/classify_default.rs:46-118` — retry-after 解析
  - `crates/gw-health/src/store.rs:70` — "v1 ignores retry_after" 注释

  **Acceptance Criteria**:
  - [ ] 429 + Retry-After: 30 → cooldown 30s（非默认 60s）
  - [ ] 429 无 Retry-After → 保持默认 cooldown
  - [ ] "v1 ignores" 注释已删除
  - [ ] metric incremented

  **QA Scenarios**:

  ```
  Scenario: retry-after header 驱动 cooldown
    Tool: Bash (curl + metrics)
    Steps:
      1. fake upstream 返回 429 + Retry-After: 10
      2. curl 发请求触发 429
      3. 立即再次请求 → 断言该 key 被 cooldown（选择备用 key）
      4. 等 10s → 请求该 key → 断言恢复可用
      5. curl metrics | grep gw_retry_after_consumed_total
    Expected Result: cooldown 精确按 Retry-After 值
    Evidence: .omo/evidence/pd-1-retry-after.txt

  Scenario: 无 Retry-After 保持默认
    Tool: Bash
    Steps:
      1. fake upstream 返回 429 无 Retry-After header
      2. 断言使用默认 cooldown 时间
    Expected Result: 向后兼容
    Evidence: .omo/evidence/pd-1-no-header-default.txt
  ```

  **Commit**: YES
  - Message: `feat(health): wire retry-after header to apply_cooldown [P-D]`
  - Files: `crates/gw-health/`, tests
  - Pre-commit: `cargo test -p gw-health && bash scripts/check-invariants.sh`

- [x] 2. adaptive_rpm 精度改进

  **What to do**:
  - `adaptive_rpm` 当前仅在 429 时触发（`gateway.rs:571-572`）
  - 有了精确 retry-after 后，adaptive_rpm 可以用 cooldown 到期时间校正其窗口
  - 在 `observe_429` 中传入 `retry_after_secs` 参数
  - 无 retry-after 时行为不变

  **Must NOT do**:
  - 不改 adaptive_rpm 的核心算法
  - 不在每请求上触发 adaptive_rpm（保持 429-only）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-proxy/src/gateway.rs:571-572` — adaptive_rpm 429 触发
  - `crates/gw-auth/src/ratelimit/adaptive.rs:24-38` — AdaptiveRpmManager

  **Acceptance Criteria**:
  - [ ] retry-after 值传递到 adaptive_rpm
  - [ ] 无 retry-after 时行为不变

  **QA Scenarios**:

  ```
  Scenario: adaptive_rpm 使用 retry-after
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-auth -- test_adaptive_rpm_with_retry_after
      2. observe_429(retry_after=10) → 断言窗口调整
    Expected Result: 更精确的 RPM 适应
    Evidence: .omo/evidence/pd-2-adaptive-rpm.txt
  ```

  **Commit**: YES
  - Message: `feat(health): improve adaptive_rpm with retry-after precision [P-D]`
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace
bash scripts/check-invariants.sh
# 性能零风险 — 仅 429 路径触发
```
