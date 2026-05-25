# V2 遗留代码全面清理

## TL;DR

> **Quick Summary**: 消除 ailinkmax-gateway 生产代码中所有遗留的 panic 路径、死代码、未接线功能和过时注释（P0-P5 + W1-W10 共 15 项），并新增回归测试，使项目达到完整 v2 可用状态。
>
> **Deliverables**:
> - 13 处 codec panic 替换为 graceful Err + 13 个回归测试
> - `bail!` 宏消除 26 处 `unreachable!()`
> - 死代码/死常量/占位测试清除
> - HardCapCfg 配置接线、metrics catalog 接线、IpBlockEntry PG 预加载、Responses UsageExtractor 路由
> - 过时注释清理 + docs 同步更新
>
> **Estimated Effort**: Large
> **Parallel Execution**: YES - 3 waves + Final
> **Critical Path**: T1/T3 → T6 → T12 → T13 → F1-F4

---

## Context

### Original Request
用户要求解决所有审计中发现的遗留未完成代码（P0-P5 + W1-W10），使网关达到完整可用的 v2 状态。

### Interview Summary
**Key Discussions**:
- P5 placeholder 测试：等探索后决定 → 4 个 REMOVE（Python E2E 覆盖），1 个 KEEP（e2e_tunnel）
- WARNING 项：全部处理，包括功能接线
- 测试策略：tests-after，新增回归测试

**Research Findings**:
- P1: `-> !` 不可行（stable 1.86），`bail!` 宏是最佳方案，已有 `mgmt_bail!` 先例
- P4: 6 处 dead_code 全部合理 KEEP，无需操作
- W1: HardCapCfg 跟 BillingCfg 走相同 Config→Wired→LlmGateway 接线模式
- W2: metrics_catalog.rs 集中式目录，~13 个运行时 metric 未注册
- W3: PG 表 ip_blocks 存在，Moka cache 已创建，缺 loader
- W4: `OpenAiUsageExtractor::responses()` 已实现，只需改 dispatch
- W6: _Phantom 是死代码脚手架，accumulator 已取代
- W8a/W8b: 不是 gap，W8c: Go 协调，W9: 已实现

### Metis Review
**Identified Gaps** (addressed):
- P0 panic→Err 语义变更需确认非 invariant-assertion 路径
- P1 `bail!` 宏位置需遵守 layering invariants
- W1 硬编码值必须与 TOML 默认值匹配
- W3 PG preload 需考虑启动阻塞和 fail-closed 模式
- W4 ApiShape 必须用 exhaustive match（无 `_ =>`）
- 每个 commit 必须包含 CLAUDE.md sync rule 要求的 doc 更新
- Scope lock: 发现新 gap → 记录到 docs/superpowers/plans/，不在本工作中修复

---

## Work Objectives

### Core Objective
消除所有遗留 panic 路径和死代码，接线所有 TODO 标记的未完成功能，清理过时注释，使生产代码无遗留标记。

### Concrete Deliverables
- `crates/gw-codec/src/`: 13 处 panic 替换为 ErrorIR + 13 个回归测试
- `crates/gw-proxy/src/filters/request.rs`: `bail!` 宏 + 26 处调用迁移
- `crates/gw-core/src/error/codes.rs`: 删除 `GW_REQUEST_NONSTREAM_NOT_SUPPORTED`
- `crates/gw-codec/src/wire/any.rs`: 移除 `_Phantom` 变体和整个死枚举
- `crates/gw-codec/src/stream_accumulator.rs`: `tracing::debug!` 加入 catch-all
- 4 个 placeholder 测试文件删除
- `config/default.toml` + `config.rs`: `[auth]` section + HardCapCfg 接线
- `metrics_catalog.rs`: 新增 `gw_daily_quota_increment_error_total` + 注册已有未编目 metrics
- `gw-store/src/loader.rs`: `load_ipblock()` + `ipblock_or_load()`
- `gw-proxy/src/filters/response.rs`: `extractor_for_kind` 改用 `ApiShape`
- `docs/architecture.md`: 同步更新配置表、持久化命名空间、指标
- 过时注释清理（W8c timezone、W9 FailoverExecutor）

### Definition of Done
- [ ] `cargo build --release -p gw-gateway` → exit 0
- [ ] `cargo test --workspace` → exit 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- [ ] `bash scripts/check-invariants.sh` → exit 0
- [ ] `grep -rE 'unimplemented!|todo!\(' crates/ --include='*.rs'` → 0 matches in prod
- [ ] `grep -r 'GW_REQUEST_NONSTREAM_NOT_SUPPORTED' crates/` → 0 matches
- [ ] `grep -c 'panic!("expected' crates/gw-codec/src/` → 0
- [ ] `make e2e-soak-smoke` → exit 0

### Must Have
- 所有 13 处 codec panic 替换为 Err，保留 happy-path 行为不变
- `bail!` 宏集中化 unreachable，有文档注释
- HardCapCfg TOML 默认值精确匹配当前硬编码值（1_000_000 / 5_000_000_000）
- IpBlockEntry preload 遵循 fail-closed（PG 不可用 → 启动失败）
- W4 extractor dispatch 使用 exhaustive match（无 wildcard）
- 每个 commit 遵守 CLAUDE.md sync rule 更新对应 doc

### Must NOT Have (Guardrails)
- 不扩大 P0 范围到 codec 以外的 panic
- 不重构 `emit_and_bail` 本身（只包装宏）
- 不实现 Phase 2 WireAny wire types（W6 仅移除）
- 不触碰 Go 合约/Go 侧代码
- 不添加 `unimplemented!()` / `todo!()` / `TODO(v2)` / "for v1" 标记
- 不新增 `#[allow(dead_code)]` 除非有明确理由
- 不升级 Rust 工具链（锁定 1.86.0）
- 不做全量 metric catalog sweep（仅接线已有的 ~13 个未注册 metric）
- 发现新 gap → 记录 `docs/superpowers/plans/`，不在本工作修复

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** - ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES
- **Automated tests**: YES (tests-after)
- **Framework**: cargo test (Rust built-in)
- **If TDD**: N/A — tests-after strategy

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Codec changes**: `cargo test -p gw-codec` — assert specific tests pass
- **Proxy changes**: `cargo test -p gw-proxy` — assert build + tests pass
- **Config changes**: `cargo test -p gw-gateway` — verify config parsing
- **Global gate**: `cargo clippy --workspace --all-targets -- -D warnings` + `bash scripts/check-invariants.sh`
- **Soak**: `make e2e-soak-smoke` after all tasks complete

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — dead code removal, ALL parallel):
├── Task 1: Remove dead constant GW_REQUEST_NONSTREAM_NOT_SUPPORTED [quick]
├── Task 2: Remove 4 placeholder test files [quick]
├── Task 3: Remove _Phantom dead scaffolding from wire/any.rs [quick]
├── Task 4: Clean stale comments (W8c timezone, W9 FailoverExecutor) [quick]
└── Task 5: Add tracing::debug! to stream_accumulator catch-all [quick]

Wave 2 (After Wave 1 — core refactors + feature wireups, MAX PARALLEL):
├── Task 6: P0 codec panic→Err + 13 regression tests (depends: 1, 3) [deep]
├── Task 7: P1 bail! macro + 26 call-site migration [unspecified-high]
├── Task 8: W4 extractor_for_kind ApiShape dispatch [quick]
├── Task 9: W1 HardCapCfg config→Wired→LlmGateway wireup [unspecified-high]
├── Task 10: W2 Metrics catalog wireup [unspecified-high]
└── Task 11: W3 IpBlockEntry PG preload [deep]

Wave 3 (After Wave 2 — docs + final verify):
├── Task 12: Doc sync updates (architecture.md) [writing]
└── Task 13: Final invariant + soak verification [unspecified-high]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay

