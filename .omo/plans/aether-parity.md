# Aether Parity — ailinkmax-gateway 功能补全计划

## TL;DR

> **Quick Summary**: 补全 ailinkmax-gateway 与 Aether Python 网关的全部功能差距，涵盖自适应限流、IP 限流、任务工作器全栈、CLI 协议变体、以及 Go 侧合约文档。
>
> **Deliverables**:
> - Domain A: Adaptive RPM + IP Rate Limiter (Rust, `gw-auth`)
> - Domain B: Task Worker 全栈 (Rust, `gw-task` + `gw-task-runtime` + `gw-task-worker` + gateway path)
> - Domain C: CLI Protocol Variants spec + implementation (Rust, `gw-codec` + `gw-proxy`)
> - Domain D: Go Contract Docs 补全 (`docs/go-contracts/parts/`)
>
> **Estimated Effort**: XL (~40 tasks, 4 domains)
> **Parallel Execution**: YES — 7 waves
> **Critical Path**: D (docs) → A (rate limit) → C-spec → C-impl → B (task worker)

---

## Context

### Original Request
用户要求：为 Rust 网关补足与 Aether Python 网关的所有功能差距，包括 Adaptive RPM、IP Limiter、Task Worker 编排、CLI 协议变体，以及 Go 合约文档的补全。

### Interview Summary
**Key Discussions**:
- **Rust 层**: 实现 Adaptive RPM、IP Limiter、Task Worker 编排、CLI 协议变体
- **Go 层**: 仅补充合约文档到 `docs/go-contracts/`（功能概述级别，非详细 API 设计）
- **前端**: 不在范围内
- **技术栈**: 保持 Rust gateway + Go business layer 架构
- **测试**: TDD (RED-GREEN-REFACTOR)
- **CLI 变体**: 需要 (`claude:cli`, `openai:cli`, `gemini:cli`, `openai:compact`)

**Research Findings**:
- RL-Adaptive 和 RL-IP spec 已存在，详细到 ADR 级别，零 Rust 代码
- Task Worker spec 存在 (580 行, 16 ADRs)，仅 ~10% 代码（`TaskFreezeRelease` trait）
- CLI 变体无 spec，需要先写 spec 再实现
- Go 合约 8 个领域，P0-P2 优先

### Metis Review
**Identified Gaps** (addressed):
- Domain C 需 spec-first 方法 → 计划分为 C-spec 和 C-impl 两阶段
- `gw-billing` 不得计算 cost-per-token（违反 `no_cost_micro` lint）→ 仅处理 task-level billing formulas
- 建议执行顺序 D → A → C-spec → C-impl → B → 采纳
- 每个 domain 独立 PR → 采纳

---

## Work Objectives

### Core Objective
将 ailinkmax-gateway Rust 代码库提升至与 Aether Python 网关功能对等，覆盖自适应限流、IP 限流/白名单、任务工作器全栈（3 crate + binary + 4 gateway endpoints）、CLI 协议变体（4 FORMAT_ID），并为 Go 团队补全合约文档。

### Concrete Deliverables
- `crates/gw-auth/src/ratelimit/adaptive.rs` — AdaptiveRpmManager (新建；Task 7 先将 `ratelimit.rs` 转为模块目录)
- `crates/gw-auth/src/ip_ratelimit.rs` — IpRateLimitGuard + IpAllowlistGuard
- `crates/gw-auth/src/ip_filter.rs` — ip_filter rewrite (check_ip)
- `crates/gw-task/` — 新 crate: DTOs, contract, Lua scripts
- `crates/gw-task-runtime/` — 扩展: 4 tokio loops
- `crates/gw-task-worker/` — 新 binary
- `crates/gw-proxy/` — gateway task path (4 endpoints)
- `crates/gw-billing/` — 新 crate: task-level billing (不含 cost-per-token)
- `docs/superpowers/specs/2026-05-26-cli-variants-design.md` — CLI 变体 spec
- `crates/gw-codec/` — CLI variant codecs (4 FORMAT_IDs)
- `docs/go-contracts/parts/05-wallet-payment.md` — Wallet/Payment 合约
- `docs/go-contracts/parts/06-subscription-video.md` — Subscription/Video 合约
- `docs/go-contracts/parts/07-oauth-ldap.md` — OAuth/LDAP 合约
- PgSQL migration `0004_task_worker.sql`

### Definition of Done
- [x] `cargo build --release -p gw-gateway` — 编译通过
- [x] `cargo build --release -p gw-task-worker` — 编译通过
- [x] `cargo test --workspace` — 全部通过
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 零警告
- [x] `bash scripts/check-invariants.sh` — 全部通过
- [x] `make e2e-soak-smoke` — 通过
- [x] 每个 domain 的 doc sync 已完成（architecture.md + workflow.md）

### Must Have
- Adaptive RPM: AIMD 学习 + confidence decay + probe expansion（按 spec ADR-RLA-*）
- IP Limiter: per-IP RPM + allowlist/blocklist + fail-open on Redis outage（按 spec ADR-RLIP-*）
- Task Worker: 3-crate split + 4 tokio loops + 4 gateway endpoints + state machine + billing integration（按 spec ADR-T-*）
- CLI Variants: 4 FORMAT_IDs, transparent proxy for body, full filter chain for auth/billing
- Go Contract Docs: P0 (Wallet/Payment) + P1 (Subscriptions/Video) + P2 (OAuth/LDAP)
- TDD: 每个实现模块先写失败测试
- 每个 commit 同步更新 `docs/architecture.md` 和 `docs/workflow.md`

### Must NOT Have (Guardrails)
- 不得在 Rust 中计算 cost-per-token（`no_cost_micro` lint）
- 不得添加 `unimplemented!()` / `todo!()` / `TODO(v2)` 标记（`no_v1_markers_in_prod` lint, error mode）
- 不得重构不在范围内的相邻 crate 代码
- 不得发明新模式——遵循 `gw-auth`、`gw-routing`、`gw-codec` 现有模式
- 不得在一个 PR 中合并多个 domain
- 不得为 soak harness 修改 gateway 代码（§C1 invariant）
- Domain C: 不得在 spec 审查前开始实现
- CLI 变体不得绕过 auth/permissions/rate-limit/billing filter chain
- 不得添加 scope 外的 FORMAT_ID（仅 `claude:cli`, `openai:cli`, `gemini:cli`, `openai:compact`）
- 不得为相邻未测试代码编写测试——仅测试新代码
- 不得超出 P0-P2 的 Go 合约范围（Analytics/Announcements/Email/License 推迟）

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (`cargo test --workspace` infra fully operational)
- **Automated tests**: TDD (RED-GREEN-REFACTOR)
- **Framework**: `cargo test` (Rust native)
- **If TDD**: Each task follows RED (failing test) → GREEN (minimal impl) → REFACTOR

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Rate Limiting**: Use Bash (curl + metrics endpoint) — Send requests, assert 429/403 codes, check Prometheus metrics
- **Task Worker**: Use Bash (curl + Redis CLI) — POST tasks, poll status, verify Redis state machine
- **CLI Variants**: Use Bash (curl) — Send CLI-shaped payloads, verify transparent passthrough
- **Documentation**: Use Bash (grep) — Verify required fields present in contract docs
- **Build/Lint**: Use Bash — `cargo build`, `cargo test`, `cargo clippy`, `check-invariants.sh`

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — Go contract docs + scaffolding):
├── 1. Go Contract: Wallet/Payment doc [quick]
├── 2. Go Contract: Subscription/Video doc [quick]
├── 3. Go Contract: OAuth/LDAP doc [quick]
├── 4. Go Contract: main.md index update [quick]
├── 5. Error codes registration (GW_QUOTA_IP_RPM_EXCEEDED, GW_AUTH_IP_NOT_IN_ALLOWLIST, task-related) [quick]
└── 6. Config schema additions (ip_filter, ip_ratelimit, adaptive_rpm, task sections) [quick]

Wave 2 (After Wave 1 — IP Limiter + Adaptive RPM, MAX PARALLEL):
├── 7. IP Rate Limiter: IpRateLimitGuard + tests [deep]
├── 8. IP Allowlist: IpBlockMode extension + IpAllowlistGuard + tests [deep]
├── 9. ip_filter rewrite: check_ip unified function + tests [deep]
├── 10. Adaptive RPM: data structures + AdaptiveCfg + tests [deep]
└── 11. Adaptive RPM: observe_429 + AIMD algorithm + tests [deep]

Wave 3 (After Wave 2 — Rate limit integration + CLI spec):
├── 12. Adaptive RPM: tick_decay + probe expansion + evict + tests [deep]
├── 13. Adaptive RPM: integration (K-B ErrorAnalyzer, spawn_decay_loop, EntityStore evict hook) [deep]
├── 14. IP filter chain integration (early_request_filter ordering) + tests [deep]
├── 15. Rate limit metrics registration + doc sync [quick]
├── 16. CLI Variants spec authorship [writing]
└── 17. gw-task crate: DTOs + TaskId/TaskStatus/TaskKind/TaskRecord + contract trait [deep]

Wave 4 (After Wave 3 — Task Worker core + CLI impl start):
├── 18. gw-task: Lua scripts (SUBMIT, SETTLE, RECOVER_LEASE) + tests [deep]
├── 19. gw-task: Redis protocol contract (entity:task:<id>, tasks:queue, tasks:cancel, tasks:leases) [deep]
├── 20. CLI Variants: codec scaffolding (CliCodec trait, FORMAT_ID registration) [unspecified-high]
├── 21. CLI Variants: claude:cli + openai:cli handlers [deep]
└── 22. CLI Variants: gemini:cli + openai:compact handlers [deep]

Wave 5 (After Wave 4 — Task Worker runtime + CLI integration):
├── 23. gw-task-runtime: exec loop (XREADGROUP, claim, reqwest upstream call) [deep]
├── 24. gw-task-runtime: cancel loop + watchdog loop + heartbeat loop [deep]
├── 25. gw-billing crate: task-level billing formulas (no cost-per-token) [deep]
├── 26. CLI Variants: filter chain integration (auth/perm/ratelimit/billing preserved, IR bypass) [deep]
└── 27. CLI Variants: doc sync (architecture.md + workflow.md) + tests [unspecified-high]

Wave 6 (After Wave 5 — Task Worker binary + gateway path):
├── 28. gw-task-worker binary: main, config, health/metrics endpoints [deep]
├── 29. gw-task-worker: deployment separability + config/task-default.toml [quick]
├── 30. Gateway task path: task-shape detector + task request_filter [deep]
├── 31. Gateway task path: 4 endpoints (submit/load/watch/cancel) [deep]
├── 32. TaskFreezeGuard billing integration (gateway-side freeze, worker-side settle) [deep]
└── 33. PgSQL migration 0004_task_worker.sql [quick]

Wave 7 (After Wave 6 — Integration + final touches):
├── 34. Task Worker: observability (metrics registration, detailed_log task_phases) [unspecified-high]
├── 35. Task Worker: doc sync (architecture.md, workflow.md, crate layout) [writing]
├── 36. Task Worker: worker registration protocol (entity:worker:<id>) [quick]
├── 37. check-invariants.sh updates (new crates, CLI filter chain rule) [quick]
├── 38. Full workspace build + clippy + test gate [quick]
└── 39. e2e-soak-smoke regression gate [quick]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── F1. Plan compliance audit (oracle)
├── F2. Code quality review (unspecified-high)
├── F3. Real manual QA (unspecified-high)
└── F4. Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks |
|------|-----------|--------|
| 1-4 | — | 5, 6 (weak) |
| 5 | — | 7-14, 17-19, 30-31 |
| 6 | — | 7-14, 28-29 |
| 7 | 5, 6 | 14 |
| 8 | 5, 6 | 9, 14 |
| 9 | 8 | 14 |
| 10 | 5, 6 | 11 |
| 11 | 10 | 12, 13 |
| 12 | 11 | 13 |
| 13 | 12 | 15 |
| 14 | 7, 8, 9 | 15 |
| 15 | 13, 14 | 38 |
| 16 | — | 20, 21, 22 |
| 17 | 5 | 18, 19 |
| 18 | 17 | 23, 25 |
| 19 | 17, 18 | 23 |
| 20 | 16 | 21, 22 |
| 21 | 20 | 26 |
| 22 | 20 | 26 |
| 23 | 18, 19 | 24, 28 |
| 24 | 23 | 28 |
| 25 | 18 | 32 |
| 26 | 21, 22 | 27 |
| 27 | 26 | 38 |
| 28 | 23, 24, 6 | 29, 30 |
| 29 | 28 | 38 |
| 30 | 5, 28 | 31 |
| 31 | 30 | 32 |
| 32 | 25, 31 | 34 |
| 33 | 17 | 28, 34 |
| 34 | 32 | 35 |
| 35 | 34 | 38 |
| 36 | 28 | 38 |
| 37 | 15, 27, 35 | 38 |
| 38 | all | 39 |
| 39 | 38 | F1-F4 |
| F1-F4 | 39 | — |

