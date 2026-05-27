# P-C: 统一 Error Classification + Passthrough Pipeline

## TL;DR

> **Quick Summary**: 接线已存在但悬空的 `ErrorClassifier` trait 和 `render_error_to_client`，建立 `classify→policy(wrap|passthrough|cross-render)→emit` 统一 error pipeline。同时完成 T6（error classification 可配置化）和 T7（error passthrough 可配置）。
>
> **Deliverables**: ErrorClassifier 接线、configurable status codes、passthrough policy、统一 emit 路径
> **Estimated Effort**: Medium
> **包含**: T6 (Error Classification 可配置化) + T7 (Error Passthrough 可配置)

---

## Context

### 根因分析

错误分类逻辑 **两处割裂**:
- `failover.rs:35-44`: 硬编码 429+500-599 做 failover 决策（纯 status code）
- `error/classify_default.rs`: 做 cooldown/retryable 决策（status + headers + vendor_payload）

`ErrorClassifier` trait 已存在（`error/class.rs:110`）含 `classify_upstream(headers, vendor_payload)`，但生产代码直接 `new DefaultClassifier()`（`gateway.rs:661`）— 扩展点已设计好，只是没接线。

`render_error_to_client`（`error_passthrough.rs:15`）**无生产调用者** — 错误透传当前是隐式的（同协议 verbatim / 跨协议 fallback）。

### 根治路径

用 config 驱动 `ErrorClassifier` 的注入，让 failover 也消费同一 classifier 的 `ClassifierOutcome.retryable`。error passthrough 接线现有的 `render_error_to_client`，定义统一 `classify → policy(passthrough|wrap|cross-render) → emit` 管道。

---

## Work Objectives

### Must Have
- `ErrorClassifier` trait 注入到生产路径（替代直接 `DefaultClassifier`）
- failover 消费 `ClassifierOutcome.retryable`（而非独立硬编码列表）
- 可配置 retryable status codes（`[error.classification]` in config）
- error passthrough 配置化（per-format or global）
- `render_error_to_client` 正式接线为 passthrough 路径
- 统一 `classify → policy → emit` 管道
- 配置 default values 保持向后兼容（retryable = [429, 500-599]）

### Must NOT Have
- 不重设计 `gw-core/src/error/codes.rs` 错误码体系
- 不做 per-provider 分层（scope creep）
- 不改成功路径（成功路径被 `status >= 400` 严格守卫，完全不触发）
- 不引入新 Cargo 依赖

---

## Verification Strategy

- **Performance**: 零风险 — 仅错误路径触发
- **Tests**: 单元测试 + 集成测试（同 PR）

---

## Execution Strategy

```
Wave 1 [serial]:
├── 1. ErrorClassifier 接线 + config 驱动
├── 2. failover 消费 ClassifierOutcome
├── 3. Error passthrough policy + render_error_to_client 接线
└── 4. 集成测试 + docs 更新
```

---

## TODOs

- [x] 1. ErrorClassifier 接线 + config 驱动

  **What to do**:
  - 在 `crates/gw-gateway/src/config.rs` 新增 `[error.classification]` section:
    ```toml
    [error.classification]
    retryable_status_codes = [429, 500, 502, 503, 504]  # 默认值 = 当前硬编码
    ```
  - 创建 `ConfigurableClassifier` impl `ErrorClassifier` trait
  - 从 config 读取 retryable status codes，构建 `HashSet<u16>`
  - 在 gateway 启动时注入 `Arc<dyn ErrorClassifier>` 替代直接 `DefaultClassifier`
  - boot-time only（与所有新 config 一致）

  **Must NOT do**:
  - 不改 `ErrorClassifier` trait 签名
  - 不做 hot-reload
  - 不删除 `DefaultClassifier`（保留为 fallback）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Serial
  - **Blocks**: Task 2
  - **Blocked By**: None

  **References**:
  - `crates/gw-core/src/error/class.rs:110` — ErrorClassifier trait 定义
  - `crates/gw-core/src/error/classify_default.rs` — DefaultClassifier 实现
  - `crates/gw-proxy/src/gateway.rs:661` — 生产中直接使用 DefaultClassifier 的位置
  - `crates/gw-gateway/src/config.rs` — 配置文件

  **Acceptance Criteria**:
  - [ ] `ConfigurableClassifier` 通过 config 初始化测试
  - [ ] 默认 config 行为与当前 DefaultClassifier 完全一致
  - [ ] `config/default.toml` 含 `[error.classification]` section

  **QA Scenarios**:

  ```
  Scenario: 默认 config 等价
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-core -- test_configurable_classifier_default
      2. 对比 ConfigurableClassifier(default) vs DefaultClassifier 对同一组 status codes
      3. 断言行为一致
    Expected Result: 默认配置向后兼容
    Evidence: .omo/evidence/pc-1-default-compat.txt

  Scenario: 自定义 status codes
    Tool: Bash (cargo test)
    Steps:
      1. 配置 retryable = [429, 502]
      2. 断言 500 不再 retryable
      3. 断言 429/502 仍 retryable
    Expected Result: config 生效
    Evidence: .omo/evidence/pc-1-custom-codes.txt
  ```

  **Commit**: YES (groups with 2, 3, 4)

