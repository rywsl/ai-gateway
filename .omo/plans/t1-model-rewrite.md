# T1: Model Name 请求体重写

## TL;DR

> **Quick Summary**: 利用 P-A 的 `passthrough_overrides` + mutation flag 框架，实现 `ModelMapping.default_upstream_model` → 请求体 model 字段替换。同协议/跨协议都生效。
>
> **Deliverables**: model rewrite filter + metric + config + tests
> **Estimated Effort**: Short
> **Depends On**: P-A (IR 任意字段注入架构)

---

## Context

### 现状

`ModelMapping` 实体已有 `default_upstream_model` 字段（`entity.rs`），但 `apply_forced_params_to_ir`（`filters/upstream.rs:478`）只处理 `temperature`/`top_p`，从不替换 `ir.model`。同协议 passthrough 路径 `mutations.is_empty()` 时跳过 re-render，即使改了 `ir.model` 也不生效。

### P-A 提供的基础

P-A 完成后：
- `passthrough_overrides: Option<Map<String, RawValue>>` 可容纳 model 字段
- mutation flag 注册使 passthrough 强制走 parse→render
- codec render 阶段合并 overrides

### 根治方案

在 `upstream_request_filter` 阶段（P-A 基础上），当 `ModelMapping.default_upstream_model` 非空时：
1. 将 `{"model": "<upstream_model>"}` 注入 `passthrough_overrides`
2. 同时更新 `ir.model`（供 routing/logging 使用）
3. mutation flag 自动注册 → passthrough 强制 re-render → 上游收到正确 model

---

## Work Objectives

### Must Have
- `upstream_request_filter` 中检测 `default_upstream_model`，注入 override
- 同时更新 `ir.model` 保持内部一致性
- 响应 model 字段保留上游返回值（不回写）
- Prometheus metric `gw_model_rewrite_total{from, to}`
- config 无新增（复用 `ModelMapping.default_upstream_model`）
- 单元测试 + 集成测试

### Must NOT Have
- 不做通用 model 名称规范化/别名系统
- 不改 `ModelMapping` entity 结构
- 不碰 response 路径（不回写 model 字段）
- 不引入新 Cargo 依赖
- 不 refactor `apply_forced_params_to_ir` 的现有逻辑

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Bench gate**: `cargo bench -p gw-codec --bench passthrough` P99 ≤ 5% drift
- **Invariants**: `scripts/check-invariants.sh` exit 0

---

## Execution Strategy

```
Wave 1 [serial — 单一 task]:
└── Task 1: model rewrite 实现 + tests [quick]
```

---

## TODOs