### Agent Dispatch Summary

- **Wave 1**: **6** — T1-T4 → `quick`, T5-T6 → `quick`
- **Wave 2**: **5** — T7-T11 → `deep`
- **Wave 3**: **6** — T12-T14 → `deep`, T15 → `quick`, T16 → `writing`, T17 → `deep`
- **Wave 4**: **5** — T18-T19 → `deep`, T20 → `unspecified-high`, T21-T22 → `deep`
- **Wave 5**: **5** — T23-T26 → `deep`, T27 → `unspecified-high`
- **Wave 6**: **6** — T28,T30-T32 → `deep`, T29,T33 → `quick`
- **Wave 7**: **6** — T34 → `unspecified-high`, T35 → `writing`, T36-T39 → `quick`
- **FINAL**: **4** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. Go Contract: Wallet/Payment 合约文档

  **What to do**:
  - RED: Write a test script that `grep`s `docs/go-contracts/parts/05-wallet-payment.md` for required sections (`## Redis Keys`, `## PgSQL Tables`, `## Request/Response Payloads`, `## Failure Modes`) — must fail (file doesn't exist)
  - GREEN: Create `docs/go-contracts/parts/05-wallet-payment.md` covering:
    - Wallet balance read/write contract (Redis `counter:balance:*` keys, PgSQL `wallets` table)
    - Payment integration touchpoints (webhook payloads, status enum, idempotency keys)
    - Deposit/withdraw flow with Rust gateway interaction (freeze/unfreeze via `usage:records`)
    - JSON examples for each payload
  - REFACTOR: Ensure consistent formatting with existing `parts/01-03` files

  **Must NOT do**:
  - 不得设计 Go API 接口——仅描述 Rust↔Go 的数据合约
  - 不得修改现有 `parts/01-03` 文件

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 纯文档编写，无代码实现
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - `git-master`: 仅需最后一次 commit

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4, 5, 6)
  - **Blocks**: Task 4 (index update)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `docs/go-contracts/parts/01-go-as-producer.md` — 现有合约文档格式、section 结构（Go 作为生产者）
  - `docs/go-contracts/parts/02-go-as-consumer.md` — Go 作为消费者的文档模式
  - `docs/go-contracts/parts/03-shared-schema.md` — 共享 schema 合约格式

  **External References (Aether 具体文件)**:
  - Aether `src/api/wallet/views.py` — Wallet CRUD 端点（deposit/withdraw/balance 路由定义）
  - Aether `src/api/wallet/schemas.py` — Wallet 请求/响应 schema（字段名、类型、验证规则）
  - Aether `src/services/wallet/service.py` — Wallet 业务逻辑（freeze/unfreeze/settle 流程）
  - Aether `src/api/payment/views.py` — Payment 端点（webhook 回调、状态查询）
  - Aether `src/api/payment/schemas.py` — Payment payload schema（idempotency key、status enum）
  - 已有合约 `docs/go-contracts/parts/02-go-as-consumer.md` — `usage:records` stream schema（Wallet 读取此流做结算）

  **WHY Each Reference Matters**:
  - `parts/01-03` 提供格式模板——新文档必须风格一致
  - Aether `schemas.py` 文件提供具体字段列表——合约文档中的 JSON example 必须匹配这些字段
  - Aether `service.py` 文件描述 freeze/unfreeze 流程——合约文档必须描述 Rust gateway 与 Go service 在此流程中的交互点
  - `02-go-as-consumer.md` 中 `usage:records` schema 是 Wallet 结算的输入源——合约文档须交叉引用

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Wallet/Payment 文档包含所有必需 sections
    Tool: Bash (grep)
    Preconditions: Task 1 completed
    Steps:
      1. grep -c "## Redis Keys" docs/go-contracts/parts/05-wallet-payment.md → 1
      2. grep -c "## PgSQL Tables" docs/go-contracts/parts/05-wallet-payment.md → 1
      3. grep -c "## Request/Response Payloads" docs/go-contracts/parts/05-wallet-payment.md → 1
      4. grep -c "## Failure Modes" docs/go-contracts/parts/05-wallet-payment.md → 1
      5. grep -c "```json" docs/go-contracts/parts/05-wallet-payment.md → ≥2 (JSON examples)
    Expected Result: All grep counts match expected values
    Failure Indicators: Any grep returns 0 or file not found
    Evidence: .omo/evidence/task-1-wallet-payment-sections.txt

  Scenario: 文档不包含 API 路由设计
    Tool: Bash (grep)
    Preconditions: Task 1 completed
    Steps:
      1. grep -ic "router\|endpoint.*handler\|@app\.\|@router\." docs/go-contracts/parts/05-wallet-payment.md → 0
    Expected Result: No API routing patterns found
    Evidence: .omo/evidence/task-1-wallet-payment-no-api.txt
  ```

  **Commit**: YES (groups with Tasks 2, 3, 4)
  - Message: `docs(go-contracts): add wallet/payment, subscription/video, oauth/ldap contract docs`
  - Files: `docs/go-contracts/parts/05-wallet-payment.md`
  - Pre-commit: `bash scripts/check-invariants.sh`

- [x] 2. Go Contract: Subscription/Video 合约文档

  **What to do**:
  - RED: `grep` test for `docs/go-contracts/parts/06-subscription-video.md` required sections — must fail
  - GREEN: Create `docs/go-contracts/parts/06-subscription-video.md` covering:
    - Subscription plan schema (PgSQL `subscriptions` table, status enum, billing cycle)
    - Video task contract (task_type enum, input/output payload schema, status transitions)
    - RPM/quota override interaction (subscription tier → permission merge)
    - Redis keys for subscription cache invalidation
    - JSON examples for each payload
  - REFACTOR: Consistent with existing parts + Task 1

  **Must NOT do**:
  - 不得设计 Go API 接口
  - 不得引入新的 Redis stream（仅文档化现有约定）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3, 4, 5, 6)
  - **Blocks**: Task 4
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `docs/go-contracts/parts/01-go-as-producer.md` — 格式模板（Go 作为生产者）
  - `docs/go-contracts/parts/02-go-as-consumer.md` — 格式模板（Go 作为消费者）

  **External References (Aether 具体文件)**:
  - Aether `src/api/subscription/views.py` — Subscription CRUD 端点（plan 列表、创建、续期路由）
  - Aether `src/api/subscription/schemas.py` — Subscription schema（plan_id、status enum、expiry 字段）
  - Aether `src/services/subscription/service.py` — Subscription 业务逻辑（配额授予、过期处理）
  - Aether `src/api/admin/video_tasks/views.py` — Video Task 管理端点（提交、查询、取消）
  - Aether `src/api/admin/video_tasks/schemas.py` — Video Task payload schema（task_type、provider、status）
  - Aether `src/services/video/service.py` — Video 业务逻辑（任务分发、状态机）

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Subscription/Video 文档完整性
    Tool: Bash (grep)
    Preconditions: Task 2 completed
    Steps:
      1. grep -c "## Redis Keys" docs/go-contracts/parts/06-subscription-video.md → 1
      2. grep -c "## PgSQL Tables" docs/go-contracts/parts/06-subscription-video.md → 1
      3. grep -c "## Failure Modes" docs/go-contracts/parts/06-subscription-video.md → 1
      4. grep -c "```json" docs/go-contracts/parts/06-subscription-video.md → ≥2
    Expected Result: All sections present with JSON examples
    Evidence: .omo/evidence/task-2-subscription-video-sections.txt
  ```

  **Commit**: YES (groups with Tasks 1, 3, 4)
  - Files: `docs/go-contracts/parts/06-subscription-video.md`

- [x] 3. Go Contract: OAuth/LDAP 合约文档

  **What to do**:
  - RED: `grep` test for `docs/go-contracts/parts/07-oauth-ldap.md` required sections — must fail
  - GREEN: Create `docs/go-contracts/parts/07-oauth-ldap.md` covering:
    - OAuth provider configuration schema (PgSQL `oauth_providers` table, token refresh contract)
    - LDAP integration touchpoints (bind config, group→permission mapping)
    - Auth config cache invalidation (Redis key patterns, `auth_configs` global seed)
    - Token exchange flow (Go → Redis → Rust gateway reads)
    - JSON examples
  - REFACTOR: Consistent formatting

  **Must NOT do**:
  - 不得修改 Rust auth 代码
  - 不得设计 Go API

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 4, 5, 6)
  - **Blocks**: Task 4
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `docs/go-contracts/parts/01-go-as-producer.md` — 格式模板（Go 作为生产者）

  **External References**:
  **External References (Aether 具体文件)**:
  - Aether `src/api/oauth/views.py` — OAuth 端点（provider CRUD、token exchange）
  - Aether `src/modules/oauth/provider.py` — OAuth provider 配置模型（provider_type、client_id 等字段）
  - Aether `src/modules/oauth/token_manager.py` — Token 刷新逻辑（refresh_token、expiry 处理）
  - Aether `src/modules/ldap/client.py` — LDAP 连接和 bind 配置（host、port、base_dn）
  - Aether `src/modules/ldap/mapper.py` — LDAP group→permission 映射逻辑
  - Aether `src/services/auth/service.py` — auth_configs 缓存和 Redis 种子机制

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: OAuth/LDAP 文档完整性
    Tool: Bash (grep)
    Preconditions: Task 3 completed
    Steps:
      1. grep -c "## OAuth" docs/go-contracts/parts/07-oauth-ldap.md → ≥1
      2. grep -c "## LDAP" docs/go-contracts/parts/07-oauth-ldap.md → ≥1
      3. grep -c "## Redis Keys" docs/go-contracts/parts/07-oauth-ldap.md → 1
      4. grep -c "```json" docs/go-contracts/parts/07-oauth-ldap.md → ≥2
    Expected Result: Both OAuth and LDAP sections present
    Evidence: .omo/evidence/task-3-oauth-ldap-sections.txt
  ```

  **Commit**: YES (groups with Tasks 1, 2, 4)
  - Files: `docs/go-contracts/parts/07-oauth-ldap.md`

- [x] 4. Go Contract: main.md 索引更新

  **What to do**:
  - RED: `grep` for `05-wallet-payment`, `06-subscription-video`, `07-oauth-ldap` in `docs/go-contracts/main.md` — must fail
  - GREEN: Update `docs/go-contracts/main.md` index section to include links to the 3 new parts:
    - `parts/05-wallet-payment.md` — Wallet & Payment contracts
    - `parts/06-subscription-video.md` — Subscription & Video Task contracts
    - `parts/07-oauth-ldap.md` — OAuth & LDAP contracts
  - REFACTOR: Ensure index ordering and formatting matches existing `parts/01-03` entries

  **Must NOT do**:
  - 不得修改 `parts/01-03` 内容
  - 不得修改 main.md 中非索引的章节

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 单文件索引更新
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (sequential after Tasks 1-3)
  - **Parallel Group**: Wave 1 tail (after Tasks 1-3 complete)
  - **Blocks**: None (doc-only)
  - **Blocked By**: Tasks 1, 2, 3 (索引指向的文件必须先存在)

  **References**:

  **Pattern References**:
  - `docs/go-contracts/main.md` — 现有索引格式和链接模式

  **API/Type References**:
  - 新建的 `parts/04-06` 文件路径

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: main.md 索引完整性
    Tool: Bash (grep)
    Preconditions: Tasks 1-3 completed, parts/04-06 exist
    Steps:
      1. grep -c "05-wallet-payment" docs/go-contracts/main.md → 1
      2. grep -c "06-subscription-video" docs/go-contracts/main.md → 1
      3. grep -c "07-oauth-ldap" docs/go-contracts/main.md → 1
      4. test -f docs/go-contracts/parts/05-wallet-payment.md && echo OK → OK
    Expected Result: All 3 new parts referenced in main.md index
    Evidence: .omo/evidence/task-4-main-index.txt

  Scenario: 索引链接有效性
    Tool: Bash
    Preconditions: All Wave 1 docs committed
    Steps:
      1. for f in $(grep -oP 'parts/\S+\.md' docs/go-contracts/main.md); do test -f "docs/go-contracts/$f" || echo "MISSING: $f"; done → no output
    Expected Result: All referenced files exist (zero MISSING output)
    Evidence: .omo/evidence/task-4-link-validity.txt
  ```

  **Commit**: YES (groups with Tasks 1, 2, 3)
  - Files: `docs/go-contracts/main.md`
  - Pre-commit: `grep -c "05-wallet-payment" docs/go-contracts/main.md`