- [x] 2. Failover 消费 ClassifierOutcome

  **What to do**:
  - 修改 `crates/gw-proxy/src/filters/failover.rs:35-44` 的硬编码 status 列表
  - 改为读取注入的 `Arc<dyn ErrorClassifier>` 的 `classify_upstream()` 结果
  - `ClassifierOutcome.retryable == true` → failover
  - 删除 failover.rs 中的独立 status code 列表（消除两套分类口径）
  - 确保成功路径不触发（`status >= 400` guard 仍保留在 `gateway.rs:634-635`）

  **Must NOT do**:
  - 不改成功路径 guard
  - 不引入新的分类判断维度

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-proxy/src/filters/failover.rs:35-44` — 硬编码列表
  - `crates/gw-proxy/src/gateway.rs:634-635` — status >= 400 guard

  **Acceptance Criteria**:
  - [ ] failover.rs 不再含硬编码 status code 列表
  - [ ] failover 决策完全由 ClassifierOutcome 驱动
  - [ ] 回归测试：429 仍触发 failover, 200 不触发

  **QA Scenarios**:

  ```
  Scenario: failover 由 classifier 驱动
    Tool: Bash (curl)
    Steps:
      1. 配置 retryable = [429, 503]
      2. fake upstream 第一 key 返回 500
      3. 断言 500 不触发 failover（不在列表中）
      4. fake upstream 返回 503 → 断言触发 failover
    Expected Result: 按 config 决定 failover
    Evidence: .omo/evidence/pc-2-failover-classifier.txt
  ```

  **Commit**: YES (groups with 1, 3, 4)

- [x] 3. Error passthrough policy

  **What to do**:
  - 在 config 新增 `[error.passthrough]`:
    ```toml
    [error.passthrough]
    enabled = false  # 默认 wrap（保持现行为）
    # 启用后: upstream 4xx/5xx body 直接透传给 client（同协议时）
    ```
  - 在 error emit 路径检查 policy:
    - `enabled = false` → 当前行为（wrap into protocol envelope / `error_emit.rs`）
    - `enabled = true` + 同协议 → 调用 `render_error_to_client`（`error_passthrough.rs:15`）直接透传
    - `enabled = true` + 跨协议 → 仍 wrap（跨协议无法原样透传）
  - 正式接线 `render_error_to_client` — 该函数终于有生产调用者

  **Must NOT do**:
  - 不改 local error（非 upstream）的处理
  - 不做 per-route passthrough 粒度
  - 不透传 HTML body（如果 upstream 返回 HTML，仍 wrap 为 JSON）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 2

  **References**:
  - `crates/gw-codec/src/error_passthrough.rs:15` — render_error_to_client（无生产调用者）
  - `crates/gw-proxy/src/filters/` — error emit 路径

  **Acceptance Criteria**:
  - [ ] enabled=false 行为与当前完全一致
  - [ ] enabled=true + 同协议 → upstream error body 透传
  - [ ] enabled=true + 跨协议 → 仍 wrap
  - [ ] HTML body → 仍 wrap 为 JSON

  **QA Scenarios**:

  ```
  Scenario: passthrough enabled 同协议
    Tool: Bash (curl)
    Steps:
      1. 配置 error.passthrough.enabled = true
      2. fake upstream 返回 503 {"error": "overloaded", "retry_after": 30}
      3. curl OpenAI→OpenAI 请求
      4. 断言 response body == upstream 原始 body
    Expected Result: upstream error body 直接透传
    Evidence: .omo/evidence/pc-3-passthrough-same-proto.txt

  Scenario: passthrough disabled 默认 wrap
    Tool: Bash (curl)
    Steps:
      1. 配置 error.passthrough.enabled = false (默认)
      2. fake upstream 返回 503
      3. 断言 response body 是 gateway 包装的 JSON envelope
    Expected Result: 当前行为不变
    Evidence: .omo/evidence/pc-3-passthrough-disabled.txt
  ```

  **Commit**: YES (groups with 1, 2, 4)

- [x] 4. 集成测试 + docs 更新

  **What to do**:
  - 端到端测试：config 各组合 × 各 status code × 同/跨协议
  - 更新 `docs/architecture.md` §Configuration 新增 error.classification / error.passthrough
  - 更新 `docs/workflow.md` §error fanout 描述新 pipeline

  **Must NOT do**:
  - 不新增其他 error 功能

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 3

  **Acceptance Criteria**:
  - [ ] 端到端测试覆盖 4 种组合
  - [ ] docs/architecture.md 更新
  - [ ] docs/workflow.md 更新
  - [ ] bash scripts/check-invariants.sh exit 0

  **QA Scenarios**:

  ```
  Scenario: 全组合矩阵
    Tool: Bash
    Steps:
      1. 4 组合: (passthrough on/off) × (custom/default codes)
      2. 每组合发 429, 500, 503 请求
      3. 断言 failover/passthrough 行为符合 config
    Expected Result: 所有组合正确
    Evidence: .omo/evidence/pc-4-matrix.txt
  ```

  **Commit**: YES (groups with 1, 2, 3)
  - Message: `refactor(error): unified error classify→policy→emit pipeline [P-C]`
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace
bash scripts/check-invariants.sh
# 性能无风险 — 仅错误路径触发
```
