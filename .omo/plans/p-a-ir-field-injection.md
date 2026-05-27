# P-A: IR 任意字段注入架构

## TL;DR

> **Quick Summary**: 解决 IR typed 子集 + zero-copy passthrough 被 `cow.into_owned()` 戳穿的根因问题。新增 `passthrough_overrides` 字段 + mutation flag 注册框架，使 model rewrite (T1) 和 body rules (T9) 可以根治性实现。
>
> **Deliverables**: IR 扩展、mutation flag 框架、passthrough 路径修正
> **Estimated Effort**: Medium
> **Blocks**: T1 (model rewrite), T9 (body rules)

---

## Context

### 根因分析

当前 `RequestIR` 是 typed 字段集合（`ir.rs:14`），`ForcedParams` 只有 `temperature`/`top_p` 两个字段（`entity.rs:266`）。upstream body 是 **从 IR re-render** 的（`upstream.rs:592-601`），不是 patch 原始 bytes。因此：

1. Model rewrite 改 `ir.model` → 同协议 zero-copy passthrough 路径（`mutations.is_empty()` 为 true）会 **跳过 re-render**，改写静默失效
2. Body rules 要设默认值 → 设置的字段在 re-render 时被 IR（不含该字段）丢弃
3. 所谓 "zero-copy passthrough" 其实已被 `bytes::Bytes::from(cow.into_owned())` 戳穿 — `Cow<[u8]>` 强制 `Vec<u8>` 拷贝

### 根治路径

- 扩展 IR 新增 `passthrough_overrides: Option<Map<String, RawValue>>`
- 在 codec render 阶段，overrides 合并到输出 JSON
- Model rewrite 和 body rules 都通过注册 mutation flag 强制走 parse→mutate→render 分支
- 顺带修正 `cow.into_owned()` 的假零拷贝

---

## Work Objectives

### Must Have
- IR 新增 `passthrough_overrides` 字段（Option, None 时零开销）
- mutation flag 注册框架（使 `mutations.is_empty()` 在有 override 时返回 false）
- codec render 阶段合并 overrides
- 修正 `upstream.rs:568` 的假零拷贝
- 单元测试 + 集成测试

### Must NOT Have
- 不改 IR 现有 typed 字段语义
- 不引入通用 request transformation DSL
- 不碰 response 路径
- 不新增 Cargo 依赖

---

## Verification Strategy

- **Automated tests**: Tests-after（同 PR）
- **Bench gate**: `cargo bench -p gw-codec --bench passthrough` P99 ≤ 5% drift
- **Key assertion**: 无 override 配置时同协议路径字节级不变

---

## Execution Strategy

```
Wave 1 [serial]:
├── 1. IR struct + mutation flag 扩展
├── 2. Codec render merge 逻辑
└── 3. Passthrough 路径修正 + 测试
```

---

## TODOs

- [x] 1. IR struct 扩展 + mutation flag

  **What to do**:
  - 在 `crates/gw-core/src/ir.rs` 的 `RequestIR` struct 新增 `passthrough_overrides: Option<HashMap<String, serde_json::value::RawValue>>`
  - 在 `Mutations` struct/enum 新增 `has_overrides` flag（或新增 variant）
  - 确保 `mutations.is_empty()` 在 `passthrough_overrides.is_some()` 时返回 false
  - 在 `apply_forced_params_to_ir`（`upstream.rs:478`）同级新增 `apply_overrides_to_mutations` 函数
  - None 时短路（零 alloc）

  **Must NOT do**:
  - 不改 IR 现有 typed 字段（model, messages, temperature, top_p 等）
  - 不引入 `HashMap` 以外的复杂容器

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Serial Wave 1
  - **Blocks**: Task 2, 3
  - **Blocked By**: None

  **References**:
  - `crates/gw-core/src/ir.rs:14` — RequestIR struct 定义
  - `crates/gw-proxy/src/filters/upstream.rs:478` — `apply_forced_params_to_ir`
  - `crates/gw-proxy/src/filters/upstream.rs:565` — `mutations.is_empty()` 分支点
  - `crates/gw-core/src/entity.rs:266` — ForcedParams struct（参考现有 mutation 注册模式）

  **Acceptance Criteria**:
  - [ ] `cargo test -p gw-codec` 含 IR override 设置/读取测试
  - [ ] `mutations.is_empty()` 在有 override 时返回 false 的测试
  - [ ] None override 时无额外 alloc 的编译时验证（Option<HashMap> 不分配）

  **QA Scenarios**:

  ```
  Scenario: override 触发 mutation flag
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-codec -- test_override_sets_mutation_flag
      2. 断言 PASS
    Expected Result: override 存在时 mutations.is_empty() == false
    Evidence: .omo/evidence/pa-1-mutation-flag.txt

  Scenario: 无 override 时零 alloc
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-codec -- test_no_override_zero_alloc
      2. 断言 passthrough_overrides == None
      3. 断言 mutations.is_empty() == true（无其他 mutation 时）
    Expected Result: 默认路径不变
    Evidence: .omo/evidence/pa-1-no-override.txt
  ```

  **Commit**: YES (groups with 2, 3)
  - Message: `refactor(codec): add IR passthrough_overrides + mutation flag [P-A]`