- [x] 5. Error codes 注册

  **What to do**:
  - RED: `grep` for `GW_QUOTA_IP_RPM_EXCEEDED`, `GW_AUTH_IP_NOT_IN_ALLOWLIST`, `GW_TASK_*` in `crates/gw-core/src/error/codes.rs` — must fail
  - GREEN: Add error code constants to `codes.rs`:
    - `GW_QUOTA_IP_RPM_EXCEEDED` (429) — IP RPM exceeded
    - `GW_AUTH_IP_NOT_IN_ALLOWLIST` (403) — IP not in allowlist
    - `GW_TASK_SUBMIT_FAILED` (500) — Task submission failed
    - `GW_TASK_NOT_FOUND` (404) — Task not found
    - `GW_TASK_FREEZE_FAILED` (402) — Insufficient balance for task freeze
    - `GW_TASK_CANCELLED` (200) — Task cancellation acknowledged
  - REFACTOR: Follow existing error code patterns (tuple struct, display impl)

  **Must NOT do**:
  - 不得修改现有 error codes
  - 不得添加未在 spec 中定义的 error codes

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 单文件添加常量
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1-4, 6)
  - **Blocks**: Tasks 7-14, 17-19, 30-31
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-core/src/error/codes.rs` — 现有 error code 定义模式（GW_AUTH_*, GW_QUOTA_* 等）

  **API/Type References**:
  - RL-IP spec ADR-RLIP-1: `GW_QUOTA_IP_RPM_EXCEEDED` 定义
  - RL-IP spec ADR-RLIP-2: `GW_AUTH_IP_NOT_IN_ALLOWLIST` 定义
  - Task Worker spec ADR-T4: Task state machine error codes

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 所有新 error codes 已注册
    Tool: Bash (grep)
    Preconditions: Task 5 completed
    Steps:
      1. grep -c "GW_QUOTA_IP_RPM_EXCEEDED" crates/gw-core/src/error/codes.rs → ≥1
      2. grep -c "GW_AUTH_IP_NOT_IN_ALLOWLIST" crates/gw-core/src/error/codes.rs → ≥1
      3. grep -c "GW_TASK_SUBMIT_FAILED" crates/gw-core/src/error/codes.rs → ≥1
      4. grep -c "GW_TASK_NOT_FOUND" crates/gw-core/src/error/codes.rs → ≥1
      5. grep -c "GW_TASK_FREEZE_FAILED" crates/gw-core/src/error/codes.rs → ≥1
      6. cargo test -p gw-core → PASS
    Expected Result: All codes registered and crate compiles
    Evidence: .omo/evidence/task-5-error-codes.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-core): register error codes for IP ratelimit, allowlist, task worker`
  - Files: `crates/gw-core/src/error/codes.rs`
  - Pre-commit: `cargo test -p gw-core`

- [x] 6. Config schema 扩展

  **What to do**:
  - RED: 在 config struct 中 `grep` `IpRatelimitCfg`, `AdaptiveRpmCfg`, `TaskCfg` — must fail
  - GREEN: Add config sections to gateway config structs:
    - `[ip_filter]`: `allowlist_mode: IpAllowlistMode` (Off/Enforce)
    - `[ip_ratelimit]`: `global_per_ip_rpm: u32` (600), `max_entries: usize` (100_000), `sweep_interval_secs: u64` (300)
    - `[adaptive_rpm]`: full `AdaptiveCfg` struct fields with defaults per spec
    - `[task]`: `enabled: bool`, `queue_stream: String`, `cancel_stream: String`
  - Add corresponding entries to `config/default.toml`
  - REFACTOR: Follow existing config patterns (Figment deserialization)

  **Must NOT do**:
  - 不得修改现有 config 字段
  - 不得在 TOML 中放置 secrets

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Config struct + TOML defaults
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1-5)
  - **Blocks**: Tasks 7-14, 28-29
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-core/src/config.rs` 或类似 — 现有 config struct 定义模式
  - `config/default.toml` — 现有 TOML 层配置模式
  - RL-Adaptive spec ADR-RLA-2: `AdaptiveCfg` 字段定义和默认值
  - RL-IP spec ADR-RLIP-6: IP ratelimit config 字段

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Config structs 编译且有默认值
    Tool: Bash
    Preconditions: Task 6 completed
    Steps:
      1. grep -c "ip_ratelimit" config/default.toml → ≥1
      2. grep -c "adaptive_rpm" config/default.toml → ≥1
      3. grep -c "ip_filter" config/default.toml → ≥1
      4. cargo build -p gw-gateway → exit 0
    Expected Result: All config sections present and gateway compiles
    Evidence: .omo/evidence/task-6-config-schema.txt

  Scenario: 不含 secrets in TOML
    Tool: Bash (grep)
    Steps:
      1. grep -ic "password\|secret\|token\|kek" config/default.toml → 仅注释中出现或 0
    Expected Result: No plaintext secrets
    Evidence: .omo/evidence/task-6-no-secrets.txt
  ```

  **Commit**: YES
  - Message: `feat(config): add ip_filter, ip_ratelimit, adaptive_rpm, task config sections`
  - Files: `crates/gw-core/src/config.rs`, `config/default.toml`
  - Pre-commit: `cargo build -p gw-gateway`

