# Feature Parity 功能补齐 — 总控计划

## TL;DR

> **Quick Summary**: 补齐 ailinkmax-gateway 对比 Aether 的 18 项网关数据面功能缺口。经 Metis 根因分析，4 项表面修补被拆解为 6 个前置架构 plan + 6 个依赖功能 plan + 1 个直接执行 plan，共 13 个 plan 文件。
>
> **Deliverables**:
> - 6 项前置架构改造（IR 字段注入、路由 scoring 管线、统一 error pipeline、retry-after 接线、Gemini Files trait 重设计、Reservation 全局视图）
> - 10 项可直接执行的独立功能（count_tokens、images/edits、videos、TTFT 强制、/v1/models、Auth IP 限流、active-requests、candidate chain、auto-ban、Go 契约）
> - 6 项依赖前置的功能（model rewrite、body rules、multi-dim+presets、Gemini Files 实现、quota probe、adaptive reservation）
> - 所有 `docs/` 同步更新 + Go 契约文档
>
> **Estimated Effort**: XL
> **Parallel Execution**: YES - T=0 同时 11 task, T=1 接入 7 task, T=2 接入 2 task
> **Critical Path**: P-B → P-F → T15 → FINAL
> **Quality Mode**: High Accuracy (Momus review) — 用户选择高精度审查，Momus 循环待执行

---

## 计划结构

### 13 Plan 文件索引

| 文件 | 类型 | 含 Task | 依赖 | 状态 |
|------|------|---------|------|------|
| `feature-parity-direct.md` | 直接执行 | T2,T3,T4,T8,T11,T12,T13,T16,T18,T19 | 无 | 待生成 |
| `p-a-ir-field-injection.md` | 前置架构 | IR passthrough_overrides + mutation flag | 无 | 待生成 |
| `p-b-routing-scoring-pipeline.md` | 前置架构 | filter→extract→gate→score→pick 管线 | 无 | 待生成 |
| `p-c-unified-error-pipeline.md` | 前置架构+T6+T7 | ErrorClassifier 接线 + passthrough | 无 | 待生成 |
| `p-d-retry-after-consumption.md` | 前置架构+T14p1 | retry-after → apply_cooldown | 无 | 待生成 |
| `p-e-gemini-files-trait.md` | 前置架构 | GeminiFilesGateway trait 重设计 | 无 | 待生成 |
| `p-f-reservation-global-view.md` | 前置基础设施 | 异步聚合 + 全局视图 | P-B | 待生成 |
| `t1-model-rewrite.md` | 依赖功能 | Model name 请求体重写 | P-A | 待生成 |
| `t5-t17-multi-dim-presets.md` | 依赖功能 | 多维度调度 + presets | P-B | 待生成 |
| `t9-body-rules.md` | 依赖功能 | Per-endpoint body rules | P-A | 待生成 |
| `t10-gemini-files-impl.md` | 依赖功能 | Gemini Files 真实实现 | P-E | 待生成 |
| `t14-quota-probe.md` | 依赖功能 | Quota probe（可选增强） | P-D | 待生成 |
| `t15-adaptive-reservation.md` | 依赖功能 | Adaptive reservation | P-F | 待生成 |

### 执行图

