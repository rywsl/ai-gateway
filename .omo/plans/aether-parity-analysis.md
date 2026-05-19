# Aether → ailinkmax-gateway Feature Parity Gap Analysis

## TL;DR

> **Quick Summary**: Produce a single comprehensive markdown gap-analysis report comparing Aether (canonical product at `/Users/whereslow/Desktop/work/Aether`) against the Rust LLM gateway `ailinkmax-gateway`, stratifying every gap as `rust-proxy` / `go-web` / `worker` / `shared` with explicit confidence, so the engineering team can drive the parity roadmap.
>
> **Deliverables**:
> - `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md` — single self-contained report containing: executive summary, rubric, stratification rollup, per-capability narratives, full flat catalog, architecture decisions needed, deliberately not catalogued.
>
> **Estimated Effort**: Medium (research + writing; ~8h of parallel agent work + synthesis)
> **Parallel Execution**: YES — 4 waves
> **Critical Path**: T1 (rubric) → T2-T4 (parallel: gateway map, Aether services map, Aether api+modules map) → T5-T11 (parallel: 7 capability deep-dives + schema diff + worker-spec audit) → T12 (semantic-diff pass on 4 high-overlap areas) → T13 (synthesis writes the report) → T14 (completeness audit) → F1-F4 (final review wave) → user approval

---

## Context

### Original Request
用户指令：「分析当切项目（Aether）和网关部分的功能缺口」。经澄清后明确：

- 目标：让 Rust 网关 `ailinkmax-gateway` 与 Aether 达到功能对齐，输出一份用于驱动 roadmap 的缺口分析报告。
- 范围：Aether 全量（src/、aether-proxy/、aether-hub/、frontend/、alembic/ 等），但需把每个缺口分流到 `rust-proxy` / `go-web` / `worker` / `shared`。
- 受众：网关团队，用来确定 v1.1+ 的功能补齐路线。
- 方法：docs-first，源码验证。

### Interview Summary

**Key Discussions**:
- Aether 是 canonical，每个 Aether 能力都是候选缺口，除非已被 Rust 网关覆盖。
- 输出同时包含「能力领域叙事」和「扁平结构化清单」两种形式。
- 每行缺口必须打分层标签 + 分层置信度（`Definite` / `Likely` / `Open question`）。
- 不写任何代码、不设计补齐方案 —— 只列缺口和参考位置。

**Research Findings (preliminary)**:
- Aether 是完整的 LLM 网关产品（Python FastAPI 主体 + Rust 转发代理 + 控制面 + 前端 UI），文档头部自述「一站式 AI 基础设施平台」。
- `ailinkmax-gateway` 当前是 Pingora 0.8 数据平面，9 个 crate，已知 v1 gap 列表见 `CLAUDE.md`。
- 关键设计文档已存在：`docs/superpowers/specs/2026-05-18-llm-gateway-design.md`、`2026-05-19-adapter-layer-design.md`、`2026-05-19-task-worker-design.md`。

### Metis Review

**Identified Gaps (incorporated)**:
- 分层是最大风险 → 强制加 `Stratification Confidence` 列 + 「Architecture Decisions Needed」专节。
- 状态分类容易塌成「有/无」二元 → 强制使用 `Missing / Partial / Divergent / Present / N/A / Deprecated` 六态。
- 「双方都有但语义不同」是最高价值的发现 → 必须为 4 个高重叠领域（auth / rate-limit / billing / routing）做专门的语义 diff。
- Aether 配置驱动 / schema 隐含的能力容易遗漏 → 必须扫 `alembic/versions/`。
- 优先级靠直觉容易拉低可信度 → 锁定迁移阻塞标准（P0 阻塞迁移 / P1 有 workaround / P2 nice-to-have）。
- 8 个 agent 并行容易产出 8 套不同 schema → 第一步必须先定义「Rubric」并所有任务复用。

---

## Work Objectives

### Core Objective
产出一份单文件、自包含的功能缺口分析报告，作为 `ailinkmax-gateway` 与 Aether 对齐路线图的输入。

### Concrete Deliverables
- 单一文件：`docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md`
- 文件内必须包含的小节：
  - `## Executive Summary`（≤300 字，含分层 rollup 数据）
  - `## Rubric`（列定义、状态分类、分层置信度、优先级、工作量、引用约定）
  - `## Stratification Rollup`（按状态 × 分层的二维计数表，加总必须等于 catalog 行数）
  - `## Per-Capability Narratives`（每个能力域一节，含上下文段落 + 该域的局部清单）
  - `## Full Gap Catalog`（全平表 — Feature / Aether 引用 / Rust 状态 / Rust 引用 / 分层 / 分层置信度 / 优先级 / 工作量 / Notes）
  - `## Architecture Decisions Needed`（所有 `Open question` 行的去重列表 + 一行决策说明）
  - `## Deliberately Not Catalogued`（明确跳过的 Aether 顶层目录/文件）

### Definition of Done
- [ ] 文件存在且大于 8KB（防止空壳）
- [ ] 7 个必备 `## ` 顶级小节全部出现（grep 验证）
- [ ] 每个 `Aether/src/services/` 子目录名至少在报告中出现 1 次
- [ ] 每个 `Aether/src/api/` 子目录名至少在报告中出现 1 次
- [ ] 每个 `Aether/src/modules/` 子目录名至少在报告中出现 1 次
- [ ] `aether-proxy` 与 `aether-hub` 各出现 ≥ 1 次
- [ ] `Aether/alembic/versions/` 下的每个 migration 文件被引用或被「已覆盖」备注覆盖
- [ ] Catalog 表的行数 == Stratification Rollup 表 4 个分层格子之和
- [ ] Catalog 表无空格 / `TODO` / `???`（grep 反向验证）
- [ ] 每个 `Stratification Confidence == Open question` 的 Feature 都出现在 `## Architecture Decisions Needed` 中
- [ ] F1-F4 final review wave 全部 APPROVE
- [ ] 用户显式确认 OK