- [x] 7. IP Rate Limiter: IpRateLimitGuard

  **What to do**:
  - RED: Write tests in `crates/gw-auth/tests/ip_ratelimit.rs`:
    - `test_ip_under_limit_passes` — 599 requests from same IP → all pass
    - `test_ip_over_limit_rejected` — 601 requests → 601st returns `GW_QUOTA_IP_RPM_EXCEEDED`
    - `test_different_ips_independent` — 2 IPs each get own quota
    - `test_sweep_clears_entries` — after sweep, IP counter resets
    - `test_max_entries_eviction` — exceeding 100k entries triggers oldest eviction
  - GREEN: Implement `IpRateLimitGuard` in `crates/gw-auth/src/ip_ratelimit.rs`:
    - `DashMap<IpAddr, Arc<DefaultDirectRateLimiter>>` with configurable RPM
    - `check(ip: IpAddr) -> Result<(), GwError>` returns `GW_QUOTA_IP_RPM_EXCEEDED` on exceed
    - `sweep()` method for periodic entry cleanup
    - Max entries cap with LRU-style eviction
  - REFACTOR: Convert `crates/gw-auth/src/ratelimit.rs` → `crates/gw-auth/src/ratelimit/mod.rs` (re-export existing code unchanged), creating module directory for Task 10 to add `adaptive.rs` into

  **Must NOT do**:
  - 不得使用 Redis（纯内存 DashMap per spec）
  - 不得修改 RL-Core 代码

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 核心限流逻辑，需要精确的并发安全实现
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 8, 9, 10, 11)
  - **Blocks**: Task 14
  - **Blocked By**: Tasks 5, 6

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/ratelimit.rs` — 现有 RL-Core 实现（单文件，本 Task 将其转为 `ratelimit/mod.rs` 模块目录）

  **API/Type References**:
  - RL-IP spec ADR-RLIP-1: `IpRateLimitGuard` 完整 API 设计
  - `crates/gw-core/src/error/codes.rs`: `GW_QUOTA_IP_RPM_EXCEEDED` (Task 5 注册)

  **Test References**:
  - RL-IP spec ADR-RLIP-10: 9 个测试场景定义
  - `crates/gw-auth/tests/` — 现有 test 文件组织模式

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: IP limiter 在 RPM 内通过
    Tool: Bash
    Preconditions: Task 7 completed, cargo test -p gw-auth passes
    Steps:
      1. cargo test -p gw-auth test_ip_under_limit_passes → PASS
      2. cargo test -p gw-auth test_ip_over_limit_rejected → PASS
      3. cargo test -p gw-auth test_different_ips_independent → PASS
    Expected Result: All 3 tests pass
    Evidence: .omo/evidence/task-7-ip-ratelimit-tests.txt

  Scenario: DashMap 并发安全
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_ip_ratelimit_concurrent -- --test-threads=1 → PASS (如有并发测试)
    Expected Result: No data races under concurrent access
    Evidence: .omo/evidence/task-7-ip-concurrent.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): add IpRateLimitGuard with per-IP RPM enforcement`
  - Files: `crates/gw-auth/src/ip_ratelimit.rs`, `crates/gw-auth/tests/ip_ratelimit.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 8. IP Allowlist: IpBlockMode 扩展 + IpAllowlistGuard

  **What to do**:
  - RED: Write tests:
    - `test_allowlist_enforce_blocks_unlisted_ip` — IP not in allowlist → `GW_AUTH_IP_NOT_IN_ALLOWLIST`
    - `test_allowlist_enforce_passes_listed_ip` — IP in allowlist → pass
    - `test_allowlist_off_passes_all` — allowlist mode Off → all pass
    - `test_blocklist_deny_blocks` — IP in deny list → blocked
    - `test_blocklist_and_allowlist_combined` — deny takes priority over allow
  - GREEN: Implement:
    - Extend `IpBlockEntry` with `IpBlockMode` enum (Deny/Allow)
    - `IpAllowlistMode` enum (Off/Enforce) from config
    - `IpAllowlistGuard::check(ip, user_id, api_key_id, store)` — reads allowlist from EntityStore
  - REFACTOR: Ensure `IpBlockEntry` changes are backward-compatible

  **Must NOT do**:
  - 不得破坏现有 blocklist 功能
  - 不得引入数据库 migration（使用现有 PgSQL schema）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 7, 9, 10, 11)
  - **Blocks**: Tasks 9, 14
  - **Blocked By**: Tasks 5, 6

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/ip_filter.rs` — 现有 IP block 实现
  - `crates/gw-store/` — EntityStore 读取模式

  **API/Type References**:
  - RL-IP spec ADR-RLIP-2: `IpBlockMode`, `IpAllowlistMode` 定义
  - RL-IP spec ADR-RLIP-3: `check_ip` 函数签名

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Allowlist enforce 模式阻止未列入 IP
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_allowlist_enforce_blocks_unlisted_ip → PASS
      2. cargo test -p gw-auth test_allowlist_enforce_passes_listed_ip → PASS
      3. cargo test -p gw-auth test_blocklist_and_allowlist_combined → PASS
    Expected Result: All allowlist tests pass
    Evidence: .omo/evidence/task-8-allowlist-tests.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): add IP allowlist guard with Deny/Allow modes`
  - Files: `crates/gw-auth/src/ip_filter.rs`, `crates/gw-auth/src/ip_allowlist.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 9. ip_filter 重写: check_ip 统一函数

  **What to do**:
  - RED: Write tests:
    - `test_check_ip_deny_first` — IP in deny list → blocked regardless of allowlist
    - `test_check_ip_allow_then_ratelimit` — allowlist pass → IP ratelimit check
    - `test_check_ip_scoped_check` — second check_ip with User/ApiKey scope after auth
    - `test_check_ip_redis_outage_failopen` — simulated Redis failure → IP limiter passes (fail-open)
  - GREEN: Rewrite `check_ip(ip, user_id, api_key_id, store, cfg)`:
    - Order: Deny blocklist → Allowlist (if Enforce) → IP ratelimit
    - Integrate with `early_request_filter` position
    - Second call after auth with User/ApiKey scope for scoped checks
  - REFACTOR: Remove old ip_filter code paths, consolidate into single `check_ip`

  **Must NOT do**:
  - 不得改变 filter chain 顺序（blocklist → allowlist → IP ratelimit → auth → scoped check_ip）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 8)
  - **Parallel Group**: Wave 2 (can start after Task 8)
  - **Blocks**: Task 14
  - **Blocked By**: Task 8

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/ip_filter.rs` — 现有 ip_filter 实现（要重写）
  - `crates/gw-proxy/src/filters/` — Pingora filter chain integration pattern

  **API/Type References**:
  - RL-IP spec ADR-RLIP-3: `check_ip` 完整函数签名
  - RL-IP spec ADR-RLIP-5: filter chain 排序定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: check_ip 执行顺序正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_check_ip_deny_first → PASS
      2. cargo test -p gw-auth test_check_ip_allow_then_ratelimit → PASS
      3. cargo test -p gw-auth test_check_ip_redis_outage_failopen → PASS
    Expected Result: Filter chain ordering verified
    Evidence: .omo/evidence/task-9-check-ip-tests.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): rewrite ip_filter with unified check_ip function`
  - Files: `crates/gw-auth/src/ip_filter.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 10. Adaptive RPM: 数据结构 + AdaptiveCfg

  **What to do**:
  - RED: Write tests:
    - `test_adaptive_cfg_defaults` — default AdaptiveCfg values match spec
    - `test_key_adaptive_state_initial` — new state has confidence=0, no learned_rpm
    - `test_confidence_clamp` — confidence value clamped to [0.0, 1.0]
  - GREEN: Implement in `crates/gw-auth/src/ratelimit/adaptive.rs`:
    - `AdaptiveCfg` struct with all fields per spec (enforcement_threshold=0.6, confidence_decay_per_min=0.02, etc.)
    - `KeyAdaptiveState` struct (configured_rpm, learned_rpm, confidence, last_429_at_ms, consistent_observations, last_observed_limit)
    - `Confidence` struct (value: f32, last_decay_at_ms: u64)
    - `AdaptiveRpmManager` with `DashMap<KeyId, Arc<KeyAdaptiveState>>`
    - Constructor `new(cfg: AdaptiveCfg)` with empty map
  - REFACTOR: Ensure Figment deserialization works for AdaptiveCfg

  **Must NOT do**:
  - 不得实现 observe_429 逻辑（Task 11）
  - 不得实现 tick_decay 逻辑（Task 12）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 7, 8, 9, 11)
  - **Blocks**: Task 11
  - **Blocked By**: Tasks 5, 6

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/ratelimit/mod.rs` — Task 7 已将 `ratelimit.rs` 转为模块目录，此处添加 `adaptive.rs`

  **API/Type References**:
  - RL-Adaptive spec ADR-RLA-1: 数据结构完整定义
  - RL-Adaptive spec ADR-RLA-2: `AdaptiveCfg` 所有字段 + 默认值

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: AdaptiveCfg 默认值匹配 spec
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_adaptive_cfg_defaults → PASS
      2. cargo test -p gw-auth test_key_adaptive_state_initial → PASS
    Expected Result: All data structure tests pass
    Evidence: .omo/evidence/task-10-adaptive-structs.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): add adaptive RPM data structures and config`
  - Files: `crates/gw-auth/src/ratelimit/adaptive.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 11. Adaptive RPM: observe_429 + AIMD 算法

  **What to do**:
  - RED: Write tests:
    - `test_observe_429_with_header_learns_after_2` — 2 consistent header observations → learned_rpm set
    - `test_observe_429_without_header_learns_after_3` — 3 consistent no-header observations → learned_rpm set
    - `test_observe_429_inconsistent_resets` — different limit value resets counter
    - `test_aimd_multiplicative_decrease` — on 429, ceiling = learned_rpm * buffer_ratio
    - `test_enforcement_below_threshold_skips` — confidence < 0.6 → no enforcement
    - `test_enforcement_above_threshold_enforces` — confidence ≥ 0.6 → enforce learned ceiling
  - GREEN: Implement `observe_429(key_id, info: RateLimitInfo, current_local_rpm)`:
    - Parse `x-ratelimit-limit` header if present
    - Multi-confirmation: header → 2 observations, no header → 3 observations
    - On confirmation: set `learned_rpm`, boost confidence to 1.0
    - AIMD: multiplicative decrease on 429 (ceiling = learned * buffer_ratio)
    - Implement `should_enforce(key_id) -> Option<u32>` for RL-Core cooperation
  - REFACTOR: Ensure `RateLimitInfo` struct is reusable

  **Must NOT do**:
  - 不得实现 tick_decay（Task 12）
  - 不得直接拦截请求——仅提供 `should_enforce` 查询

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 10)
  - **Parallel Group**: Wave 2 (after Task 10)
  - **Blocks**: Tasks 12, 13
  - **Blocked By**: Task 10

  **References**:

  **Pattern References**:
  - `crates/gw-auth/src/ratelimit/mod.rs` — RL-Core rate limit 判断模式（由 Task 7 从 `ratelimit.rs` 转换而来）

  **API/Type References**:
  - RL-Adaptive spec ADR-RLA-3: `observe_429` 完整接口定义
  - RL-Adaptive spec ADR-RLA-5: RL-Core cooperation — `RateLimitKind::Adaptive` 选择逻辑

  **Test References**:
  - RL-Adaptive spec ADR-RLA-10: 测试场景 1-6（学习、确认、AIMD）

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: AIMD 学习算法正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_observe_429_with_header_learns_after_2 → PASS
      2. cargo test -p gw-auth test_observe_429_without_header_learns_after_3 → PASS
      3. cargo test -p gw-auth test_aimd_multiplicative_decrease → PASS
      4. cargo test -p gw-auth test_enforcement_below_threshold_skips → PASS
      5. cargo test -p gw-auth test_enforcement_above_threshold_enforces → PASS
    Expected Result: All AIMD tests pass
    Evidence: .omo/evidence/task-11-observe-429-tests.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): implement observe_429 with AIMD learning algorithm`
  - Files: `crates/gw-auth/src/ratelimit/adaptive.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 12. Adaptive RPM: tick_decay + probe expansion + evict

  **What to do**:
  - RED: Write tests:
    - `test_tick_decay_reduces_confidence` — after 1 min, confidence reduced by 0.02
    - `test_tick_decay_probe_expansion` — confidence below threshold → ceiling += probe_increment
    - `test_tick_decay_below_zero_clamps` — confidence can't go below 0.0
    - `test_evict_removes_key` — after evict, key state is gone
    - `test_probe_respects_max_rpm` — probe expansion capped at max_rpm
  - GREEN: Implement:
    - `tick_decay(&self)` — iterate all keys, decay confidence, expand ceiling when below threshold
    - `evict(&self, key_id: &KeyId)` — remove key from DashMap (called on VendorKey invalidation)
    - Probe logic: when confidence drops below enforcement_threshold and probe_after_min elapsed, increment ceiling by probe_increment
  - REFACTOR: Optimize DashMap iteration (batch processing)

  **Must NOT do**:
  - 不得实现 spawn_decay_loop（Task 13 集成任务）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after Task 11)
  - **Blocks**: Task 13
  - **Blocked By**: Task 11

  **References**:

  **API/Type References**:
  - RL-Adaptive spec ADR-RLA-4: background task spec (tick_decay interval)
  - RL-Adaptive spec ADR-RLA-7: K-A boundary (evict on VendorKey invalidation)

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Confidence decay 和 probe 正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_tick_decay_reduces_confidence → PASS
      2. cargo test -p gw-auth test_tick_decay_probe_expansion → PASS
      3. cargo test -p gw-auth test_evict_removes_key → PASS
    Expected Result: Decay + probe + evict all pass
    Evidence: .omo/evidence/task-12-decay-probe-tests.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): add tick_decay, probe expansion, and evict for adaptive RPM`
  - Files: `crates/gw-auth/src/ratelimit/adaptive.rs`
  - Pre-commit: `cargo test -p gw-auth`

- [x] 13. Adaptive RPM: 集成 (K-B ErrorAnalyzer, spawn_decay_loop, EntityStore evict hook)

  **What to do**:
  - RED: Write integration tests:
    - `test_error_analyzer_calls_observe_429` — mock 429 response → observe_429 called
    - `test_decay_loop_ticks` — spawn_decay_loop → confidence decays over time
    - `test_entity_store_invalidation_evicts` — VendorKey cache invalidation → evict called
  - GREEN: Wire up:
    - In `crates/gw-adapter/src/analyzer.rs` (K-B `DefaultErrorAnalyzer::analyze`): call `adaptive_rpm.observe_429()` on 429 responses
    - In `crates/gw-gateway/src/main.rs` or bootstrap: `spawn_decay_loop(adaptive_rpm, interval_secs)` tokio task
    - In EntityStore VendorKey invalidation callback: call `adaptive_rpm.evict(key_id)`
    - Add `AdaptiveRpmManager` to gateway shared state (Arc)
  - REFACTOR: Ensure `AdaptiveRpmManager` access pattern matches existing shared state patterns

  **Must NOT do**:
  - 不得重构 ErrorAnalyzer 接口——仅添加 observe_429 调用
  - 不得修改 EntityStore 缓存逻辑——仅添加 evict hook

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after Task 12)
  - **Blocks**: Task 15
  - **Blocked By**: Task 12

  **References**:

  **Pattern References**:
  - `crates/gw-adapter/src/analyzer.rs` — ErrorAnalyzer 现有 analyze 方法
  - `crates/gw-gateway/src/` — gateway bootstrap 模式（共享状态注入）
  - `crates/gw-store/` — EntityStore 缓存失效回调模式

  **API/Type References**:
  - RL-Adaptive spec ADR-RLA-4: spawn_decay_loop spec
  - RL-Adaptive spec ADR-RLA-7: K-A boundary (ErrorAnalyzer integration point)

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 429 触发 observe_429
    Tool: Bash
    Steps:
      1. cargo test -p gw-auth test_error_analyzer_calls_observe_429 → PASS
      2. cargo test -p gw-auth test_entity_store_invalidation_evicts → PASS
      3. cargo build --release -p gw-gateway → exit 0
    Expected Result: Integration wiring compiles and tests pass
    Evidence: .omo/evidence/task-13-adaptive-integration.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): integrate adaptive RPM into ErrorAnalyzer, gateway bootstrap, EntityStore`
  - Files: `crates/gw-adapter/src/analyzer.rs`, `crates/gw-gateway/src/main.rs`, `crates/gw-store/src/`
  - Pre-commit: `cargo test --workspace`

- [x] 14. IP filter chain 集成 (early_request_filter ordering)

  **What to do**:
  - RED: Write integration tests:
    - `test_filter_chain_order` — verify blocklist → allowlist → IP ratelimit → auth → scoped check
    - `test_blocked_ip_never_reaches_auth` — blocked IP → 403 before auth check
    - `test_ratelimited_ip_never_reaches_auth` — IP over RPM → 429 before auth
  - GREEN: Wire `check_ip` into `early_request_filter`:
    - Position: after TLS/connection setup, before auth
    - First call: global scope (no user/api_key context)
    - Second call: after auth, with User/ApiKey scope
    - Integrate `IpRateLimitGuard` + `IpAllowlistGuard` into gateway shared state
  - REFACTOR: Ensure filter chain documentation matches code

  **Must NOT do**:
  - 不得改变现有 filter chain 中其他 filter 的顺序
  - 不得在 `upstream_request_filter` 中添加 IP 检查

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after Tasks 7, 8, 9)
  - **Blocks**: Task 15
  - **Blocked By**: Tasks 7, 8, 9

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/` — 现有 filter chain 实现
  - `docs/workflow.md` — filter chain 文档

  **API/Type References**:
  - RL-IP spec ADR-RLIP-5: filter chain ordering 完整定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: IP filter chain 顺序正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-proxy test_filter_chain_order → PASS
      2. cargo test -p gw-proxy test_blocked_ip_never_reaches_auth → PASS
      3. cargo build --release -p gw-gateway → exit 0
    Expected Result: Filter chain integration correct
    Evidence: .omo/evidence/task-14-filter-chain.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-proxy): integrate IP filter chain into early_request_filter`
  - Files: `crates/gw-proxy/src/filters/`
  - Pre-commit: `cargo test --workspace`

- [x] 15. Rate limit metrics 注册 + doc sync

  **What to do**:
  - Register all rate limit metrics per specs:
    - Adaptive: gw_adaptive_rpm_observe_total, gw_adaptive_rpm_enforced_total, gw_adaptive_rpm_probe_total, gw_adaptive_rpm_released_total, gw_adaptive_rpm_learned_value, gw_adaptive_rpm_confidence
    - IP: gw_ip_blocklist_hit_total, gw_ip_allowlist_check_total, gw_ip_ratelimit_check_total, gw_ip_ratelimit_entries_active, gw_ip_ratelimit_sweep_total, gw_ip_blocklist_entries, gw_ip_allowlist_entries
  - Update `docs/architecture.md` with new crate additions and IP filter changes
  - Update `docs/workflow.md` with IP filter chain position in filter lifecycle

  **Must NOT do**:
  - 不得添加 spec 未定义的 metrics
  - 不得修改非 rate-limit 相关文档

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after Tasks 13, 14)
  - **Blocks**: Task 38
  - **Blocked By**: Tasks 13, 14

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/metrics.rs` 或类似 — 现有 metrics 注册模式
  - `docs/architecture.md` — architecture 文档结构
  - `docs/workflow.md` — workflow 文档结构

  **API/Type References**:
  - RL-Adaptive spec ADR-RLA-9: metrics 完整列表
  - RL-IP spec ADR-RLIP-8: metrics 完整列表

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 所有 metrics 已注册
    Tool: Bash (grep)
    Steps:
      1. grep -c "gw_adaptive_rpm" crates/gw-auth/src/ratelimit/adaptive.rs → ≥6
      2. grep -c "gw_ip_" crates/gw-auth/src/ip_ratelimit.rs → ≥4
      3. grep -c "ip_filter\|ip_ratelimit\|adaptive_rpm" docs/architecture.md → ≥3
      4. grep -c "IP.*filter\|ip_filter" docs/workflow.md → ≥1
    Expected Result: All metrics registered and docs updated
    Evidence: .omo/evidence/task-15-metrics-docs.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-auth): register rate limit metrics; docs: sync architecture + workflow`
  - Files: `crates/gw-auth/src/ratelimit/adaptive.rs`, `crates/gw-auth/src/ip_ratelimit.rs`, `docs/architecture.md`, `docs/workflow.md`
  - Pre-commit: `cargo build --release -p gw-gateway && bash scripts/check-invariants.sh`