Critical Path: T1/T3 → T6 → T12 → T13 → F1-F4 → user okay
Parallel Speedup: ~70% faster than sequential
Max Concurrent: 6 (Wave 2)
```

### Dependency Matrix

| Task | Depends On | Blocks |
|------|-----------|--------|
| 1 | — | 6 |
| 2 | — | 13 |
| 3 | — | 6 |
| 4 | — | 12 |
| 5 | — | 12 |
| 6 | 1, 3 | 12, 13 |
| 7 | — | 12, 13 |
| 8 | — | 12, 13 |
| 9 | — | 12, 13 |
| 10 | — | 12, 13 |
| 11 | — | 12, 13 |
| 12 | 4, 5, 6, 7, 8, 9, 10, 11 | 13 |
| 13 | 12 | F1-F4 |
| F1-F4 | 13 | — |

### Agent Dispatch Summary

- **Wave 1**: **5** — T1-T5 → `quick`
- **Wave 2**: **6** — T6 → `deep`, T7 → `unspecified-high`, T8 → `quick`, T9 → `unspecified-high`, T10 → `unspecified-high`, T11 → `deep`
- **Wave 3**: **2** — T12 → `writing`, T13 → `unspecified-high`
- **FINAL**: **4** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. Remove dead constant `GW_REQUEST_NONSTREAM_NOT_SUPPORTED`

  **What to do**:
  - Delete the constant definition at `crates/gw-core/src/error/codes.rs:18-21` (the `pub const GW_REQUEST_NONSTREAM_NOT_SUPPORTED` and its doc comment about "v1.0 only — parse_response is not yet implemented")
  - Search entire workspace for any remaining references and remove them
  - The constant has zero usage sites (confirmed by grep) — it's dead code with a stale v1 comment
  - **Doc sync (CLAUDE.md rule)**: In same Wave 1 commit, remove closed items from `docs/architecture.md` §Known gaps (GW_REQUEST_NONSTREAM_NOT_SUPPORTED, _Phantom scaffolding, placeholder tests)

  **Must NOT do**:
  - Do not touch other error codes in the same file
  - Do not remove the comment about non-stream support being available — just remove the dead constant

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4, 5)
  - **Blocks**: Task 6 (P0 works in same error module)
  - **Blocked By**: None

  **References**:
  - `crates/gw-core/src/error/codes.rs:18-21` — The dead constant to remove. Lines 18-19 are the doc comment, line 21 is the `pub const`.
  - S-codec v2 landing confirmed non-stream support — the constant's premise ("parse_response not yet implemented") is false.

  **Acceptance Criteria**:
  - [ ] `grep -r 'GW_REQUEST_NONSTREAM_NOT_SUPPORTED' crates/` → 0 matches
  - [ ] `cargo build --workspace` → exit 0
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` → exit 0

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Dead constant fully removed
    Tool: Bash
    Steps:
      1. grep -r 'GW_REQUEST_NONSTREAM_NOT_SUPPORTED' crates/ --include='*.rs'
      2. Assert exit code 1 (no matches)
      3. cargo check --workspace
      4. Assert exit code 0
    Expected Result: Zero grep matches, clean build
    Evidence: .omo/evidence/task-1-dead-const-removed.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `refactor(cleanup): remove dead code, stale constants, placeholder tests`
  - Files: `crates/gw-core/src/error/codes.rs`

---

- [x] 2. Remove 4 placeholder test files

  **What to do**:
  - Delete `crates/gw-gateway/tests/e2e_cross_protocol_response.rs`
  - Delete `crates/gw-gateway/tests/e2e_full_flow.rs`
  - Delete `crates/gw-proxy/tests/end_to_end_openai.rs`
  - Delete `crates/gw-proxy/tests/connect_retry.rs`
  - Verify `crates/gw-gateway/tests/e2e_tunnel.rs` is KEPT (do NOT delete)
  - Check `Cargo.toml` files for any `[[test]]` entries referencing deleted files and remove them

  **Must NOT do**:
  - Do NOT delete `e2e_tunnel.rs` — it has real test bodies and no Python equivalent
  - Do NOT modify any other test files

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3, 4, 5)
  - **Blocks**: Task 13
  - **Blocked By**: None

  **References**:
  - `crates/gw-gateway/tests/e2e_cross_protocol_response.rs` — S-codec T9 stub, covered by `tests/e2e/client/scenarios/test_non_stream.py::test_cross_protocol_non_stream_round_trips`
  - `crates/gw-gateway/tests/e2e_full_flow.rs` — full e2e stub, covered by 30+ Python E2E scenarios
  - `crates/gw-proxy/tests/end_to_end_openai.rs` — broken stub (`build_test_gateway()` always returns None), covered by Python `test_happy_streaming.py`
  - `crates/gw-proxy/tests/connect_retry.rs` — empty placeholder, covered by Python `test_routing_retry.py` (5 scenarios)
  - `crates/gw-gateway/tests/e2e_tunnel.rs` — KEEP: real test bodies, only needs Docker, no Python equivalent

  **Acceptance Criteria**:
  - [ ] 4 files deleted, `e2e_tunnel.rs` still exists
  - [ ] `cargo test --workspace` → exit 0
  - [ ] `cargo test -p gw-gateway` → exit 0 (no missing test target errors)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Placeholder tests removed, tunnel kept
    Tool: Bash
    Steps:
      1. test ! -f crates/gw-gateway/tests/e2e_cross_protocol_response.rs
      2. test ! -f crates/gw-gateway/tests/e2e_full_flow.rs
      3. test ! -f crates/gw-proxy/tests/end_to_end_openai.rs
      4. test ! -f crates/gw-proxy/tests/connect_retry.rs
      5. test -f crates/gw-gateway/tests/e2e_tunnel.rs
      6. cargo test --workspace 2>&1 | tail -5
    Expected Result: All 4 files absent, tunnel present, tests pass
    Evidence: .omo/evidence/task-2-placeholder-tests-removed.txt

  Scenario: No broken test references
    Tool: Bash
    Steps:
      1. grep -r 'e2e_cross_protocol_response\|e2e_full_flow\|end_to_end_openai\|connect_retry' crates/gw-gateway/Cargo.toml crates/gw-proxy/Cargo.toml
      2. Assert exit code 1 (no matches)
    Expected Result: No Cargo.toml references to deleted tests
    Evidence: .omo/evidence/task-2-no-broken-refs.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `refactor(cleanup): remove dead code, stale constants, placeholder tests`
  - Files: 4 deleted test files

---

- [x] 3. Remove `_Phantom` dead scaffolding from `wire/any.rs`

  **What to do**:
  - Remove the entire `WireAnyResponse` enum and its `impl` block from `crates/gw-codec/src/wire/any.rs`
  - Remove the entire `WireAnyStreamChunk` enum and its `impl` block
  - These enums contain only `_Phantom(PhantomData)` variants — they are completely uninhabited dead code
  - The actual cross-protocol response rendering uses the S-codec accumulator, not these enums
  - Use `lsp_find_references` or `grep` to confirm zero usage outside `wire/any.rs` before removal
  - If the file becomes empty (only `WireAnyRequest` remains), keep the file with just `WireAnyRequest`
  - Remove any `use` imports of the deleted types across the workspace

  **Must NOT do**:
  - Do NOT remove `WireAnyRequest` — it has real variants (OpenAi, Anthropic, Gemini) and is actively used
  - Do NOT implement Phase 2 wire types — removal only
  - Do NOT touch the accumulator-based response rendering path

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 4, 5)
  - **Blocks**: Task 6 (works in gw-codec)
  - **Blocked By**: None

  **References**:
  - `crates/gw-codec/src/wire/any.rs:38-72` — The two dead enums with `_Phantom` variants and `unreachable!()` in `kind()` methods
  - `crates/gw-codec/src/wire/any.rs:1-36` — `WireAnyRequest` — KEEP this, it has real variants
  - `crates/gw-proxy/src/filters/response.rs` — The S-codec accumulator path that superseded WireAny response enums

  **Acceptance Criteria**:
  - [ ] `grep -r '_Phantom' crates/gw-codec/src/wire/` → 0 matches
  - [ ] `grep -r 'WireAnyResponse\|WireAnyStreamChunk' crates/` → 0 matches (outside of any remaining doc comments)
  - [ ] `cargo check --workspace --all-targets` → exit 0

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Dead enums removed, WireAnyRequest preserved
    Tool: Bash
    Steps:
      1. grep -c '_Phantom' crates/gw-codec/src/wire/any.rs || echo "0"
      2. Assert output is "0"
      3. grep -c 'WireAnyRequest' crates/gw-codec/src/wire/any.rs
      4. Assert output > 0 (WireAnyRequest still exists)
      5. cargo check --workspace --all-targets
      6. Assert exit code 0
    Expected Result: _Phantom gone, WireAnyRequest preserved, clean build
    Evidence: .omo/evidence/task-3-phantom-removed.txt

  Scenario: No dangling imports
    Tool: Bash
    Steps:
      1. grep -rn 'WireAnyResponse\|WireAnyStreamChunk' crates/ --include='*.rs'
      2. Assert exit code 1 (no matches) OR only doc comments remain
    Expected Result: Zero usage of removed types
    Evidence: .omo/evidence/task-3-no-dangling-imports.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `refactor(cleanup): remove dead code, stale constants, placeholder tests`
  - Files: `crates/gw-codec/src/wire/any.rs`

