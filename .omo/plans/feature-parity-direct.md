# Feature Parity — 直接执行 Plan（10 独立功能）

## TL;DR

> **Quick Summary**: 10 项已验证为根治的独立功能，无需前置架构改造，可立即并行执行。
>
> **Deliverables**: count_tokens 端点、images/edits 端点、videos 端点、TTFT 超时强制、/v1/models 转发、Auth IP 限流、/healthz/active-requests、candidate chain 字段、auto-ban 上报、Go 契约文档
>
> **Estimated Effort**: Large
> **Parallel Execution**: YES - Wave-D1 (8 quick) + Wave-D2 (2 unspecified-high)

---

## Context

这 10 项任务经 Metis 根因分析确认为"已根治"——既有抽象足以支撑，仅需扩展/新增而非重设计。

---

## Work Objectives

### Must Have
- 每项功能含单元测试 + 集成测试（同 PR）
- `scripts/check-invariants.sh` per-task exit 0
- `cargo clippy + fmt` per-task
- `make e2e-soak-smoke` Wave-D1 / Wave-D2 各完成后运行

### Must NOT Have
- 新 Cargo 依赖
- 机会主义重构
- `unimplemented!()` / `todo!()` 标记
- 人工验收

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **QA Policy**: 每 task 含 curl + cargo test 可执行 QA scenarios
- **Evidence**: `.omo/evidence/task-{N}-{scenario}.{ext}`

---

## Execution Strategy

```
Wave-D1 [quick × 8, 立即并行]:
├── 1. count_tokens 端点
├── 2. images/edits 端点
├── 3. videos 端点（Task mode）
├── 4. /v1/models 转发到 Go
├── 5. /healthz/active-requests
├── 6. candidate chain 字段
├── 7. auto-ban 上报
└── 8. Go 契约文档

Wave-D2 [unspecified-high × 2, D1 后]:
├── 9. TTFT 超时强制
└── 10. Auth 失败 IP 独立限流

Final → 随 overview plan FINAL wave
```

---

## TODOs

- [x] 1. count_tokens 端点（Anthropic `/v1/messages/count_tokens`）

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/codec.rs` 的路径识别中新增 `/v1/messages/count_tokens` 匹配
  - 该路径走 Anthropic codec 正常请求流程（鉴权、routing、upstream 转发），不需要特殊处理
  - 决策：count_tokens 请求 **不产生** `usage:records` XADD（不计费），但 **计入** RPM 限流
  - 新增 Prometheus metric: `gw_count_tokens_total{format="anthropic"}`
  - 单元测试：codec 路径识别测试
  - 集成测试：端到端 curl 验证

  **Must NOT do**:
  - 不做 Rust 侧 token 计算（转发到上游）
  - 不改 `usage:records` schema
  - 不碰其他 codec 路径

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-codec/src/anthropic/codec.rs` — Anthropic codec 路径识别逻辑
  - `crates/gw-proxy/src/filters/task.rs:26-50` — `detect_task_shape` match arms（参考 images/generations 的识别模式）
  - `crates/gw-proxy/src/filters/upstream.rs` — upstream 请求构建流程
  - Anthropic API: `POST /v1/messages/count_tokens` — 请求/响应 JSON schema

  **Acceptance Criteria**:
  - [ ] `cargo test -p gw-codec` 全绿（含新增路径识别测试）
  - [ ] `bash scripts/check-invariants.sh` exit 0

  **QA Scenarios**:

  ```
  Scenario: count_tokens 正常请求
    Tool: Bash (curl)
    Preconditions: Gateway 启动，fake upstream 运行
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/messages/count_tokens \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hello"}]}'
      2. 断言 HTTP 200
      3. 断言响应 JSON 含 "input_tokens" 字段（integer）
      4. curl -s http://localhost:9090/metrics | grep gw_count_tokens_total
      5. 断言 metric 值 >= 1
    Expected Result: 200 + JSON with input_tokens + metric incremented
    Failure Indicators: HTTP 404/400, missing input_tokens field
    Evidence: .omo/evidence/task-d1-count-tokens-happy.txt

  Scenario: count_tokens 不产生 usage:records
    Tool: Bash (redis-cli)
    Preconditions: count_tokens 请求已发送
    Steps:
      1. redis-cli XLEN usage:records（发送前）
      2. 发送 count_tokens 请求
      3. redis-cli XLEN usage:records（发送后）
      4. 断言两次 XLEN 值相同
    Expected Result: usage:records 不新增条目
    Evidence: .omo/evidence/task-d1-count-tokens-no-billing.txt
  ```

  **Commit**: YES
  - Message: `feat(codec): add /v1/messages/count_tokens endpoint`
  - Files: `crates/gw-codec/src/anthropic/codec.rs`, tests
  - Pre-commit: `cargo test -p gw-codec && bash scripts/check-invariants.sh`