- [x] 16. CLI Variants spec 编写

  **What to do**:
  - Write design spec `docs/superpowers/specs/2026-05-26-cli-variants-design.md`:
    - Define 4 FORMAT_IDs: `claude:cli`, `openai:cli`, `gemini:cli`, `openai:compact`
    - Document transparent proxy model: body passthrough, no IR conversion
    - Document filter chain preservation: auth, permissions, rate limiting, billing all active
    - Define how billing records are generated without IR (token counting from raw response)
    - Define `CliCodec` trait surface and registration mechanism
    - Define detection heuristic (how gateway identifies CLI vs standard requests)
    - ADR series: CLI-1 through CLI-8 covering each design decision
    - Test plan: minimum acceptance scenarios

  **Must NOT do**:
  - 不得编写实现代码——纯 spec
  - 不得发明未在 Aether 中存在的 FORMAT_IDs

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: 设计文档编写
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 12-15, 17)
  - **Blocks**: Tasks 20, 21, 22
  - **Blocked By**: None (spec work independent of code)

  **References**:

  **Pattern References**:
  - `docs/superpowers/specs/2026-05-19-task-worker-design.md` — 现有 spec 格式（ADR 系列风格）
  - `docs/superpowers/specs/` — spec 命名约定

  **External References**:
  - Aether `src/services/provider/handlers/claude_cli/` — claude:cli 行为参考
  - Aether `src/services/provider/handlers/openai_cli/` — openai:cli 行为参考（含 Responses API）
  - Aether `src/services/provider/handlers/gemini_cli/` — gemini:cli 行为参考
  - Aether `src/services/provider/base/cli_adapter.py` — CliAdapterBase 模式

  **WHY Each Reference Matters**:
  - Task worker spec 提供 ADR 编写格式模板
  - Aether CLI handlers 提供行为真相——spec 必须匹配 Aether 行为

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: CLI spec 包含所有必需 sections
    Tool: Bash (grep)
    Steps:
      1. grep -c "## FORMAT_ID" docs/superpowers/specs/2026-05-26-cli-variants-design.md → ≥1
      2. grep -c "ADR-CLI-" docs/superpowers/specs/2026-05-26-cli-variants-design.md → ≥8
      3. grep -c "claude:cli\|openai:cli\|gemini:cli\|openai:compact" docs/superpowers/specs/2026-05-26-cli-variants-design.md → ≥4
      4. grep -c "## Test Plan\|## Tests" docs/superpowers/specs/2026-05-26-cli-variants-design.md → ≥1
    Expected Result: All ADRs and FORMAT_IDs documented
    Evidence: .omo/evidence/task-16-cli-spec.txt

  Scenario: Spec 不包含实现代码
    Tool: Bash (grep)
    Steps:
      1. grep -ic "fn \|impl \|struct \|pub " docs/superpowers/specs/2026-05-26-cli-variants-design.md → 仅在代码块中
    Expected Result: No implementation code outside of illustrative snippets
    Evidence: .omo/evidence/task-16-cli-spec-no-code.txt
  ```

  **Commit**: YES
  - Message: `docs(specs): add CLI variants design spec with ADR-CLI-1 through CLI-8`
  - Files: `docs/superpowers/specs/2026-05-26-cli-variants-design.md`

- [x] 17. gw-task crate: DTOs + 合约 trait

  **What to do**:
  - RED: Write tests:
    - `test_task_id_display_roundtrip` — TaskId Display/FromStr roundtrip
    - `test_task_status_transitions` — valid state transitions per ADR-T4
    - `test_task_record_serialize` — TaskRecord serde roundtrip
    - `test_task_kind_enum` — all TaskKind variants present
  - GREEN: Create `crates/gw-task/` crate:
    - `Cargo.toml` with minimal deps (serde, uuid, thiserror)
    - `src/lib.rs` — re-exports
    - `src/types.rs` — TaskId, TaskStatus (Pending/Queued/Running/Succeeded/Failed/Cancelled/Cancelling), TaskKind, TaskRecord
    - `src/contract.rs` — `TaskContract` trait: submit, load, watch, request_cancel, claim, update_running, settle, recover_lease
    - NO Lua scripts yet (Task 18)
  - REFACTOR: Ensure types are `#[derive(Clone, Debug, Serialize, Deserialize)]`

  **Must NOT do**:
  - 不得实现 Lua scripts（Task 18）
  - 不得实现 Redis protocol（Task 19）
  - 不得添加 gw-store 依赖

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 新 crate 创建，需要精确的 type 设计
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3 (with Tasks 12-16)
  - **Blocks**: Tasks 18, 19
  - **Blocked By**: Task 5

  **References**:

  **Pattern References**:
  - `crates/gw-core/` — 现有 crate 结构（Cargo.toml, lib.rs 组织）
  - `crates/gw-codec/` — 另一个业务 crate 组织参考

  **API/Type References**:
  - Task Worker spec ADR-T1: dependency graph（`gw-task` 位置）
  - Task Worker spec ADR-T3: Redis protocol contract（TaskRecord 字段）
  - Task Worker spec ADR-T4: state machine（TaskStatus 枚举）
  - Task Worker spec: gw-task API surface 完整定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: gw-task crate 编译且类型正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-task → PASS
      2. cargo test -p gw-task test_task_id_display_roundtrip → PASS
      3. cargo test -p gw-task test_task_status_transitions → PASS
      4. cargo test -p gw-task test_task_record_serialize → PASS
    Expected Result: All type tests pass
    Evidence: .omo/evidence/task-17-gw-task-types.txt

  Scenario: 不依赖 gw-store
    Tool: Bash (grep)
    Steps:
      1. grep -c "gw-store" crates/gw-task/Cargo.toml → 0
    Expected Result: No gw-store dependency
    Evidence: .omo/evidence/task-17-no-store-dep.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task): create task contract crate with DTOs and trait`
  - Files: `crates/gw-task/Cargo.toml`, `crates/gw-task/src/lib.rs`, `crates/gw-task/src/types.rs`, `crates/gw-task/src/contract.rs`
  - Pre-commit: `cargo test -p gw-task`

- [x] 18. gw-task: Lua scripts (SUBMIT, SETTLE, RECOVER_LEASE)

  **What to do**:
  - RED: Write tests:
    - `test_submit_lua_creates_hash_and_xadd` — SUBMIT script creates entity:task:<id> HASH + XADD to tasks:queue
    - `test_settle_lua_atomic_state_transition` — SETTLE atomically transitions state + updates billing
    - `test_recover_lease_lua_reclaims_expired` — RECOVER_LEASE finds expired leases in tasks:leases ZSET
  - GREEN: Implement Lua scripts as const &str in `src/scripts.rs`:
    - `SUBMIT_LUA`: HSET entity:task:<id> + XADD tasks:queue + ZADD tasks:leases
    - `SETTLE_LUA`: HSET entity:task:<id> status + ZREM tasks:leases + billing settlement
    - `RECOVER_LEASE_LUA`: ZRANGEBYSCORE tasks:leases with expired timestamps → re-queue
  - REFACTOR: Validate scripts compile (redis-rs Script validation)

  **Must NOT do**:
  - 不得在 Lua 中实现 billing 金额计算（Go 侧职责）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 19, 20, 21, 22)
  - **Blocks**: Tasks 23, 25
  - **Blocked By**: Task 17

  **References**:

  **Pattern References**:
  - `crates/gw-task-runtime/src/` — 现有 Lua script 模式（TaskFreezeRelease 的 Lua）
  - `crates/gw-store/src/` — Redis script 执行模式

  **API/Type References**:
  - Task Worker spec ADR-T3: Redis protocol contract 完整定义
  - Task Worker spec ADR-T8: billing integration (settle.lua)

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Lua scripts 格式正确且可加载
    Tool: Bash
    Steps:
      1. cargo test -p gw-task test_submit_lua_creates_hash_and_xadd → PASS
      2. cargo test -p gw-task test_settle_lua_atomic_state_transition → PASS
      3. cargo test -p gw-task test_recover_lease_lua_reclaims_expired → PASS
    Expected Result: All Lua script tests pass
    Evidence: .omo/evidence/task-18-lua-scripts.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task): add SUBMIT, SETTLE, RECOVER_LEASE Lua scripts`
  - Files: `crates/gw-task/src/scripts.rs`
  - Pre-commit: `cargo test -p gw-task`