```
═══════════════════════════════════════════════════════════════
T=0 立即启动（无依赖，最大并行度 = 11 task）
═══════════════════════════════════════════════════════════════
─── Direct Plan (10 tasks) ──────────────────────────────────
  Wave-D1 [quick × 8]: T2, T3, T4, T11, T13, T16, T18, T19
  Wave-D2 [unspecified-high × 2]: T8, T12

─── 5 前置架构 Plan 同时启动 ────────────────────────────────
  P-A [deep]: IR 任意字段注入
  P-B [deep]: 路由 scoring 管线
  P-C [unspecified-high]: 统一 error pipeline (含 T6+T7)
  P-D [unspecified-high]: retry-after 接线 (含 T14-part1)
  P-E [deep]: GeminiFiles trait 重设计

═══════════════════════════════════════════════════════════════
T=1 前置完成后接入依赖功能
═══════════════════════════════════════════════════════════════
  P-A done → T1 [quick] + T9 [unspecified-high] (并行)
  P-B done → T5+T17 [deep + quick] (串行)
  P-C done → (T6+T7 已合并在 P-C 内)
  P-D done → T14-part2 [quick] (可选 probe)
  P-E done → T10 [deep]

═══════════════════════════════════════════════════════════════
T=2 依赖 P-B 完成
═══════════════════════════════════════════════════════════════
  P-F [deep] → T15 [unspecified-high]

═══════════════════════════════════════════════════════════════
FINAL 所有完成后并行 4 review
═══════════════════════════════════════════════════════════════
  F1. Plan compliance audit (oracle)
  F2. Code quality review (unspecified-high)
  F3. Real manual QA (unspecified-high)
  F4. Scope fidelity + perf regression (deep)
```

### 依赖矩阵

| Plan/Task | 依赖 | Blocks | 性能门 |
|-----------|------|--------|--------|
| Direct T2-T4 | — | — | soak-smoke |
| Direct T8 | — | — | soak-smoke + stream bench |
| Direct T11/T13 | — | — | soak-smoke |
| Direct T12 | — | — | soak-smoke |
| Direct T16/T18/T19 | — | — | soak-smoke |
| P-A | — | T1, T9 | passthrough bench |
| P-B | — | T5+T17, P-F | strategy_pick bench |
| P-C | — | — | soak-smoke |
| P-D | — | T14-part2 | soak-smoke |
| P-E | — | T10 | multipart bench |
| P-F | P-B | T15 | strategy_pick bench |
| T1 | P-A | — | passthrough bench |
| T9 | P-A | — | passthrough bench |
| T5+T17 | P-B | — | strategy_pick bench |
| T10 | P-E | — | multipart bench |
| T14-p2 | P-D | — | soak-smoke |
| T15 | P-F | — | strategy_pick + soak full |

### Critical Path
`P-B → P-F → T15 → FINAL`

---

## 全局约束（所有 13 plan 共享）

### Must Have
- `cargo test --workspace` 全绿
- `cargo clippy --workspace --all-targets -- -D warnings` 全绿
- `bash scripts/check-invariants.sh` exit 0
- `cargo fmt --all -- --check` 全绿
- `cargo bench --workspace --no-run` 全绿
- `make e2e-soak-smoke` per-tier exit gate
- `docs/architecture.md` 和 `docs/workflow.md` 同 commit 更新
- Tests 在同 PR 内（tests-after-in-time ≠ tests-after-in-PR）
- 新配置 boot-time only，`config/default.toml` 含安全默认值
- 每个 plan 含 bench baseline 比对（P99 ≤ 5% drift）

### Must NOT Have
- 新 Cargo 依赖（未经审批）
- Rust 工具链或 Pingora 版本升级
- `unimplemented!()` / `todo!()` / `TODO(v2)` / "for v1" / "wave-X follow-up" 标记
- 机会主义重构（不碰相邻代码）
- 触碰 cost computation（`no_cost_micro` / `no_billing_crate` lint 必须保持绿色）
- 新发明文档目录结构（复用 `docs/superpowers/specs|plans` + `docs/go-contracts/contract-change`）
- 人工确认型验收标准
- 热路径新增 Mutex / Redis 同步调用
- 热路径新增每请求 alloc（未启用相应功能时）

### 6 处已修正文件路径（全部 plan 必须使用正确路径）

| ❌ 错误路径 | ✅ 正确路径 |
|---|---|
| `crates/gw-config/` | `crates/gw-gateway/src/config.rs` |
| `filters/rewrite.rs` | `crates/gw-proxy/src/filters/upstream.rs:478` (`apply_forced_params_to_ir`) |
| `filters/error_passthrough.rs` | `crates/gw-codec/src/error_passthrough.rs` |
| `gw-routing/src/health.rs` | `crates/gw-health/src/format_health.rs` |
| `gw-routing/src/classify_default.rs` | `crates/gw-core/src/error/classify_default.rs` |
| `gw-ratelimit/` | `crates/gw-auth/src/ratelimit/` |

