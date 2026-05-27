# Feature Parity Completion — ailinkmax-gateway 功能补齐计划

## TL;DR

> **Quick Summary**: 补齐 ailinkmax-gateway 对比 Aether 的 18 项网关层功能缺口，涵盖代理端点、路由调度、请求处理、安全/Auth、可观测性五大类。Split 型缺口同步产出 Go 侧契约文档。
> 
> **Deliverables**:
> - 13 项纯 Rust 网关功能实现
> - 5 项 Split（Rust 半边实现 + Go 契约文档）
> - `docs/go-contracts/contract-change/2026-05-26-feature-parity.md` 契约变更记录
> - `docs/architecture.md` / `docs/workflow.md` 同步更新
> - `config/default.toml` 新增配置段
> 
> **Estimated Effort**: XL
> **Parallel Execution**: YES - 5 waves
> **Critical Path**: Task 1 (model rewrite) → Task 5 (multi-dim scheduling) → Task 9 (body rules) → Final Wave

---

## Context

### Original Request
对比 Aether（Python/FastAPI）和 ailinkmax-gateway（Rust/Pingora 0.8）功能集，识别网关数据面层缺口，编写功能补齐计划。Go 侧配合项需同步更新 `docs/go-contracts/` 和 `docs/go-contracts/contract-change/`。

### Interview Summary
**Key Discussions**:
- 从 28 项声称缺口验证到 23 项确认缺失
- 重新归类：13 项纯 Rust、5 项 Split（Rust+Go）、5 项纯 Go（排除）
- 最终 18 项 Rust TODO 按 P0-P3 四级优先

**Research Findings**:
- `ModelMapping.default_upstream_model` 已存在但 IR model 字段从未被替换
- `forced_params` 仅支持 temperature/top_p
- `failover.rs` 硬编码 429+500-599 重试分类
- `error_passthrough.rs` 包装为协议信封而非原始透传
- Health 有 half-open 探测但非 quota 专用
- Task detector 识别 images/generations 但不识别 edits
- Codec 不识别 `/v1/models` 列表路径或 `/v1/messages/count_tokens`
- `usage:records` 携带 `attempt_index` 但无完整候选链

**User Decisions**:
- 测试策略：Tests after（同 PR 内补测试）
- 全部 18 项必须落地
- 新配置 boot-time only
- 功能对齐语义：ailinkmax 必须是 Aether 超集
- Video 代理：Task 模式（提交+查询），不含视频流
- Go 契约文档一并产出

### Metis Review
**Identified Gaps** (addressed):
- PR 策略：按 P-tier 分批，每 tier 可独立 ship
- Config 命名：`[domain.subdomain]` 统一格式
- Schema 兼容：additive only（新 optional 字段）
- Feature flags：行为变更类默认 disabled
- 性能门：`make e2e-soak-smoke` 作为 per-tier 回归门

---

## Work Objectives

### Core Objective
补齐 ailinkmax-gateway 对 Aether 的 18 项网关层功能缺口，使 ailinkmax 成为 Aether 的功能超集。

### Concrete Deliverables
- 4 项新代理端点路径（images/edits, videos, count_tokens, Gemini Files）
- 1 项代理转发（/v1/models → Go）
- Model name 请求体重写功能
- 多维度可组合调度框架
- 可配置 error classification + error passthrough
- TTFT 超时强制
- Per-endpoint body rules
- Auth 失败 IP 独立限流
- `/healthz/active-requests` 管理端点
- Quota probing 增强、Adaptive reservation、Candidate chain 字段
- Scheduling presets 配置执行、Auto-ban 上报
- Go 契约变更文档

### Definition of Done
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 全绿
- [ ] `cargo fmt --all -- --check` 全绿
- [ ] `bash scripts/check-invariants.sh` 全绿
- [ ] `cargo bench --workspace --no-run` 全绿
- [ ] `make e2e-soak-smoke` 通过（per-tier 门）
- [ ] 无 `unimplemented!()` / `todo!()` / `TODO(v2)` / "for v1" 标记
- [ ] `no_cost_micro` / `no_billing_crate` lint 绿
- [ ] Go 契约文档 5 项 Split 全部产出

### Must Have
- 每项功能的 happy-path + failure-path 测试
- 每项新配置在 `config/default.toml` 有安全默认值
- 每项 filter phase 变更同步更新 `docs/workflow.md`
- 每项架构变更同步更新 `docs/architecture.md`
- Split 项的 Go 契约文档遵循现有 `contract-change/` 格式

### Must NOT Have (Guardrails)
- **NO** 新 Cargo 依赖（除非明确审批）
- **NO** Rust 工具链 / Pingora 版本变更
- **NO** cost computation 逻辑（`no_cost_micro` / `no_billing_crate` 强制）
- **NO** `unimplemented!()` / `todo!()` 占位符
- **NO** 顺便重构（"while I'm here" 重构禁止）
- **NO** 超出 18 项清单的 Aether 功能移植
- **NO** 修改已有 `usage:records` 字段语义（仅 additive）
- **NO** 热重载机制（所有新配置 boot-time only）
- **NO** 新文档目录结构（复用 `docs/superpowers/` 和 `docs/go-contracts/`）
- **NO** body rules 的通用 DSL/正则引擎（用 Serde JSON 操作即可）

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (`cargo test --workspace`)
- **Automated tests**: Tests-after（实现完成后同 PR 补测试）
- **Framework**: `cargo test` (Rust built-in) + `tests/e2e/client/soak/` (soak)

### QA Policy
Every task MUST include agent-executed QA scenarios. Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Proxy endpoints**: Bash (curl) — Send requests to localhost:8000, assert status + response JSON
- **Config/routing**: Bash (cargo test) — Unit test + integration test
- **Admin endpoints**: Bash (curl) — Token-gated admin API at localhost:8080
- **Metrics**: Bash (curl) — Query localhost:9090/metrics, grep counter values

### Cross-Cutting Acceptance
- Per-task: `scripts/check-invariants.sh` exit 0
- Per-task: `cargo clippy --workspace --all-targets -- -D warnings` exit 0
- Per-task: `cargo test --workspace` exit 0
- Per-tier: `make e2e-soak-smoke` passes
- Per-Split: `docs/go-contracts/contract-change/` entry exists

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (P0 — Protocol Coverage, start immediately):
├── Task 1: Model name 请求体重写 [quick]
├── Task 2: /v1/messages/count_tokens 转发 [quick]
├── Task 3: /v1/images/edits task 识别 [quick]
└── Task 4: /v1/videos 端点路由 [quick]

Wave 2 (P1 — Competitive Edge, after Wave 1):
├── Task 5: 多维度可组合调度 [deep]
├── Task 6: Error classification 可配置化 [unspecified-high]
├── Task 7: Error passthrough 可配置 [unspecified-high]
└── Task 8: TTFT 超时强制 [unspecified-high]

Wave 3 (P2 — Experience Enhancement, after Wave 2):
├── Task 9: Per-endpoint body rules [unspecified-high]
├── Task 10: Gemini Files 代理 [deep]
├── Task 11: /v1/models 转发到 Go [quick] (Split)
├── Task 12: Auth 失败 IP 独立限流 [unspecified-high]
└── Task 13: /healthz/active-requests [quick] (Split)

Wave 4 (P3 — Optional Enhancements, after Wave 3):
├── Task 14: Health quota probe 增强 [unspecified-high]
├── Task 15: Adaptive reservation [deep]
├── Task 16: usage:records candidate chain 字段 [quick] (Split)
├── Task 17: Scheduling presets 配置执行 [unspecified-high] (Split)
└── Task 18: Auto-ban 连续失败上报 [quick] (Split)