- [x] 19. gw-task: Redis protocol contract 实现

  **What to do**:
  - RED: Write tests (需要 Docker/testcontainers，标记 `#[ignore]`):
    - `test_submit_stores_and_queues` — submit → HGET entity:task:<id> returns all fields
    - `test_load_returns_task_record` — load → returns deserialized TaskRecord
    - `test_request_cancel_xadd` — request_cancel → XADD tasks:cancel
    - `test_claim_xreadgroup` — claim → XREADGROUP from tasks:queue
  - GREEN: Implement `RedisTaskContract` in `src/redis_contract.rs`:
    - Implement all `TaskContract` trait methods using Lua scripts from Task 18
    - Connection pool management via fred (existing Redis client)
    - Key naming: `entity:task:{id}`, `tasks:queue`, `tasks:cancel`, `tasks:leases`
  - REFACTOR: Follow existing fred usage patterns

  **Must NOT do**:
  - 不得引入新的 Redis client library——使用现有 fred

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Tasks 20, 21, 22)
  - **Parallel Group**: Wave 4 (with Tasks 20, 21, 22; after Task 18)
  - **Blocks**: Task 23
  - **Blocked By**: Tasks 17, 18 (uses Lua scripts from Task 18)

  **References**:

  **Pattern References**:
  - `crates/gw-store/src/redis/` — 现有 fred Redis 使用模式
  - `crates/gw-task-runtime/src/` — 现有 Redis task 操作模式

  **API/Type References**:
  - Task Worker spec ADR-T3: Redis key naming + field definitions
  - Task Worker spec ADR-T5: persistence layering

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Redis contract 功能正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-task -- --ignored test_submit_stores_and_queues → PASS
      2. cargo test -p gw-task -- --ignored test_load_returns_task_record → PASS
    Expected Result: Integration tests pass with Docker
    Evidence: .omo/evidence/task-19-redis-contract.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task): implement Redis task contract with fred`
  - Files: `crates/gw-task/src/redis_contract.rs`
  - Pre-commit: `cargo test -p gw-task`

- [x] 20. CLI Variants: codec scaffolding

  **What to do**:
  - RED: Write tests:
    - `test_cli_format_id_registration` — all 4 FORMAT_IDs registered in codec registry
    - `test_cli_codec_trait_surface` — CliCodec trait has required methods
  - GREEN: Implement in `crates/gw-codec/src/cli/`:
    - `mod.rs` — module declaration
    - `trait.rs` — `CliCodec` trait (or extend existing codec trait)
    - FORMAT_ID constants: `CLAUDE_CLI`, `OPENAI_CLI`, `GEMINI_CLI`, `OPENAI_COMPACT`
    - Registration into existing codec dispatch table
    - Detection heuristic skeleton (identify CLI vs standard requests)
  - REFACTOR: Follow existing codec module patterns

  **Must NOT do**:
  - 不得实现具体协议处理（Tasks 21, 22）
  - 不得修改现有 codec 的行为

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4 (with Tasks 18, 19, 21, 22)
  - **Blocks**: Tasks 21, 22
  - **Blocked By**: Task 16

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/` — 现有 codec 模块组织（openai/, anthropic/, gemini/）
  - `crates/gw-codec/src/registry.rs` 或类似 — codec 注册/dispatch 模式

  **API/Type References**:
  - CLI Variants spec (Task 16): FORMAT_ID 定义 + CliCodec trait

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: CLI FORMAT_IDs 注册成功
    Tool: Bash
    Steps:
      1. cargo test -p gw-codec test_cli_format_id_registration → PASS
      2. grep -c "claude:cli\|openai:cli\|gemini:cli\|openai:compact" crates/gw-codec/src/cli/ -r → ≥4
    Expected Result: All FORMAT_IDs in code
    Evidence: .omo/evidence/task-20-cli-scaffold.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-codec): scaffold CLI variant codec with 4 FORMAT_IDs`
  - Files: `crates/gw-codec/src/cli/mod.rs`, `crates/gw-codec/src/cli/trait.rs`
  - Pre-commit: `cargo test -p gw-codec`

- [x] 21. CLI Variants: claude:cli + openai:cli handlers

  **What to do**:
  - RED: Write tests:
    - `test_claude_cli_passthrough` — CLI-shaped Claude request → body passed through unmodified
    - `test_openai_cli_passthrough` — CLI-shaped OpenAI request → body passed through
    - `test_openai_cli_responses_api` — /v1/responses shaped request → transparent proxy
    - `test_cli_preserves_headers` — auth headers preserved through proxy
  - GREEN: Implement:
    - `crates/gw-codec/src/cli/claude_cli.rs` — Claude CLI codec (transparent body passthrough)
    - `crates/gw-codec/src/cli/openai_cli.rs` — OpenAI CLI codec (transparent, including Responses API)
    - Both: receive raw bytes, forward to upstream, return raw bytes (no IR conversion)
    - Both: extract minimal metadata for billing (model name, stream flag)
  - REFACTOR: Ensure response streaming works for CLI passthrough

  **Must NOT do**:
  - 不得构建 IR（整个 CLI 的核心原则是绕过 IR）
  - 不得添加 spec 未定义的 header 处理

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Task 22)
  - **Parallel Group**: Wave 4 (with Tasks 18, 19, 20, 22)
  - **Blocks**: Task 26
  - **Blocked By**: Task 20

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/openai/` — 现有 OpenAI codec 模式
  - `crates/gw-codec/src/anthropic/` — 现有 Anthropic codec 模式

  **External References**:
  - Aether `src/services/provider/handlers/claude_cli/` — Claude CLI 行为参考
  - Aether `src/services/provider/handlers/openai_cli/normalizer.py` — OpenAI CLI + Responses API 参考

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Claude CLI 透明代理
    Tool: Bash
    Steps:
      1. cargo test -p gw-codec test_claude_cli_passthrough → PASS
      2. cargo test -p gw-codec test_openai_cli_passthrough → PASS
      3. cargo test -p gw-codec test_openai_cli_responses_api → PASS
    Expected Result: CLI passthrough tests pass
    Evidence: .omo/evidence/task-21-cli-claude-openai.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-codec): implement claude:cli and openai:cli handlers`
  - Files: `crates/gw-codec/src/cli/claude_cli.rs`, `crates/gw-codec/src/cli/openai_cli.rs`
  - Pre-commit: `cargo test -p gw-codec`

- [x] 22. CLI Variants: gemini:cli + openai:compact handlers

  **What to do**:
  - RED: Write tests:
    - `test_gemini_cli_passthrough` — CLI-shaped Gemini request → body passthrough
    - `test_openai_compact_passthrough` — compact format request → body passthrough
    - `test_gemini_cli_streaming` — streaming response → transparent SSE passthrough
  - GREEN: Implement:
    - `crates/gw-codec/src/cli/gemini_cli.rs` — Gemini CLI codec
    - `crates/gw-codec/src/cli/openai_compact.rs` — OpenAI compact codec
    - Same transparent proxy model as Task 21
  - REFACTOR: Extract shared passthrough logic if duplicated with Task 21

  **Must NOT do**:
  - 不得构建 IR
  - 不得处理 cross-protocol 转换（CLI ↔ CLI 不转换）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Task 21)
  - **Parallel Group**: Wave 4 (with Tasks 18, 19, 20, 21)
  - **Blocks**: Task 26
  - **Blocked By**: Task 20

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/gemini/` — 现有 Gemini codec 模式
  - Task 21 — claude_cli/openai_cli 实现模式

  **External References**:
  - Aether `src/services/provider/handlers/gemini_cli/` — Gemini CLI 参考

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Gemini CLI + OpenAI compact 透明代理
    Tool: Bash
    Steps:
      1. cargo test -p gw-codec test_gemini_cli_passthrough → PASS
      2. cargo test -p gw-codec test_openai_compact_passthrough → PASS
    Expected Result: All CLI codec tests pass
    Evidence: .omo/evidence/task-22-cli-gemini-compact.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-codec): implement gemini:cli and openai:compact handlers`
  - Files: `crates/gw-codec/src/cli/gemini_cli.rs`, `crates/gw-codec/src/cli/openai_compact.rs`
  - Pre-commit: `cargo test -p gw-codec`

- [x] 23. gw-task-runtime: exec loop

  **What to do**:
  - RED: Write tests:
    - `test_exec_loop_claims_and_executes` — loop claims from tasks:queue, executes via reqwest
    - `test_exec_loop_updates_running_state` — after claim, task state → Running
    - `test_exec_loop_settles_on_success` — successful execution → settle (Succeeded)
    - `test_exec_loop_settles_on_failure` — upstream error → settle (Failed)
    - `test_exec_loop_respects_timeout` — task exceeds timeout → abort + settle (Failed)
  - GREEN: Implement exec loop in `crates/gw-task-runtime/src/exec_loop.rs`:
    - `XREADGROUP` concurrent claim from tasks:queue (per ADR-T6)
    - `reqwest` direct call to upstream vendor
    - Timeouts by vendor+task_type (configurable)
    - 4-layer concurrency counters (global, per-vendor, per-model, per-key)
    - On completion: call SETTLE_LUA
  - REFACTOR: Use existing reqwest patterns if any in codebase

  **Must NOT do**:
  - 不得直接导入 gw-proxy（ADR-T1 禁止边）
  - 不得在 exec loop 中实现 billing 计算

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5 (after Tasks 18, 19)
  - **Blocks**: Tasks 24, 28
  - **Blocked By**: Tasks 18, 19

  **References**:

  **Pattern References**:
  - `crates/gw-task-runtime/src/` — 现有 runtime 代码模式
  - `crates/gw-store/src/redis/` — XREADGROUP 使用模式

  **API/Type References**:
  - Task Worker spec ADR-T6: execution model 完整定义
  - Task Worker spec ADR-T3: Redis protocol (XREADGROUP)
  - Task Worker spec ADR-T12: gateway collaboration (zero crate interdependency)

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Exec loop claim + execute + settle
    Tool: Bash
    Steps:
      1. cargo test -p gw-task-runtime test_exec_loop_claims_and_executes → PASS
      2. cargo test -p gw-task-runtime test_exec_loop_settles_on_success → PASS
      3. cargo test -p gw-task-runtime test_exec_loop_respects_timeout → PASS
    Expected Result: Exec loop lifecycle tests pass
    Evidence: .omo/evidence/task-23-exec-loop.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task-runtime): implement exec loop with XREADGROUP claim and upstream execution`
  - Files: `crates/gw-task-runtime/src/exec_loop.rs`
  - Pre-commit: `cargo test -p gw-task-runtime`

- [x] 24. gw-task-runtime: cancel loop + watchdog loop + heartbeat loop

  **What to do**:
  - RED: Write tests:
    - `test_cancel_loop_receives_cancel` — XADD to tasks:cancel → cancel loop picks up
    - `test_cancel_loop_aborts_running` — cancelling running task → abort reqwest + settle (Cancelled)
    - `test_watchdog_recovers_expired_lease` — expired lease in tasks:leases → re-queue
    - `test_heartbeat_updates_lease` — running task → lease timestamp refreshed
  - GREEN: Implement:
    - `cancel_loop.rs`: XREADGROUP from tasks:cancel stream, fan-out to exec loop (ADR-T7 approach A)
    - `watchdog_loop.rs`: periodic RECOVER_LEASE_LUA, re-queue expired tasks
    - `heartbeat_loop.rs`: periodic ZADD tasks:leases with updated timestamp for running tasks
  - REFACTOR: Ensure graceful shutdown (tokio CancellationToken)

  **Must NOT do**:
  - 不得在 cancel 时跳过 billing settlement

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5 (after Task 23)
  - **Blocks**: Task 28
  - **Blocked By**: Task 23

  **References**:

  **API/Type References**:
  - Task Worker spec ADR-T7: cancel semantics (approach A)
  - Task Worker spec ADR-T3: tasks:cancel stream, tasks:leases ZSET
  - Task Worker spec ADR-T6: heartbeat interval

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Cancel + watchdog + heartbeat loops
    Tool: Bash
    Steps:
      1. cargo test -p gw-task-runtime test_cancel_loop_receives_cancel → PASS
      2. cargo test -p gw-task-runtime test_watchdog_recovers_expired_lease → PASS
      3. cargo test -p gw-task-runtime test_heartbeat_updates_lease → PASS
    Expected Result: All 3 secondary loops pass
    Evidence: .omo/evidence/task-24-secondary-loops.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task-runtime): add cancel, watchdog, and heartbeat loops`
  - Files: `crates/gw-task-runtime/src/cancel_loop.rs`, `crates/gw-task-runtime/src/watchdog_loop.rs`, `crates/gw-task-runtime/src/heartbeat_loop.rs`
  - Pre-commit: `cargo test -p gw-task-runtime`

- [x] 25. gw-billing crate: task-level billing formulas

  **What to do**:
  - RED: Write tests:
    - `test_freeze_amount_calculation` — given task config → correct freeze amount
    - `test_settle_amount_calculation` — given task result → correct settlement amount
    - `test_no_cost_per_token` — billing formulas never reference token prices
  - GREEN: Create `crates/gw-billing/` crate:
    - `Cargo.toml` — minimal deps (gw-task types)
    - `src/lib.rs` — re-exports
    - `src/formulas.rs` — task-level billing formulas:
      - `calculate_freeze_amount(task_kind, task_config) -> Decimal`
      - `calculate_settle_amount(task_kind, task_result) -> Decimal`
    - Formula: based on task type + vendor + duration (NOT token count)
  - REFACTOR: Ensure compatibility with existing `no_billing_crate` lint (may need lint update)

  **Must NOT do**:
  - 绝对不得计算 cost-per-token（`no_cost_micro` lint）
  - 不得导入 gw-codec 或 gw-proxy
  - 不得存储价格表——仅公式，价格来自配置/Go 侧

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 5 (with Tasks 23, 24, 26, 27)
  - **Blocks**: Task 32
  - **Blocked By**: Task 18

  **References**:

  **Pattern References**:
  - `crates/gw-task/src/types.rs` — TaskKind, TaskRecord 类型

  **API/Type References**:
  - Task Worker spec ADR-T8: billing integration（freeze + settle 语义）

  **External References (Aether 具体文件)**:
  - Aether `src/services/billing/calculator.py` — 计费公式引擎（freeze_amount 和 settle_amount 计算逻辑）
  - Aether `src/services/billing/formula_engine.py` — 公式定义（task_type→formula 映射规则）
  - Aether `src/services/billing/dimension_collector_service.py` — 维度采集（duration、vendor、task_type 作为计费输入）
  - **注意**: Aether 的 `token_normalization.py` 和 `precision.py` 属于 token-level 计费——Rust 侧禁止实现

  **WHY Each Reference Matters**:
  - ADR-T8 定义 freeze/settle 接口——gw-billing 的 pub API 必须匹配 ADR-T8 签名
  - Aether `calculator.py` 的 `freeze_amount(task_kind, vendor, config)` 签名是功能参考——但 Rust 实现必须遵守 `no_cost_micro` lint
  - Aether `formula_engine.py` 说明 task_type→formula 映射规则——Rust 侧需要等价的 enum dispatch

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Billing 公式正确且不含 token 价格
    Tool: Bash
    Steps:
      1. cargo test -p gw-billing test_freeze_amount_calculation → PASS
      2. cargo test -p gw-billing test_settle_amount_calculation → PASS
      3. cargo test -p gw-billing test_no_cost_per_token → PASS
      4. grep -ric "cost_per_token\|price_per_token\|token_price" crates/gw-billing/src/ → 0
    Expected Result: Billing formulas pass + no token pricing
    Evidence: .omo/evidence/task-25-gw-billing.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-billing): create task-level billing formula crate`
  - Files: `crates/gw-billing/Cargo.toml`, `crates/gw-billing/src/lib.rs`, `crates/gw-billing/src/formulas.rs`
  - Pre-commit: `cargo test -p gw-billing && bash scripts/check-invariants.sh`