- [x] 2. Codec render merge 逻辑

  **What to do**:
  - 在 OpenAI/Anthropic/Gemini 各 codec 的 `render_request` 函数中，序列化 JSON 后合并 `passthrough_overrides`
  - 合并策略：override 的 key 覆盖 IR render 的同名字段（flat-only, top-level）
  - 使用 `serde_json::Map` merge 而非重新 parse 完整 body
  - 仅在 `passthrough_overrides.is_some()` 时执行（短路）

  **Must NOT do**:
  - 不支持嵌套 JSON path（flat-only 是永久 scope，非 v1 限制）
  - 不改 response render 路径
  - 不影响同协议无 override 路径

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Serial (after Task 1)
  - **Blocks**: Task 3
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/openai/codec.rs:34-45` — OpenAI render_request（serde_json::to_vec）
  - `crates/gw-codec/src/anthropic/codec.rs` — Anthropic render
  - `crates/gw-codec/src/gemini/codec.rs` — Gemini render

  **Acceptance Criteria**:
  - [ ] 3 协议的 render_request 都支持 override merge
  - [ ] 单元测试：override {"temperature": 0.5} 覆盖 IR 默认温度
  - [ ] 单元测试：无 override 时输出与当前 byte-identical

  **QA Scenarios**:

  ```
  Scenario: OpenAI override merge
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-codec -- test_openai_render_with_override
      2. 构造 IR + override {"custom_field": "value"}
      3. 断言 render 输出 JSON 含 "custom_field": "value"
    Expected Result: override 合并到 render 输出
    Evidence: .omo/evidence/pa-2-openai-merge.txt

  Scenario: 无 override 字节不变
    Tool: Bash (cargo test)
    Steps:
      1. cargo test -p gw-codec -- test_render_no_override_unchanged
      2. 比较 有/无 override 的 render 输出
      3. 断言无 override 时 byte-identical
    Expected Result: 默认路径零影响
    Evidence: .omo/evidence/pa-2-no-override-unchanged.txt
  ```

  **Commit**: YES (groups with 1, 3)

- [x] 3. Passthrough 路径修正 + bench

  **What to do**:
  - 修正 `upstream.rs:568` 的 `bytes::Bytes::from(cow.into_owned())` → 使用 `Bytes::copy_from_slice` 或 Pingora 持有的请求 body `Bytes` 直接 `clone()`（refcount-only）
  - 确认 `mutations.is_empty()` 分支在有 override 时正确跳转到 parse→render 路径
  - 集成测试：同协议 + override → 上游收到正确 body
  - Bench: `cargo bench -p gw-codec --bench passthrough` 与修正前基线比对

  **Must NOT do**:
  - 不改 response passthrough 路径
  - 不引入新的 body 缓冲策略

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Serial (after Task 2)
  - **Blocks**: T1 (model-rewrite), T9 (body-rules)
  - **Blocked By**: Task 2

  **References**:
  - `crates/gw-proxy/src/filters/upstream.rs:565-601` — passthrough 分支 + render 分支
  - `crates/gw-proxy/src/filters/upstream.rs:568` — `cow.into_owned()` 假零拷贝

  **Acceptance Criteria**:
  - [ ] `cow.into_owned()` 被替换为 refcount-only clone
  - [ ] 集成测试：同协议 + model override → 上游 body 含新 model
  - [ ] bench P99 ≤ 基线（修正假零拷贝后应略改善）

  **QA Scenarios**:

  ```
  Scenario: 同协议 + override 生效
    Tool: Bash (curl)
    Preconditions: 配置 ModelMapping default_upstream_model（通过 T1 实现，此处预验证框架）
    Steps:
      1. 设置 passthrough_overrides = {"model": "test-override"}
      2. curl 发送 OpenAI→OpenAI 请求
      3. 检查 fake upstream 收到的 body
      4. 断言 body 中 model == "test-override"
    Expected Result: 同协议 passthrough 路径也应用了 override
    Evidence: .omo/evidence/pa-3-passthrough-override.txt

  Scenario: bench 无退化
    Tool: Bash
    Steps:
      1. cargo bench -p gw-codec --bench passthrough -- --save-baseline after
      2. 比较 before vs after
      3. 断言 P99 差异 ≤ 5%
    Expected Result: 性能持平或改善（修正假零拷贝）
    Evidence: .omo/evidence/pa-3-bench.txt
  ```

  **Commit**: YES (groups with 1, 2)
  - Message: `refactor(codec): fix false zero-copy + enable override passthrough [P-A]`
  - Pre-commit: `cargo test --workspace && cargo bench -p gw-codec --bench passthrough && bash scripts/check-invariants.sh`

---

## Final Verification Wave

> 随 overview plan 统一执行。

---

## Success Criteria

```bash
cargo test --workspace                                        # 全绿
cargo bench -p gw-codec --bench passthrough                   # P99 ≤ 5% drift
bash scripts/check-invariants.sh                              # exit 0
```