- [x] 2. images/edits 端点

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/task.rs` 的 `detect_task_shape` 中新增 `/v1/images/edits` 路径匹配
  - 复用现有 `/v1/images/generations` 的处理逻辑（Task mode 或直接代理）
  - 注意 multipart/form-data 透传：`request_body_filter` 不应因 EOS 缓冲破坏 multipart 边界
  - 新增 metric: `gw_images_edits_total`

  **Must NOT do**:
  - 不做图片预处理/验证
  - 不改 images/generations 逻辑
  - 不碰 multipart parsing（直接透传原始 body）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-proxy/src/filters/task.rs:46` — `TaskKind::ImageGeneration` 匹配（参考此处添加 ImageEdit）
  - `crates/gw-proxy/src/filters/upstream.rs` — multipart body 透传路径
  - OpenAI API: `POST /v1/images/edits` — multipart (image, mask, prompt, model)

  **Acceptance Criteria**:
  - [ ] `cargo test -p gw-proxy` 含 images/edits 路径识别测试
  - [ ] `bash scripts/check-invariants.sh` exit 0

  **QA Scenarios**:

  ```
  Scenario: images/edits 路径识别
    Tool: Bash (curl)
    Preconditions: Gateway + fake upstream
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/images/edits \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -F "image=@test.png" -F "prompt=make it blue" -F "model=dall-e-2"
      2. 断言 HTTP 200 (fake upstream echo)
      3. curl -s http://localhost:9090/metrics | grep gw_images_edits_total
    Expected Result: 请求正确路由到上游，metric incremented
    Failure Indicators: HTTP 404 (路径未识别), multipart body 损坏
    Evidence: .omo/evidence/task-d2-images-edits-happy.txt

  Scenario: images/edits 无效请求
    Tool: Bash (curl)
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/images/edits \
           -H "x-api-key: invalid" -H "x-api-key-id: invalid"
      2. 断言 HTTP 401
    Expected Result: 鉴权失败返回 401
    Evidence: .omo/evidence/task-d2-images-edits-auth-fail.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): add /v1/images/edits path detection`
  - Files: `crates/gw-proxy/src/filters/task.rs`, tests
  - Pre-commit: `cargo test -p gw-proxy && bash scripts/check-invariants.sh`