Wave 5 (Go 契约文档, parallel with Wave 3-4):
└── Task 19: Go 契约变更文档汇总 [writing]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── F1. Plan compliance audit (oracle)
├── F2. Code quality review (unspecified-high)
├── F3. Real manual QA (unspecified-high)
└── F4. Scope fidelity check (deep)
→ Present results → Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | — | 5, 9 | 1 |
| 2 | — | — | 1 |
| 3 | — | — | 1 |
| 4 | — | — | 1 |
| 5 | 1 | 17 | 2 |
| 6 | — | — | 2 |
| 7 | — | — | 2 |
| 8 | — | — | 2 |
| 9 | 1 | — | 3 |
| 10 | — | — | 3 |
| 11 | — | 19 | 3 |
| 12 | — | — | 3 |
| 13 | — | 19 | 3 |
| 14 | — | — | 4 |
| 15 | — | — | 4 |
| 16 | — | 19 | 4 |
| 17 | 5 | 19 | 4 |
| 18 | — | 19 | 4 |
| 19 | 11, 13, 16, 17, 18 | F1-F4 | 5 |
| F1-F4 | 1-19 | — | FINAL |

### Agent Dispatch Summary

- **Wave 1**: **4 tasks** — T1-T4 → `quick`
- **Wave 2**: **4 tasks** — T5 → `deep`, T6-T8 → `unspecified-high`
- **Wave 3**: **5 tasks** — T9 → `unspecified-high`, T10 → `deep`, T11 → `quick`, T12 → `unspecified-high`, T13 → `quick`
- **Wave 4**: **5 tasks** — T14 → `unspecified-high`, T15 → `deep`, T16 → `quick`, T17 → `unspecified-high`, T18 → `quick`
- **Wave 5**: **1 task** — T19 → `writing`
- **FINAL**: **4 tasks** — F1 → `oracle`, F2-F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [ ] 1. Model Name 请求体重写

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/` 的 request rewrite 阶段，当 `ModelMapping.default_upstream_model` 非空时，将 `ir.model` 替换为 upstream model name
  - 替换发生在 key 选择之后、upstream 发送之前（`upstream_request_filter` 阶段）
  - 响应中保留原始 client model name（不改写 response.model）
  - 添加 Prometheus counter `gw_model_rewrite_total{from, to}`
  - 在 `config/default.toml` 添加 `[model_rewrite]` 段，`enabled = true`（默认开启，因为只在 ModelMapping 有 upstream_model 时触发）
  - 补充单元测试 + 集成测试

  **Must NOT do**:
  - 不改动 `ModelMapping` 数据结构本身
  - 不重构 model name 在 codec 层的通用处理
  - 不修改 `gw-codec` crate（重写发生在 proxy filter 层）
  - 不触碰 `forced_params` 逻辑

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 单点改动，涉及 1-2 个文件的 filter 逻辑修改
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4)
  - **Blocks**: Tasks 5, 9
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/rewrite.rs` — 现有 request rewrite filter，forced_params 替换逻辑在此
  - `crates/gw-proxy/src/filters/upstream.rs` — upstream_request_filter 阶段
  - `crates/gw-store/src/entity.rs` — `ModelMapping` 结构体，含 `default_upstream_model` 字段

  **API/Type References**:
  - `crates/gw-core/src/ir.rs` — `IR` 结构体，`ir.model` 是需要被替换的字段
  - `crates/gw-core/src/ctx.rs` — `ReqCtx`，携带 model_mapping 信息

  **Test References**:
  - `crates/gw-proxy/tests/` — 现有 proxy filter 测试模式

  **External References**:
  - 无外部依赖

  **WHY Each Reference Matters**:
  - `rewrite.rs`: 学习 filter 中修改 IR 的模式（forced_params 如何在 request 阶段修改 IR 字段）
  - `entity.rs` → ModelMapping: 确认 `default_upstream_model` 的 Option<String> 语义
  - `ir.rs`: 理解 `ir.model` 在 IR 中的位置和类型

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-proxy` 新增 model_rewrite 测试通过
  - [ ] 当 ModelMapping 有 `default_upstream_model` 时，上游请求体中 model 字段被替换
  - [ ] 当 ModelMapping 无 `default_upstream_model` 时，model 字段不变
  - [ ] 响应体中 model 字段保持客户端原始值
  - [ ] `gw_model_rewrite_total` counter 在替换发生时递增

  **QA Scenarios**:

  ```
  Scenario: Happy path — model name rewritten to upstream
    Tool: Bash (curl + cargo test)
    Preconditions: ModelMapping 配置 {client_model: "gpt-4-turbo", default_upstream_model: "gpt-4-0613"}
    Steps:
      1. cargo test -p gw-proxy --test model_rewrite -- --nocapture
      2. 验证测试中 IR.model 从 "gpt-4-turbo" 变为 "gpt-4-0613"
      3. 验证 response model 保持 "gpt-4-turbo"
    Expected Result: 测试全绿，IR model 被正确替换
    Failure Indicators: IR.model 未变 / response model 被错误替换
    Evidence: .omo/evidence/task-1-model-rewrite-happy.txt

  Scenario: No upstream model configured — pass through
    Tool: Bash (cargo test)
    Preconditions: ModelMapping 配置 {client_model: "gpt-4", default_upstream_model: None}
    Steps:
      1. cargo test -p gw-proxy --test model_rewrite_passthrough
      2. 验证 IR.model 保持 "gpt-4" 不变
    Expected Result: model 字段不被修改
    Failure Indicators: model 字段被意外修改
    Evidence: .omo/evidence/task-1-model-rewrite-passthrough.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): model name rewrite via ModelMapping.default_upstream_model`
  - Files: `crates/gw-proxy/src/filters/rewrite.rs`, `crates/gw-core/src/ir.rs` (if needed), tests
  - Pre-commit: `cargo test -p gw-proxy && cargo clippy -p gw-proxy`

- [ ] 2. /v1/messages/count_tokens 端点转发

  **What to do**:
  - 在 `crates/gw-codec/` 的 Anthropic codec 路径识别中，新增 `/v1/messages/count_tokens` 路径
  - 该路径作为 non-streaming 请求直接透传到上游 Anthropic API
  - 不计入 `usage:records`（纯元数据查询，不消耗 token）
  - 鉴权流程与普通请求相同（api-key pair / JWT）
  - 限流照常（per-key RPM）

  **Must NOT do**:
  - 不在 Rust 侧实现 token 计数逻辑
  - 不添加 billing/usage 记录
  - 不修改 Anthropic Messages codec 主路径

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 新增一个路径识别 + 透传，模式已存在
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3, 4)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/anthropic/` — Anthropic codec，路径匹配逻辑
  - `crates/gw-proxy/src/filters/task.rs` — 现有路径识别模式

  **API/Type References**:
  - Anthropic API: `POST /v1/messages/count_tokens` — 接受 Messages 格式 body，返回 `{"input_tokens": N}`

  **WHY Each Reference Matters**:
  - Anthropic codec: 学习路径匹配和格式识别模式
  - task.rs: 理解非标准路径如何被识别和分发

  **Acceptance Criteria**:

  - [ ] `/v1/messages/count_tokens` POST 请求被正确识别为 Anthropic 格式
  - [ ] 请求透传到上游，响应原样返回
  - [ ] 无 `usage:records` XADD（验证 Redis stream 无新条目）
  - [ ] 鉴权正常工作（无 key → 401）

  **QA Scenarios**:

  ```
  Scenario: Happy path — count_tokens proxied to upstream
    Tool: Bash (curl)
    Preconditions: 网关运行，有效 API key 配置
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/messages/count_tokens \
           -H "x-api-key: test-key-001" -H "x-api-key-id: ak_test001" \
           -H "Content-Type: application/json" \
           -d '{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hello"}]}'
      2. 验证 HTTP 200
      3. 验证响应 JSON 包含 "input_tokens" 字段
    Expected Result: HTTP 200, {"input_tokens": <integer>}
    Failure Indicators: 404 / 路径未被识别 / 被当作普通 Messages 请求处理
    Evidence: .omo/evidence/task-2-count-tokens-happy.json

  Scenario: Unauthenticated request rejected
    Tool: Bash (curl)
    Preconditions: 网关运行
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/messages/count_tokens \
           -H "Content-Type: application/json" \
           -d '{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hello"}]}'
      2. 验证 HTTP 401
    Expected Result: HTTP 401, GW_AUTH_* error code
    Failure Indicators: HTTP 200 / 无鉴权拦截
    Evidence: .omo/evidence/task-2-count-tokens-unauth.json
  ```

  **Commit**: YES
  - Message: `feat(codec): add /v1/messages/count_tokens passthrough`
  - Files: `crates/gw-codec/src/anthropic/`, tests
  - Pre-commit: `cargo test -p gw-codec`