- [x] 1. Model Name 请求体重写实现

  **What to do**:
  - 在 `filters/upstream.rs` 的 `apply_forced_params_to_ir`（或同级新函数）中：
    - 通过 `gw-capability::ModelMapper` 获取 upstream model — 现有 `map_for_dispatch`（`crates/gw-capability/src/mapper.rs:81`）和 `map_for_routing`（`:51`）已能返回 `default_upstream_model`。在 filter 阶段，从已有的 capability 解析结果中取 `upstream_model`（`ReqCtx` 已在 `request_filter` 阶段通过 capability resolver 拿到路由信息）
    - 若 `upstream_model` 非空且与 `ir.model` 不同：将 `{"model": "<upstream_model>"}` 注入 `ir.passthrough_overrides`
    - 同时 `ir.model = upstream_model.clone()`
  - 新增 Prometheus counter `gw_model_rewrite_total{from, to}` 在替换时自增
  - 单元测试：
    - `default_upstream_model` 为空 → IR 不变
    - `default_upstream_model` 设置 → `ir.model` 被替换 + `passthrough_overrides` 含 model
    - 同协议路径（OpenAI→OpenAI）：断言上游收到替换后的 model
    - 跨协议路径（Anthropic→OpenAI）：断言上游收到替换后的 model
  - 更新 `docs/workflow.md` §upstream_request_filter 新增 model rewrite 描述
  - 更新 `docs/architecture.md` §Known gaps 移除相关条目

  **Must NOT do**:
  - 不碰 response codec
  - 不 refactor `apply_forced_params_to_ir` 的 temperature/top_p 逻辑
  - 不改 `ModelMapping` entity 定义
  - 不新增 config 字段（复用现有 `default_upstream_model`）

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 单文件改动 + mutation flag 注入，P-A 基础上直接实现
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - `playwright`: 非 UI 任务

  **Parallelization**:
  - **Can Run In Parallel**: NO（依赖 P-A 完成）
  - **Parallel Group**: T=1 (与 T9 body rules 并行)
  - **Blocks**: None
  - **Blocked By**: P-A

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/upstream.rs:478` — `apply_forced_params_to_ir` 现有 forced_params 注入模式
  - `crates/gw-proxy/src/filters/upstream.rs:565-568` — `mutations.is_empty()` passthrough 分支判定

  **API/Type References**:
  - `crates/gw-core/src/entity.rs:393` — `ModelMapping` struct，`default_upstream_model` 字段
  - `crates/gw-core/src/ir.rs:15` — `RequestIR` struct（P-A 完成后含 `passthrough_overrides`）
  - `crates/gw-capability/src/mapper.rs:81` — `ModelMapper::map_for_dispatch` 返回 `default_upstream_model`
  - `crates/gw-capability/src/mapper.rs:51` — `ModelMapper::map_for_routing` 设置 `upstream_model`

  **External References**:
  - 无

  **WHY Each Reference Matters**:
  - `upstream.rs:478`: 这是注入点 — 在同一 filter 阶段添加 model override 逻辑
  - `upstream.rs:565`: 理解 passthrough 分支 — P-A 的 mutation flag 如何让此处走 re-render
  - `entity.rs:393 ModelMapping`: 确认 `default_upstream_model` 字段类型和来源
  - `ir.rs:15 RequestIR`: P-A 新增的 `passthrough_overrides` 字段，model 值注入目标
  - `mapper.rs:81 map_for_dispatch`: 现有的 upstream model 解析路径 — 复用此方法获取 `default_upstream_model`，而非直接从 ctx 读 ModelMapping

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-proxy` 通过（含新增 model rewrite tests）
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] `scripts/check-invariants.sh` exit 0
  - [ ] `cargo bench -p gw-codec --bench passthrough` P99 ≤ 5% drift

  **QA Scenarios**:

  ```
  Scenario: Happy path — 同协议 model 重写
    Tool: Bash (curl)
    Preconditions: fake upstream 启动; ModelMapping 配置 {client_model: "gpt-4-turbo", default_upstream_model: "gpt-4-0613"}
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/chat/completions \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"gpt-4-turbo","messages":[{"role":"user","content":"hi"}]}'
      2. 检查 fake_upstream 收到的请求体: jq -r '.model' < fake_upstream_log.jsonl | tail -1
      3. 断言结果为 "gpt-4-0613"
      4. curl -s http://localhost:9090/metrics | grep 'gw_model_rewrite_total{from="gpt-4-turbo",to="gpt-4-0613"}'
      5. 断言 counter >= 1
    Expected Result: 上游收到 model="gpt-4-0613", metric 自增
    Failure Indicators: 上游 model 仍为 "gpt-4-turbo", metric 不存在或为 0
    Evidence: .omo/evidence/task-t1-1-model-rewrite-happy.txt

  Scenario: Passthrough 路径验证 — 同协议无 cross-render 时 model 也被改写
    Tool: Bash (curl)
    Preconditions: 同上；确认请求和上游都是 OpenAI 格式（同协议）
    Steps:
      1. 同上 curl 请求
      2. 验证 fake_upstream 收到的 Content-Type 为 application/json
      3. jq -r '.model' < fake_upstream_log.jsonl | tail -1 == "gpt-4-0613"
    Expected Result: 即使同协议也走 re-render（因 mutation flag），model 被正确替换
    Failure Indicators: model 仍为 "gpt-4-turbo"（说明 passthrough 短路未被 mutation flag 打断）
    Evidence: .omo/evidence/task-t1-1-passthrough-verify.txt

  Scenario: 无配置时不改写
    Tool: Bash (curl)
    Preconditions: ModelMapping.default_upstream_model 为空或未设置
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/chat/completions \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"gpt-4-turbo","messages":[{"role":"user","content":"hi"}]}'
      2. jq -r '.model' < fake_upstream_log.jsonl | tail -1
      3. 断言结果为 "gpt-4-turbo"（未改写）
    Expected Result: model 保持原样
    Failure Indicators: model 被意外改写
    Evidence: .omo/evidence/task-t1-1-no-config.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): implement model name rewrite via passthrough_overrides`
  - Files: `crates/gw-proxy/src/filters/upstream.rs`, `docs/workflow.md`, `docs/architecture.md`
  - Pre-commit: `cargo test -p gw-proxy && scripts/check-invariants.sh`

---

## Success Criteria

### Verification Commands
```bash
cargo test -p gw-proxy                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings     # Expected: exit 0
scripts/check-invariants.sh                                # Expected: exit 0
cargo bench -p gw-codec --bench passthrough               # Expected: P99 ≤ 5% drift
```

### Final Checklist
- [ ] `default_upstream_model` 非空时 model 被替换
- [ ] `default_upstream_model` 空时 model 不变
- [ ] 同协议 passthrough 路径 model 也被替换
- [ ] `gw_model_rewrite_total` metric 正确
- [ ] docs 同步更新