### Must Have
- 全量覆盖 Aether 的 `src/services/`、`src/api/`、`src/modules/`、`aether-proxy/`、`aether-hub/`、`alembic/versions/`。
- 4 个高重叠领域（auth / rate-limit / billing / routing）必须有专门的语义 diff 段落，列出双方子能力的对照表。
- 每行 catalog 必须填满 9 个字段，0 空白。
- `Open question` 行必须在 `Architecture Decisions Needed` 节中再次出现。
- Stratification Rollup 与 catalog 行数一致。
- 引用 Aether 源码时使用「相对仓库根的路径 + 行号或符号名」格式（例：`Aether/src/services/auth/jwt.py:142` 或 `Aether/src/services/billing/ledger.py:LedgerService`）。

### Must NOT Have (Guardrails)
- ❌ 不提任何「补齐方案 / 怎么实现 / 应该用哪个库」—— Notes 列只能写「参考 Aether 中 X 的位置」之类的事实，不能写设计建议。
- ❌ 不擅自决定 rust-proxy ↔ go-web 边界。无法判定时一律打 `Open question` 并进入「Architecture Decisions Needed」。
- ❌ 不跳过「双方都有但行为不同」的项 —— 这是最高价值的一类发现。
- ❌ 不做 React 组件级别的前端枚举。`frontend/` 在 `Deliberately Not Catalogued` 中提一次即可。
- ❌ 不允许 catalog 中出现 `TODO` / `???` / 空白。无法判定的字段填 `?` 加 Notes 解释为什么。
- ❌ 不允许靠直觉乱填优先级。每个 P0 必须能说出它阻塞的具体迁移场景。
- ❌ 不允许 agent 自创列名 / 自创状态值 / 自创分层值 —— 严格使用 Rubric 中定义的取值集。
- ❌ 不允许任务并行时切到同一个 Aether 子目录（避免重复工作）。

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — 所有验证都由 agent 执行。

### Test Decision
- **Infrastructure exists**: 不适用（纯写作交付物）
- **Automated tests**: NONE（无代码改动）
- **Framework**: 不适用
- **Verification**: 全部通过 grep / 文件存在性 / 行数计数等机械检查 + 一个 `oracle` 二审

### QA Policy
每个任务必须产出可验证的中间文物（intermediate artifact）：
- 探查类任务：在 `.omo/evidence/parity/{task-N}.md` 写一份发现纪要。
- 综合类任务：最终交付文件 + 一份 `.omo/evidence/parity/synthesis-checklist.txt` 检查清单的运行输出。
- 完整性审计任务：执行 grep / ls / wc 命令，把命令 + 输出贴在证据文件里。

工具选择：
- **文件读取**：`read` / `grep` / `glob`
- **目录扫描**：`bash` 跑 `ls`
- **结构化代码模式**：`ast_grep_search`（如需 import / class 模式比对）
- **二审**：`oracle` 子代理（read-only，专门用于发现遗漏）

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (启动 — 基础与 rubric):
└── T1: 定义并发布 Rubric + 引用约定 [unspecified-high]

Wave 2 (映射 — 全面并行扫描):
├── T2: Rust 网关现状全景图（crates + 已知 v1 gap） [explore]
├── T3: Aether src/services 全量扫描 [explore]
├── T4: Aether src/api + src/modules 全量扫描 [explore]
├── T5: Aether aether-proxy + aether-hub 扫描 [explore]
├── T6: Aether alembic schema diff 对比 Gateway migrations [explore]
└── T7: 读 docs/superpowers/specs/2026-05-19-task-worker-design.md，输出 worker 已宣告的能力清单 [unspecified-low]

Wave 3 (深度对比 — 高价值领域):
├── T8: 语义 diff — auth（5-tier 权限合并 / JWT / API key / OAuth / LDAP / 管理 token） [deep]
├── T9: 语义 diff — rate-limit & quota（RPM/RPD/TPM/TPD/hard caps/freeze） [deep]
├── T10: 语义 diff — billing & usage（ledger / pricing / markup / wallet / subscription） [deep]
└── T11: 语义 diff — routing & candidate（8 strategies / health / circuit / sticky / provider_ops） [deep]

Wave 4 (合成 + 完整性):
└── T12: 综合写作 — 拼装最终报告到 docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md [unspecified-high]
└── T13: 完整性审计 — 跑 grep/ls 检查清单，验证所有 acceptance criteria [unspecified-high]

Wave FINAL (4 个评审并行 → 用户 OK):
├── F1: 计划合规审计（oracle） — Must Have / Must NOT Have 逐条核查
├── F2: 报告内容质量审计（unspecified-high） — 表格完整、无空白、引用可达
├── F3: 实测 QA（unspecified-high） — 跑机械检查清单，确认每项 PASS
└── F4: 范围保真度审计（deep） — Aether 全目录是否都被覆盖或显式跳过
→ 呈现合并结果 → 等待用户显式 okay