- [x] 26. CLI Variants: filter chain 集成

  **What to do**:
  - RED: Write integration tests:
    - `test_cli_request_passes_auth` — CLI request without valid API key → 401
    - `test_cli_request_passes_ratelimit` — CLI request over RPM → 429
    - `test_cli_request_billing_recorded` — CLI request → usage:records XADD
    - `test_cli_request_body_not_ir_converted` — CLI request body bytes == upstream request body bytes
  - GREEN: Wire CLI codecs into filter chain:
    - Detection: identify CLI FORMAT_ID from request path/headers
    - Auth: pass through existing auth filter (no bypass)
    - Permissions: pass through existing permission merge
    - Rate limiting: pass through existing RPM + adaptive RPM
    - Billing: extract model name + stream flag for usage:records (without IR)
    - Body: bypass IR conversion, forward raw bytes to upstream
    - Response: bypass IR conversion, forward raw bytes to client
  - REFACTOR: Ensure no performance regression for non-CLI requests

  **Must NOT do**:
  - 绝对不得绕过 auth/permissions/rate-limit/billing
  - 不得修改非 CLI 请求的处理路径

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5 (after Tasks 21, 22)
  - **Blocks**: Task 27
  - **Blocked By**: Tasks 21, 22

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/` — filter chain integration 模式
  - `crates/gw-codec/src/` — codec dispatch 模式
  - `docs/workflow.md` — filter chain lifecycle

  **API/Type References**:
  - CLI Variants spec (Task 16): filter chain preservation 定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: CLI 请求通过完整 filter chain
    Tool: Bash
    Steps:
      1. cargo test -p gw-proxy test_cli_request_passes_auth → PASS
      2. cargo test -p gw-proxy test_cli_request_passes_ratelimit → PASS
      3. cargo test -p gw-proxy test_cli_request_billing_recorded → PASS
      4. cargo test -p gw-proxy test_cli_request_body_not_ir_converted → PASS
    Expected Result: Full filter chain integration verified
    Evidence: .omo/evidence/task-26-cli-filter-chain.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-proxy): integrate CLI variants into filter chain with full auth/billing`
  - Files: `crates/gw-proxy/src/filters/`, `crates/gw-codec/src/cli/`
  - Pre-commit: `cargo test --workspace`

- [x] 27. CLI Variants: doc sync + 集成测试

  **What to do**:
  - Update `docs/architecture.md`:
    - Add CLI variant codec to §Workspace layout
    - Add CLI FORMAT_IDs to §Wire formats section
    - Document transparent proxy behavior in §Deliberate behaviours
  - Update `docs/workflow.md`:
    - Add CLI detection step in filter chain
    - Document IR bypass path
  - Write integration tests:
    - `test_cli_e2e_claude_cli` — full request/response cycle for claude:cli
    - `test_cli_e2e_openai_cli` — full cycle for openai:cli

  **Must NOT do**:
  - 不得修改非 CLI 相关文档 sections

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 5 (after Task 26)
  - **Blocks**: Task 38
  - **Blocked By**: Task 26

  **References**:

  **Pattern References**:
  - `docs/architecture.md` — 现有文档结构
  - `docs/workflow.md` — filter chain 文档

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 文档已更新
    Tool: Bash (grep)
    Steps:
      1. grep -c "CLI\|cli" docs/architecture.md → ≥3
      2. grep -c "CLI\|cli" docs/workflow.md → ≥2
      3. cargo test --workspace test_cli_e2e → PASS (if integration tests exist)
    Expected Result: Docs updated and integration tests pass
    Evidence: .omo/evidence/task-27-cli-docs.txt
  ```

  **Commit**: YES
  - Message: `docs: sync architecture + workflow for CLI variants; add CLI integration tests`
  - Files: `docs/architecture.md`, `docs/workflow.md`, test files
  - Pre-commit: `cargo test --workspace && bash scripts/check-invariants.sh`

- [x] 28. gw-task-worker binary: main + config + health/metrics

  **What to do**:
  - RED: Write tests:
    - `test_worker_config_deserialize` — config/task-default.toml deserializes correctly
    - `test_worker_health_endpoint` — /livez returns 200
    - `test_worker_metrics_endpoint` — /metrics returns Prometheus text
  - GREEN: Create `crates/gw-task-worker/`:
    - `Cargo.toml` — binary crate, deps: gw-task, gw-task-runtime, gw-billing, tokio, hyper, fred
    - `src/main.rs` — bootstrap: load config, init Redis pool, spawn 4 loops, start health server
    - Health: `:8081/livez`, `:8081/readyz`
    - Metrics: `:9091/metrics` (Prometheus)
    - Config: `config/task-default.toml` (Figment layers like gateway)
    - Graceful shutdown via tokio signal + CancellationToken
  - REFACTOR: Follow `gw-gateway` bootstrap patterns

  **Must NOT do**:
  - 不得添加 business HTTP endpoints（ADR-T2: no business HTTP on worker）
  - 不得导入 gw-proxy

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 6 (after Tasks 23, 24)
  - **Blocks**: Tasks 29, 30
  - **Blocked By**: Tasks 23, 24, 6

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/main.rs` — gateway bootstrap 模式（Figment, health, metrics）

  **API/Type References**:
  - Task Worker spec ADR-T10: ports/config 定义
  - Task Worker spec ADR-T2: worker responsibilities

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Worker binary 编译并启动
    Tool: Bash
    Steps:
      1. cargo build --release -p gw-task-worker → exit 0
      2. cargo test -p gw-task-worker test_worker_config_deserialize → PASS
    Expected Result: Binary builds and config works
    Evidence: .omo/evidence/task-28-worker-binary.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task-worker): create standalone worker binary with health and metrics`
  - Files: `crates/gw-task-worker/Cargo.toml`, `crates/gw-task-worker/src/main.rs`, `config/task-default.toml`
  - Pre-commit: `cargo build --release -p gw-task-worker`

- [x] 29. gw-task-worker: deployment separability

  **What to do**:
  - Verify ADR-T16 acceptance scenarios:
    - Gateway runs without worker → no task endpoints error gracefully
    - Worker runs without gateway → worker starts, waits for tasks
    - N gateways + M workers → tasks distributed correctly
  - Create deployment doc section in architecture.md
  - Add deployment-related config comments to task-default.toml

  **Must NOT do**:
  - 不得引入 gateway↔worker 直接通信（仅 Redis）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 6 (after Task 28)
  - **Blocks**: Task 38
  - **Blocked By**: Task 28

  **References**:

  **API/Type References**:
  - Task Worker spec ADR-T11: deployment model
  - Task Worker spec ADR-T16: deployment separability 5 scenarios

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Gateway 无 worker 时优雅降级
    Tool: Bash
    Steps:
      1. 启动 gateway (无 worker 进程) → gateway starts normally
      2. curl POST /v1/tasks → returns graceful error (not crash)
    Expected Result: Gateway doesn't crash without worker
    Evidence: .omo/evidence/task-29-separability.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task-worker): verify deployment separability per ADR-T16`
  - Files: `docs/architecture.md`, `config/task-default.toml`

- [x] 30. Gateway task path: task-shape detector + task request_filter

  **What to do**:
  - RED: Write tests:
    - `test_task_shape_detector_identifies_task` — task-shaped request detected
    - `test_non_task_request_passes_through` — normal chat request not detected as task
    - `test_task_request_filter_sets_context` — task request → ReqCtx flagged as task
  - GREEN: Implement in `crates/gw-proxy/`:
    - Task-shape detector: identify task requests from path/body patterns (per ADR-T15)
    - Task request_filter: set `ReqCtx` fields for task routing
    - Route task requests to task endpoints instead of upstream proxy
  - REFACTOR: Integrate with existing request_filter chain

  **Must NOT do**:
  - 不得修改非 task 请求的路由逻辑
  - 不得在 gateway 中执行 task（仅 submit/load/watch/cancel）

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 6 (after Task 28)
  - **Blocks**: Task 31
  - **Blocked By**: Tasks 5, 28

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/` — 现有 filter 模式
  - `crates/gw-core/src/` — ReqCtx 定义

  **API/Type References**:
  - Task Worker spec ADR-T15: gateway task path 完整定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Task 检测正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-proxy test_task_shape_detector_identifies_task → PASS
      2. cargo test -p gw-proxy test_non_task_request_passes_through → PASS
    Expected Result: Task detection works without false positives
    Evidence: .omo/evidence/task-30-task-detector.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-proxy): add task-shape detector and task request_filter`
  - Files: `crates/gw-proxy/src/`
  - Pre-commit: `cargo test -p gw-proxy`

- [x] 31. Gateway task path: 4 endpoints (submit/load/watch/cancel)

  **What to do**:
  - RED: Write tests:
    - `test_submit_endpoint_creates_task` — POST /v1/tasks → 201 with task_id
    - `test_load_endpoint_returns_task` — GET /v1/tasks/:id → 200 with TaskRecord
    - `test_watch_endpoint_streams_status` — GET /v1/tasks/:id/watch → SSE status updates
    - `test_cancel_endpoint_requests_cancel` — POST /v1/tasks/:id/cancel → 200
    - `test_submit_with_invalid_body` — malformed body → 400
    - `test_load_nonexistent_task` — unknown task_id → GW_TASK_NOT_FOUND (404)
  - GREEN: Implement 4 endpoints in gateway:
    - Submit: validate → TaskFreezeGuard::try_freeze → TaskContract::submit → 201
    - Load: TaskContract::load → serialize TaskRecord → 200
    - Watch: TaskContract::watch → SSE stream of status changes
    - Cancel: TaskContract::request_cancel → XADD tasks:cancel → 200
  - REFACTOR: Follow existing endpoint patterns

  **Must NOT do**:
  - 不得在 gateway 中执行 task（提交到 Redis，worker 执行）
  - 不得绕过 auth/billing for task endpoints

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 6 (after Task 30)
  - **Blocks**: Task 32
  - **Blocked By**: Task 30

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/` — 现有 endpoint routing 模式

  **API/Type References**:
  - Task Worker spec ADR-T15: 4 endpoints 完整定义
  - Task Worker spec ADR-T2: gateway responsibilities

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 4 task endpoints 功能正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-proxy test_submit_endpoint_creates_task → PASS
      2. cargo test -p gw-proxy test_load_endpoint_returns_task → PASS
      3. cargo test -p gw-proxy test_cancel_endpoint_requests_cancel → PASS
      4. cargo test -p gw-proxy test_load_nonexistent_task → PASS
    Expected Result: All 4 endpoints work correctly
    Evidence: .omo/evidence/task-31-task-endpoints.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-proxy): implement 4 task endpoints (submit/load/watch/cancel)`
  - Files: `crates/gw-proxy/src/`
  - Pre-commit: `cargo test -p gw-proxy`

- [x] 32. TaskFreezeGuard billing 集成

  **What to do**:
  - RED: Write tests:
    - `test_freeze_guard_freezes_on_submit` — task submit → balance frozen
    - `test_freeze_guard_insufficient_balance` — insufficient balance → GW_TASK_FREEZE_FAILED (402)
    - `test_settle_releases_freeze` — task completion → freeze released, actual amount deducted
    - `test_cancel_releases_freeze` — task cancel → freeze fully released
  - GREEN: Wire billing integration:
    - Gateway-side: `TaskFreezeGuard::try_freeze(task_kind, config, user_balance)` using gw-billing formulas
    - Worker-side: `settle.lua` integrates with billing settlement
    - Connect existing `TaskFreezeRelease` trait to new gw-billing formulas
  - REFACTOR: Ensure freeze/settle is atomic (Lua script level)

  **Must NOT do**:
  - 不得计算 token-level 价格
  - 不得绕过 InflightLedger 机制

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 6 (after Tasks 25, 31)
  - **Blocks**: Task 34
  - **Blocked By**: Tasks 25, 31

  **References**:

  **Pattern References**:
  - `crates/gw-task-runtime/src/` — 现有 TaskFreezeRelease trait
  - `crates/gw-ledger/` — InflightLedger 模式

  **API/Type References**:
  - Task Worker spec ADR-T8: billing integration 完整定义

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Freeze + settle lifecycle
    Tool: Bash
    Steps:
      1. cargo test test_freeze_guard_freezes_on_submit → PASS
      2. cargo test test_freeze_guard_insufficient_balance → PASS
      3. cargo test test_settle_releases_freeze → PASS
    Expected Result: Billing lifecycle correct
    Evidence: .omo/evidence/task-32-billing-integration.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task): integrate TaskFreezeGuard with gw-billing formulas`
  - Files: `crates/gw-task-runtime/src/`, `crates/gw-proxy/src/`
  - Pre-commit: `cargo test --workspace`

- [x] 33. PgSQL migration 0004_task_worker.sql

  **What to do**:
  - RED: Verify migration file doesn't exist: `ls crates/gw-store/migrations/0004*` → not found
  - GREEN: Create `crates/gw-store/migrations/0004_task_worker.sql`:
    - Task records table (task_id, user_id, api_key_id, kind, status, input_payload, output_payload, created_at, updated_at, settled_at)
    - Indexes on (user_id, status), (api_key_id), (created_at)
    - Worker registration table (worker_id, last_heartbeat, status)
  - REFACTOR: Follow existing migration style (0001, 0002, 0003)

  **Must NOT do**:
  - 不得修改现有 migrations
  - 不得添加 auto-apply 机制

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 6 (with other Wave 6 tasks)
  - **Blocks**: Tasks 28, 34
  - **Blocked By**: Task 17

  **References**:

  **Pattern References**:
  - `crates/gw-store/migrations/0001_initial.sql` — migration 格式
  - `crates/gw-store/migrations/0002_gemini_file_mapping.sql` — migration 命名

  **API/Type References**:
  - Task Worker spec ADR-T5: persistence layering
  - Task Worker spec ADR-T13: worker registration table

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Migration SQL 语法正确
    Tool: Bash
    Steps:
      1. test -f crates/gw-store/migrations/0004_task_worker.sql → true
      2. grep -c "CREATE TABLE" crates/gw-store/migrations/0004_task_worker.sql → ≥2
      3. grep -c "CREATE INDEX" crates/gw-store/migrations/0004_task_worker.sql → ≥2
    Expected Result: Migration file exists with correct structure
    Evidence: .omo/evidence/task-33-migration.txt
  ```

  **Commit**: YES
  - Message: `chore(migration): add 0004_task_worker.sql for task records and worker registration`
  - Files: `crates/gw-store/migrations/0004_task_worker.sql`