- [ ] 3. /v1/images/edits Task 识别

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/task.rs` 的 task detector 中，新增 `/v1/images/edits` 路径识别
  - 复用现有 `/v1/images/generations` 的 task 提交模式
  - Multipart 请求体需正确转发（image + mask + prompt）
  - 添加对应的 TaskKind variant（如 `ImageEdit`）

  **Must NOT do**:
  - 不实现图片处理逻辑
  - 不修改 `/v1/images/generations` 现有路径
  - 不添加新的 multipart 解析库

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 扩展现有 task detector 模式，单文件改动
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 4)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/task.rs:46` — 现有 images/generations task 识别逻辑
  - `crates/gw-core/src/ctx.rs` — TaskKind enum

  **WHY Each Reference Matters**:
  - task.rs:46: 直接复制 generations 模式，改为 edits 路径
  - ctx.rs: 需要新增 TaskKind variant

  **Acceptance Criteria**:

  - [ ] `/v1/images/edits` POST 被识别为 image task
  - [ ] Multipart body 正确透传到上游
  - [ ] 新 TaskKind variant 编译通过，match exhaustive

  **QA Scenarios**:

  ```
  Scenario: Happy path — images/edits recognized as task
    Tool: Bash (cargo test)
    Preconditions: task detector 测试环境
    Steps:
      1. cargo test -p gw-proxy --test task_detector -- images_edits
      2. 验证 /v1/images/edits 被识别为 TaskKind::ImageEdit
    Expected Result: 路径正确识别，task 提交成功
    Failure Indicators: 路径 404 / 被误识别为其他类型
    Evidence: .omo/evidence/task-3-images-edits-happy.txt

  Scenario: Non-POST method rejected
    Tool: Bash (cargo test)
    Steps:
      1. 验证 GET /v1/images/edits 返回 405
    Expected Result: 仅接受 POST
    Evidence: .omo/evidence/task-3-images-edits-method.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): recognize /v1/images/edits as image task`
  - Files: `crates/gw-proxy/src/filters/task.rs`, `crates/gw-core/src/ctx.rs`
  - Pre-commit: `cargo test -p gw-proxy -p gw-core`

