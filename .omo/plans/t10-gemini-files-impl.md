# T10: Gemini Files 真实代理实现

## TL;DR

> **Quick Summary**: 利用 P-E 重设计的 `GeminiFilesGateway` trait（含 list/get/delete + stream body + vendor key context），实现对 Gemini Files API 的 4 个端点代理。
>
> **Deliverables**: 4 个 Gemini Files HTTP handlers + routing + tests
> **Estimated Effort**: Medium
> **Depends On**: P-E (GeminiFilesGateway trait 重设计)

---

## Context

### 现状

- `GeminiFilesGateway` trait 原只有 `ensure_uploaded(ctx, bytes, mime)` 单方法
- 仅 `FakeFilesGateway` 实现，无真实 HTTP 代理
- DB 已有 `gemini_file_mapping` 表（migration 0002）

### P-E 提供的基础

P-E 完成后：
- 新 trait 含 `upload`, `list`, `get`, `delete` 方法
- 支持 stream body（不全量缓冲）
- 传入 vendor key 选择上下文（复用 routing 逻辑选 key）
- HTTP handler skeleton 已建立

---

## Work Objectives

### Must Have
- Upload: `POST /v1/files` → 代理到 Gemini Files API（multipart）
- List: `GET /v1/files` → 代理到 Gemini Files API
- Get: `GET /v1/files/{file_id}` → 代理到 Gemini Files API
- Delete: `DELETE /v1/files/{file_id}` → 代理到 Gemini Files API
- 鉴权：复用现有 API key auth（request_filter 链）
- Vendor key 选择：复用 routing 逻辑选 Gemini key
- gemini_file_mapping 表记录映射关系
- 单元测试 + 集成测试

### Must NOT Have
- 不做文件缓存/CDN
- 不做跨 vendor 文件迁移
- 不做 TTL 自动清理（Go 侧责任）
- 不新增 Cargo 依赖
- 不碰 chat completions 热路径

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Key assertion**: Gemini Files 路径不影响 chat completions 性能

---

## Execution Strategy

```
Wave 1 [serial]:
├── Task 1: Upload + List 实现 [deep]
└── Task 2: Get + Delete + 集成测试 [unspecified-high]
```

---

## TODOs

- [x] 1. Gemini Files Upload + List 实现

  **What to do**:
  - 实现 P-E trait 的 `upload` 和 `list` 方法，作为真实 HTTP proxy
  - Upload (`POST /v1/files`):
    - 接收 multipart 请求体
    - 通过 routing 选择 Gemini vendor key
    - stream body 转发到 `https://generativelanguage.googleapis.com/upload/v1beta/files`
    - 记录 file_id → vendor_key mapping 到 `gemini_file_mapping` 表
    - 返回 Gemini API 响应
  - List (`GET /v1/files`):
    - 通过 routing 选择 Gemini vendor key
    - 代理到 `https://generativelanguage.googleapis.com/v1beta/files`
    - 返回 Gemini API 响应
  - HTTP handler 注册到 Pingora service（P-E skeleton 基础上）
  - 单元测试（mock upstream）

  **Must NOT do**:
  - 不缓冲完整文件到内存（stream 转发）
  - 不做文件内容转换
  - 不碰 chat completions 路径

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: multipart stream 转发 + vendor key routing 集成需要深入理解 Pingora 连接管理
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（Task 2 依赖 Task 1 的基础）
  - **Parallel Group**: Sequential
  - **Blocks**: Task 2
  - **Blocked By**: P-E

  **References**:

  **Pattern References**:
  - P-E 产出的 `GeminiFilesGateway` trait + HTTP handler skeleton
  - `crates/gw-store/migrations/0002_gemini_file_mapping.sql` — DB schema
  - `crates/gw-proxy/src/filters/upstream.rs` — vendor key routing 模式

  **WHY Each Reference Matters**:
  - P-E trait: 必须实现的接口
  - migration 0002: gemini_file_mapping 表结构
  - upstream.rs: 复用 vendor key 选择逻辑

  **Acceptance Criteria**:

  - [ ] `cargo test -p gw-proxy` 通过（含 Gemini Files tests）
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0
  - [ ] `scripts/check-invariants.sh` exit 0

  **QA Scenarios**:

  ```
  Scenario: Happy path — Upload 文件
    Tool: Bash (curl)
    Preconditions: fake Gemini upstream 启动; Gemini vendor key 配置
    Steps:
      1. curl -s -X POST http://localhost:8000/v1/files \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001" \
           -F "file=@test_data/small.txt" -F "purpose=assistants"
      2. 断言 HTTP 200
      3. jq -r '.name' 从响应提取 file_id
      4. 检查 gemini_file_mapping 表有对应记录
    Expected Result: 文件上传成功，mapping 记录
    Failure Indicators: HTTP 4xx/5xx 或 mapping 未记录
    Evidence: .omo/evidence/task-t10-1-upload.txt

  Scenario: List 文件
    Tool: Bash (curl)
    Preconditions: 至少一个文件已上传
    Steps:
      1. curl -s http://localhost:8000/v1/files \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001"
      2. 断言 HTTP 200
      3. jq '.files | length' >= 1
    Expected Result: 返回文件列表
    Evidence: .omo/evidence/task-t10-1-list.txt
  ```

  **Commit**: YES (与 Task 2 合并)
  - Message: `feat(proxy): gemini files proxy (upload/list/get/delete)`
  - Files: Gemini files impl, handlers, tests
  - Pre-commit: `cargo test -p gw-proxy && scripts/check-invariants.sh`