- [x] 3. Videos 端点（Task mode: submit + poll）

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/task.rs` 新增 video 路径匹配:
    - `POST /v1/videos/generations` → TaskKind::VideoGeneration（提交）
    - `GET /v1/videos/generations/{id}` → TaskKind::VideoGenerationPoll（查询）
  - Video 仅支持 Task 模式（提交+轮询），不含视频流
  - 复用现有 Task mode 基础设施
  - 新增 metric: `gw_video_generations_total`, `gw_video_polls_total`

  **Must NOT do**:
  - 不做视频流/WebSocket 传输
  - 不做视频内容解析
  - 不碰现有 task 基础设施实现

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-proxy/src/filters/task.rs:26-50` — detect_task_shape 现有 match arms
  - `crates/gw-proxy/src/filters/task.rs:46` — "video" 映射已存在但未实现
  - OpenAI video API spec: POST submit + GET poll 模式

  **Acceptance Criteria**:
  - [ ] `cargo test -p gw-proxy` 含 video 路径识别测试
  - [ ] 同时覆盖 submit 和 poll 两条路径

  **QA Scenarios**:

  ```
  Scenario: video generation 提交
    Tool: Bash (curl)
    Preconditions: Gateway + fake upstream
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/videos/generations \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -H "Content-Type: application/json" \
           -d '{"model":"sora","prompt":"a cat","size":"1080p"}'
      2. 断言 HTTP 200
      3. 断言响应 JSON 含 "id" 字段
    Expected Result: 任务提交成功，返回 task ID
    Evidence: .omo/evidence/task-d3-video-submit.txt

  Scenario: video generation 轮询
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1/videos/generations/vid_test123 \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001"
      2. 断言 HTTP 200
      3. 断言响应含 "status" 字段
    Expected Result: 返回任务状态
    Evidence: .omo/evidence/task-d3-video-poll.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): add video generation task endpoints`
  - Files: `crates/gw-proxy/src/filters/task.rs`, tests
  - Pre-commit: `cargo test -p gw-proxy && bash scripts/check-invariants.sh`