### 性能回归门（所有 plan 共享）

```bash
# Per-plan (前置架构 plan 必做):
cargo bench -p gw-codec --bench passthrough -- --baseline before
cargo bench -p gw-routing --bench strategy_pick -- --baseline before

# Per-tier exit gate:
make e2e-soak-smoke

# 阈值: P99 退化 >5% 阻断 PR
```

---

## 根因分析摘要

### 4 个表面修补 → 根治方案

| 原任务 | 根因 | 前置 Plan | 根治路径 |
|--------|------|-----------|----------|
| T5+T17 (Multi-dim + presets) | `StrategyKind` 互斥 enum，无 scoring 管线 | P-B | filter→extract→gate→score→pick 管线，8 策略改为管线预设 |
| T1+T9 (Model rewrite + body rules) | IR 是 typed 子集，零拷贝被 `cow.into_owned()` 戳穿 | P-A | `passthrough_overrides: Map<String,RawValue>` + mutation flag |
| T10 (Gemini Files) | trait 只有 `ensure_uploaded`，无 list/get/delete/vendor-key | P-E | 重设计 trait + 4 端点方法 |
| T14 (Quota probe) | 429 Retry-After 已解析但被丢弃 | P-D | 接线 retry-after → apply_cooldown |

### 9 个已根治（直接执行）
T2, T3, T4, T8, T11, T12(修正), T13, T16, T18

---

## Final Verification Wave

> 所有 plan 完成后并行执行。ALL must APPROVE。

- [x] F1. **Plan Compliance Audit** — `oracle`
  读全部 13 plan。验证每个 Must Have/Must NOT Have。检查 `.omo/evidence/` 证据文件。
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Plans [13/13] | VERDICT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  `tsc/clippy/fmt/bench --no-run` + 逐文件审查 AI slop。
  Output: `Build [PASS/FAIL] | Lint [PASS/FAIL] | Tests [N/N] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high` + `playwright` skill
  执行每个 plan 的 QA scenarios，含跨 plan 集成测试。
  Output: `Scenarios [N/N] | Integration [N/N] | Edge Cases [N] | VERDICT`

- [x] F4. **Scope Fidelity + Perf Regression** — `deep`
  逐 task diff 比对。bench baseline 对比。性能退化检查。
  Output: `Tasks [N/N] | Bench [PASS/FAIL] | Contamination [CLEAN/N] | VERDICT`

---

## Commit Strategy

每个 plan 独立 PR，commit message 格式：
- 前置架构: `refactor(<scope>): <description> [P-X]`
- 直接功能: `feat(<scope>): <description>`
- Go 契约: `docs(go-contracts): <description>`
- 修正类: `fix(<scope>): <description>`

---

## Success Criteria

### Verification Commands
```bash
cargo test --workspace                                        # 全绿
cargo clippy --workspace --all-targets -- -D warnings         # 全绿
cargo fmt --all -- --check                                    # 全绿
cargo bench --workspace --no-run                              # 编译通过
bash scripts/check-invariants.sh                              # exit 0
make e2e-soak-smoke                                           # 通过
cargo bench -p gw-codec --bench passthrough                   # P99 ≤ 5% drift
cargo bench -p gw-routing --bench strategy_pick               # P99 ≤ 5% drift
```

### Final Checklist
- [ ] 全部 18 项功能补齐完成
- [ ] 6 个前置架构改造完成
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
- [ ] Go 契约文档产出
- [ ] docs/architecture.md 更新
- [ ] docs/workflow.md 更新
- [ ] config/default.toml 新增配置段
- [ ] 性能 bench 无退化
