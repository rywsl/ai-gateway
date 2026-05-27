# T9: Per-Endpoint Body Rules

## TL;DR

> **Quick Summary**: 利用 P-A 的 `passthrough_overrides` 框架，实现 per-endpoint 的 JSON 字段默认值注入。仅支持 flat key-value（不做嵌套/条件/DSL）。
>
> **Deliverables**: body rules 配置 + filter 注入 + tests + docs
> **Estimated Effort**: Short
> **Depends On**: P-A (IR 任意字段注入架构)

---

## Context

### 现状

`ForcedParams`（`entity.rs:266`）只有 `temperature`/`top_p` 两个 typed 字段。upstream body 从 IR re-render，不是 patch 原始 bytes。任意字段无法注入。

### P-A 提供的基础

P-A 完成后，`passthrough_overrides: Option<Map<String, RawValue>>` 可容纳任意 flat JSON 字段，codec render 阶段自动合并。

### 设计决策

- **永久 scope**: flat key-value only（顶层 JSON 字段）
- **语义**: "set-if-absent" — 仅在请求体没有该字段时注入默认值
- **不做**: 嵌套路径、JSONPath、条件规则、正则、request transformation DSL
- **ReDoS 保护**: 不涉及 regex（纯 key 匹配）

---

## Work Objectives

### Must Have
- 配置格式：`[body_rules.<endpoint_pattern>]`，值为 flat JSON 对象
- filter 阶段：检查请求体是否缺失配置的 key，缺失时注入到 `passthrough_overrides`
- "set-if-absent" 语义（不覆盖已有字段）
- Prometheus metric `gw_body_rules_applied_total{endpoint, key}`
- 配置更新 `config/default.toml`（默认空）
- 单元测试 + 集成测试

### Must NOT Have
- 不做嵌套 JSON path 支持
- 不做条件规则（if-then-else）
- 不做正则匹配
- 不做请求体删除/修改已有字段
- 不做 request transformation DSL
- 不新增 Cargo 依赖

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Bench gate**: `cargo bench -p gw-codec --bench passthrough` P99 ≤ 5% drift
- **Key assertion**: 无 body rules 配置时零开销（Option::None 短路）

---

## Execution Strategy

```
Wave 1 [serial — 单一 task]:
└── Task 1: body rules 实现 + tests [unspecified-high]
```

---

## TODOs

- [x] 1. Per-Endpoint Body Rules 实现

  **What to do**:
  - 在 `crates/gw-gateway/src/config.rs` 新增 `[body_rules]` section：
    ```toml
    [body_rules]
    # 默认空，示例：
    # "/v1/chat/completions" = { "temperature" = 0.7, "top_p" = 0.9 }
    ```
  - 在 `filters/upstream.rs`（`apply_forced_params_to_ir` 同级或之后）：
    - 按请求路径匹配 body_rules 配置
    - 遍历配置的 key-value，对每个 key 检查请求 body（已 parse 的 IR 或 passthrough_overrides）是否已存在
    - 不存在时注入到 `ir.passthrough_overrides`
    - 注册 mutation flag（如未注册）
  - Prometheus counter `gw_body_rules_applied_total{endpoint, key}` 每次注入自增
  - 单元测试：
    - 无 body rules 配置 → 零影响
    - 有配置但字段已存在 → 不覆盖
    - 有配置且字段缺失 → 注入 + metric 自增
    - 多字段配置 → 部分注入（已有的跳过）
  - 更新 `config/default.toml`（空的 `[body_rules]` section 注释说明格式）
  - 更新 `docs/architecture.md` §Configuration + `docs/workflow.md` §upstream_request_filter

  **Must NOT do**:
  - 不做嵌套路径（`a.b.c` 式）
  - 不做字段覆盖（仅 set-if-absent）
  - 不做条件/正则
  - 不改 P-A 的 `passthrough_overrides` 接口

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 需要理解 IR parse/render 流程和 passthrough_overrides 注入语义
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES（与 T1 并行，两者都依赖 P-A 但互不依赖）
  - **Parallel Group**: T=1 (与 T1 并行)
  - **Blocks**: None
  - **Blocked By**: P-A

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/upstream.rs:478` — `apply_forced_params_to_ir` 注入模式
  - `crates/gw-gateway/src/config.rs` — figment 配置解析模式

  **API/Type References**:
  - P-A 产出的 `ir.passthrough_overrides` 字段
  - `crates/gw-store/src/entity.rs:266` — `ForcedParams` struct（参考但不改）

  **WHY Each Reference Matters**:
  - `upstream.rs:478`: 注入点位置，body rules 在同一 filter 阶段执行
  - `config.rs`: 遵循 figment 层级 + `GW_*` env override 模式
  - `passthrough_overrides`: body rules 的值注入目标

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-proxy` 通过
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] `scripts/check-invariants.sh` exit 0
  - [ ] `cargo bench -p gw-codec --bench passthrough` P99 ≤ 5% drift

  **QA Scenarios**:

  ```
  Scenario: Happy path — 缺失字段被注入
    Tool: Bash (curl)
    Preconditions: body_rules 配置 "/v1/chat/completions" = { "temperature" = 0.7 }; fake upstream 启动
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/chat/completions \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}'
      2. jq -r '.temperature' < fake_upstream_log.jsonl | tail -1
      3. 断言结果为 0.7
      4. curl -s http://localhost:9090/metrics | grep 'gw_body_rules_applied_total'
    Expected Result: 上游收到 temperature=0.7, metric 自增
    Failure Indicators: temperature 缺失或 metric 不存在
    Evidence: .omo/evidence/task-t9-1-body-rules-happy.txt

  Scenario: 已有字段不覆盖
    Tool: Bash (curl)
    Preconditions: 同上 body_rules 配置
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/chat/completions \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"temperature":0.3}'
      2. jq -r '.temperature' < fake_upstream_log.jsonl | tail -1
      3. 断言结果为 0.3（不是 0.7）
    Expected Result: 用户指定的值保留，不被覆盖
    Failure Indicators: temperature 变为 0.7
    Evidence: .omo/evidence/task-t9-1-no-override.txt

  Scenario: 无配置时零影响
    Tool: Bash (cargo test)
    Preconditions: body_rules 为空
    Steps:
      1. cargo test -p gw-proxy --test body_rules -- --exact test_empty_config_no_op
    Expected Result: passthrough_overrides 为 None，无 mutation flag
    Evidence: .omo/evidence/task-t9-1-empty-config.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): per-endpoint body rules via passthrough_overrides`
  - Files: `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-gateway/src/config.rs`, `config/default.toml`, `docs/architecture.md`, `docs/workflow.md`
  - Pre-commit: `cargo test -p gw-proxy && scripts/check-invariants.sh`

---

## Success Criteria

```bash
cargo test -p gw-proxy                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings     # Expected: exit 0
cargo bench -p gw-codec --bench passthrough               # Expected: P99 ≤ 5% drift
scripts/check-invariants.sh                                # Expected: exit 0
```

### Final Checklist
- [ ] body rules 配置可解析
- [ ] set-if-absent 语义正确
- [ ] 无配置时零开销
- [ ] metric 正确
- [ ] config/docs 更新