Critical Path: T1 → T3 → T8 → T12 → T13 → F1-F4 → 用户 OK
Parallel Speedup: ~60% 相比串行
Max Concurrent: 6（Wave 2 & Wave 3 各最多 6）
```

### Dependency Matrix

- **T1**: blocked-by=none; blocks=T2..T13
- **T2**: blocked-by=T1; blocks=T8,T9,T10,T11,T12
- **T3**: blocked-by=T1; blocks=T8,T9,T10,T11,T12
- **T4**: blocked-by=T1; blocks=T8,T9,T10,T11,T12
- **T5**: blocked-by=T1; blocks=T12
- **T6**: blocked-by=T1; blocks=T10,T12
- **T7**: blocked-by=T1; blocks=T12
- **T8-T11**: blocked-by=T1,T2,T3,T4 (+T6 for T10); blocks=T12
- **T12**: blocked-by=T1..T11; blocks=T13
- **T13**: blocked-by=T12; blocks=F1..F4
- **F1-F4**: blocked-by=T13; blocks=用户 OK

### Agent Dispatch Summary

- **Wave 1**: 1 任务 — T1 → `unspecified-high`
- **Wave 2**: 6 任务 — T2-T6 → `explore`, T7 → `unspecified-low`
- **Wave 3**: 4 任务 — T8-T11 → `deep`
- **Wave 4**: 2 任务 — T12 → `unspecified-high`, T13 → `unspecified-high`
- **Wave FINAL**: 4 任务 — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [ ] 1. **定义 Rubric（列定义、状态分类、引用约定）**

  **What to do**:
  - 在 `.omo/evidence/parity/00-rubric.md` 写下后续所有任务必须严格复用的 schema。
  - Rubric 必须包含：
    1. Catalog 表的 9 列定义（Feature / Aether Reference / Rust Status / Rust Reference / Stratum / Stratification Confidence / Priority / Effort / Notes）。
    2. Rust Status 取值集：`Missing` / `Partial` / `Divergent` / `Present` / `N/A` / `Deprecated in Aether`。每个值附 1 行判定说明。
    3. Stratum 取值集：`rust-proxy` / `go-web` / `worker` / `shared`。附启发式规则（hot-path → rust-proxy；CRUD/admin/async/UI → go-web；周期/队列 → worker；schema 与错误码 → shared）。
    4. Stratification Confidence：`Definite` / `Likely` / `Open question`。`Open question` 行必须列入「Architecture Decisions Needed」。
    5. Priority 取值集（migration-blocker frame）：`P0` 阻塞客户从 Aether 迁到 Rust 网关 / `P1` 影响对齐但有 workaround / `P2` nice-to-have / admin-only / observability。每个值给 1 个示例。
    6. Effort 取值集：`S` / `M` / `L` / `XL`，给出方向性示例（S = 单 PR 单 crate 改动；M = 跨 2-3 crate；L = 涉及 schema + 多 crate；XL = 新建 crate 或重大架构）。
    7. 引用约定：Aether 引用统一写 `Aether/<相对路径>:<行号或符号>`；Rust 引用统一写 `<crate>/<相对路径>:<行号或符号>`。
    8. Forbidden 模式清单（agent 看到必须避免）：实现建议、设计草案、库选型、未来路线图、空白/TODO/???。

  **Must NOT do**:
  - 不要先开始做缺口分析。这一步只产出 Rubric。
  - 不要把 Rust 已知 v1 gap 当作 Rubric 的一部分写进来 —— 那是后续任务的输入。

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 需要规则/文档思维，不需要并行探查或深度推理。
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO（基础工作，必须先完成）
  - **Parallel Group**: Wave 1
  - **Blocks**: T2-T13（所有后续任务都引用 Rubric）
  - **Blocked By**: 无

  **References**:
  - 计划文件「Must NOT Have (Guardrails)」段落 — 这是 Forbidden 模式的来源。
  - 计划文件「Metis Review」段落 — Rubric 必须吸收 Metis 提出的所有约束。
  - `docs/superpowers/specs/2026-05-19-task-worker-design.md` — 用于明确 `worker` 分层的判定边界（这一步可以预读，但完整解析交给 T7）。

  **WHY Each Reference Matters**:
  - Guardrails 段 = Rubric 中 Forbidden 模式清单的唯一来源；
  - Metis Review 段 = Rubric 必须落实的所有强制约束；
  - worker spec = `worker` 分层不能凭空捏造，必须有依据。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/00-rubric.md` 存在且 > 2KB。
  - [ ] grep 验证 9 列定义齐全。
  - [ ] grep 验证 6 个 Rust Status 取值都被定义且各有判定说明。
  - [ ] grep 验证 4 个 Stratum 取值都被定义且附启发式规则。
  - [ ] grep 验证 3 个 Stratification Confidence 取值都被定义。
  - [ ] grep 验证 3 个 Priority 取值都被定义且各有示例。
  - [ ] grep 验证 4 个 Effort 取值都被定义且有方向性示例。
  - [ ] grep 验证 Forbidden 模式至少列 5 条。

  **QA Scenarios**:

  ```
  Scenario: Rubric 文件结构完整
    Tool: Bash (grep + wc)
    Preconditions: T1 完成
    Steps:
      1. `wc -c .omo/evidence/parity/00-rubric.md` 应 > 2048
      2. `grep -c "^### " .omo/evidence/parity/00-rubric.md` 应 ≥ 8（对应 8 个小节）
      3. 对 Rust Status 6 个取值逐一 grep，每个 ≥ 1 次
      4. 对 Stratum 4 个取值逐一 grep，每个 ≥ 1 次
    Expected Result: 所有 grep 命中数 ≥ 预期
    Failure Indicators: 任一缺失 → REJECT，回到 T1
    Evidence: .omo/evidence/parity/task-1-rubric-check.txt

  Scenario: Rubric 不含 Forbidden 模式（自指检查）
    Tool: Bash (grep -i)
    Preconditions: T1 完成
    Steps:
      1. `grep -iE "应该用|recommend|suggest|TODO|\?\?\?" .omo/evidence/parity/00-rubric.md`
    Expected Result: 命中数为 0
    Evidence: .omo/evidence/parity/task-1-forbidden-check.txt
  ```

  **Commit**: NO（合并到 T12 一起 commit）