- [x] 4. Clean stale comments (W8c timezone, W9 FailoverExecutor, misc follow-up)

  **What to do**:
  - `crates/gw-proxy/src/filters/request.rs:909` — Update comment "per-tenant timezone support is a v2 follow-up" to "per-tenant timezone support requires Go billing coordination; tracked separately" (remove v2 reference)
  - `crates/gw-proxy/src/gateway.rs:641` — Remove "remains deferred to the routing-r-e `FailoverExecutor v2`" since FailoverExecutor is already implemented and wired in `response_filter` (lines 515-556). Replace with accurate description of current state.
  - `crates/gw-proxy/src/filters/early.rs:6` — Review "can be added in a follow-up" and update to reflect current state
  - `crates/gw-proxy/src/filters/request.rs:699` — Review "fast-path follow-up" — the circuit check IS implemented (lines 773-794), update comment to be descriptive not aspirational
  - `crates/gw-gateway/src/lib.rs:73` — Update "Wave 1+ placeholders (known-gaps closure)" header since fields are now wired
  - `crates/gw-store/src/loader.rs:360-362` — Will be updated by Task 11 (IpBlockEntry preload), leave for now
  - `crates/gw-codec/src/openai/responses/codec.rs:8` — Update "deferred to T6" to reflect current state (T6 has landed or the approach changed)
  - `crates/gw-codec/src/openai/responses/conversion.rs:6` — Review "left to S2 / S4 follow-ups"

  **Must NOT do**:
  - Do NOT modify any logic — comment-only changes
  - Do NOT remove comments that describe correct current behavior (W8a pricing, W8b circuit)
  - Do NOT add `TODO(v2)` or similar markers — `no_v1_markers_in_prod` lint is error mode

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 3, 5)
  - **Blocks**: Task 12 (doc sync)
  - **Blocked By**: None

  **References**:
  - `crates/gw-proxy/src/gateway.rs:515-556` — FailoverExecutor IS wired in response_filter (proof W9 is stale)
  - `crates/gw-proxy/src/filters/request.rs:773-794` — USER_CIRCUIT check IS implemented (proof W8b is done)
  - `crates/gw-routing/src/failover.rs` — Full FailoverExecutor with Retryable5xx/429/NotRetryable classification
  - `scripts/check-invariants.sh` — `no_v1_markers_in_prod` lint will catch any new TODO(v2) markers

  **Acceptance Criteria**:
  - [ ] No comments referencing "v2 follow-up", "deferred to v2", "remains deferred" in modified files
  - [ ] `bash scripts/check-invariants.sh` → exit 0
  - [ ] `cargo build --workspace` → exit 0 (comment-only, but verify)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Stale follow-up comments cleaned
    Tool: Bash
    Steps:
      1. grep -n 'remains deferred' crates/gw-proxy/src/gateway.rs
      2. Assert exit code 1 (no matches)
      3. grep -n 'v2 follow-up' crates/gw-proxy/src/filters/request.rs
      4. Assert exit code 1 (no matches)
      5. bash scripts/check-invariants.sh
      6. Assert exit code 0
    Expected Result: No stale deferral comments, invariants pass
    Evidence: .omo/evidence/task-4-stale-comments-cleaned.txt

  Scenario: No new v1/v2 markers introduced
    Tool: Bash
    Steps:
      1. grep -rE 'TODO\(v2\)|for v1|wave-X' crates/gw-proxy/src/ --include='*.rs'
      2. Assert exit code 1 (no matches)
    Expected Result: Zero forbidden markers
    Evidence: .omo/evidence/task-4-no-v2-markers.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `refactor(cleanup): remove dead code, stale constants, placeholder tests`
  - Files: `crates/gw-proxy/src/gateway.rs`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/early.rs`, `crates/gw-gateway/src/lib.rs`, `crates/gw-codec/src/openai/responses/codec.rs`, `crates/gw-codec/src/openai/responses/conversion.rs`

---

- [x] 5. Add `tracing::debug!` to stream_accumulator catch-all

  **What to do**:
  - In `crates/gw-codec/src/stream_accumulator.rs:144`, the `_ => {}` catch-all silently drops unknown SSE event types
  - Replace `_ => {}` with `_ => { tracing::debug!(target: "gw_codec", event_type = %event_type_var, "unknown SSE event type dropped during accumulation"); }`
  - Identify the variable name holding the event type from surrounding code context
  - This enables future debugging when new upstream event types are added

  **Must NOT do**:
  - Do NOT change from `tracing::debug!` to a higher level (warn/error) — this is informational
  - Do NOT change the control flow — the catch-all still drops unknown events
  - Do NOT add a metric counter (out of scope)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 3, 4)
  - **Blocks**: Task 12 (doc sync)
  - **Blocked By**: None

  **References**:
  - `crates/gw-codec/src/stream_accumulator.rs:144` — The `_ => {}` catch-all to add logging to
  - `crates/gw-codec/src/stream_accumulator.rs:1-50` — Read module header for context on what the accumulator does and what event types exist
  - Other `tracing::debug!` uses in `gw-codec` — follow existing target naming convention (`"gw_codec"` or similar)

  **Acceptance Criteria**:
  - [ ] `grep -A1 '_ =>' crates/gw-codec/src/stream_accumulator.rs | grep 'tracing::debug'` → match found
  - [ ] `cargo test -p gw-codec` → exit 0
  - [ ] `cargo clippy -p gw-codec -- -D warnings` → exit 0

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Catch-all now logs instead of silently dropping
    Tool: Bash
    Steps:
      1. grep -A2 '_ =>' crates/gw-codec/src/stream_accumulator.rs
      2. Assert output contains 'tracing::debug!'
      3. cargo test -p gw-codec
      4. Assert exit code 0
    Expected Result: tracing::debug! present in catch-all arm, tests pass
    Evidence: .omo/evidence/task-5-accumulator-logging.txt

  Scenario: No elevated log level
    Tool: Bash
    Steps:
      1. grep -E 'tracing::(warn|error|info)!' crates/gw-codec/src/stream_accumulator.rs | grep -v '//'
      2. Assert exit code 1 (no non-comment matches for elevated levels in this file's catch-all)
    Expected Result: Only debug-level logging for unknown events
    Evidence: .omo/evidence/task-5-debug-level-only.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `refactor(cleanup): remove dead code, stale constants, placeholder tests`
  - Files: `crates/gw-codec/src/stream_accumulator.rs`

- [x] 6. P0: Replace 13 codec `panic!("expected X")` with `Err(ErrorIR)` + regression tests

  **What to do**:
  - Replace all 13 `panic!("expected X")` match arms in codec production code with `Err(internal_err("expected X, got ...".into()))` returns
  - The `internal_err` helper already exists at `crates/gw-codec/src/common.rs:24` — use it
  - Each panic site is a `_ => panic!(...)` or `other => panic!(...)` in a match arm during IR construction
  - After replacement, the function returns `Err(ErrorIR { category: Internal, ... })` which propagates to the proxy error handler and surfaces as a 500 (not a process crash)
  - Verify happy-path still works: each codec's existing tests must still pass
  - Add 13 regression tests (one per site) in the corresponding test file that constructs the malformed IR state and asserts `Err` is returned (not a panic)
  - For test helper: use `std::panic::catch_unwind` pattern to prove OLD code would have panicked, NEW code returns Err
  - **Doc sync (CLAUDE.md rule)**: If `docs/architecture.md` §Known gaps or §Deliberate behaviours references codec panic paths, update in same commit. If no codec panic reference exists in docs, no doc change needed for this task.

  **All 13 sites** (file:line → panic message):
  1. `gw-codec/src/openai/codec.rs:442` → `panic!("expected ContentIR::Text")`
  2. `gw-codec/src/openai/codec.rs:444` → `panic!("expected Assistant")`
  3. `gw-codec/src/openai/codec.rs:474` → `panic!("expected tool_calls")`
  4. `gw-codec/src/openai/via.rs:477` → `panic!("expected ToolChoiceIR::Function, got {other:?}")`
  5. `gw-codec/src/openai/responses/codec.rs:607` → `panic!("expected ContentIR::Text")`
  6. `gw-codec/src/openai/responses/codec.rs:609` → `panic!("expected Assistant")`
  7. `gw-codec/src/openai/responses/codec.rs:647` → `panic!("expected Assistant")`
  8. `gw-codec/src/anthropic/codec.rs:635` → `panic!("expected ContentIR::Text")`
  9. `gw-codec/src/anthropic/codec.rs:637` → `panic!("expected Assistant")`
  10. `gw-codec/src/gemini/codec.rs:552` → `panic!("expected Assistant")`
  11. `gw-codec/src/gemini/codec.rs:590` → `panic!("expected ToolCallStart first, got {other:?}")`
  12. `gw-codec/src/gemini/codec.rs:601` → `panic!("expected ToolCallDelta second, got {other:?}")`
  13. `gw-codec/src/gemini/codec.rs:605` → `panic!("expected End last, got {other:?}")`

  **Must NOT do**:
  - Do NOT touch panic!() in TEST code (`gw-codec/tests/*.rs`) — those are valid test assertions
  - Do NOT change happy-path behavior — only the error path changes from panic to Err
  - Do NOT expand scope to panics outside gw-codec
  - Do NOT change function signatures unless returning Result requires it (propagate Err via `?`)
  - If a function currently returns a non-Result type and contains a panic, wrap the relevant section to return Result and update call sites minimally

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 13 sites across 6 files, each needs careful context analysis + regression test
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T7, T8)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12, Task 13
  - **Blocked By**: Task 1 (same error module), Task 3 (same crate)

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/common.rs:24-30` — `internal_err()` helper that constructs `ErrorIR { category: Internal }`. USE THIS for all replacements.
  - `crates/gw-codec/src/common.rs:18-22` — `bad_request_err()` — DO NOT use this, it's for client errors. Use `internal_err` for IR invariant violations.

  **File References** (all sites):
  - `crates/gw-codec/src/openai/codec.rs:430-480` — 3 panic sites in `render_response` / tool-call handling
  - `crates/gw-codec/src/openai/via.rs:470-480` — 1 panic in ToolChoice conversion
  - `crates/gw-codec/src/openai/responses/codec.rs:600-650` — 3 panic sites in render_response
  - `crates/gw-codec/src/anthropic/codec.rs:630-640` — 2 panic sites in render_response
  - `crates/gw-codec/src/gemini/codec.rs:550-610` — 4 panic sites in render_response / tool-call streaming

  **Test References**:
  - `crates/gw-codec/tests/openai_roundtrip.rs` — Existing test pattern for OpenAI codec
  - `crates/gw-codec/tests/anthropic_roundtrip.rs` — Existing test pattern for Anthropic codec
  - `crates/gw-codec/tests/gemini_roundtrip.rs` — Existing test pattern for Gemini codec

  **Acceptance Criteria**:
  - [ ] `grep -c 'panic!("expected' crates/gw-codec/src/` → 0
  - [ ] `cargo test -p gw-codec` → all existing tests pass (happy-path preserved)
  - [ ] 13 new regression tests pass, each asserting `Err(ErrorIR)` on malformed input
  - [ ] `cargo clippy -p gw-codec -- -D warnings` → exit 0

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Zero panics remain in codec production code
    Tool: Bash
    Steps:
      1. grep -rn 'panic!("expected' crates/gw-codec/src/ --include='*.rs'
      2. Assert exit code 1 (no matches)
      3. cargo test -p gw-codec
      4. Assert exit code 0
    Expected Result: No panic!("expected in src/, all tests pass
    Evidence: .omo/evidence/task-6-panics-removed.txt

  Scenario: Regression tests verify Err on malformed IR
    Tool: Bash
    Steps:
      1. cargo test -p gw-codec -- ir_invariant 2>&1
      2. Assert output shows 13 tests passing (naming convention: test_*_ir_invariant_*)
    Expected Result: 13 regression tests, all pass
    Evidence: .omo/evidence/task-6-regression-tests.txt

  Scenario: Happy path preserved
    Tool: Bash
    Steps:
      1. cargo test -p gw-codec -- roundtrip 2>&1
      2. Assert all existing roundtrip tests still pass
    Expected Result: Zero regressions on happy path
    Evidence: .omo/evidence/task-6-happy-path.txt
  ```

  **Commit**: YES
  - Message: `fix(codec): replace panic with Err(Internal) in IR match arms`
  - Files: `crates/gw-codec/src/openai/codec.rs`, `crates/gw-codec/src/openai/via.rs`, `crates/gw-codec/src/openai/responses/codec.rs`, `crates/gw-codec/src/anthropic/codec.rs`, `crates/gw-codec/src/gemini/codec.rs`, + new test files
  - Pre-commit: `cargo test -p gw-codec && cargo clippy -p gw-codec -- -D warnings`

---

- [x] 7. P1: Create `bail!` macro + migrate 26 `unreachable!()` call sites

  **What to do**:
  - Create a `bail!` macro in `crates/gw-proxy/src/filters/request.rs` (same file where `emit_and_bail` and `mgmt_bail!` live)
  - Follow the existing `mgmt_bail!` pattern at line 74 as template
  - The macro wraps `emit_and_bail(session, ctx, &err, &request_id).await?; unreachable!()` into a single invocation
  - Replace all 26 occurrences of `emit_and_bail(...).await?; unreachable!()` with the new `bail!(...)` macro call
  - Add a doc comment on the macro explaining: (1) it always returns Err via `?`, (2) the `unreachable!()` is centralized here, (3) modifying `emit_and_bail` to return Ok would be a correctness bug
  - Add one unit test proving the macro propagates Err correctly
  - **Doc sync (CLAUDE.md rule)**: Update `docs/architecture.md` §Deliberate behaviours to document `bail!` macro — include in same commit

  **Macro signature** (approximate):
  ```rust
  /// Emit an error response to the client, then short-circuit the filter chain.
  ///
  /// This macro wraps `emit_and_bail` + `unreachable!()` — the `unreachable!()` is
  /// centralized here rather than repeated at 26 call sites. `emit_and_bail` always
  /// returns `Err`, so the `?` operator unwinds before `unreachable!()` is reached.
  macro_rules! bail {
      ($session:expr, $ctx:expr, $err:expr, $rid:expr) => {{
          emit_and_bail($session, $ctx, $err, $rid).await?;
          unreachable!("emit_and_bail always returns Err; see fn doc")
      }};
  }
  ```

  **Must NOT do**:
  - Do NOT refactor `emit_and_bail` itself — only wrap it
  - Do NOT export the macro outside `gw-proxy` (layering invariant)
  - Do NOT change `emit_and_bail`'s return type
  - Do NOT touch `mgmt_bail!` — it's a separate macro for the management path

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 26 mechanical replacements + 1 macro definition, moderate scope
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T6, T8)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12, Task 13
  - **Blocked By**: None (different file from T6)

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/request.rs:74-82` — `mgmt_bail!` macro — FOLLOW THIS PATTERN exactly for the new `bail!` macro
  - `crates/gw-proxy/src/filters/request.rs:926-938` — `emit_and_bail` fn definition — the function being wrapped

  **All 26 call sites** (each follows pattern `emit_and_bail(...).await?; unreachable!()`):
  - Lines: 88, 215, 222, 249, 295, 301, 317, 337, 360, 384, 407, 479, 494, 509, 528, 554, 574, 625, 645, 727, 768, 793, 806, 815, 830, 838

  **Acceptance Criteria**:
  - [ ] `grep -c 'unreachable!()' crates/gw-proxy/src/filters/request.rs` — reduced by 26 (only macro body + mgmt_bail body remain)
  - [ ] `cargo test -p gw-proxy` → exit 0
  - [ ] `cargo clippy -p gw-proxy -- -D warnings` → exit 0
  - [ ] `bail!` macro has doc comment

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: unreachable!() centralized into macro
    Tool: Bash
    Steps:
      1. grep -c 'unreachable!()' crates/gw-proxy/src/filters/request.rs
      2. Assert count ≤ 3 (1 in bail! body, 1 in mgmt_bail! body, possibly 1 in doc comment)
      3. grep -c 'bail!' crates/gw-proxy/src/filters/request.rs
      4. Assert count ≥ 26 (macro calls) + 1 (macro definition)
    Expected Result: unreachable centralized, bail! macro used at all 26 sites
    Evidence: .omo/evidence/task-7-bail-macro.txt

  Scenario: Macro does not leak outside gw-proxy
    Tool: Bash
    Steps:
      1. grep -rn 'bail!' crates/ --include='*.rs' | grep -v 'gw-proxy' | grep -v 'mgmt_bail' | grep -v test | grep -v '//'
      2. Assert no production code outside gw-proxy uses bail!
    Expected Result: bail! macro is crate-local
    Evidence: .omo/evidence/task-7-no-leak.txt

  Scenario: Build and tests pass
    Tool: Bash
    Steps:
      1. cargo test -p gw-proxy
      2. cargo clippy -p gw-proxy -- -D warnings
      3. Assert both exit 0
    Expected Result: Clean build, all tests pass
    Evidence: .omo/evidence/task-7-build-test.txt
  ```

  **Commit**: YES
  - Message: `refactor(proxy): introduce bail! macro, eliminate 26 unreachable!()`
  - Files: `crates/gw-proxy/src/filters/request.rs`
  - Pre-commit: `cargo test -p gw-proxy && cargo clippy -p gw-proxy -- -D warnings`

---

- [x] 8. W4: Route Responses UsageExtractor via `ApiShape` dispatch

  **What to do**:
  - In `crates/gw-proxy/src/filters/response.rs`, change `extractor_for_kind(kind: ProtocolKind)` to dispatch on `ApiShape` instead of `ProtocolKind`
  - New mapping (exhaustive match, NO wildcard arm):
    - `ApiShape::OpenAiChat` → `OpenAiUsageExtractor::chat()`
    - `ApiShape::OpenAiResponses` → `OpenAiUsageExtractor::responses()`
    - `ApiShape::AnthropicMessages` → `AnthropicUsageExtractor`
    - `ApiShape::GeminiGenerate` → `GeminiUsageExtractor`
  - Update the call site of `extractor_for_kind` to pass `ApiShape` instead of `ProtocolKind`
  - If `ReqCtx` doesn't already store `api_shape`, check where `ProtocolKind` is set and whether `ApiShape` is available at the same point
  - Remove the stale comment "the routing to `::responses()` is deferred to a later wave" at line 333

  **Must NOT do**:
  - Do NOT use a `_ =>` wildcard arm — exhaustive match enforces compile-time safety when new ApiShape variants are added
  - Do NOT change the UsageExtractor implementations — only the dispatch routing
  - Do NOT remove or modify `ProtocolKind` enum itself — other code uses it

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single function signature change + 1 match arm addition
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T6, T7)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12, Task 13
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-proxy/src/filters/response.rs:334-344` — Current `extractor_for_kind` implementation to modify
  - `crates/gw-core/src/protocol.rs:35-39` — `ApiShape` enum definition with 4 variants

  **API References**:
  - `crates/gw-codec/src/openai/usage_extractor.rs:20-28` — `OpenAiUsageExtractor::chat()` and `::responses()` constructors — both already fully implemented
  - `crates/gw-proxy/src/ctx.rs` — `ReqCtx` struct — check if `api_shape` field exists or needs to be added

  **Acceptance Criteria**:
  - [ ] `extractor_for_kind` dispatches on `ApiShape` with exhaustive match (no `_ =>`)
  - [ ] `cargo test -p gw-proxy` → exit 0
  - [ ] `cargo clippy -p gw-proxy -- -D warnings` → exit 0
  - [ ] Stale "deferred to a later wave" comment removed

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Exhaustive ApiShape match, no wildcard
    Tool: Bash
    Steps:
      1. Read crates/gw-proxy/src/filters/response.rs and find extractor_for_kind
      2. Verify match arms cover OpenAiChat, OpenAiResponses, AnthropicMessages, GeminiGenerate
      3. grep '_ =>' in the extractor_for_kind function — assert no wildcard
      4. cargo check -p gw-proxy
    Expected Result: Exhaustive match, clean build
    Evidence: .omo/evidence/task-8-api-shape-dispatch.txt

  Scenario: Stale comment removed
    Tool: Bash
    Steps:
      1. grep -n 'deferred to a later wave' crates/gw-proxy/src/filters/response.rs
      2. Assert exit code 1 (no match)
    Expected Result: No stale deferral comment
    Evidence: .omo/evidence/task-8-stale-comment.txt
  ```

  **Commit**: YES
  - Message: `fix(codec): route Responses usage extractor via ApiShape`
  - Files: `crates/gw-proxy/src/filters/response.rs`, possibly `crates/gw-proxy/src/ctx.rs`
  - Pre-commit: `cargo test -p gw-proxy && cargo clippy -p gw-proxy -- -D warnings`

- [x] 9. W1: Wire HardCapCfg through Config → Wired → LlmGateway

  **What to do**:
  - **Step A**: Create `AuthCfg` struct in `crates/gw-gateway/src/config.rs` with:
    ```rust
    #[derive(Debug, Clone, Deserialize)]
    pub struct AuthCfg {
        #[serde(default = "default_hard_cap_max_input_tokens")]
        pub hard_cap_max_input_tokens: u32,
        #[serde(default = "default_hard_cap_max_cost_micro")]
        pub hard_cap_max_cost_micro: u64,
    }
    ```
    Default values MUST match current hardcoded: `1_000_000` and `5_000_000_000`
  - **Step B**: Add `#[serde(default)] pub auth: AuthCfg` to `Config` struct, replacing the `TODO(phase4-wireup)` comment at line 46
  - **Step C**: Add `[auth]` section to `config/default.toml` with the default values
  - **Step D**: Add validation in `Config::validate()` for non-zero values
  - **Step E**: Add `hard_cap_cfg: HardCapCfg` field to `Wired` struct in `lib.rs`, populate from `cfg.auth.*` in `bootstrap()`
  - **Step F**: Thread through `LlmGateway::new()` — add field to `LlmGateway` struct and its constructor
  - **Step G**: Replace hardcoded `HardCapCfg { max_input_tokens: 1_000_000, max_cost_micro: 5_000_000_000 }` at `request.rs:824-827` with `self.hard_cap_cfg` (or equivalent from LlmGateway field)
  - **Step H**: Remove the `TODO(phase4-wireup)` comment from both `config.rs` and `default.toml`
  - **Doc sync (CLAUDE.md rule)**: Update `docs/architecture.md` §Configuration table to add `[auth]` section with `hard_cap_max_input_tokens` and `hard_cap_max_cost_micro` — include in same commit
  - Update `crates/gw-gateway/tests/e2e_failure_modes.rs` if it constructs `HardCapCfg` directly

  **Must NOT do**:
  - Do NOT change default behavior — TOML defaults must exactly match prior hardcoded values
  - Do NOT add hot-reload — boot-time only, consistent with `[tunnel]` precedent
  - Do NOT rename `HardCapCfg` or change its API

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Multi-file config threading across 4 layers
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T6, T7, T8, T10, T11)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/config.rs:44-45` — `BillingCfg` pattern (FOLLOW THIS EXACTLY): `#[serde(default)] pub billing: BillingCfg`
  - `crates/gw-gateway/src/config.rs:515-533` — `Config::validate()` with cross-field invariant checks
  - `crates/gw-gateway/src/lib.rs:60-101` — `Wired` struct fields
  - `crates/gw-gateway/src/lib.rs:183-207` — `Wired` unpacking into `LlmGateway::new()`
  - `crates/gw-gateway/src/lib.rs:605` — `bootstrap()` return of `Wired`

  **Target References**:
  - `crates/gw-auth/src/hard_cap.rs:11` — `HardCapCfg` struct definition (2 fields: `max_input_tokens: u32`, `max_cost_micro: u64`)
  - `crates/gw-proxy/src/filters/request.rs:824-827` — Hardcoded construction to replace
  - `crates/gw-proxy/src/gateway.rs:97-155` — `LlmGateway` struct + `::new()` constructor
  - `config/default.toml:86-89` — TODO comment to replace with `[auth]` section

  **Acceptance Criteria**:
  - [ ] `grep -r 'TODO(phase4-wireup)' crates/ config/` → 0 matches
  - [ ] `grep -r 'max_input_tokens: 1_000_000' crates/gw-proxy/src/filters/request.rs` → 0 matches (no more hardcoded)
  - [ ] `cargo test -p gw-gateway` → exit 0 (config parsing test)
  - [ ] `cargo test --workspace` → exit 0
  - [ ] Default values match: `max_input_tokens = 1_000_000`, `max_cost_micro = 5_000_000_000`

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Config parsing with defaults matches prior hardcoded values
    Tool: Bash
    Steps:
      1. grep 'hard_cap_max_input_tokens' config/default.toml
      2. Assert value is 1_000_000
      3. grep 'hard_cap_max_cost_micro' config/default.toml
      4. Assert value is 5_000_000_000
      5. cargo test -p gw-gateway -- config 2>&1
      6. Assert exit code 0
    Expected Result: TOML defaults match hardcoded, config tests pass
    Evidence: .omo/evidence/task-9-config-defaults.txt

  Scenario: Hardcoded construction removed
    Tool: Bash
    Steps:
      1. grep -n 'HardCapCfg {' crates/gw-proxy/src/filters/request.rs
      2. Assert no inline construction with literal values (only field access from self)
      3. grep -r 'TODO(phase4' crates/ config/
      4. Assert exit code 1 (no matches)
    Expected Result: No hardcoded HardCapCfg, no TODO marker
    Evidence: .omo/evidence/task-9-hardcoded-removed.txt
  ```

  **Commit**: YES
  - Message: `feat(config): wire HardCapCfg through Config→Wired→LlmGateway`
  - Files: `crates/gw-gateway/src/config.rs`, `crates/gw-gateway/src/lib.rs`, `crates/gw-proxy/src/gateway.rs`, `crates/gw-proxy/src/filters/request.rs`, `config/default.toml`
  - Pre-commit: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`

---

- [x] 10. W2: Register metrics catalog entries + wire daily quota error counter

  **What to do**:
  - **Part A**: Add `MetricSpec` entry for `gw_daily_quota_increment_error_total` to `crates/gw-gateway/src/metrics_catalog.rs` METRICS array:
    ```rust
    MetricSpec {
        name: "gw_daily_quota_increment_error_total",
        kind: MetricKind::Counter,
        help: "Redis INCRBY+EXPIRE failures when bumping per-user daily token counter",
    },
    ```
  - **Part B**: Wire emission at `crates/gw-proxy/src/filters/logging.rs:314-316` — add `metrics::counter!("gw_daily_quota_increment_error_total").increment(1);` alongside the existing `tracing::warn!` in the error branch
  - **Part C**: Add test assertion in `crates/gw-gateway/tests/metrics_catalog.rs`
  - **Part D**: Register the ~13 existing but uncatalogued runtime metrics into the catalog. Known list:
    - `gateway_started_total` (observability.rs:52)
    - `gateway_requests_total` (logging.rs:98)
    - `gateway_tpm_consumed_total` (logging.rs:328)
    - `gateway_freeze_leak_recovered_total` (freeze.rs:135)
    - `gateway_usage_records_lost_total` (flusher.rs:90,215,223)
    - `gateway_usage_channel_full_total` (flusher.rs:108)
    - `gateway_usage_serialize_errors_total` (flusher.rs:174)
    - `gateway_usage_queue_errors_total` (flusher.rs:199)
    - `gateway_usage_flush_errors_total` (flusher.rs:209)
    - `gw_deduct_gc_removed_total` (deduct_gc.rs:56)
    - `gw_forced_params_applied_total` (upstream.rs:561)
    - `gw_classifier_outcome_total` (listeners/metrics.rs:12)
    - `gw_error_handler_ledger_total` (listeners/ledger.rs:23)
  - For each, add a `MetricSpec` entry with appropriate `kind` and `help` text
  - **Doc sync (CLAUDE.md rule)**: Update `docs/architecture.md` §Observability / Metrics to list `gw_daily_quota_increment_error_total` and note the ~13 newly catalogued metrics — include in same commit

  **Must NOT do**:
  - Do NOT add new metrics emission sites — only catalog what already emits
  - Do NOT add high-cardinality labels without review
  - Do NOT change metric names — register them as-is

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 14 catalog entries + 1 emission wireup across multiple files
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T6, T7, T8, T9, T11)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-gateway/src/metrics_catalog.rs` — Full catalog file, follow existing `MetricSpec` entry format
  - `crates/gw-gateway/tests/metrics_catalog.rs` — Catalog test file, add assertions following existing pattern
  - `crates/gw-proxy/src/error/listeners/metrics.rs:12` — Example of `metrics::counter!` emission

  **Target References**:
  - `crates/gw-proxy/src/filters/logging.rs:297-317` — TODO(metrics) site + error branch where counter emission goes
  - All 13 emission sites listed in Part D above — verify each metric name by reading the file

  **Acceptance Criteria**:
  - [ ] `grep 'gw_daily_quota_increment_error_total' crates/gw-gateway/src/metrics_catalog.rs` → match found
  - [ ] `grep 'gw_daily_quota_increment_error_total' crates/gw-proxy/src/filters/logging.rs` → match found (emission)
  - [ ] `grep 'TODO(metrics)' crates/gw-proxy/` → 0 matches
  - [ ] `cargo test -p gw-gateway -- metrics_catalog` → exit 0
  - [ ] All 14 metrics registered in catalog (count MetricSpec entries before/after)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Daily quota error metric registered and wired
    Tool: Bash
    Steps:
      1. grep -c 'gw_daily_quota_increment_error_total' crates/gw-gateway/src/metrics_catalog.rs
      2. Assert count ≥ 1
      3. grep -c 'gw_daily_quota_increment_error_total' crates/gw-proxy/src/filters/logging.rs
      4. Assert count ≥ 1
      5. cargo test -p gw-gateway -- metrics_catalog
      6. Assert exit code 0
    Expected Result: Metric in catalog + emission site, tests pass
    Evidence: .omo/evidence/task-10-daily-quota-metric.txt

  Scenario: All previously uncatalogued metrics now registered
    Tool: Bash
    Steps:
      1. grep -c 'MetricSpec' crates/gw-gateway/src/metrics_catalog.rs
      2. Assert count increased by ≥14 from baseline
      3. For each of: gateway_started_total, gateway_requests_total, gateway_tpm_consumed_total — grep in catalog
      4. Assert all found
    Expected Result: All runtime metrics registered in catalog
    Evidence: .omo/evidence/task-10-catalog-complete.txt
  ```

  **Commit**: YES
  - Message: `feat(metrics): register daily quota error counter + catalog uncatalogued metrics`
  - Files: `crates/gw-gateway/src/metrics_catalog.rs`, `crates/gw-gateway/tests/metrics_catalog.rs`, `crates/gw-proxy/src/filters/logging.rs`
  - Pre-commit: `cargo test -p gw-gateway -- metrics_catalog && cargo clippy --workspace --all-targets -- -D warnings`

---

- [x] 11. W3: Implement IpBlockEntry PG preload

  **What to do**:
  - **Step A**: Add `IP_BLOCK_SQL` constant in `crates/gw-store/src/loader.rs` following existing patterns (`USER_SQL`, `APIKEY_SQL`, etc.):
    ```rust
    const IP_BLOCK_SQL: &str = "SELECT payload FROM ip_blocks WHERE id = $1 AND deleted_at IS NULL";
    ```
  - **Step B**: Add `load_ipblock()` function in `loader.rs` following the `load_user()`/`load_apikey()` pattern:
    - L1: check Redis hash `entity:ipblock:{id}`
    - L2: fall through to PG via `load_entity()` with `IP_BLOCK_SQL`
    - Write-back to Redis with TTL (300s, matching `ttl_for("ipblock")`)
  - **Step C**: Add `ipblock_or_load(&self, id: &str)` method on `EntityStore` following `user_or_load()` pattern:
    - Moka dedup + cold-load timeout wrapping `load_ipblock()`
  - **Step D**: Add bootstrap preloader in `crates/gw-gateway/src/lib.rs` `bootstrap()`:
    - `SELECT id FROM ip_blocks WHERE deleted_at IS NULL` → for each id, call `store.ipblock_or_load(&id).await`
    - Use paginated/chunked loading (batch of 100) to avoid blocking on large tables
    - Log count at startup: `tracing::info!("preloaded {} ip_block entries", count)`
  - **Step E**: If PG query fails at bootstrap → fail-closed (return error, gateway doesn't start). Matches project's fail-closed pattern.
  - **Step F**: The existing `EntityStore::ipblock_snapshot()` (Moka cache iterator) continues working — now the cache is populated.
  - **Doc sync (CLAUDE.md rule)**: Update `docs/architecture.md` §Persistence namespaces to add `entity:ipblock:{id}` and §Bootstrap to note ip_block preload — include in same commit
  - Add integration test (with `#[ignore]` for testcontainers) that seeds an ip_block row, calls `ipblock_or_load`, and asserts the entry is returned.

  **Must NOT do**:
  - Do NOT implement a Patricia trie optimization — out of scope
  - Do NOT add hot-reload / cache invalidation beyond existing Moka TTL
  - Do NOT change the `ip_filter.rs` consumer — it already reads from `ipblock_snapshot()`

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Multi-layer implementation (PG SQL + Redis + Moka + bootstrap)
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T6, T7, T8, T9, T10)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 12
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-store/src/loader.rs:1-100` — `USER_SQL`, `load_user()` pattern to follow exactly
  - `crates/gw-store/src/loader.rs:90` — `ttl_for("ipblock") => 300` — TTL already configured
  - `crates/gw-store/src/entity_store.rs:176-205` — Moka cache for ipblock already created (max 10,000 entries), `ipblock_snapshot()` method
  - `crates/gw-store/src/entity_store.rs:100-140` — `user_or_load()` pattern to follow

  **Schema References**:
  - `crates/gw-store/migrations/0001_initial.sql:76-82` — `ip_blocks` table DDL: `(id, payload JSONB, version, updated_at, deleted_at)`
  - `crates/gw-core/src/entity.rs:290-311` — `IpBlockEntry` struct: `cidr`, `scope` (Global|User|ApiKey), `reason`, `expires_at`

  **Consumer References**:
  - `crates/gw-auth/src/ip_filter.rs` — `check_blocklist()` calls `store.ipblock_snapshot()` — does NOT need modification

  **Acceptance Criteria**:
  - [ ] `grep 'load_ipblock' crates/gw-store/src/loader.rs` → match found
  - [ ] `grep 'ipblock_or_load' crates/gw-store/src/entity_store.rs` → match found
  - [ ] `grep 'ip_block' crates/gw-gateway/src/lib.rs` → bootstrap preload code present
  - [ ] `cargo test --workspace` → exit 0
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
  - [ ] `grep 'preload-from-PG is a follow-up' crates/gw-store/src/loader.rs` → 0 matches (stale comment removed)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Loader function exists and compiles
    Tool: Bash
    Steps:
      1. grep -n 'fn load_ipblock' crates/gw-store/src/loader.rs
      2. Assert match found
      3. grep -n 'fn ipblock_or_load' crates/gw-store/src/entity_store.rs
      4. Assert match found
      5. cargo check --workspace
      6. Assert exit code 0
    Expected Result: Functions exist, clean build
    Evidence: .omo/evidence/task-11-loader-exists.txt

  Scenario: Bootstrap preload wired
    Tool: Bash
    Steps:
      1. grep -A5 'ip_block' crates/gw-gateway/src/lib.rs | head -10
      2. Assert bootstrap preload code is present
      3. grep 'preload-from-PG is a follow-up' crates/gw-store/src/loader.rs
      4. Assert exit code 1 (stale comment removed)
    Expected Result: Preload wired, stale comment gone
    Evidence: .omo/evidence/task-11-bootstrap-preload.txt

  Scenario: Fail-closed on PG error
    Tool: Bash
    Steps:
      1. Read the bootstrap ip_block preload code
      2. Verify it propagates PG errors with `?` (not swallowing)
      3. Verify no `.unwrap_or_default()` or similar fail-open pattern
    Expected Result: PG failure at boot → gateway fails to start
    Evidence: .omo/evidence/task-11-fail-closed.txt
  ```

  **Commit**: YES
  - Message: `feat(store): implement IpBlockEntry PG preload`
  - Files: `crates/gw-store/src/loader.rs`, `crates/gw-store/src/entity_store.rs`, `crates/gw-gateway/src/lib.rs`, new test file
  - Pre-commit: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`

- [x] 12. Doc sync audit: verify all code commits included their doc updates, fix any gaps

  **What to do**:
  - Per CLAUDE.md sync rule, doc updates MUST be in the same commit as the code change. The code tasks (T6, T7, T9, T10, T11) are each responsible for updating `docs/architecture.md` in their own commit.
  - This task is a **verification + gap-fix** task, NOT the primary doc authoring task.
  - **Each code task's doc responsibility** (executor must include in their commit):
    - T7 (bail! macro): Add to `docs/architecture.md` §Deliberate behaviours
    - T9 (HardCapCfg): Add to `docs/architecture.md` §Configuration table
    - T10 (metrics): Add to `docs/architecture.md` §Observability / Metrics
    - T11 (IpBlockEntry): Add to `docs/architecture.md` §Persistence namespaces + §Bootstrap
    - T1/T3/T6: Remove closed items from `docs/architecture.md` §Known gaps
  - **This task (T12) does**:
    1. Read `docs/architecture.md` end-to-end after all code commits land
    2. Verify each expected doc update is present (§Configuration, §Persistence, §Deliberate behaviours, §Metrics, §Known gaps)
    3. Fix any gaps that code tasks missed — create `git commit --fixup=<owning-sha>` and `git rebase -i --autosquash` to amend into the owning code commit (NEVER a standalone docs-only commit)
    4. Review `docs/workflow.md` for any stale references to removed tests or deferral comments — fix if found
    5. Verify no references to "IpBlockEntry preload is a follow-up", "HardCapCfg hardcoded", "_Phantom scaffolding" remain in docs

  **Must NOT do**:
  - Do NOT rewrite docs sections not affected by this plan
  - Do NOT add documentation for features that weren't changed
  - Do NOT update `docs/go-contracts/` (no Go changes in this plan)

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: Technical documentation updates
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (sequential after Waves 1-2)
  - **Blocks**: Task 13
  - **Blocked By**: Tasks 4, 5, 6, 7, 8, 9, 10, 11 (all code changes must land first)

  **References**:

  **Sync Rule Table** (from CLAUDE.md):
  - Configuration tables → `docs/architecture.md` §Configuration
  - Persistence namespaces → `docs/architecture.md` §Persistence
  - Background-task graph → `docs/architecture.md` (if bootstrap preloader changes startup)
  - Deliberate behaviours → `docs/architecture.md` §Deliberate behaviours
  - Known gaps → `docs/architecture.md` §Known gaps

  **Target Files**:
  - `docs/architecture.md` — Primary update target
  - `docs/workflow.md` — Check for stale references only

  **Acceptance Criteria**:
  - [ ] `docs/architecture.md` §Configuration includes `[auth]` section
  - [ ] `docs/architecture.md` §Persistence includes `entity:ipblock:{id}`
  - [ ] `docs/architecture.md` mentions `bail!` macro in §Deliberate behaviours
  - [ ] No references to "IpBlockEntry preload is a follow-up" or "HardCapCfg hardcoded" in docs
  - [ ] `cargo test --workspace` → exit 0 (docs don't break build)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Architecture doc updated for all changes
    Tool: Bash
    Steps:
      1. grep -c '\[auth\]' docs/architecture.md
      2. Assert count ≥ 1
      3. grep -c 'entity:ipblock' docs/architecture.md
      4. Assert count ≥ 1
      5. grep -c 'bail!' docs/architecture.md
      6. Assert count ≥ 1
    Expected Result: All sync rule items present
    Evidence: .omo/evidence/task-12-doc-sync.txt

  Scenario: No stale gap references in docs
    Tool: Bash
    Steps:
      1. grep -i 'follow-up\|deferred\|placeholder' docs/architecture.md | grep -i 'ipblock\|hardcap\|phantom\|nonstream'
      2. Assert exit code 1 (no matches for now-closed items)
    Expected Result: Closed gaps removed from docs
    Evidence: .omo/evidence/task-12-no-stale-gaps.txt
  ```

  **Commit**: YES
  - Message: `docs: sync architecture.md with v2 cleanup changes`
  - Files: `docs/architecture.md`, possibly `docs/workflow.md`

---

- [x] 13. Final invariant + soak verification

  **What to do**:
  - Run the full verification gate after ALL code and doc changes land:
    1. `cargo build --release -p gw-gateway` → exit 0
    2. `cargo test --workspace` → exit 0, capture test count
    3. `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
    4. `cargo fmt --all -- --check` → exit 0
    5. `bash scripts/check-invariants.sh` → exit 0
    6. Verify: `grep -rE 'unimplemented!|todo!\(' crates/ --include='*.rs'` → 0 matches in production paths
    7. Verify: `grep -rE 'TODO\(v2\)|for v1|wave-X follow' crates/ --include='*.rs'` → 0 matches in production paths (test/bench comments are OK)
    8. `make e2e-soak-smoke` → exit 0, zero invariant violations
  - Save all command output as evidence
  - If any check fails, identify which task introduced the failure and report

  **Must NOT do**:
  - Do NOT fix failures in this task — report them back for the responsible task to fix
  - Do NOT skip any verification step
  - Do NOT run full `make e2e-soak` (50k req) — smoke (100 rps × 10s) is the merge gate; full soak is post-merge

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Multi-step verification requiring all prior work complete
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after T12)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 12

  **References**:
  - `scripts/check-invariants.sh` — Architectural lints
  - `tests/e2e/client/.soak_report/` — Soak report output location
  - CLAUDE.md §Common commands — all verification commands listed

  **Acceptance Criteria**:
  - [ ] All 8 verification steps pass
  - [ ] Evidence files saved for each step
  - [ ] Zero production `unimplemented!`/`todo!`/`TODO(v2)` markers

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Full verification gate
    Tool: Bash
    Steps:
      1. cargo build --release -p gw-gateway 2>&1 | tail -3
      2. cargo test --workspace 2>&1 | tail -10
      3. cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
      4. cargo fmt --all -- --check 2>&1
      5. bash scripts/check-invariants.sh 2>&1 | tail -20
      6. grep -rE 'unimplemented!|todo!\(' crates/ --include='*.rs' | grep -v '/tests/' | grep -v '/benches/' | wc -l
      7. Assert line 6 output is "0"
    Expected Result: All gates green, zero forbidden markers
    Evidence: .omo/evidence/task-13-full-verify.txt

  Scenario: Soak smoke passes
    Tool: Bash
    Preconditions: Docker running, gateway binary built
    Steps:
      1. make e2e-soak-smoke 2>&1 | tail -20
      2. Assert exit code 0
      3. cat tests/e2e/client/.soak_report/summary.json | head -20
      4. Assert "invariant_violations": 0
    Expected Result: Soak smoke clean, zero violations
    Evidence: .omo/evidence/task-13-soak-smoke.txt
  ```

  **Commit**: NO (verification only)

---

## Final Verification Wave

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

  **Acceptance Criteria**:
  - [ ] Every "Must Have" item verified with file evidence or command output
  - [ ] Every "Must NOT Have" item verified absent via grep
  - [ ] All 13 task evidence files exist in `.omo/evidence/`
  - [ ] Verdict rendered as APPROVE or REJECT with justification

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check` + `cargo test --workspace`. Review all changed files for: empty catches, println! in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.
  Output: `Build [PASS/FAIL] | Lint [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

  **Acceptance Criteria**:
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
  - [ ] `cargo fmt --all -- --check` → exit 0
  - [ ] `cargo test --workspace` → exit 0 with zero failures
  - [ ] No `println!` in production code (only `tracing::*`)
  - [ ] No commented-out code blocks in changed files

- [x] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Run `cargo build --release -p gw-gateway`. Run `cargo test --workspace`. Run `bash scripts/check-invariants.sh`. Run `make e2e-soak-smoke`. Verify all pass with zero failures. Save evidence to `.omo/evidence/final-qa/`.
  Output: `Build [PASS/FAIL] | Tests [N/N] | Invariants [PASS/FAIL] | Soak [PASS/FAIL] | VERDICT`

  **Acceptance Criteria**:
  - [ ] `cargo build --release -p gw-gateway` → exit 0
  - [ ] `cargo test --workspace` → exit 0
  - [ ] `bash scripts/check-invariants.sh` → exit 0
  - [ ] `make e2e-soak-smoke` → exit 0, zero invariant violations
  - [ ] Evidence saved to `.omo/evidence/final-qa/`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

  **Acceptance Criteria**:
  - [ ] All 13 tasks verified 1:1 against spec
  - [ ] Zero scope creep detected (no changes beyond task spec)
  - [ ] Zero cross-task contamination (task N not touching task M's files without justification)
  - [ ] All "Must NOT do" items verified absent

---

## Commit Strategy

> **CLAUDE.md sync rule**: doc updates MUST be in the same commit as the code change. Each code commit below includes its doc update.

- **Commit A** (Wave 1): `refactor(cleanup): remove dead code, stale constants, placeholder tests` — T1-T5 + remove closed items from `docs/architecture.md` §Known gaps
- **Commit B** (Wave 2a): `fix(codec): replace panic with Err(Internal) in IR match arms` — T6
- **Commit C** (Wave 2b): `refactor(proxy): introduce bail! macro, eliminate 26 unreachable!()` — T7 + update `docs/architecture.md` §Deliberate behaviours
- **Commit D** (Wave 2c): `fix(codec): route Responses usage extractor via ApiShape` — T8
- **Commit E** (Wave 2d): `feat(config): wire HardCapCfg through Config→Wired→LlmGateway` — T9 + update `docs/architecture.md` §Configuration
- **Commit F** (Wave 2e): `feat(metrics): register daily quota error counter + catalog uncatalogued metrics` — T10 + update `docs/architecture.md` §Observability
- **Commit G** (Wave 2f): `feat(store): implement IpBlockEntry PG preload` — T11 + update `docs/architecture.md` §Persistence namespaces
- **Commit H** (Wave 3, if needed): `fixup! <owning commit>` — T12 audit finds gap → amend/autosquash into the owning code commit (NOT a standalone docs-only commit). Use `git commit --fixup=<sha>` + `git rebase -i --autosquash` to maintain CLAUDE.md same-commit rule.

---

## Success Criteria

### Verification Commands
```bash
cargo build --release -p gw-gateway   # Expected: exit 0
cargo test --workspace                 # Expected: exit 0, all tests pass
cargo clippy --workspace --all-targets -- -D warnings  # Expected: exit 0
cargo fmt --all -- --check             # Expected: exit 0
bash scripts/check-invariants.sh       # Expected: exit 0
make e2e-soak-smoke                    # Expected: exit 0, zero invariant violations
```

### Final Checklist
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
- [ ] No `panic!("expected` in gw-codec production code
- [ ] No `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` anywhere
- [ ] No `_Phantom` in wire/any.rs
- [ ] `bail!` macro replaces all emit_and_bail + unreachable patterns
- [ ] HardCapCfg reads from config, TOML defaults match prior hardcoded values
- [ ] IpBlockEntry loads from PG at bootstrap
- [ ] Responses extractor dispatched via ApiShape
- [ ] Metrics catalog includes all runtime-emitted metrics
- [ ] docs/architecture.md updated per sync rules