---

- [x] 2. Gemini Files Get + Delete + 集成测试

  **What to do**:
  - Get (`GET /v1/files/{file_id}`):
    - 查询 `gemini_file_mapping` 获取 vendor_key mapping
    - 代理到 Gemini API
    - 返回文件元数据
  - Delete (`DELETE /v1/files/{file_id}`):
    - 查询 `gemini_file_mapping` 获取 vendor_key mapping
    - 代理 DELETE 到 Gemini API
    - 成功后从 mapping 表删除记录
  - 集成测试：完整 CRUD 流程（upload → list → get → delete → list 验证删除）
  - 更新 `docs/workflow.md` 新增 Gemini Files 路径描述
  - 更新 `docs/architecture.md` §Known gaps 移除相关条目

  **Must NOT do**:
  - 不做文件内容缓存
  - 不做跨 vendor 迁移

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: CRUD 完成 + 集成测试
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（依赖 Task 1）
  - **Blocks**: None
  - **Blocked By**: Task 1

  **References**:
  - Task 1 产出的 upload/list 实现模式
  - `crates/gw-store/migrations/0002_gemini_file_mapping.sql` — 表结构

  **Acceptance Criteria**:

  - [ ] 完整 CRUD 集成测试通过
  - [ ] Delete 后 mapping 表清理

  **QA Scenarios**:

  ```
  Scenario: 完整 CRUD 流程
    Tool: Bash (curl)
    Steps:
      1. Upload: POST /v1/files → 获取 file_id
      2. Get: GET /v1/files/{file_id} → 200 + 元数据
      3. Delete: DELETE /v1/files/{file_id} → 200
      4. Get: GET /v1/files/{file_id} → 404（已删除）
    Expected Result: 完整生命周期正确
    Evidence: .omo/evidence/task-t10-2-crud.txt

  Scenario: 不存在的 file_id
    Tool: Bash (curl)
    Steps:
      1. GET /v1/files/nonexistent-id → 断言 404
      2. DELETE /v1/files/nonexistent-id → 断言 404
    Expected Result: 正确 404 + GW_* 错误码
    Evidence: .omo/evidence/task-t10-2-not-found.txt
  ```

  **Commit**: YES (与 Task 1 合并)

---

## Success Criteria

```bash
cargo test -p gw-proxy                                    # Expected: all pass (incl Gemini Files)
cargo clippy --workspace --all-targets -- -D warnings     # Expected: exit 0
scripts/check-invariants.sh                                # Expected: exit 0
```

### Final Checklist
- [ ] 4 端点（upload/list/get/delete）全部可用
- [ ] gemini_file_mapping 记录正确
- [ ] Stream 转发（不全量缓冲）
- [ ] 鉴权复用现有链
- [ ] docs 更新