- [ ] 2. **Rust 网关现状全景图（crates + 已知 v1 gap）**

  **What to do**:
  - 在 `.omo/evidence/parity/02-gateway-snapshot.md` 输出 Rust 网关当前能力的「正面清单」+「已知 v1 gap」。
  - 正面清单按 crate 维度组织：每个 crate 列出 (1) 暴露的核心类型/函数 (2) 它实现了 Aether 哪个能力域 (3) 当前 v1 限制。
  - 已知 v1 gap：从 `CLAUDE.md` 的「v1 gaps to be aware of when extending」段落 + `README.md` 的「Known gaps」段落 + `docs/superpowers/plans/2026-05-19-v1.1-residual-issues.md` 完整抄录，去重合并。
  - 不要在这里做 Aether 对比 —— 只产出 Rust 侧的事实清单。

  **Must NOT do**:
  - 不要凭印象写「Rust 网关有 X 功能」—— 每个声明必须有 crate/文件引用。
  - 不要把「打算做但还没做」算作已实现。

  **Recommended Agent Profile**:
  - **Category**: 不使用 category，直接调用 `explore` subagent_type（这是结构化探查任务）。
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES（与 T3-T7 并行）
  - **Parallel Group**: Wave 2
  - **Blocks**: T8-T12（语义 diff 和综合都依赖此清单）
  - **Blocked By**: T1（必须引用 Rubric 的 Rust Status 取值集）

  **References**:
  - `CLAUDE.md`（仓库根） — 包含权威的 v1 gap 列表。
  - `README.md`（仓库根）— 包含面向用户的能力声明。
  - `crates/gw-core/src/lib.rs`、`crates/gw-codec/src/lib.rs`、`crates/gw-store/src/lib.rs`、`crates/gw-auth/src/lib.rs`、`crates/gw-routing/src/lib.rs`、`crates/gw-ledger/src/lib.rs`、`crates/gw-proxy/src/lib.rs`、`crates/gw-gateway/src/lib.rs` — 9 个 crate 的入口，用于映射「这个 crate 实现了什么」。
  - `docs/superpowers/specs/2026-05-18-llm-gateway-design.md` — v1 设计权威。
  - `docs/superpowers/plans/2026-05-19-v1.1-residual-issues.md` — v1.1 残留问题清单。

  **WHY Each Reference Matters**:
  - 9 个 crate lib.rs 给出 public 表面 → 防止凭空捏造能力；
  - CLAUDE.md「v1 gaps」段 + v1.1 残留问题 plan 一起去重 → 形成最权威的「已知 gap」基线，避免与 Aether 比对时把已知问题二次发现成「新 gap」。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/02-gateway-snapshot.md` 存在且 > 4KB。
  - [ ] 9 个 crate 每个都有独立小节。
  - [ ] 每个 crate 小节至少给出 3 条「Implements」陈述，每条带文件路径 + 行号或符号名。
  - [ ] 「Known v1 gaps」段单独成节，至少包含 CLAUDE.md 列出的 12 条。

  **QA Scenarios**:

  ```
  Scenario: 9 个 crate 全部覆盖
    Tool: Bash (grep)
    Steps:
      1. 对 gw-core/gw-codec/gw-codec-macros/gw-store/gw-auth/gw-routing/gw-ledger/gw-proxy/gw-gateway 逐一 grep
    Expected Result: 9/9 命中
    Evidence: .omo/evidence/parity/task-2-crate-coverage.txt

  Scenario: 已知 gap 至少 12 条
    Tool: Bash (grep -c)
    Steps:
      1. 数 Known v1 gaps 节中的列表项行数（grep -c "^- " 该节）
    Expected Result: ≥ 12
    Evidence: .omo/evidence/parity/task-2-known-gaps-count.txt
  ```

  **Commit**: NO

- [ ] 3. **Aether `src/services/` 全量扫描**

  **What to do**:
  - 在 `.omo/evidence/parity/03-aether-services.md` 输出 Aether `src/services/` 下所有子目录的能力提要。
  - 对每个子目录（analytics / auth / billing / cache / candidate / capability / email / health / license / management_token / message / model / orchestration / payment / provider / provider_keys / provider_ops / proxy_node / rate_limit / request / scheduling / subscription / system / task / usage / user / wallet 以及 `gemini_files_mapping.py`）：
    1. 列出该 service 暴露的主要类 / 函数 / 配置。
    2. 一句话总结业务能力（中文）。
    3. 标注它是否在请求 hot path 上（用于 T8-T11 + T12 的分层启发）。
    4. 引用最关键的 1-2 个源码位置（文件:行号 或 文件:符号）。
  - 不要做 Rust 侧对比 —— 这一步只产出 Aether 事实清单。

  **Must NOT do**:
  - 不要写实现建议、不要写「Rust 应该…」。
  - 不要遗漏任何 `src/services/` 子项。如果某个子目录是空壳，也要标注。

  **Recommended Agent Profile**:
  - **subagent_type**: `explore`（结构化代码探查 + 多文件归纳）
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES（与 T2/T4-T7 并行）
  - **Parallel Group**: Wave 2
  - **Blocks**: T8-T12
  - **Blocked By**: T1

  **References**:
  - `/Users/whereslow/Desktop/work/Aether/src/services/` — 主要扫描目标。
  - `/Users/whereslow/Desktop/work/Aether/src/services/__init__.py` — 入口，可能注册 service 工厂。
  - `/Users/whereslow/Desktop/work/Aether/src/core/`、`/Users/whereslow/Desktop/work/Aether/src/database/` — 用于理解 service 依赖的基础设施。
  - `.omo/evidence/parity/00-rubric.md` — 必须遵守的引用格式。

  **WHY Each Reference Matters**:
  - services/__init__.py 揭示哪些 service 真正被注册，避免把废弃模块当成现役能力；
  - core/ 与 database/ 可能含权威的基类（BaseService / Repository），决定每个 service 的实际职责面。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/03-aether-services.md` 存在且 > 8KB。
  - [ ] `ls /Users/whereslow/Desktop/work/Aether/src/services/` 每个 entry 在文件中都有独立段落（grep 验证）。
  - [ ] 每个 service 段落含至少 1 个源码引用，格式 `Aether/src/services/<x>/<file>.py:<行号或符号>`。
  - [ ] 每个 service 段落含「hot-path: yes/no/conditional」标签。

  **QA Scenarios**:

  ```
  Scenario: 每个 service 子目录至少出现一次
    Tool: Bash (循环 grep)
    Steps:
      1. for d in /Users/whereslow/Desktop/work/Aether/src/services/*/; do
           name=$(basename "$d")
           grep -qi "$name" .omo/evidence/parity/03-aether-services.md || echo "MISSING: $name"
         done
    Expected Result: 输出为空（无 MISSING）
    Evidence: .omo/evidence/parity/task-3-coverage.txt

  Scenario: 每段都有 hot-path 标签
    Tool: Bash (grep -c)
    Steps:
      1. grep -c "hot-path:" .omo/evidence/parity/03-aether-services.md
    Expected Result: ≥ Aether services 子目录数
    Evidence: .omo/evidence/parity/task-3-hotpath.txt
  ```

  **Commit**: NO