- [x] 4. /v1/models 转发到 Go 控制面

  **What to do**:
  - 在 `crates/gw-proxy/src/filters/` 路径识别层新增 `GET /v1/models` 和 `GET /v1/models/{model_id}` 匹配
  - 复用现有 `/admin/*` reverse-proxy 到 Go 的模式：Rust 鉴权 → 转发到 Go 服务
  - Go 服务负责聚合 model 列表、去重、格式化
  - 新增 metric: `gw_models_proxy_total`

  **Must NOT do**:
  - 不在 Rust 侧聚合 vendor model 列表
  - 不改 `/admin/*` 反向代理基础设施
  - 不碰 model registry 逻辑

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-proxy/src/filters/` — 现有 admin 反向代理模式
  - `crates/gw-gateway/src/config.rs` — Go 控制面地址配置
  - OpenAI API: `GET /v1/models` 响应格式

  **Acceptance Criteria**:
  - [ ] `cargo test` 含 /v1/models 路径识别测试
  - [ ] Go 契约文档中含对应端点定义

  **QA Scenarios**:

  ```
  Scenario: /v1/models 列表请求
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1/models \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001"
      2. 断言 HTTP 200
      3. 断言响应 JSON 含 "data" 数组
    Expected Result: 返回 model 列表（来自 Go 服务）
    Evidence: .omo/evidence/task-d4-models-list.txt

  Scenario: /v1/models 无鉴权
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1/models
      2. 断言 HTTP 401
    Expected Result: 无 API key 返回 401
    Evidence: .omo/evidence/task-d4-models-no-auth.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): proxy /v1/models to Go control plane`
  - Files: proxy filters, config
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

- [x] 5. /healthz/active-requests 管理端点

  **What to do**:
  - 在 `crates/gw-gateway/` 管理端口 (8080) 新增 `/healthz/active-requests` 端点
  - 复用既有 `/healthz/*` token-gated 模式（需 admin token）
  - 读取 in-flight tracking 数据（`InFlightStore` 的 per-key atomics）
  - 返回 JSON: `{"count": <int>, "by_format": {...}, "by_model_group": {...}}`
  - 注意：数据是 per-instance（与现有 in-flight 一致）

  **Must NOT do**:
  - 不新增其他 `/healthz/*` 端点
  - 不改 InFlightStore 实现
  - 不做跨实例聚合（Go 侧职责）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-gateway/src/` — 现有 `/healthz/circuits` 端点实现（参考 token-gated 模式）
  - `crates/gw-health/src/in_flight.rs` — InFlightStore atomics
  - `crates/gw-proxy/src/filters/` — in-flight guard acquire/release 逻辑

  **Acceptance Criteria**:
  - [ ] `cargo test` 含 active-requests 端点测试
  - [ ] 返回正确 JSON 结构
  - [ ] 无 admin token 返回 401

  **QA Scenarios**:

  ```
  Scenario: active-requests 正常查询
    Tool: Bash (curl)
    Steps:
      1. curl -s -H "Authorization: Bearer ${ADMIN_TOKEN}" \
           http://localhost:8080/healthz/active-requests
      2. 断言 HTTP 200
      3. echo $response | jq -e '.count >= 0'
      4. echo $response | jq -e '.by_format | type == "object"'
    Expected Result: JSON with count + breakdowns
    Evidence: .omo/evidence/task-d5-active-requests-happy.txt

  Scenario: active-requests 无 token
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8080/healthz/active-requests
      2. 断言 HTTP 401
    Expected Result: 401 Unauthorized
    Evidence: .omo/evidence/task-d5-active-requests-no-auth.txt
  ```

  **Commit**: YES
  - Message: `feat(admin): add /healthz/active-requests endpoint`
  - Files: `crates/gw-gateway/src/`, tests
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

- [x] 6. usage:records Candidate Chain 字段

  **What to do**:
  - 在 `UsageRecord` struct 新增 `Option<Vec<AttemptedKey>>` 字段（additive, backward-compatible）
  - `AttemptedKey` 结构: `{ vendor_key_id: String, vendor: String, model: String, error_code: Option<String>, attempt_index: u32 }`
  - 从 `ctx.picked_candidates` + `ctx.retry_blacklist` 提取完整候选链
  - flusher 序列化时包含新字段（JSON optional field）
  - 单元测试：UsageRecord 序列化/反序列化含新字段

  **Must NOT do**:
  - 不重组 UsageRecord 既有字段
  - 不改 Redis stream key 名
  - 不做 Go 侧消费（Go 契约文档覆盖）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-ledger/src/` — UsageRecord 定义 + flusher 逻辑
  - `crates/gw-proxy/src/filters/` — `ctx.picked_candidates`, `ctx.retry_blacklist`, `ctx.attempt_index`
  - `docs/go-contracts/` — 现有 usage:records schema 定义

  **Acceptance Criteria**:
  - [ ] `cargo test -p gw-ledger` 含序列化测试
  - [ ] XADD 后 XREAD 断言新字段存在
  - [ ] 向后兼容：新字段 Optional，旧消费者不受影响

  **QA Scenarios**:

  ```
  Scenario: candidate chain 字段存在
    Tool: Bash (redis-cli + curl)
    Steps:
      1. 发送正常请求触发 usage:records XADD
      2. redis-cli XREVRANGE usage:records + - COUNT 1
      3. 解析 JSON，断言含 "attempted_keys" 字段
      4. 断言 attempted_keys 数组至少 1 个元素
      5. 断言每个元素含 vendor_key_id, vendor, model, attempt_index
    Expected Result: usage:records 新增 attempted_keys 字段
    Evidence: .omo/evidence/task-d6-candidate-chain.txt

  Scenario: failover 时 candidate chain 多条目
    Tool: Bash
    Steps:
      1. 配置 fake upstream 第一个 key 返回 500
      2. 发送请求（触发 failover）
      3. XREAD usage:records 最新条目
      4. 断言 attempted_keys 长度 >= 2
      5. 断言第一个元素 error_code 非 null
    Expected Result: 多次 attempt 完整记录
    Evidence: .omo/evidence/task-d6-candidate-chain-failover.txt
  ```

  **Commit**: YES
  - Message: `feat(ledger): add candidate chain field to usage:records`
  - Files: `crates/gw-ledger/`, tests
  - Pre-commit: `cargo test -p gw-ledger && bash scripts/check-invariants.sh`

- [x] 7. Auto-ban 连续失败上报

  **What to do**:
  - 在 `crates/gw-health/src/format_health.rs` 检测 `consecutive_failures` 达到阈值
  - 触发 `events:entity` Redis stream XADD，上报 `{ op: "auto_ban_candidate", key_id, consecutive_failures, last_error_code }`
  - Go 侧权威决策是否永久移除（Rust 仅上报）
  - Debounce：同一 key 60s 内最多上报一次（防 XADD 风暴）
  - 新配置: `[health.auto_ban]` → `threshold = 10`, `debounce_secs = 60`（boot-time only）

  **Must NOT do**:
  - 不在 Rust 侧做永久移除决策
  - 不改 KeyHealthStore 状态机
  - 不碰 cooldown/half-open 逻辑

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `crates/gw-health/src/format_health.rs:35` — `consecutive_failures` 字段
  - `crates/gw-health/src/` — 健康状态机
  - 现有 `events:entity` 反向写模式（参考 admin 模块的上报格式）
  - `config/default.toml` — 配置格式

  **Acceptance Criteria**:
  - [ ] 单元测试：连续失败 >= threshold 触发上报
  - [ ] 单元测试：debounce 60s 内不重复上报
  - [ ] config/default.toml 含 `[health.auto_ban]` section

  **QA Scenarios**:

  ```
  Scenario: 连续失败触发上报
    Tool: Bash
    Steps:
      1. 配置 threshold=3, fake upstream 持续返回 500
      2. 发送 3 个请求（同一 key）
      3. redis-cli XREVRANGE events:entity + - COUNT 1
      4. 断言 op == "auto_ban_candidate"
      5. 断言 consecutive_failures >= 3
    Expected Result: 第 3 次失败后 events:entity 新增条目
    Evidence: .omo/evidence/task-d7-auto-ban-report.txt

  Scenario: debounce 防重复
    Tool: Bash
    Steps:
      1. 连续发送 10 个失败请求
      2. redis-cli XLEN events:entity（仅含 auto_ban 类型）
      3. 断言 count == 1（60s 内只上报一次）
    Expected Result: debounce 生效
    Evidence: .omo/evidence/task-d7-auto-ban-debounce.txt
  ```

  **Commit**: YES
  - Message: `feat(health): add auto-ban consecutive failure reporting`
  - Files: `crates/gw-health/`, `config/default.toml`, `docs/architecture.md`, tests
  - Pre-commit: `cargo test -p gw-health && bash scripts/check-invariants.sh`

- [x] 8. Go 契约文档

  **What to do**:
  - 创建 `docs/go-contracts/contract-change/2026-05-27-feature-parity.md`
  - 更新 `docs/go-contracts/main.md` 索引表
  - 覆盖 Split 型变更的 Go 侧消费规范:
    - `/v1/models` 代理：Go 提供 model 聚合 API
    - candidate chain 字段：Go 消费 usage:records 新字段
    - auto-ban 上报：Go 消费 events:entity 新 op
    - /healthz/active-requests：Go 可选聚合多实例
    - scheduling presets：Go 配置 → Rust 执行
  - 使用现有 `docs/go-contracts/contract-change/` 格式

  **Must NOT do**:
  - 不发明新文档结构
  - 不写 Go 代码
  - 不改 Rust 代码

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave-D1
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `docs/go-contracts/main.md` — 现有索引格式
  - `docs/go-contracts/parts/01-*.md` ~ `03-*.md` — 现有契约格式参考
  - `docs/go-contracts/contract-change/` — 变更记录目录

  **Acceptance Criteria**:
  - [ ] 契约文档遵循现有格式
  - [ ] main.md 索引更新
  - [ ] 每个 Split 项含 Redis stream key / PgSQL column / payload field 的证据路径

  **QA Scenarios**:

  ```
  Scenario: 文档格式合规
    Tool: Bash
    Steps:
      1. cat docs/go-contracts/contract-change/2026-05-27-feature-parity.md
      2. 断言文件存在
      3. grep "Status:" 断言含状态字段
      4. grep -c "## " 断言含多个 section
    Expected Result: 文档存在且结构完整
    Evidence: .omo/evidence/task-d8-go-contracts.txt
  ```

  **Commit**: YES
  - Message: `docs(go-contracts): feature-parity contract changes`
  - Files: `docs/go-contracts/`
  - Pre-commit: N/A (docs only)

- [x] 9. TTFT 超时强制（Wave-D2）

  **What to do**:
  - 在流式响应阶段新增 TTFT timeout 检测：首 token 到达前启动 tokio timer
  - TTFT 超时触发: 取消当前 upstream 连接 → failover 到下一 key（复用现有 failover 路径）
  - 取消路径必须真正中断 Pingora upstream 连接（非仅返回错误后连接泄漏）
  - 新配置: `[timeouts]` 下新增 `ttft_timeout_ms = 0`（0 = 不启用，向后兼容）
  - 新增 metric: `gw_ttft_timeout_total{format, model_group}`
  - 超时后 `in-flight guard` 必须正确释放
  - 更新 `docs/workflow.md` 的流式响应阶段描述

  **Must NOT do**:
  - 不改非流式请求的超时逻辑
  - 不碰 `response_deadline` 实现
  - 不引入新的 timeout 配置层级

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (within Wave-D2)
  - **Parallel Group**: Wave-D2
  - **Blocks**: None
  - **Blocked By**: Wave-D1 完成

  **References**:
  - `crates/gw-proxy/src/filters/upstream.rs` — response_deadline + stream 处理
  - `crates/gw-proxy/src/filters/` — failover 路径（`failover.rs:35-44`）
  - `crates/gw-health/src/in_flight.rs` — InFlightGuard release
  - `crates/gw-gateway/src/config.rs` — timeouts 配置
  - `docs/workflow.md` — 流式响应阶段

  **Acceptance Criteria**:
  - [ ] `cargo test` 含 TTFT timeout 触发 + failover 测试
  - [ ] `cargo test` 含 TTFT=0（禁用）不影响现有行为测试
  - [ ] in-flight gauge 超时后归零断言

  **QA Scenarios**:

  ```
  Scenario: TTFT 超时触发 failover
    Tool: Bash (curl + metrics)
    Preconditions: ttft_timeout_ms=1000, fake upstream 延迟 2s 首 token
    Steps:
      1. curl -sN http://localhost:8000/v1/chat/completions \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -d '{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"stream":true}'
      2. 断言 1s 后 failover 到第二个 key 并返回响应
      3. curl -s http://localhost:9090/metrics | grep gw_ttft_timeout_total
      4. 断言 metric >= 1
    Expected Result: 首 key TTFT 超时，failover 到备用 key 成功返回
    Evidence: .omo/evidence/task-d9-ttft-timeout.txt

  Scenario: TTFT 超时后 in-flight 释放
    Tool: Bash (curl + metrics)
    Steps:
      1. 记录 gw_in_flight gauge 值
      2. 触发 TTFT 超时
      3. 等待 failover 完成
      4. 断言 gw_in_flight gauge 回归初始值
    Expected Result: in-flight guard 正确释放
    Evidence: .omo/evidence/task-d9-ttft-inflight-release.txt

  Scenario: TTFT=0 禁用不影响现有
    Tool: Bash
    Steps:
      1. 配置 ttft_timeout_ms=0
      2. 发送流式请求（upstream 延迟 5s）
      3. 断言正常返回（无超时触发）
    Expected Result: ttft_timeout 禁用时行为不变
    Evidence: .omo/evidence/task-d9-ttft-disabled.txt
  ```

  **Commit**: YES
  - Message: `feat(proxy): enforce TTFT timeout with failover`
  - Files: `crates/gw-proxy/src/filters/`, `config/default.toml`, `docs/workflow.md`, tests
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

- [x] 10. Auth 失败 IP 独立限流（Wave-D2）

  **What to do**:
  - 在 `crates/gw-auth/src/ratelimit/` 新增 auth-failure IP limiter
  - **必须复用** `early.rs:192 client_ip_from_session(session, trust_xff)`，继承 `security.trust_xff` 语义
  - 区分 auth-failure IP RPM（如 10/min）和通用 IP RPM 限流
  - 在 `request_filter` 鉴权失败后递增 auth-failure 计数器
  - 达到阈值返回 `429 Too Many Requests`（非 401）
  - 新配置: `[auth.ip_limiting]` → `auth_failure_rpm = 10`, `enabled = true`（boot-time only）
  - 新增 metric: `gw_auth_ip_limit_total{reason="auth_failure"}`

  **Must NOT do**:
  - ~~不支持 XFF~~ → **必须复用 `client_ip_from_session`，继承 trust_xff**
  - 不改通用 IP RPM limiter
  - 不碰 HMAC 签名验证逻辑
  - 不做 per-key IP 限流（仅 per-IP）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (within Wave-D2)
  - **Parallel Group**: Wave-D2
  - **Blocks**: None
  - **Blocked By**: Wave-D1 完成

  **References**:
  - `crates/gw-auth/src/ratelimit/` — 现有限流实现
  - `crates/gw-proxy/src/filters/early.rs:192` — `client_ip_from_session(session, trust_xff)` IP 提取
  - `crates/gw-gateway/src/config.rs` — security.trust_xff 配置
  - `crates/gw-proxy/src/filters/request.rs` — 鉴权失败路径

  **Acceptance Criteria**:
  - [ ] 单元测试：auth 失败 11 次后返回 429
  - [ ] 单元测试：trust_xff=true 时使用 XFF IP
  - [ ] `config/default.toml` 含 `[auth.ip_limiting]` section
  - [ ] `docs/architecture.md` 更新

  **QA Scenarios**:

  ```
  Scenario: auth 失败 IP 限流触发
    Tool: Bash (curl loop)
    Preconditions: auth_failure_rpm=5
    Steps:
      1. for i in {1..6}; do curl -s -o /dev/null -w "%{http_code}" \
           http://localhost:8000/v1/chat/completions \
           -H "x-api-key: invalid" -H "x-api-key-id: invalid"; done
      2. 断言前 5 次返回 401
      3. 断言第 6 次返回 429
      4. curl -s http://localhost:9090/metrics | grep gw_auth_ip_limit_total
    Expected Result: 超过阈值后 429 而非 401
    Evidence: .omo/evidence/task-d10-auth-ip-limit.txt

  Scenario: trust_xff=true 使用 XFF IP
    Tool: Bash (curl)
    Steps:
      1. 配置 trust_xff=true, auth_failure_rpm=3
      2. 从不同源 IP 发送失败请求，但 X-Forwarded-For 相同
      3. 断言第 4 次返回 429（按 XFF IP 计数）
    Expected Result: 按 XFF IP 而非连接 IP 限流
    Evidence: .omo/evidence/task-d10-auth-ip-xff.txt
  ```

  **Commit**: YES
  - Message: `feat(auth): add auth-failure IP rate limiting`
  - Files: `crates/gw-auth/src/ratelimit/`, `config/default.toml`, `docs/architecture.md`, tests
  - Pre-commit: `cargo test -p gw-auth && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 的 F1-F4 统一执行。

---

## Commit Strategy

- 每 task 独立 commit
- `feat(codec): add count_tokens endpoint`
- `feat(proxy): add images/edits path detection`
- `feat(proxy): add video task endpoints`
- `feat(proxy): proxy /v1/models to Go control plane`
- `feat(admin): add /healthz/active-requests endpoint`
- `feat(ledger): add candidate chain field to usage:records`
- `feat(health): add auto-ban consecutive failure reporting`
- `docs(go-contracts): feature-parity contract changes`
- `feat(proxy): enforce TTFT timeout with failover`
- `feat(auth): add auth-failure IP rate limiting`