- [ ] 4. /v1/videos 端点路由（Task 模式）

  **What to do**:
  - 在 task detector 中新增 `/v1/videos/generations` 路径识别（submit）
  - 新增 `/v1/videos/generations/{id}` 路径识别（poll result）
  - 复用现有 task 提交 + 轮询机制
  - 确保 TaskKind 包含 `VideoGeneration` variant
  - 不涉及视频流传输，仅 JSON task 提交和结果查询

  **Must NOT do**:
  - 不实现视频流上传/下载
  - 不添加视频处理逻辑
  - 不修改现有 task 框架核心

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 扩展 task detector，模式复用
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 3)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/task.rs` — task detector 和 task 提交/轮询模式
  - `crates/gw-core/src/ctx.rs` — TaskKind enum，已有 "video" 字符串映射

  **WHY Each Reference Matters**:
  - task.rs: task 提交和轮询的完整模式在此
  - ctx.rs: 已有 video TaskKind 映射基础

  **Acceptance Criteria**:

  - [ ] `/v1/videos/generations` POST 被识别为 video task
  - [ ] `/v1/videos/generations/{id}` GET 被识别为 task poll
  - [ ] TaskKind::VideoGeneration variant 编译通过

  **QA Scenarios**:

  ```
  Scenario: Happy path — video task submission
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-proxy --test task_detector -- video
      2. 验证 POST /v1/videos/generations 识别为 VideoGeneration task
      3. 验证 GET /v1/videos/generations/vid_xxx 识别为 task poll
    Expected Result: 路径正确识别，task 提交成功
    Failure Indicators: 路径 404 / 未识别为 task 类型
    Evidence: .omo/evidence/task-4-videos-happy.txt

  Scenario: Invalid video task path
    Tool: Bash (cargo test)
    Steps:
      1. 验证 POST /v1/videos (无 /generations) 不被识别
    Expected Result: 非法路径不触发 task detector
    Evidence: .omo/evidence/task-4-videos-invalid-path.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): add /v1/videos/generations task routing`
  - Files: `crates/gw-proxy/src/filters/task.rs`, `crates/gw-core/src/ctx.rs`
  - Pre-commit: `cargo test -p gw-proxy -p gw-core`

- [ ] 5. 多维度可组合调度框架

  **What to do**:
  - 在 `crates/gw-routing/` 新增 `MultiDimStrategy`，允许对候选 key 按多维度加权评分
  - 维度包括：health_score, latency_p99, cost_per_token, quota_remaining, in_flight_ratio
  - 每个维度有 0.0-1.0 权重，通过 `config/default.toml` 的 `[routing.multi_dim]` 段配置
  - 归一化各维度到 [0, 1] 后加权求和，选最高分候选
  - 新策略作为第 9 种 Strategy variant（`MultiDim`），不修改现有 8 种策略逻辑
  - 当 model_group 选择 `MultiDim` 策略时生效，否则走原有互斥策略
  - `config/default.toml` 新增段：
    ```toml
    [routing.multi_dim]
    enabled = false  # 默认关闭
    weights = { health = 0.3, latency = 0.3, cost = 0.2, quota = 0.1, inflight = 0.1 }
    ```

  **Must NOT do**:
  - 不修改现有 8 种策略的实现代码
  - 不改变 Strategy trait 接口（新增 variant 即可）
  - 不引入新依赖
  - 不做 cost computation（读 pricing metadata 的 relative cost weight 即可）

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 需要理解路由系统架构，设计归一化算法，确保不破坏现有策略
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 6, 7, 8)
  - **Blocks**: Task 17 (scheduling presets)
  - **Blocked By**: Task 1 (model rewrite，因为共享 rewrite 阶段上下文)

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/strategy/` — 现有 8 种策略实现，特别是 `cost_first.rs` 和 `latency_first.rs`
  - `crates/gw-routing/src/candidate.rs` — 候选 key 筛选逻辑
  - `crates/gw-routing/src/health.rs` — KeyHealth 结构体（health score 来源）

  **API/Type References**:
  - `crates/gw-core/src/entity.rs` — VendorKey 实体，含 health/latency/pricing 信息
  - `crates/gw-routing/src/strategy/mod.rs` — Strategy enum 定义

  **WHY Each Reference Matters**:
  - strategy/: 学习策略实现模式，确保新策略遵循相同 trait
  - health.rs: health score 是多维度评分的数据源
  - candidate.rs: 理解候选池如何传入策略

  **Acceptance Criteria**:

  - [ ] `MultiDim` strategy variant 编译通过
  - [ ] 当 model_group 使用 MultiDim 策略时，按加权评分选 key
  - [ ] 当使用其他策略时，行为完全不变
  - [ ] 权重配置从 `default.toml` 正确加载
  - [ ] 所有维度归一化到 [0,1]，加权和正确

  **QA Scenarios**:

  ```
  Scenario: MultiDim strategy selects highest-score key
    Tool: Bash (cargo test)
    Preconditions: 3 个候选 key，不同 health/latency/cost 值
    Steps:
      1. cargo test -p gw-routing --test multi_dim_strategy
      2. 验证 key 按加权评分排序选择
      3. 验证权重变化导致不同 key 被选中
    Expected Result: 加权评分最高的 key 被选中
    Failure Indicators: 选择了错误的 key / panic / 除零
    Evidence: .omo/evidence/task-5-multi-dim-happy.txt

  Scenario: All weights zero — fallback behavior
    Tool: Bash (cargo test)
    Steps:
      1. 设置所有权重为 0.0
      2. 验证不 panic，选择第一个候选
    Expected Result: 安全 fallback，不 panic
    Evidence: .omo/evidence/task-5-multi-dim-zero-weights.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): add MultiDim composable scheduling strategy`
  - Files: `crates/gw-routing/src/strategy/multi_dim.rs`, `crates/gw-routing/src/strategy/mod.rs`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 6. Error Classification 可配置化

  **What to do**:
  - 将 `crates/gw-routing/src/failover.rs` 中硬编码的 429+500-599 重试规则提取为配置
  - 在 `config/default.toml` 新增 `[error_classification]` 段：
    ```toml
    [error_classification]
    retry_status_codes = [429, 500, 502, 503, 504]
    failover_status_codes = [429, 500, 502, 503]
    no_retry_status_codes = [400, 401, 403, 404]
    ```
  - 将 `classify_default.rs` 的硬编码分类改为读取 config
  - 保持 boot-time only（配置在 App 初始化时加载，不运行时变更）
  - 添加 config validation：retry 和 no_retry 不能有交集

  **Must NOT do**:
  - 不重新设计 `gw-core/src/error/codes.rs` 错误码体系
  - 不修改 error 枚举本身
  - 不添加 per-provider 级别的 error classification（scope creep）
  - 不引入正则匹配响应体内容

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及 config 加载链 + failover 逻辑修改，需仔细保持兼容
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5, 7, 8)
  - **Blocks**: None (soft: Task 7 benefits from reviewing Task 6's changes first)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/failover.rs:35-44` — 硬编码重试状态码
  - `crates/gw-routing/src/classify_default.rs` — 硬编码错误分类逻辑
  - `crates/gw-config/` — Figment config 加载模式

  **API/Type References**:
  - `config/default.toml` — 现有配置结构
  - `crates/gw-core/src/error/codes.rs` — GW_* 错误码（不修改，仅参考）

  **WHY Each Reference Matters**:
  - failover.rs:35-44: 这是需要从硬编码改为配置读取的确切位置
  - config 加载: 学习如何通过 Figment 加载新段

  **Acceptance Criteria**:

  - [ ] 默认配置行为与当前硬编码完全一致（向后兼容）
  - [ ] 修改 config 后重启，新的状态码分类生效
  - [ ] retry 和 no_retry 交集 → 启动报错
  - [ ] 未配置时使用硬编码默认值

  **QA Scenarios**:

  ```
  Scenario: Default config matches current hardcoded behavior
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-routing --test error_classification
      2. 验证 429 → retry, 500-504 → retry, 400 → no_retry
    Expected Result: 与当前行为完全一致
    Evidence: .omo/evidence/task-6-error-class-default.txt

  Scenario: Custom status code added to retry list
    Tool: Bash (cargo test)
    Steps:
      1. 配置 retry_status_codes 包含 408
      2. 验证 408 触发重试
    Expected Result: 408 被正确分类为 retry
    Evidence: .omo/evidence/task-6-error-class-custom.txt

  Scenario: Conflicting config rejected at startup
    Tool: Bash (cargo test)
    Steps:
      1. 配置 retry_status_codes = [429] 且 no_retry_status_codes = [429]
      2. 验证启动报错
    Expected Result: Config validation error，进程不启动
    Evidence: .omo/evidence/task-6-error-class-conflict.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): configurable error classification for retry/failover`
  - Files: `crates/gw-routing/src/failover.rs`, `crates/gw-routing/src/classify_default.rs`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 7. Error Passthrough 可配置

  **What to do**:
  - 在 `config/default.toml` 新增 `[error_handling]` 段：
    ```toml
    [error_handling]
    passthrough_upstream_errors = false  # 默认不透传
    passthrough_status_codes = []        # 可选：仅透传特定状态码的原始响应
    ```
  - 当 `passthrough_upstream_errors = true` 时，上游非 2xx 响应直接透传给客户端（不包装为 GW_* 错误信封）
  - 当 `passthrough_status_codes` 非空时，仅对指定状态码透传
  - 修改 `crates/gw-proxy/src/filters/` 中的 error handler，增加透传分支
  - 透传模式下仍记录 `usage:records`（status=error + 原始 http_status）

  **Must NOT do**:
  - 不修改 GW_* 错误码体系
  - 不删除或修改现有 error 包装逻辑（仅新增分支）
  - 不做 per-provider 配置
  - 不透传 2xx 响应中的 error 字段（那是正常响应）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及 error fanout 路径修改，需理解 workflow.md 中的错误处理链
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5, 6, 8)
  - **Blocks**: None
  - **Blocked By**: None (soft preference: start after Task 6 for shared error context)

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/error_passthrough.rs` — 现有 error 包装逻辑
  - `docs/workflow.md` §error fanout — 错误处理链文档

  **API/Type References**:
  - `crates/gw-core/src/error/` — 错误类型定义

  **WHY Each Reference Matters**:
  - error_passthrough.rs: 理解当前包装逻辑，在此基础上添加 bypass 分支
  - workflow.md: 确保不破坏 error fanout 的既定顺序

  **Acceptance Criteria**:

  - [ ] 默认行为不变（passthrough_upstream_errors = false）
  - [ ] 开启后，上游 4xx/5xx 原始 body 直接返回客户端
  - [ ] `usage:records` 仍正确记录（error status + 原始 http_status）
  - [ ] `passthrough_status_codes` 筛选正确工作

  **QA Scenarios**:

  ```
  Scenario: Passthrough disabled — error wrapped as usual
    Tool: Bash (cargo test)
    Steps:
      1. passthrough_upstream_errors = false
      2. 上游返回 500 + JSON error body
      3. 验证客户端收到 GW_UPSTREAM_5XX 包装响应
    Expected Result: 标准 GW_* 错误信封
    Evidence: .omo/evidence/task-7-passthrough-disabled.txt

  Scenario: Passthrough enabled — raw upstream error forwarded
    Tool: Bash (cargo test)
    Steps:
      1. passthrough_upstream_errors = true
      2. 上游返回 500 + {"error": "internal failure"}
      3. 验证客户端收到原始 {"error": "internal failure"} + status 500
    Expected Result: 原始 upstream 响应透传
    Evidence: .omo/evidence/task-7-passthrough-enabled.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): configurable error passthrough for upstream errors`
  - Files: `crates/gw-proxy/src/filters/error_passthrough.rs`, config, tests
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 8. TTFT 超时强制

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/` 的流式响应处理中，添加 TTFT（Time To First Token）超时
  - 当前 TTFT 已被测量（metric `gw_ttft_seconds`），但不作为中断条件
  - 在 `config/default.toml` 新增：
    ```toml
    [timeouts]
    ttft_timeout_ms = 30000  # 首 token 超时，默认 30s
    ```
  - 超时触发时：
    1. 取消上游连接
    2. 返回 `GW_UPSTREAM_TTFT_TIMEOUT` 错误码（需要在 codes.rs 新增）
    3. 触发 failover（如果有重试预算）
    4. 记录 `usage:records`（status=error, error_code=GW_UPSTREAM_TTFT_TIMEOUT）
  - `ttft_timeout_ms = 0` 表示禁用（向后兼容）

  **Must NOT do**:
  - 不修改已有的 `stream_total` / `total` 超时逻辑
  - 不改变 TTFT metric 的测量方式
  - 不修改非流式请求的超时行为

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及流式响应的取消路径，需理解 Pingora upstream 连接管理
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5, 6, 7)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/` — 流式响应处理 filter
  - `crates/gw-core/src/timeouts.rs` — 现有超时配置结构（`stream_total`, `total`）
  - `crates/gw-core/src/ctx.rs` — TTFT 测量逻辑位置

  **API/Type References**:
  - `crates/gw-core/src/error/codes.rs` — 需新增 `GW_UPSTREAM_TTFT_TIMEOUT` 错误码

  **WHY Each Reference Matters**:
  - timeouts.rs: 理解现有超时结构，在同一位置添加 TTFT timeout
  - 流式 filter: 确定在哪个生命周期点检查 TTFT

  **Acceptance Criteria**:

  - [ ] 流式请求首 token 超过 ttft_timeout_ms → 返回 GW_UPSTREAM_TTFT_TIMEOUT
  - [ ] 首 token 在超时内到达 → 正常处理
  - [ ] ttft_timeout_ms = 0 → 禁用（现有行为）
  - [ ] 超时触发 failover（如有重试预算）
  - [ ] `usage:records` 记录正确的 error_code
  - [ ] `gw_ttft_timeout_total` counter 递增

  **QA Scenarios**:

  ```
  Scenario: TTFT within limit — normal streaming
    Tool: Bash (cargo test)
    Steps:
      1. ttft_timeout_ms = 5000
      2. 上游 100ms 内返回首 token
      3. 验证流式响应正常完成
    Expected Result: 正常流式响应
    Evidence: .omo/evidence/task-8-ttft-normal.txt

  Scenario: TTFT exceeded — timeout error
    Tool: Bash (cargo test)
    Steps:
      1. ttft_timeout_ms = 100
      2. 上游模拟 500ms 延迟才返回首 token
      3. 验证返回 GW_UPSTREAM_TTFT_TIMEOUT
    Expected Result: 100ms 后返回超时错误
    Failure Indicators: 请求不中断 / 错误码不对
    Evidence: .omo/evidence/task-8-ttft-timeout.txt

  Scenario: TTFT disabled — no enforcement
    Tool: Bash (cargo test)
    Steps:
      1. ttft_timeout_ms = 0
      2. 上游模拟 60s 延迟
      3. 验证仍等待（不因 TTFT 超时）
    Expected Result: 不触发 TTFT 超时
    Evidence: .omo/evidence/task-8-ttft-disabled.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): enforce TTFT timeout for streaming requests`
  - Files: `crates/gw-core/src/timeouts.rs`, `crates/gw-core/src/error/codes.rs`, `crates/gw-proxy/src/filters/`, config, tests
  - Pre-commit: `cargo test -p gw-core -p gw-proxy`

- [ ] 9. Per-endpoint Body Rules

  **What to do**:
  - 扩展 `forced_params` 机制，从仅支持 temperature/top_p 到支持任意 JSON 字段的默认值设置
  - 在 `config/default.toml` 新增 `[body_rules]` 段：
    ```toml
    [[body_rules.rules]]
    path_prefix = "/v1/chat/completions"
    defaults = { max_tokens = 4096, temperature = 0.7 }
    overrides = { stream = true }  # overrides 强制覆盖用户值
    ```
  - `defaults`: 仅在用户请求体中没有该字段时注入
  - `overrides`: 无条件覆盖用户请求体中的字段
  - 在 `crates/gw-proxy/src/filters/rewrite.rs` 中扩展 `apply_forced_params_to_ir` 或新增 `apply_body_rules`
  - 操作对象是 JSON body（不是 IR），因为需要覆盖任意字段
  - 添加 mutation tracking metric `gw_body_rules_applied_total{path, field, action}`

  **Must NOT do**:
  - 不实现正则/JSONPath DSL — 仅 flat key-value 操作
  - 不支持嵌套路径（如 `messages[0].content`）— 仅顶层字段
  - 不修改 IR 类型定义
  - 不引入 ReDoS 风险（path_prefix 是 literal match，不是 regex）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及请求体修改逻辑，需要理解 forced_params 和 rewrite 管线
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 10, 11, 12, 13)
  - **Blocks**: None
  - **Blocked By**: Task 1 (共享 rewrite 阶段)

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/rewrite.rs` — `apply_forced_params_to_ir` 当前实现
  - `crates/gw-core/src/entity.rs` — ForcedParams 结构体

  **WHY Each Reference Matters**:
  - rewrite.rs: 直接扩展现有 forced_params 逻辑
  - entity.rs: 理解当前 ForcedParams 仅含 temperature/top_p 的限制

  **Acceptance Criteria**:

  - [ ] `defaults` 仅在字段缺失时注入
  - [ ] `overrides` 无条件覆盖
  - [ ] `path_prefix` 匹配正确（literal，非 regex）
  - [ ] 无匹配规则时行为不变
  - [ ] mutation tracking metric 正确

  **QA Scenarios**:

  ```
  Scenario: Default value injected when field missing
    Tool: Bash (cargo test)
    Steps:
      1. 配置 defaults = { max_tokens = 4096 } for /v1/chat/completions
      2. 发送请求不含 max_tokens
      3. 验证上游收到 max_tokens = 4096
    Expected Result: 字段被注入
    Evidence: .omo/evidence/task-9-body-rules-default.txt

  Scenario: Override replaces user value
    Tool: Bash (cargo test)
    Steps:
      1. 配置 overrides = { stream = true }
      2. 发送请求 stream = false
      3. 验证上游收到 stream = true
    Expected Result: 字段被强制覆盖
    Evidence: .omo/evidence/task-9-body-rules-override.txt

  Scenario: No matching rule — pass through unchanged
    Tool: Bash (cargo test)
    Steps:
      1. 配置 path_prefix = "/v1/images"
      2. 发送 /v1/chat/completions 请求
      3. 验证请求体不变
    Expected Result: 无匹配时不修改
    Evidence: .omo/evidence/task-9-body-rules-nomatch.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): per-endpoint body rules (defaults + overrides)`
  - Files: `crates/gw-proxy/src/filters/rewrite.rs`, config, tests
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 10. Gemini Files 代理

  **What to do**:
  - 为 Gemini Files API 添加完整的 HTTP 代理路径：
    - `POST /upload/v1beta/files` — 文件上传（multipart）
    - `GET /v1beta/files` — 列出文件
    - `GET /v1beta/files/{name}` — 获取文件元数据
    - `DELETE /v1beta/files/{name}` — 删除文件
  - 在 codec 层识别这些路径，通过 Gemini capable key 选择上游
  - 复用 `GeminiFilesGateway` trait（已定义但仅有 FakeFilesGateway 实现）
  - 文件上传需正确透传 multipart body（不解析，直接 stream forward）
  - 记录 file mapping 到 `gemini_file_mapping` 表（migration 已存在）

  **Must NOT do**:
  - 不实现文件内容处理/转换
  - 不做文件大小限制（由上游 Gemini API 控制）
  - 不新建 migration（使用已有 0002_gemini_file_mapping.sql）
  - 不实现文件缓存

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 多端点 + multipart + 数据库映射，复杂度较高
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 9, 11, 12, 13)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/gemini/` — Gemini codec 路径识别
  - `crates/gw-store/migrations/0002_gemini_file_mapping.sql` — 已有 migration
  - `crates/gw-core/src/gemini_files.rs` (或类似) — `GeminiFilesGateway` trait

  **API/Type References**:
  - Gemini Files API: https://ai.google.dev/api/files

  **WHY Each Reference Matters**:
  - Gemini codec: 路径匹配模式
  - migration: 不需新建，但需理解 file_mapping 表结构
  - trait: 提供真实实现替代 FakeFilesGateway

  **Acceptance Criteria**:

  - [ ] 4 个 Files API 路径全部被 codec 识别
  - [ ] Upload multipart 正确透传
  - [ ] File mapping 记录写入 PgSQL
  - [ ] 鉴权 + key 选择正常工作

  **QA Scenarios**:

  ```
  Scenario: List files via Gemini API
    Tool: Bash (cargo test)
    Steps:
      1. GET /v1beta/files with valid Gemini key
      2. 验证透传到上游 + 响应返回
    Expected Result: HTTP 200, files array
    Evidence: .omo/evidence/task-10-gemini-files-list.txt

  Scenario: Upload file multipart
    Tool: Bash (cargo test)
    Steps:
      1. POST /upload/v1beta/files with multipart body
      2. 验证 file mapping 记录写入
    Expected Result: Upload 成功, mapping 记录存在
    Evidence: .omo/evidence/task-10-gemini-files-upload.txt
  ```

  **Commit**: YES
  - Message: `feat(codec): implement Gemini Files API proxy (upload/list/get/delete)`
  - Files: `crates/gw-codec/src/gemini/`, `crates/gw-store/`, `crates/gw-proxy/`, tests
  - Pre-commit: `cargo test -p gw-codec -p gw-store -p gw-proxy`

- [ ] 11. /v1/models 转发到 Go (Split)

  **What to do**:
  - 在 codec 层识别 `GET /v1/models` 和 `GET /v1/models/{model_id}` 路径
  - 将这些请求转发到 Go admin API（类似现有 `/admin/*` 的 reverse proxy 模式）
  - Go 侧负责组装模型列表（基于权限、定价、可用性）
  - Rust 侧仅做：鉴权 → 转发到 Go → 响应透传
  - Go 契约：在 `docs/go-contracts/contract-change/2026-05-26-feature-parity.md` 记录

  **Must NOT do**:
  - 不在 Rust 侧组装模型列表
  - 不查询 PgSQL model 表（Go 负责）
  - 不修改 `/admin/*` 转发逻辑（新增路径即可）

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 复用现有 admin proxy 模式，单点改动
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 9, 10, 12, 13)
  - **Blocks**: Task 19 (Go contract doc)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/admin.rs` — `/admin/*` reverse proxy 逻辑

  **WHY Each Reference Matters**:
  - admin.rs: 直接复制 admin proxy 模式用于 /v1/models

  **Acceptance Criteria**:

  - [ ] `GET /v1/models` 转发到 Go admin API
  - [ ] `GET /v1/models/{id}` 转发到 Go admin API
  - [ ] 鉴权正常（api-key pair / JWT）
  - [ ] Go 契约文档已更新

  **QA Scenarios**:

  ```
  Scenario: Model list proxied to Go
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1/models \
           -H "x-api-key: test-key-001" -H "x-api-key-id: ak_test001"
      2. 验证请求到达 Go admin API
    Expected Result: 请求被转发，响应透传
    Evidence: .omo/evidence/task-11-models-proxy.json

  Scenario: Unauthenticated model list rejected
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1/models (无 auth header)
      2. 验证 HTTP 401
    Expected Result: 401 GW_AUTH_*
    Evidence: .omo/evidence/task-11-models-unauth.json
  ```

  **Commit**: YES
  - Message: `feat(proxy): proxy /v1/models to Go admin API`
  - Files: `crates/gw-proxy/src/filters/admin.rs` (或新文件), tests
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 12. Auth 失败 IP 独立限流

  **What to do**:
  - 在 `crates/gw-auth/` 中添加 auth-failure-specific IP rate limiter
  - 与通用 per-IP RPM 分离：auth 失败使用更严格的限制
  - 在 `config/default.toml` 新增：
    ```toml
    [auth.ip_limiting]
    enabled = false
    max_auth_failures_per_ip = 10     # 每 IP 每窗口最大失败次数
    window_seconds = 300              # 5 分钟窗口
    ban_duration_seconds = 600        # 超限后封禁 10 分钟
    ```
  - 使用 Moka cache 存储 IP → failure_count（内存，不持久化）
  - 封禁期间该 IP 所有请求直接返回 `GW_AUTH_IP_BANNED`（429）
  - 新增 metric `gw_auth_ip_ban_total`

  **Must NOT do**:
  - 不修改通用 per-IP RPM limiter
  - 不持久化到 Redis（内存即可，重启清零）
  - 不支持 CIDR（仅单 IP，后续可扩展）
  - 不支持 X-Forwarded-For 信任链（使用连接 IP）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 新增 rate limiter 模块，需集成到 auth filter chain
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/` — 认证模块
  - `crates/gw-ratelimit/` — 现有限流模块模式

  **Acceptance Criteria**:

  - [ ] Auth 失败累计超限 → IP 被 ban
  - [ ] Ban 期间所有请求返回 429 GW_AUTH_IP_BANNED
  - [ ] 窗口过期后计数器重置
  - [ ] 默认 disabled
  - [ ] 不影响通用 RPM limiter

  **QA Scenarios**:

  ```
  Scenario: IP banned after repeated auth failures
    Tool: Bash (cargo test)
    Steps:
      1. enabled = true, max_auth_failures_per_ip = 3
      2. 同一 IP 发送 4 次 invalid key 请求
      3. 第 4 次验证返回 429 GW_AUTH_IP_BANNED
    Expected Result: 第 4 次被 ban
    Evidence: .omo/evidence/task-12-auth-ip-ban.txt

  Scenario: Different IPs independent
    Tool: Bash (cargo test)
    Steps:
      1. IP-A 发送 3 次失败
      2. IP-B 发送正常请求
      3. 验证 IP-B 不受影响
    Expected Result: IP 隔离
    Evidence: .omo/evidence/task-12-auth-ip-isolation.txt
  ```

  **Commit**: YES
  - Message: `feat(auth): auth-failure IP rate limiting with ban`
  - Files: `crates/gw-auth/src/`, config, tests
  - Pre-commit: `cargo test -p gw-auth`

- [ ] 13. /healthz/active-requests 管理端点 (Split)

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/admin.rs`（或 health 模块）新增 `/healthz/active-requests` 端点
  - 返回当前 in-flight 请求的详细信息：
    ```json
    {
      "count": 42,
      "requests": [
        {
          "request_id": "req_xxx",
          "model": "gpt-4",
          "protocol": "openai",
          "strategy": "round_robin",
          "vendor_key_id": "vk_xxx",
          "elapsed_ms": 1234,
          "streaming": true
        }
      ]
    }
    ```
  - Token-gated（与现有 `/healthz/*` 一致）
  - 数据源：遍历当前 in-flight tracking 结构（InflightLedger 或类似）
  - Go 契约：通知 Go 侧可选聚合多网关实例

  **Must NOT do**:
  - 不添加其他 `/healthz/*` 端点
  - 不持久化请求列表
  - 不暴露 API key 明文

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 新增管理端点，复用现有 health 模式
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 19 (Go contract doc)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/admin.rs` — 现有 `/healthz/*` 端点模式
  - `crates/gw-ledger/src/inflight.rs` — InflightLedger（in-flight tracking）

  **Acceptance Criteria**:

  - [ ] `/healthz/active-requests` 返回 JSON 请求列表
  - [ ] Token-gated（无 token → 401）
  - [ ] 不暴露敏感信息
  - [ ] 响应时间 < 50ms

  **QA Scenarios**:

  ```
  Scenario: Active requests returned
    Tool: Bash (curl)
    Steps:
      1. curl -s -H "Authorization: Bearer ${ADMIN_TOKEN}" \
           http://localhost:8080/healthz/active-requests
      2. 验证 JSON 包含 count 和 requests 数组
    Expected Result: HTTP 200, valid JSON
    Evidence: .omo/evidence/task-13-active-requests.json

  Scenario: Unauthorized access rejected
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8080/healthz/active-requests (无 auth)
      2. 验证 401
    Expected Result: HTTP 401
    Evidence: .omo/evidence/task-13-active-requests-unauth.json
  ```

  **Commit**: YES
  - Message: `feat(admin): /healthz/active-requests endpoint`
  - Files: `crates/gw-proxy/src/filters/admin.rs`, tests
  - Pre-commit: `cargo test -p gw-proxy`

- [ ] 14. Health Quota Probe 增强

  **What to do**:
  - 增强 `crates/gw-routing/src/health.rs` 中 half-open 探测机制，添加 quota-specific 逻辑
  - 当 key 因 429 冷却后进入 half-open 状态时，发送一个低成本探测请求（如 `max_tokens=1` 的 chat completion）
  - 探测成功 → 恢复 key；探测失败（仍 429）→ 延长冷却时间（倍数退避）
  - 在 `config/default.toml` 新增：
    ```toml
    [routing.quota_probe]
    enabled = false
    probe_model = "gpt-3.5-turbo"   # 低成本模型
    probe_max_tokens = 1
    backoff_multiplier = 2.0
    max_cooldown_seconds = 3600
    ```
  - 探测请求不计入用户 usage，标记为 `internal_probe = true`

  **Must NOT do**:
  - 不修改非 429 场景的 half-open 逻辑
  - 不发送高成本探测请求
  - 不将探测结果记入 usage:records

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及 health 状态机修改和内部请求发送
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 15, 16, 17, 18)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/health.rs` — KeyHealth 状态机，half-open 逻辑

  **Acceptance Criteria**:

  - [ ] 429 冷却后发送探测请求
  - [ ] 探测成功 → key 恢复
  - [ ] 探测失败 → 退避延长冷却
  - [ ] 默认 disabled
  - [ ] 探测请求不记入 usage

  **QA Scenarios**:

  ```
  Scenario: Quota probe recovers key
    Tool: Bash (cargo test)
    Steps:
      1. enabled = true, key 因 429 进入冷却
      2. 冷却结束，探测请求返回 200
      3. 验证 key 恢复到 healthy
    Expected Result: key 恢复可用
    Evidence: .omo/evidence/task-14-quota-probe-recover.txt

  Scenario: Probe still 429 — extend cooldown
    Tool: Bash (cargo test)
    Steps:
      1. 冷却结束，探测请求仍返回 429
      2. 验证冷却时间加倍
    Expected Result: 冷却时间 × backoff_multiplier
    Evidence: .omo/evidence/task-14-quota-probe-backoff.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): quota-specific probe on cooldown recovery`
  - Files: `crates/gw-routing/src/health.rs`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 15. Adaptive Reservation

  **What to do**:
  - 在 `crates/gw-routing/` 新增 adaptive reservation 模块
  - 基于最近 N 分钟内每个模型的请求频率，为高频模型预留 key capacity
  - 预留 = 在候选筛选时，为高频模型的 key 保留一定的 in-flight 配额不被低频模型占满
  - 在 `config/default.toml` 新增：
    ```toml
    [routing.adaptive_reservation]
    enabled = false
    window_minutes = 5
    reservation_ratio = 0.2   # 预留 20% in-flight cap 给高频模型
    min_requests_threshold = 10  # 最近窗口 >= 10 请求才触发预留
    ```
  - 使用 sliding window counter（已有 `adaptive_rpm` 类似模式）

  **Must NOT do**:
  - 不修改 in-flight cap 核心逻辑
  - 不引入新依赖
  - 不做跨实例协调（单实例内存即可）

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 需要设计预留算法，理解 in-flight cap 与候选筛选的交互
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: None
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/candidate.rs` — in-flight cap 检查
  - `crates/gw-ratelimit/` — adaptive_rpm sliding window 模式

  **Acceptance Criteria**:

  - [ ] 高频模型的 key 有预留 in-flight quota
  - [ ] 低频模型不被预留阻挡（预留外的 capacity 仍可用）
  - [ ] 默认 disabled
  - [ ] 窗口内请求不足阈值 → 不预留

  **QA Scenarios**:

  ```
  Scenario: High-frequency model gets reserved capacity
    Tool: Bash (cargo test)
    Steps:
      1. enabled = true, reservation_ratio = 0.3
      2. model-A 在窗口内有 50 次请求
      3. 验证 model-A 的 key 有 30% in-flight 预留
    Expected Result: 预留生效
    Evidence: .omo/evidence/task-15-reservation-active.txt

  Scenario: Below threshold — no reservation
    Tool: Bash (cargo test)
    Steps:
      1. min_requests_threshold = 100
      2. model-A 窗口内仅 5 次请求
      3. 验证无预留
    Expected Result: 不触发预留
    Evidence: .omo/evidence/task-15-reservation-below-threshold.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): adaptive reservation for high-frequency models`
  - Files: `crates/gw-routing/src/`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 16. usage:records Candidate Chain 字段 (Split)

  **What to do**:
  - 在 `UsageRecord` 中新增 optional 字段 `attempted_keys`:
    ```json
    "attempted_keys": [
      {"vendor_key_id": "vk_001", "attempt": 0, "error_code": null},
      {"vendor_key_id": "vk_002", "attempt": 1, "error_code": "GW_UPSTREAM_5XX"}
    ]
    ```
  - 数据源：`ctx.picked_candidates` / `ctx.retry_blacklist`（已在内存中维护）
  - 在 ledger flusher 组装 UsageRecord 时填充此字段
  - ADDITIVE change: 新字段为 `Option<Vec<AttemptedKey>>`，不影响现有消费者
  - Go 契约更新：在 `parts/02-go-as-consumer.md` §2.1.2 记录新字段

  **Must NOT do**:
  - 不修改现有 UsageRecord 字段
  - 不改变 XADD 序列化格式（仍是 JSON payload 单字段）
  - 不持久化到 PgSQL（Go 决定是否存储）

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 新增一个 optional JSON 字段，代码改动小
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 19 (Go contract doc)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-ledger/src/usage.rs` — UsageRecord 定义
  - `crates/gw-ledger/src/flusher.rs` — XADD 组装逻辑
  - `crates/gw-core/src/ctx.rs` — `picked_candidates` 字段

  **Acceptance Criteria**:

  - [ ] UsageRecord JSON 包含 `attempted_keys` 字段（当有重试时）
  - [ ] 单次成功请求：`attempted_keys` 仅 1 条，`error_code: null`
  - [ ] 多次重试：按 attempt 顺序记录每次尝试
  - [ ] 现有 Go settler 消费不报错（additive field）

  **QA Scenarios**:

  ```
  Scenario: Single attempt — one entry in chain
    Tool: Bash (cargo test)
    Steps:
      1. 单次成功请求
      2. 验证 usage:records 中 attempted_keys 仅 1 条
    Expected Result: [{vendor_key_id: "vk_x", attempt: 0, error_code: null}]
    Evidence: .omo/evidence/task-16-candidate-chain-single.txt

  Scenario: Retry — full chain recorded
    Tool: Bash (cargo test)
    Steps:
      1. 首次 key 返回 500（重试），第二次成功
      2. 验证 attempted_keys 有 2 条
    Expected Result: 完整候选链
    Evidence: .omo/evidence/task-16-candidate-chain-retry.txt
  ```

  **Commit**: YES
  - Message: `feat(ledger): add attempted_keys field to UsageRecord`
  - Files: `crates/gw-ledger/src/usage.rs`, `crates/gw-ledger/src/flusher.rs`, tests
  - Pre-commit: `cargo test -p gw-ledger`

- [ ] 17. Scheduling Presets 配置执行 (Split)

  **What to do**:
  - 在 `config/default.toml` 新增 `[routing.presets]` 段，定义预设策略组合：
    ```toml
    [routing.presets.health_first]
    strategy = "multi_dim"
    weights = { health = 0.6, latency = 0.2, cost = 0.1, quota = 0.05, inflight = 0.05 }

    [routing.presets.cost_first]
    strategy = "multi_dim"
    weights = { health = 0.1, latency = 0.1, cost = 0.6, quota = 0.1, inflight = 0.1 }

    [routing.presets.balanced]
    strategy = "multi_dim"
    weights = { health = 0.2, latency = 0.2, cost = 0.2, quota = 0.2, inflight = 0.2 }
    ```
  - model_group 可以引用 preset 名称代替手动配置权重
  - Preset 在 boot-time 解析为 MultiDim 配置
  - Go 契约：Go 管理面可通过 model_group entity 设置 `scheduling_preset` 字段

  **Must NOT do**:
  - 不硬编码预设（全部来自 config）
  - 不允许运行时修改预设（boot-time only）
  - 不修改 MultiDim 策略核心逻辑

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及 config 解析 + entity 字段映射
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (但依赖 Task 5 的 MultiDim 策略)
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 19 (Go contract doc)
  - **Blocked By**: Task 5 (MultiDim strategy)

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/strategy/multi_dim.rs` — Task 5 产出
  - `crates/gw-config/` — config 解析模式

  **Acceptance Criteria**:

  - [ ] Preset 名称正确解析为 MultiDim weights
  - [ ] 未知 preset → 启动报错
  - [ ] model_group 可引用 preset 名称

  **QA Scenarios**:

  ```
  Scenario: Preset resolved to MultiDim weights
    Tool: Bash (cargo test)
    Steps:
      1. model_group 设置 scheduling_preset = "health_first"
      2. 验证路由使用 health=0.6 权重配置
    Expected Result: Preset 正确解析
    Evidence: .omo/evidence/task-17-preset-resolved.txt

  Scenario: Unknown preset name — startup error
    Tool: Bash (cargo test)
    Steps:
      1. scheduling_preset = "nonexistent"
      2. 验证启动失败
    Expected Result: Config validation error
    Evidence: .omo/evidence/task-17-preset-unknown.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): scheduling presets for multi-dim strategy`
  - Files: `crates/gw-routing/src/`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 18. Auto-ban 连续失败上报 (Split)

  **What to do**:
  - 在 `crates/gw-routing/src/health.rs` 中，当 `consecutive_failures` 超过阈值时，通过 `events:entity` XADD 通知 Go 侧
  - 在 `config/default.toml` 新增：
    ```toml
    [routing.auto_ban]
    enabled = false
    consecutive_failure_threshold = 50
    report_stream = "events:entity"
    ```
  - Rust 侧只上报（`vendor_key.auto_ban_suggested`），不自行 ban
  - Go 侧接收上报后决定是否永久 ban（修改 key status）
  - 已有 `consecutive_failures` 计数基础

  **Must NOT do**:
  - Rust 侧不自行修改 key status（Go 权威）
  - 不修改现有冷却逻辑
  - 不发高频上报（debounce：同一 key 1 分钟内最多上报 1 次）

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 新增 XADD 上报逻辑，模式已存在
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 19 (Go contract doc)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-routing/src/health.rs` — consecutive_failures 计数
  - `crates/gw-store/src/stream_consumer.rs` — events:entity XADD 模式（反向写）
  - `docs/go-contracts/parts/02-go-as-consumer.md` §2.4 — Rust 反向写 entity 的 op 枚举

  **Acceptance Criteria**:

  - [ ] 连续失败超阈值 → XADD events:entity
  - [ ] 上报包含 vendor_key_id + failure_count + last_error
  - [ ] Debounce 1 分钟
  - [ ] 默认 disabled

  **QA Scenarios**:

  ```
  Scenario: Auto-ban report triggered
    Tool: Bash (cargo test)
    Steps:
      1. enabled = true, threshold = 5
      2. 同一 key 连续 6 次失败
      3. 验证 events:entity 收到 auto_ban_suggested
    Expected Result: XADD 上报
    Evidence: .omo/evidence/task-18-auto-ban-report.txt

  Scenario: Debounce — no duplicate reports
    Tool: Bash (cargo test)
    Steps:
      1. 连续 10 次失败
      2. 验证 1 分钟内仅 1 次上报
    Expected Result: 仅 1 条 XADD
    Evidence: .omo/evidence/task-18-auto-ban-debounce.txt
  ```

  **Commit**: YES
  - Message: `feat(routing): auto-ban suggestion report to Go via events:entity`
  - Files: `crates/gw-routing/src/health.rs`, config, tests
  - Pre-commit: `cargo test -p gw-routing`

- [ ] 19. Go 契约变更文档汇总

  **What to do**:
  - 创建 `docs/go-contracts/contract-change/2026-05-26-feature-parity.md`
  - 遵循现有格式（参考 `2026-05-24-v2-contracts-update.md`）
  - 包含 5 项 Split 变更：
    1. `/v1/models` proxy → Go admin API（新路由端点）
    2. `scheduling_preset` entity 字段（model_group entity 扩展）
    3. `auto_ban_suggested` events:entity op（新反向写 op）
    4. `attempted_keys` usage:records 字段（UsageRecord schema 扩展）
    5. `/healthz/active-requests` 多实例聚合（可选 Go 端聚合）
  - 更新 `docs/go-contracts/main.md` 索引（如需要）
  - 每条变更标明：触发来源 + parts 落点 + 受影响 Go 模块
  - 所有变更标记 `Status: Awaiting Go-side implementation`

  **Must NOT do**:
  - 不发明新文档结构
  - 不修改 parts/ 文件内容（仅新增 contract-change 记录）
  - 不编写 Go 代码

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: 纯文档编写，遵循现有格式
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5 (sequential after all Split tasks)
  - **Blocks**: F1-F4
  - **Blocked By**: Tasks 11, 13, 16, 17, 18

  **References**:

  **Pattern References**:
  - `docs/go-contracts/contract-change/2026-05-24-v2-contracts-update.md` — 格式模板
  - `docs/go-contracts/main.md` — 索引结构
  - `docs/go-contracts/parts/02-go-as-consumer.md` — Rust → Go 契约面

  **Acceptance Criteria**:

  - [ ] 文件存在且格式正确
  - [ ] 5 项变更全部记录
  - [ ] 每条有 parts 落点 + Go 模块
  - [ ] 所有标记 Awaiting Go-side implementation

  **QA Scenarios**:

  ```
  Scenario: Contract doc exists and is valid markdown
    Tool: Bash
    Steps:
      1. cat docs/go-contracts/contract-change/2026-05-26-feature-parity.md
      2. 验证包含 5 条变更索引
      3. 验证每条有 "parts 落点" 和 "受影响 Go 模块"
    Expected Result: 格式正确，5 条变更
    Evidence: .omo/evidence/task-19-contract-doc.txt

  Scenario: Index table complete
    Tool: Bash
    Steps:
      1. grep -c "Awaiting Go-side implementation" docs/go-contracts/contract-change/2026-05-26-feature-parity.md
      2. 验证 >= 5
    Expected Result: 每条变更标记等待 Go 实施
    Evidence: .omo/evidence/task-19-contract-status.txt
  ```

  **Commit**: YES
  - Message: `docs(go-contracts): feature parity contract changes for Go team`
  - Files: `docs/go-contracts/contract-change/2026-05-26-feature-parity.md`
  - Pre-commit: N/A (docs only)

---

## Final Verification Wave

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, curl endpoint, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check` + `cargo test --workspace` + `bash scripts/check-invariants.sh`. Review all changed files for: `as any` equivalents (unsafe blocks), empty match arms, debug prints in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.
  Output: `Build [PASS/FAIL] | Lint [PASS/FAIL] | Tests [N pass/N fail] | Invariants [PASS/FAIL] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Start gateway from clean state. Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-task integration (model rewrite + body rules, error classification + error passthrough). Test edge cases: empty model mapping, missing config sections, malformed requests. Save to `.omo/evidence/final-qa/`.
  Output: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination: Task N touching Task M's files. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

每个 task 独立 commit。按 Wave 分组 PR。

- **Wave 1 PR**: `feat(codec): protocol coverage — model rewrite, count_tokens, images/edits, videos`
- **Wave 2 PR**: `feat(routing): competitive edge — multi-dim scheduling, error config, error passthrough, TTFT`
- **Wave 3 PR**: `feat(proxy): experience — body rules, gemini files, models proxy, auth IP, active-requests`
- **Wave 4 PR**: `feat(routing): enhancements — quota probe, adaptive reservation, candidate chain, presets, auto-ban`
- **Wave 5 PR**: `docs(go-contracts): feature parity contract changes`

Pre-commit gates per PR:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-invariants.sh
cargo bench --workspace --no-run
```

Per-Wave exit gate:
```bash
make e2e-soak-smoke
```

---

## Success Criteria

### Verification Commands
```bash
cargo test --workspace                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings     # Expected: 0 warnings
bash scripts/check-invariants.sh                          # Expected: exit 0
make e2e-soak-smoke                                       # Expected: PASS
```

### Final Checklist
- [ ] All 18 features implemented and tested
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All 5 Split items have Go contract docs
- [ ] `docs/architecture.md` updated for new config sections
- [ ] `docs/workflow.md` updated for filter chain changes
- [ ] No regressions in existing tests
- [ ] Soak smoke passes clean