- [ ] 4. **Aether `src/api/` + `src/modules/` 全量扫描**

  **What to do**:
  - 在 `.omo/evidence/parity/04-aether-api-modules.md` 输出 Aether 控制面 / 用户面 / 管理面 API 与 modules 的能力提要。
  - `src/api/` 子目录（admin / analytics / announcements / auth / base / handlers / internal / monitoring / oauth / payment / public / serializers / subscription / user_me / wallet + 顶层 `license.py`）：列出路由、参数、关键依赖的 service。
  - `src/modules/` 子目录（gemini_files / ldap / management_tokens / notification_email / oauth / proxy_nodes）：列出模块职责、入口函数、外部依赖（如 LDAP 服务器、邮件 SMTP）。
  - 每个条目都要标注「面向 — admin / user / internal / public」。

  **Must NOT do**:
  - 不要枚举 React 组件、不要分析前端。
  - 不要分析 Aether 的 Python 依赖列表（pyproject.toml）—— 那是部署细节。

  **Recommended Agent Profile**:
  - **subagent_type**: `explore`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T8-T12
  - **Blocked By**: T1

  **References**:
  - `/Users/whereslow/Desktop/work/Aether/src/api/`
  - `/Users/whereslow/Desktop/work/Aether/src/modules/`
  - `/Users/whereslow/Desktop/work/Aether/src/main.py` — FastAPI app 入口，可能挂载所有 router。

  **WHY Each Reference Matters**:
  - main.py 揭示哪些 router 真正挂载、按什么 prefix → 区分「定义了但未启用」的代码；
  - 子目录命名规律本身就分类（admin / user_me / public / internal / oauth / payment），无须再人为分组。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/04-aether-api-modules.md` 存在且 > 8KB。
  - [ ] `ls Aether/src/api/` 与 `ls Aether/src/modules/` 的每个 entry 在文件中都有独立段落（grep 验证）。
  - [ ] 每个段落都含「面向: admin/user/internal/public」标签。

  **QA Scenarios**:

  ```
  Scenario: api 与 modules 全覆盖
    Tool: Bash (循环 grep)
    Steps:
      1. 对 src/api/* 与 src/modules/* 逐一 grep
    Expected Result: 0 MISSING
    Evidence: .omo/evidence/parity/task-4-coverage.txt
  ```

  **Commit**: NO

- [ ] 5. **Aether `aether-proxy/` + `aether-hub/` 扫描**

  **What to do**:
  - 在 `.omo/evidence/parity/05-aether-rust-components.md` 输出这两个 Rust 子工程的能力提要。
  - 对每个工程：(1) 它在 Aether 架构中的定位（forward proxy 节点 / 控制面） (2) 主要模块、入口、配置 (3) 与 `ailinkmax-gateway` 的关系：是被替代、共存、还是无关。
  - 注意：`aether-proxy` 是部署在海外 VPS 的转发节点，`aether-hub` 是控制面 —— 这两者很可能与 Rust 网关的角色不重叠，但仍需明确写出来。

  **Must NOT do**:
  - 不要把它们误判为 `ailinkmax-gateway` 的对标 —— 它们是 Aether 内的独立 Rust 组件，与 Python 网关协作。

  **Recommended Agent Profile**:
  - **subagent_type**: `explore`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T12
  - **Blocked By**: T1

  **References**:
  - `/Users/whereslow/Desktop/work/Aether/aether-proxy/src/{app.rs,config.rs,net.rs,runtime.rs,state.rs,target_filter.rs,registration,tunnel,setup}` — 关键模块。
  - `/Users/whereslow/Desktop/work/Aether/aether-proxy/README.md` — 自述能力。
  - `/Users/whereslow/Desktop/work/Aether/aether-hub/src/{control_plane.rs,hub.rs,local_relay.rs,protocol.rs,proxy_conn.rs}` — 控制面模块。
  - `/Users/whereslow/Desktop/work/Aether/aether-hub/README.md`

  **WHY Each Reference Matters**:
  - target_filter.rs / safe_dns.rs / tunnel/ 决定 forward proxy 的行为 → 帮助判断它和 ailinkmax-gateway 的能力边界；
  - hub 的 protocol.rs 揭示控制面消息格式 → 这可能就是未来 go-web 的接口对标。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/05-aether-rust-components.md` 存在且 > 3KB。
  - [ ] 两个子工程各有独立小节。
  - [ ] 每个小节明确写出与 `ailinkmax-gateway` 的关系（替代 / 共存 / 无关）。

  **QA Scenarios**:

  ```
  Scenario: 两个子工程关系结论必现
    Tool: Bash (grep -iE)
    Steps:
      1. grep -iE "replace|coexist|unrelated|替代|共存|无关" .omo/evidence/parity/05-aether-rust-components.md
    Expected Result: ≥ 2（两个工程各有结论）
    Evidence: .omo/evidence/parity/task-5-relationship.txt
  ```

  **Commit**: NO

- [ ] 6. **Schema 对比 — Aether `alembic/versions/` vs Gateway `crates/gw-store/migrations/`**

  **What to do**:
  - 在 `.omo/evidence/parity/06-schema-diff.md` 输出两边数据库 schema 的对照表。
  - Aether 侧：扫描 `Aether/alembic/versions/` 所有 migration 文件，提取每个 migration 创建/修改的表与列，按表名归并出最终 schema。
  - Rust 网关侧：读 `crates/gw-store/migrations/0001_initial.sql` 拿到 v1 schema。
  - 输出一张表：`Aether 表名 | 关键列 | Aether 引入 migration | Rust 是否有对应表 | Rust 表/字段差异 | 备注`。
  - 这一步是 catalog 中「schema-implied」缺口的唯一权威输入。

  **Must NOT do**:
  - 不要尝试解释为什么 Aether 设计成这样 —— 只做事实对比。
  - 不要遗漏 alembic 的 `downgrade` 部分（用来识别可逆 vs 不可逆变更）。

  **Recommended Agent Profile**:
  - **subagent_type**: `explore`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T10（billing/usage 语义 diff 强依赖 schema）, T12
  - **Blocked By**: T1

  **References**:
  - `/Users/whereslow/Desktop/work/Aether/alembic/versions/` — 所有 migration。
  - `/Users/whereslow/Desktop/work/Aether/alembic.ini` — alembic 配置。
  - `/Users/whereslow/Desktop/work/ailinkmax-gateway/crates/gw-store/migrations/0001_initial.sql` — Rust 侧初始 schema。
  - `/Users/whereslow/Desktop/work/ailinkmax-gateway/crates/gw-store/src/` — 可能有额外的运行时 schema 假设。

  **WHY Each Reference Matters**:
  - migration 是「Aether 实际存了哪些业务概念」的最权威信号 —— 比 service 代码更稳定；
  - 0001_initial.sql 是 Rust v1 schema 的唯一权威 —— 任何 Aether 中存在但 Rust 中不存在的表/列即是 schema-implied 缺口。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/06-schema-diff.md` 存在且 > 4KB。
  - [ ] `ls Aether/alembic/versions/*.py` 每个文件的 revision id 都被引用至少一次。
  - [ ] 对比表至少含 1 行 / 每张 Aether 表。
  - [ ] 表头精确为 `Aether 表名 | 关键列 | Aether 引入 migration | Rust 是否有对应表 | Rust 表/字段差异 | 备注`（grep）。

  **QA Scenarios**:

  ```
  Scenario: 每个 migration 都被引用
    Tool: Bash
    Steps:
      1. for f in /Users/whereslow/Desktop/work/Aether/alembic/versions/*.py; do
           rev=$(grep -oE "revision: str = ['\"][a-f0-9]+['\"]" "$f" | head -1)
           # 把 rev 中的 hash 在对比文件里 grep
         done
    Expected Result: 0 MISSING
    Evidence: .omo/evidence/parity/task-6-migration-coverage.txt
  ```

  **Commit**: NO

- [ ] 7. **解析 `task-worker-design.md`，输出 worker 已宣告能力清单**

  **What to do**:
  - 在 `.omo/evidence/parity/07-worker-claims.md` 输出 worker spec 明确「未来由 worker 承担」的能力清单。
  - 完整阅读 `docs/superpowers/specs/2026-05-19-task-worker-design.md`，提取所有「worker will handle X」「X is delegated to the async worker」「the worker is responsible for X」类语句。
  - 每条 claim 写成：`{能力名}` — 出处 `task-worker-design.md:行号` — `worker 负责的具体动作`。
  - 这份清单是 T12 综合阶段自动给行打 `worker` 标签的唯一依据。任何不在此清单的能力，T12 都不能擅自打 `worker`。

  **Must NOT do**:
  - 不要把「可能由 worker 做」当作宣告 —— 必须是 spec 明确写下的承诺。
  - 不要把 worker spec 之外的其他文档（adapter-layer-design 等）当作 worker 宣告依据。

  **Recommended Agent Profile**:
  - **Category**: `unspecified-low`
    - Reason: 单文档抽取，工作量小，不需要并行或深度推理。
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T12
  - **Blocked By**: T1

  **References**:
  - `docs/superpowers/specs/2026-05-19-task-worker-design.md` — 唯一权威输入。
  - `.omo/evidence/parity/00-rubric.md` — `worker` 分层的判定边界来自此处。

  **WHY Each Reference Matters**:
  - 没有这份清单，T12 在分层时就只能凭感觉打 `worker`，与 Metis 提出的「分层必须有依据」直接冲突。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/07-worker-claims.md` 存在。
  - [ ] 每条 claim 含 `task-worker-design.md:` 引用。
  - [ ] grep 验证至少有 5 条 claim（否则 spec 可能没读全，需重做）。

  **QA Scenarios**:

  ```
  Scenario: 每条 claim 都有 spec 行号引用
    Tool: Bash (grep -c)
    Steps:
      1. grep -c "task-worker-design.md:" .omo/evidence/parity/07-worker-claims.md
    Expected Result: ≥ 5
    Evidence: .omo/evidence/parity/task-7-claim-count.txt
  ```

  **Commit**: NO

- [ ] 8. **语义 diff — Auth（5-tier 合并 / JWT / API key / OAuth / LDAP / 管理 token）**

  **What to do**:
  - 在 `.omo/evidence/parity/08-semantic-auth.md` 输出 Auth 域双方子能力的对照表。
  - 对照轴包括：API key 体系（前缀 / 格式 / 验证 / `expires_at` / `ip_allowlist`）、JWT（签发 / 校验 / iss/aud/nbf / 算法 / HMAC）、OAuth（provider 列表 / 流程）、LDAP（绑定 / group 映射）、management token、权限合并（5-tier merge 的层级、字段、覆盖规则）、登录会话、刷新 token。
  - 输出表头：`子能力 | Aether 实现 (引用) | Rust 实现 (引用或缺) | Status (Missing/Partial/Divergent/Present) | 备注`。
  - 必须覆盖 ≥ 15 条子能力。

  **Must NOT do**:
  - 不要写实现建议、不要写「Rust 可以用 jsonwebtoken crate」。
  - 不要把 `gw-auth` 的 ADR-023（5-tier 合并）当作和 Aether 的多租户权限完全等价 —— 要逐字段确认。

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 跨语言、跨范式的语义对比，需要深度推理识别「看起来一样实则不同」。
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES（与 T9-T11 并行）
  - **Parallel Group**: Wave 3
  - **Blocks**: T12
  - **Blocked By**: T1, T2, T3, T4

  **References**:
  - Aether 端：
    - `Aether/src/services/auth/` — JWT、密码、登录会话。
    - `Aether/src/services/management_token/` — 管理 token。
    - `Aether/src/modules/oauth/` 与 `Aether/src/api/oauth/` — OAuth provider 接入。
    - `Aether/src/modules/ldap/` — LDAP 绑定。
    - `Aether/src/api/auth/` — 登录、注册、刷新接口。
    - `Aether/src/services/user/` 中的权限字段。
  - Rust 端：
    - `crates/gw-auth/src/` — API key 验证 + 5-tier 合并（ADR-023）。
    - `docs/superpowers/specs/2026-05-18-llm-gateway-design.md` 中 ADR-023 段。
    - `CLAUDE.md` 中提到的 v1 限制：`expires_at`、`ip_allowlist`、JWT iss/aud/nbf、HMAC 缺失。
  - 中间产物：T2 (gateway snapshot) + T3 (services scan) + T4 (api/modules scan)。

  **WHY Each Reference Matters**:
  - 5-tier merge 是 Rust 网关认为自己「Auth 已强」的核心点 → 必须逐字段确认 Aether 的多租户权限合并模型是否与之等价或不同；
  - OAuth 和 LDAP 几乎肯定整体缺失于 Rust → 但要确认是「未实现」还是「设计上不归 Rust」。

  **Acceptance Criteria**:
  - [ ] 文件 `.omo/evidence/parity/08-semantic-auth.md` 存在且 > 6KB。
  - [ ] 对照表至少 15 行。
  - [ ] 每行 Status 都是 Rubric 中的合法取值。
  - [ ] 每行 Aether 引用都含路径 + 行号或符号。

  **QA Scenarios**:

  ```
  Scenario: Auth 子能力覆盖完整
    Tool: Bash (grep -ic)
    Steps:
      1. 对关键词 jwt/api.key/oauth/ldap/management.token/5-tier/expires_at/ip_allow 各 grep -ic
    Expected Result: 每个 ≥ 1
    Evidence: .omo/evidence/parity/task-8-coverage.txt
  ```

  **Commit**: NO

- [ ] 9. **语义 diff — Rate-limit & Quota（RPM/RPD/TPM/TPD / hard caps / freeze）**

  **What to do**:
  - 在 `.omo/evidence/parity/09-semantic-ratelimit.md` 输出限流与配额域的对照表。
  - 对照轴：RPM、RPD、TPM、TPD、并发上限、单请求超时、hard caps（ADR-024）、freeze-estimate（ADR-004）、`daily_quota_tokens`、token bucket 算法、滑动窗 vs 固定窗、全局 vs 每用户 vs 每 key、超限响应码与错误码。
  - 输出同样格式的对照表（≥ 12 行）。

  **Must NOT do**:
  - 不要混淆 RPM 与「governor」实现细节 —— RPM 是能力，governor 是 Rust 的实现技术。
  - 不要把 Rust 的 `GW_QUOTA_RPM_EXCEEDED` 与 `GW_UPSTREAM_OVERLOADED` 合并 —— CLAUDE.md 明确两者不同。

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T12
  - **Blocked By**: T1, T2, T3, T4

  **References**:
  - Aether 端：
    - `Aether/src/services/rate_limit/`
    - `Aether/src/services/usage/`、`Aether/src/services/billing/`（限额与计费强耦合，需横向看）
    - `Aether/src/services/subscription/`、`Aether/src/services/wallet/`（订阅级与钱包级 cap）
    - `Aether/src/services/capability/`（per-capability 限额）
  - Rust 端：
    - `crates/gw-proxy/src/filters/request.rs` — governor RPM + hard caps + freeze-estimate。
    - `docs/superpowers/specs/2026-05-18-llm-gateway-design.md` 中 ADR-024、ADR-004 段。
    - `CLAUDE.md` 中 `GW_QUOTA_RPM_EXCEEDED` vs `GW_UPSTREAM_OVERLOADED` 区分。

  **WHY Each Reference Matters**:
  - billing 与 rate-limit 在 Aether 是耦合的 → 不联看会漏掉「按余额限流」类能力；
  - subscription / wallet / capability 三个 service 是多级 cap 的源头 → Rust 只有 ADR-024 单层 hard cap，多级 cap 差异极有可能是高价值 P0 gap。

  **Acceptance Criteria**:
  - [ ] 文件存在且 > 5KB。
  - [ ] 对照表 ≥ 12 行。
  - [ ] 含 multi-level cap 的显式讨论（subscription / wallet / capability 三级是否被 Rust 表达）。

  **QA Scenarios**:

  ```
  Scenario: 多级 cap 与 freeze 都被对比
    Tool: Bash (grep -ic)
    Steps:
      1. grep -ic "multi-level\|subscription cap\|wallet cap\|freeze" 文件
    Expected Result: 每个 ≥ 1
    Evidence: .omo/evidence/parity/task-9-multilevel.txt
  ```

  **Commit**: NO

- [ ] 10. **语义 diff — Billing & Usage（ledger / pricing / markup / wallet / subscription）**

  **What to do**:
  - 在 `.omo/evidence/parity/10-semantic-billing.md` 输出账务与计费域的对照表。
  - 对照轴：单请求 billing record 字段、ledger 持久化方式、pricing 表结构、`model_markup`、wallet 充值 / 扣费 / 冻结 / 退款、subscription 周期 / 续费 / 降级、payment 接入、用量统计（按 user / org / model / endpoint）、报表导出、license。
  - 必须横向对照 T6 的 schema diff —— 任何 Aether 有表但 Rust 没的，直接进 catalog。

  **Must NOT do**:
  - 不要把 Rust 的 `BillingRecord` Redis stream 视作完整账务 —— 它只是 emit，没有 wallet/subscription 模型。
  - 不要给 payment / subscription 这种业务能力打 `rust-proxy` 分层 —— 大概率是 `go-web`。

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T12
  - **Blocked By**: T1, T2, T3, T4, T6

  **References**:
  - Aether 端：
    - `Aether/src/services/billing/`
    - `Aether/src/services/usage/`
    - `Aether/src/services/wallet/`
    - `Aether/src/services/subscription/`
    - `Aether/src/services/payment/`
    - `Aether/src/services/license.py`
    - `Aether/src/api/wallet/`、`src/api/payment/`、`src/api/subscription/`、`src/api/analytics/`
  - Rust 端：
    - `crates/gw-ledger/src/` — Redis stream emit。
    - `crates/gw-proxy/src/filters/logging.rs` — billing record 字段。
    - `CLAUDE.md` 中提到的 `Pricing.model_markup` 缺失、DLQ 缺失。
  - 中间产物：T6 schema diff 必须先有。

  **WHY Each Reference Matters**:
  - Aether 的 billing/wallet/subscription/payment 一定大量超出 Rust 当前能力 → 这是 catalog 中 go-web 分层最大的来源；
  - schema diff 已穷举了表层差异 → 语义 diff 只需聚焦「行为」差（如冻结、退款、续费策略）。

  **Acceptance Criteria**:
  - [ ] 文件存在且 > 6KB。
  - [ ] 对照表 ≥ 15 行。
  - [ ] 显式讨论 wallet 冻结 / 退款 / subscription 续费 / payment 流程是否被 Rust 表达。

  **QA Scenarios**:

  ```
  Scenario: 财务核心能力都被点名
    Tool: Bash (grep -ic)
    Steps:
      1. grep -ic "wallet\|subscription\|payment\|refund\|markup\|pricing" 文件
    Expected Result: 每个 ≥ 1
    Evidence: .omo/evidence/parity/task-10-coverage.txt
  ```

  **Commit**: NO

- [ ] 11. **语义 diff — Routing & Candidate（8 strategies / health / circuit / sticky / provider_ops）**

  **What to do**:
  - 在 `.omo/evidence/parity/11-semantic-routing.md` 输出路由与候选选择域的对照表。
  - 对照轴：候选过滤（按模型、按 capability、按健康、按余额、按租户）、负载均衡策略（双方各自实现的策略集合）、sticky session、circuit breaker、retry executor、provider key 轮询、provider 健康检查、provider_ops（启用 / 禁用 / 维护态）、超时 override（per-vendor）、proxy_node 选择（Aether 特有，对应到 Rust 时即 go-web 决策）。
  - Rust 的 8 strategies 必须逐个对照 Aether 是否有等价 / 上位 / 下位实现。

  **Must NOT do**:
  - 不要把 `aether-proxy` 的 forward-proxy 流量路由混入 LLM upstream 路由 —— 是两种路由。
  - 不要忽视 Aether `services/candidate/` 与 `services/provider_keys/` 的关系（候选选择和密钥轮询通常分离）。

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T12
  - **Blocked By**: T1, T2, T3, T4

  **References**:
  - Aether 端：
    - `Aether/src/services/candidate/` — 候选筛选。
    - `Aether/src/services/provider/`、`Aether/src/services/provider_keys/`、`Aether/src/services/provider_ops/`
    - `Aether/src/services/scheduling/`、`Aether/src/services/orchestration/`
    - `Aether/src/services/health/`、`Aether/src/services/capability/`、`Aether/src/services/proxy_node/`
    - `Aether/src/modules/proxy_nodes/`
  - Rust 端：
    - `crates/gw-routing/src/` — 8 strategies + 候选过滤。
    - `crates/gw-proxy/src/filters/upstream.rs` — 候选 → HttpPeer。
    - `CLAUDE.md` 中提到的：`Sticky` 无 TTL 清扫、retry executor 未上 hot path、`Vendor.timeout_overrides` 缺失。

  **WHY Each Reference Matters**:
  - Aether 的 `proxy_node` + `scheduling` 几乎肯定有 Rust 网关里没有的「区域 / 节点 / 网络选路」概念 → 高价值 Partial/Divergent；
  - Rust 的 8 strategies 是它自认的强项 → 必须逐个核实是否真的能匹配 Aether 的所有调度场景。

  **Acceptance Criteria**:
  - [ ] 文件存在且 > 6KB。
  - [ ] 对照表 ≥ 15 行。
  - [ ] Rust 的 8 strategies 在表中逐个出现并给出对照结论。

  **QA Scenarios**:

  ```
  Scenario: 8 strategies 全部对照
    Tool: Bash (grep -ic)
    Steps:
      1. 对 Rust 8 strategy 名称（从 crates/gw-routing 中取）逐一 grep
    Expected Result: 8/8 命中
    Evidence: .omo/evidence/parity/task-11-strategy-coverage.txt
  ```

  **Commit**: NO

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing. **Do NOT auto-proceed. Wait for user's explicit approval.**

- [ ] F1. **Plan Compliance Audit** — `oracle`
  阅读本计划与最终报告 `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md`。逐条核查「Must Have」是否都达到，对「Must NOT Have」做反向扫描（搜「实现建议 / 应该用 / recommend / suggest」等关键词，禁止出现）。检查 7 个必备小节是否齐全。检查 catalog 行字段是否全部对齐 Rubric 的取值集。
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Sections [7/7] | Rubric Conformance [N/N rows] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Report Quality Review** — `unspecified-high`
  人工读完整篇报告。检查：(1) Executive Summary 是否在 300 字内传达核心数字；(2) 每个 capability narrative 是否对应至少一行 catalog；(3) catalog 中每个 Aether 引用（路径 + 符号/行号）抽样 5 个，用 `read`/`grep` 确认引用真实可达；(4) 是否存在 AI slop（重复内容、空洞短语、夸大限定词）。
  Output: `Summary [PASS/FAIL] | Narrative-Catalog Cross-Ref [N/N] | Reference Reachability [5/5] | AI Slop [CLEAN/N issues] | VERDICT`

- [ ] F3. **Mechanical QA Checklist** — `unspecified-high`
  完整执行 Definition of Done 中的 11 项机械检查（文件存在、大小、grep section、子目录覆盖、migration 引用、rollup 加总、空格检查、Open-question 交叉引用），把每条命令 + 输出贴到 `.omo/evidence/parity/f3-checklist.txt`。
  Output: `Checks [11/11 PASS] | Evidence file present | VERDICT`

- [ ] F4. **Scope Fidelity Audit** — `deep`
  跑 `ls /Users/whereslow/Desktop/work/Aether` 拿到顶层目录列表。把每个目录在报告中搜一次。所有顶层条目必须出现在「In-depth catalog」「One-line mention」或「Deliberately Not Catalogued」三组之一。同样对 `Aether/src/services`、`src/api`、`src/modules` 子目录做穷举式核查。检测「越界」：报告是否擅自在某行写了实现建议（与 Must NOT Have 矛盾）。
  Output: `Top-level coverage [N/N] | Subdir coverage [N/N] | Boundary Violations [CLEAN/N] | VERDICT`

---

## Commit Strategy

- 计划任务全部不写代码，只产出 markdown 报告 + 证据文件。
- 唯一 commit：在 T12 输出报告 + T13 通过后做一次提交。
  - Message: `docs(parity): aether vs ailinkmax-gateway feature gap analysis`
  - Files: `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md`, `.omo/evidence/parity/*.md`
  - Pre-commit: F1-F4 全部 APPROVE + 用户 OK

---

## Success Criteria

### Verification Commands
```bash
# 文件存在且非空
test -s docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md && echo OK

# 必备小节齐全
for s in "## Executive Summary" "## Rubric" "## Stratification Rollup" \
         "## Per-Capability Narratives" "## Full Gap Catalog" \
         "## Architecture Decisions Needed" "## Deliberately Not Catalogued"; do
  grep -q "$s" docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md || echo "MISSING: $s"
done

# Aether 子目录覆盖
for d in /Users/whereslow/Desktop/work/Aether/src/services/*/; do
  name=$(basename "$d")
  grep -qi "$name" docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md \
    || echo "UNCOVERED: services/$name"
done
# 类似对 api/ 与 modules/ 重复

# Catalog 中无空白 / TODO
! grep -E '\| *(TODO|\?\?\?|) *\|' docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md
```

### Final Checklist
- [ ] 「Must Have」每条都在报告中验证可见
- [ ] 「Must NOT Have」每条都未出现违例
- [ ] F1-F4 全部 APPROVE
- [ ] 用户显式 "OK"
