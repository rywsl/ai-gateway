# P-E: GeminiFilesGateway Trait 重设计

## TL;DR

> **Quick Summary**: 重设计 `GeminiFilesGateway` trait — 现有 trait 只有 `ensure_uploaded(ctx, bytes, mime)`，缺少 list/get/delete 方法、无 vendor key 上下文、假设全量 body 缓冲。新 trait 支持 4 端点 + stream body + vendor key 选择。
>
> **Deliverables**: 新 trait 定义、FakeFilesGateway 适配、HTTP handler 骨架
> **Estimated Effort**: Medium
> **Blocks**: T10 (Gemini Files 真实实现)

---

## Context

### 根因分析

`GeminiFilesGateway` trait（`files.rs:23`）单方法 `ensure_uploaded(ctx, bytes: &[u8], mime)`:
- 假设 body 已全量缓冲为 `&[u8]`（大文件 OOM 风险）
- 无 multipart/stream handle
- **无 vendor key / routing 上下文**（无法选上游凭证）
- 无 list/get/delete 方法（3/4 端点无 trait 支持）

### 根治路径

重设计 trait:
- 4 方法: `upload`, `list`, `get`, `delete`
- upload 接受 body reader（stream）而非 `&[u8]`
- 传入 vendor key 选择上下文
- 与 `gemini_file_mapping` migration 集成

---

## Work Objectives

### Must Have
- 新 trait 含 upload/list/get/delete 4 方法
- upload 接受 `impl AsyncRead + Unpin` 而非 `&[u8]`
- 方法签名含 vendor key context（`&VendorKey` 或 routing info）
- FakeFilesGateway 适配新 trait
- HTTP handler 骨架（路径注册 + 鉴权）

### Must NOT Have
- 不实现真实 upstream 调用（T10 scope）
- 不改 `gemini_file_mapping` migration schema
- 不做文件 TTL/清理逻辑
- 不碰 KEK rotation 集成

---

## TODOs

- [x] 1. 新 GeminiFilesGateway trait 定义

  **What to do**:
  - 在 `crates/gw-codec/src/files.rs` 重定义 trait:
    ```rust
    #[async_trait]
    pub trait GeminiFilesGateway: Send + Sync {
        async fn upload(&self, ctx: &FilesCtx, body: Body, mime: &str) -> Result<FileInfo>;
        async fn list(&self, ctx: &FilesCtx) -> Result<Vec<FileInfo>>;
        async fn get(&self, ctx: &FilesCtx, file_id: &str) -> Result<FileInfo>;
        async fn delete(&self, ctx: &FilesCtx, file_id: &str) -> Result<()>;
    }

    pub struct FilesCtx {
        pub vendor_key: Arc<VendorKey>,  // routing 上下文
        pub api_key_id: String,         // 鉴权上下文
        pub model_group: String,        // 关联的 model group
    }
    ```
  - 旧 `ensure_uploaded` 标记 `#[deprecated]` 但保留（渐进迁移）
  - `Body` 使用 Pingora 的 body stream 类型或 `bytes::Bytes`

  **Must NOT do**:
  - 不在此 task 实现真实 upstream 调用
  - 不删除旧 trait 方法（deprecated 但保留）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocks**: Task 2
  - **Blocked By**: None

  **References**:
  - `crates/gw-codec/src/files.rs:23` — 现有 GeminiFilesGateway trait
  - `crates/gw-store/migrations/0002_gemini_file_mapping.sql` — DB schema
  - `crates/gw-core/src/entity.rs` — VendorKey 定义

  **Acceptance Criteria**:
  - [ ] 新 trait 编译通过
  - [ ] FilesCtx 含 vendor key 上下文
  - [ ] upload 接受 stream body

  **QA Scenarios**:

  ```
  Scenario: trait 可 impl
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-codec -- test_files_trait_impl
      2. 实现一个 mock impl 验证签名正确
    Expected Result: trait 签名可实现
    Evidence: .omo/evidence/pe-1-trait-impl.txt
  ```

  **Commit**: YES (groups with 2)
  - Message: `refactor(codec): redesign GeminiFilesGateway trait [P-E]`

- [x] 2. FakeFilesGateway 适配 + HTTP handler 骨架

  **What to do**:
  - 更新 `FakeFilesGateway` impl 新 trait（保留测试功能）
  - 在 `crates/gw-proxy/src/filters/` 注册 Gemini Files HTTP 路径:
    - `POST /v1beta/files` → upload
    - `GET /v1beta/files` → list
    - `GET /v1beta/files/{file_id}` → get
    - `DELETE /v1beta/files/{file_id}` → delete
  - handler 骨架：鉴权 → routing → 调用 trait method → 返回
  - 真实 upstream 调用留给 T10

  **Must NOT do**:
  - 不实现真实 Gemini Files API 调用
  - 不做文件存储

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-proxy/src/filters/task.rs` — 路径注册参考
  - Gemini Files API: media upload + management endpoints

  **Acceptance Criteria**:
  - [ ] FakeFilesGateway impl 新 trait 编译通过
  - [ ] 4 HTTP 路径注册
  - [ ] 鉴权 + 404 测试

  **QA Scenarios**:

  ```
  Scenario: 路径注册验证
    Tool: Bash (curl)
    Steps:
      1. curl -s http://localhost:8000/v1beta/files \
           -H "x-api-key: test-key-001" -H "x-api-key-id: test-key-id-001"
      2. 断言 HTTP 200（FakeFilesGateway 返回空列表）
    Expected Result: 路径已注册
    Evidence: .omo/evidence/pe-2-path-registered.txt
  ```

  **Commit**: YES (groups with 1)
  - Message: `refactor(codec): redesign GeminiFilesGateway + HTTP handlers [P-E]`
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace
bash scripts/check-invariants.sh
```