- [x] 34. Task Worker: observability

  **What to do**:
  - Register all task worker metrics per ADR-T9:
    - Worker: gw_task_claimed_total, gw_task_settled_total{status}, gw_task_execution_duration_seconds, gw_task_lease_recovered_total, gw_task_heartbeat_total, gw_task_cancel_received_total
    - Gateway: gw_task_gw_submit_total, gw_task_gw_freeze_total{result}, gw_task_gw_load_total, gw_task_gw_watch_total, gw_task_gw_cancel_total
  - Add detailed_log task_phases integration (if detailed_log infrastructure supports it)
  - Wire metrics into exec_loop, cancel_loop, watchdog_loop, heartbeat_loop, and gateway endpoints

  **Must NOT do**:
  - 不得添加 ADR-T9 未定义的 metrics
  - 不得引入高基数 labels (task_id 作为 label 是禁止的)

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 7 (after Task 32)
  - **Blocks**: Task 35
  - **Blocked By**: Task 32

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/metrics.rs` 或类似 — 现有 metrics registration 模式

  **API/Type References**:
  - Task Worker spec ADR-T9: observability metrics 完整列表

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Task worker metrics 已注册
    Tool: Bash (grep)
    Steps:
      1. grep -rc "gw_task_" crates/gw-task-runtime/src/ → ≥6
      2. grep -rc "gw_task_gw_" crates/gw-proxy/src/ → ≥5
      3. cargo build --release -p gw-task-worker → exit 0
    Expected Result: All metrics in code and binary compiles
    Evidence: .omo/evidence/task-34-observability.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task): register task worker metrics per ADR-T9`
  - Files: `crates/gw-task-runtime/src/`, `crates/gw-proxy/src/`
  - Pre-commit: `cargo build --release -p gw-task-worker`

- [x] 35. Task Worker: doc sync

  **What to do**:
  - Update `docs/architecture.md`:
    - Add gw-task, gw-task-runtime, gw-task-worker, gw-billing to §Workspace layout
    - Add task worker to §Persistence namespaces (Redis keys, PgSQL tables)
    - Add task-related error codes to error code section
    - Add Task Worker deployment model to deployment section
  - Update `docs/workflow.md`:
    - Add task-shape detection in filter chain
    - Add task endpoint routing
    - Document task lifecycle (gateway submit → Redis → worker execute → settle)

  **Must NOT do**:
  - 不得修改非 task 相关文档

  **Recommended Agent Profile**:
  - **Category**: `writing`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 7 (after Task 34)
  - **Blocks**: Task 38
  - **Blocked By**: Task 34

  **References**:

  **Pattern References**:
  - `docs/architecture.md` — 现有文档组织
  - `docs/workflow.md` — 现有 filter chain 文档

  **API/Type References**:
  - Task Worker spec ADR-T1 through T16: 所有 ADR 的文档化

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 文档更新完整
    Tool: Bash (grep)
    Steps:
      1. grep -c "gw-task" docs/architecture.md → ≥3
      2. grep -c "gw-billing" docs/architecture.md → ≥1
      3. grep -c "gw-task-worker" docs/architecture.md → ≥1
      4. grep -c "task" docs/workflow.md → ≥3 (new references)
    Expected Result: All new crates and concepts documented
    Evidence: .omo/evidence/task-35-task-docs.txt
  ```

  **Commit**: YES
  - Message: `docs: sync architecture + workflow for task worker system`
  - Files: `docs/architecture.md`, `docs/workflow.md`

- [x] 36. Task Worker: worker registration protocol

  **What to do**:
  - Implement `entity:worker:<id>` registration per ADR-T13:
    - Worker writes `entity:worker:<id>` HASH on startup (worker_id, start_time, status)
    - Periodic heartbeat updates `last_heartbeat`
    - Observability only — no routing decisions based on registration
  - Tests:
    - `test_worker_registers_on_startup` — worker start → entity:worker:<id> exists
    - `test_worker_heartbeat_updates` — after N seconds → last_heartbeat updated

  **Must NOT do**:
  - 不得将 worker 注册用于路由决策
  - 不得引入 worker-to-worker 通信

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 7 (with Tasks 34, 35, 37, 38, 39)
  - **Blocks**: Task 38
  - **Blocked By**: Task 28

  **References**:

  **API/Type References**:
  - Task Worker spec ADR-T13: worker registration protocol

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Worker 注册正确
    Tool: Bash
    Steps:
      1. cargo test -p gw-task-worker test_worker_registers_on_startup → PASS
    Expected Result: Registration works
    Evidence: .omo/evidence/task-36-worker-registration.txt
  ```

  **Commit**: YES
  - Message: `feat(gw-task-worker): add worker registration protocol per ADR-T13`
  - Files: `crates/gw-task-worker/src/`
  - Pre-commit: `cargo test -p gw-task-worker`

- [x] 37. check-invariants.sh 更新

  **What to do**:
  - Add new architectural invariants to `scripts/check-invariants.sh`:
    - Verify gw-task-runtime does NOT import gw-proxy (ADR-T1 forbidden edge)
    - Verify gw-billing does NOT contain `cost_per_token` or `price_per_token`
    - Verify CLI variant codecs do NOT bypass auth filter (grep for auth bypass patterns)
    - Verify new crates are listed in docs/architecture.md
  - Run `bash scripts/check-invariants.sh` to verify all pass

  **Must NOT do**:
  - 不得修改现有 lint rules
  - 不得降级任何 lint 从 error 到 warning

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 7 (after Tasks 15, 27, 35)
  - **Blocks**: Task 38
  - **Blocked By**: Tasks 15, 27, 35

  **References**:

  **Pattern References**:
  - `scripts/check-invariants.sh` — 现有 lint rule 格式

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: 所有 invariants 通过
    Tool: Bash
    Steps:
      1. bash scripts/check-invariants.sh → exit 0
      2. grep -c "gw-task-runtime.*gw-proxy\|gw-billing.*cost_per_token" scripts/check-invariants.sh → ≥2 (new rules exist)
    Expected Result: New rules added and all pass
    Evidence: .omo/evidence/task-37-invariants.txt
  ```

  **Commit**: YES
  - Message: `chore(invariants): add lint rules for task worker and CLI variant architectural boundaries`
  - Files: `scripts/check-invariants.sh`
  - Pre-commit: `bash scripts/check-invariants.sh`

- [x] 38. Full workspace build + clippy + test gate

  **What to do**:
  - Run full verification suite:
    - `cargo build --release -p gw-gateway`
    - `cargo build --release -p gw-task-worker`
    - `cargo test --workspace`
    - `cargo clippy --workspace --all-targets -- -D warnings`
    - `cargo fmt --all -- --check`
    - `bash scripts/check-invariants.sh`
  - Fix any compilation errors, warnings, or test failures
  - Verify all new crates are in workspace Cargo.toml

  **Must NOT do**:
  - 不得 suppress warnings with `#[allow(...)]` unless justified

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 7 (last, after all other tasks)
  - **Blocks**: Task 39
  - **Blocked By**: All previous tasks

  **References**:

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Full workspace 编译通过
    Tool: Bash
    Steps:
      1. cargo build --release -p gw-gateway → exit 0
      2. cargo build --release -p gw-task-worker → exit 0
      3. cargo test --workspace → all pass
      4. cargo clippy --workspace --all-targets -- -D warnings → 0 warnings
      5. cargo fmt --all -- --check → exit 0
      6. bash scripts/check-invariants.sh → exit 0
    Expected Result: All 6 gates pass
    Evidence: .omo/evidence/task-38-full-build.txt
  ```

  **Commit**: NO (verification only)

- [x] 39. e2e-soak-smoke regression gate

  **What to do**:
  - Run `make e2e-soak-smoke` to verify no regression in existing functionality
  - Verify new features don't break existing soak invariants:
    - Process liveness
    - Account consistency (usage:records + dlq == fake_upstream_hits_total)
    - Resource gauge release
    - Error-code catalog compliance
  - Document any new soak considerations for future work

  **Must NOT do**:
  - 不得修改 gateway 代码 for soak (§C1 invariant)
  - 不得 skip soak if Docker is available

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 7 (last, after Task 38)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 38

  **References**:

  **External References**:
  - `docs/superpowers/specs/2026-05-22-chaos-soak-design.md` — soak harness spec

  **Acceptance Criteria**:

  **QA Scenarios:**

  ```
  Scenario: Soak smoke 通过
    Tool: Bash
    Steps:
      1. make e2e-soak-smoke → exit 0
    Expected Result: No regression in soak invariants
    Failure Indicators: Non-zero exit code, account drift, resource leak
    Evidence: .omo/evidence/task-39-soak-smoke.txt
  ```

  **Commit**: NO (verification only)

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, curl endpoint, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo build --release -p gw-gateway`, `cargo build --release -p gw-task-worker`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Review all changed files for: `as any`/`@ts-ignore` (N/A for Rust, check `unsafe` blocks instead), empty `match` arms, `println!` in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.
  Output: `Build [PASS/FAIL] | Clippy [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-task integration (adaptive RPM + IP limiter together, task worker + billing, CLI variants + auth). Test edge cases: empty state, invalid input, rapid actions. Save to `.omo/evidence/final-qa/`.
  Output: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination: Task N touching Task M's files. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

Each domain = separate PR with atomic commits:

- **Domain D** (Wave 1): `docs(go-contracts): add wallet/payment, subscription/video, oauth/ldap contract docs`
- **Domain A** (Wave 2-3): 
  - `feat(gw-auth): add IP rate limiter + allowlist guard`
  - `feat(gw-auth): add adaptive RPM manager with AIMD learning`
  - `feat(gw-auth): integrate IP filter + adaptive RPM into filter chain`
- **Domain C** (Wave 3-5):
  - `docs(specs): add CLI variants design spec`
  - `feat(gw-codec): add CLI variant codecs (claude:cli, openai:cli, gemini:cli, openai:compact)`
- **Domain B** (Wave 3-7):
  - `feat(gw-task): add task contract crate with DTOs + Lua scripts`
  - `feat(gw-task-runtime): add worker execution loops`
  - `feat(gw-task-worker): add standalone worker binary`
  - `feat(gw-proxy): add gateway task path (4 endpoints)`
  - `feat(gw-billing): add task-level billing formulas`
  - `chore(migration): add 0004_task_worker.sql`

Pre-commit for each: `cargo test -p <crate> && cargo clippy -p <crate> -- -D warnings && bash scripts/check-invariants.sh`

---

## Success Criteria

### Verification Commands
```bash
cargo build --release -p gw-gateway      # Expected: exit 0
cargo build --release -p gw-task-worker   # Expected: exit 0
cargo test --workspace                     # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings  # Expected: 0 warnings
bash scripts/check-invariants.sh           # Expected: all pass
make e2e-soak-smoke                        # Expected: exit 0
```

### Final Checklist
- [x] All "Must Have" present (Adaptive RPM, IP Limiter, Task Worker, CLI Variants, Go Contracts)
- [x] All "Must NOT Have" absent (no cost-per-token, no todo!/unimplemented!, no scope creep)
- [x] All tests pass (TDD coverage for every new module)
- [x] Documentation synced (architecture.md, workflow.md updated in same commits)
- [ ] 4 domain PRs ready for review
