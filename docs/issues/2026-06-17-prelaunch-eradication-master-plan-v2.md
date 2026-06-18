# AILinkMax 上线前缺陷根治计划 v2（修订版）

> **状态**：v1 修订 · 与 v1 配合阅读（v1 是基线，v2 是基于 6 个 sonnet 独立验证的修订与补全）
> **v1 路径**：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md`（双仓 mirror）
> **本文件路径**：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md`（双仓 mirror）
> **起草**：2026-06-17
> **触发**：用户原则补充 + 6 个 sonnet 验证发现 v1 仍有 **70+ 项设计漏洞 + 60 项结构性缺失**

---

## 0. 阅读须知（对执行该计划的 agent）

**重要 1**：本计划的执行者**不假定熟悉 Rust 或 Go**。所有规格都用 **7 段强制格式** 写：

**重要 2（最关键的一条）**：**本计划里的 sonnet 输出（v1、v2 REV-FINAL-* 与原审计 file:line 声明）是假设，不是结论**。已经被抽样反证过：原审计 H9 声明 `api_keys` 表 "缺 `AND deleted_at IS NULL`"，实读源码发现该表根本没有 `deleted_at` 列（hard delete），原始描述与实际不符。

**对执行者的强制要求**：
- 进入任何 TDD 步骤前，先用 Read 打开 cited 的 file:line，核对字面代码是否与本文档声明一致。**不一致以源码为准，并在 sub-plan 里记录修正**。
- v2 §4 所有 REV-FINAL-* 提议的修复方案均为**候选**，**不是必须照搬**。每个修复落地前要至少考虑 1 个备选方案，挑选时写 ADR 风格的简短理由（"为什么选 A 而非 B"）。
- 任何与源码冲突的"候选"必须撤回，改用源码确证的方案。
- 若 v2 §99 的 INVESTIGATE-N 项落在你的工作范围，把它当作开放项标记到 sub-plan，**不要假定结论**。

---



1. **【是什么 What it IS】** — 一句话定义这个 task 产出物。给出**具体文件路径**。
2. **【不是什么 What it is NOT】** — 列举常见误解 + 明确范围外。
3. **【为什么 Why this exists / why bug occurs】** — 根因（不是症状）。引用原始审计 ID + sonnet 发现 ID。
4. **【做什么 DO】** — 必做的动作（顺序排列）。
5. **【不做什么 DON'T】** — 诱人的错误方案 + 反模式（会引入新 bug）。
6. **【怎么做 HOW (具体)】** — 字面代码、字面命令、字面文件路径与行号。**禁止 "see code"、"as needed"、"appropriate"** 等含糊词。
7. **【如何验证 VERIFY】** — 具体测试命令 + 期望输出。**禁止 "passes tests"** 含糊语**。

每个 TDD 步骤（无论 WS-1 还是 WS-16）必须有这 7 段。**缺一段 PR 不予合并**。

> 范例见本文件 §3（"显式格式范例"），里面是一个完整任务条目的模板。

---

## 1. 修订动机（v1 之后发生了什么）

### 1.1 用户追加的五条原则（v1 §1）+ 一条新原则

| # | 原则 | v1 是否已写入 | v2 处置 |
|---|---|---|---|
| P1 | 根因修复 | ✓ | 保留 |
| P2 | 零回归（对抗审查） | ✓ | 保留 + 适用到 WS-3..WS-16 |
| P3 | 完全修复 | ✓ | 保留 |
| P4 | 零疑点（任何疑点派 sonnet/haiku） | ✓（但 v1 内无 INVESTIGATE-N 实际标记，违反） | **修订**：本 v2 文件结尾的 §99 列出所有 v2 内 INVESTIGATE-N |
| P5 | 意外发现强制纳入 | ✓ | 保留 + §8 流程已激活 |
| **P6（新）** | **明确性原则**：所有计划与子计划必须使用 §0 的 7 段强制格式 | — | **新增**：见 §0 |

### 1.2 6 个 sonnet 验证报告（详见附录 A）

| 验证人 | 范围 | 主要产出 |
|---|---|---|
| `ae68cf9c066b334df` | §5 覆盖矩阵 | 1 项错映（**C3→T17 错指**） + 1 项孤儿（**SH1-3 未定义**） + 140 项 ID 净通过 |
| `a3dee199ac5df9432` | WS-1 修正案对抗复审 | 11 项**仍未根治**（FINAL-M1..M11） |
| `a694c3252f84c7fd8` | WS-2 修正案对抗复审 | 13 项**仍未根治**（FINAL-W1..W13） |
| `a482ea02ec22ebe61` | WS-3/4/5 首轮对抗审查 | 25 项（FINAL-V1..V25） |
| `aa7709e65f953030d` | WS-6/7/8 首轮对抗审查 | 22 项（FINAL-H6.1..H8.9） |
| `ace010fa51cdc9da1` | Meta 审查 | 9 项完全缺失 + 10 项时序灾难 + 7 项原则违反 + 9 项 DoD 不足 + 5 项验收门空缺 + 7 项工时风险 + 10 项基建缺失 + 8 项横切关注 + 8 项攻击面 + 5 项辅助发现 |

总计 v1 之后新增需处置项：**71 项设计漏洞 + 60 项结构缺失 = 131 项**。本 v2 全部纳入，编号 **REV-N**（修订）与 **FILL-N**（补全）。

---

## 2. 总体修订（对 v1 的元层调整）

### REV-001：补全缺失工作流（v1 只有 WS-1..10）

新增 6 个工作流（WS-11..16），与 v1 的 WS-1..10 协同：

| WS | 名称 | 解决什么 | 大致工时 | 必须存在的理由 |
|---|---|---|---|---|
| **WS-11** | **前端契约协同** | API 错误码变更（R7 200→502、R25 错误码归并、license 402、新 header）必须前端配合 | 3-4 人日 | v1 缺失；CLAUDE.md `tests/contract` 与 `make test-frontend` 是必跑测试 |
| **WS-12** | **CI / 构建基建** | 5+ 新 lint（goroutine 监督、`as i64`、sqlc 软删、prod compose、`_pv` bump、deploy ordering gate） | 4-5 人日 | v1 假定 lint 存在，实际全部需要新写 |
| **WS-13** | **测试 Harness** | Chaos 注入（Pingora kill、Sentinel partition、CH down、Redis CROSSSLOT、跨版本 mixed-deploy） | 5-7 人日 | v1 所有 WS DoD 都引用 "chaos 测试通过"，但 chaos 设施需要先建 |
| **WS-14** | **数据库迁移协调** | Goose 编号、上线顺序、down 脚本、跨仓 Rust `crates/gw-store/migrations/` vs Go `migrations/` 协调 | 2-3 人日 | v1 至少 8 个新 migration，无单一 owner |
| **WS-15** | **回滚 / Feature Flag 策略** | M2 webhook 5xx 切换、R7 502、license 全开、JWT 强配等高风险变更需要 feature flag 与回滚剧本 | 3-4 人日 | v1 §7 #9 提及 revert plan 但无具体 spec |
| **WS-16** | **安全披露 / CVE 流程** | R1（跨租户 JWT）、R2（HMAC 重放）、R3（kid 伪造）、R4（XFF 绕过）属 P0 安全；需 embargo / 客户通告 / 协调披露 | 2-3 人日 | v1 完全缺失 |

新工时合计：19-26 人日。原 v1 估 43-60 人日 + 19-26 = **62-86 人日**，按 7 人团队 = **9-12 个工作日**，仍可在 2 周内（含验证）。

### REV-002：覆盖矩阵纠正（v1 §5）

- **修正 C3 错映**：v1 §5.5 写 "C3 | WS-8 + WS-1 | T17"。T17 实际是 R7 的 HTTP 502 路径，与 C3（Terminated 语义统一）无关。
  - **改为**：`C3 | WS-1 + WS-8 | T16` —— T16 是 Lua 原子化 + reconciler，是 Terminated 语义真正落地的地方。
  - 同时 T16 任务描述追加："并强制 Rust Terminated 路径 `credits_consumed = Some(0)`、Go 据此 `cost_micros = 0`。"
- **解除 SH1-3 孤儿**：v1 §2.1 与 §3 WS-10 引用 "SH1-3" 但无对应 ID 描述。
  - **裁定**：SH1-3 是原审计 appendix 内 "杂项" verifier agent (`a3aa9c0a6443d4772`) 提及但未单独落地的子项。
  - **处置**：派 sonnet 复核此 agent 取出实际 SH1-3 描述；并入 WS-10 或归档 §2.5 已证伪。**不允许"SH1-3"作为悬浮标识符存活**。该任务记为 **INVESTIGATE-001**（见 §99）。

### REV-003：时序灾难修正（meta §B）

| Meta-B 项 | v1 错处 | v2 修订 |
|---|---|---|
| 跨轨同文件冲突 `modules/billing/business/dedup.go`（T21+T22+S8.4） | Track A 与 B 与 contract 三处编辑同文件 | **REV-003a**：T21+T22+S8.4 合并为单一 PR，归 Track A 唯一负责人；其他轨等其合并 |
| `crates/gw-ledger/src/lease.rs`（T13+T15+S2.12） | Track B 与 D 同文件 | **REV-003b**：S2.12（heartbeat）改为只读 lease；写 lease 仍归 T13/T15（Track B） |
| `crates/gw-proxy/src/filters/response.rs`（WS-3+WS-6+WS-8） | 三 WS 同文件 | **REV-003c**：合并 WS-3/WS-6 同文件改动为单 PR，归 Track C；WS-8 Codec 改动独立 |
| WS-2 supervisor 阻塞 WS-4，但同日落地 | D3 矛盾 | **REV-003d**：WS-2 supervisor 接口在 D1 完成；WS-4 在 D3 才开始 |
| WS-1 Rust T16 触 Go dedup.go | 跨语言跨轨 | **REV-003e**：T16 拆 T16a（Rust Lua + reconciler）与 T16b（Go dedup 配套），分别归 Track B 与 Track A，T16b 在 T22 同 PR 内合并 |
| C4 "Go 先" 与 T22 "Go 先于 Rust 部署" 冲突 | PR 顺序 vs deploy 顺序混淆 | **REV-003f**：明确"PR 合并顺序：Rust T16a 先合 → Go T16b/T22 后合 → 部署顺序：Go 先部署（含 T16b/T22）→ Rust 后部署（含 T16a）" |
| Migration goose 编号未分配 | 跨仓冲突 | **REV-003g**：WS-14 内集中分配；分配前任何新 migration PR 禁止合并 |
| sync-rust-contract.sh D0 跑，Rust HEAD D3-D8 变 | 基线过时 | **REV-003h**：每 PR 合并 Rust 改动后必跑一次，CI 强制 |
| R5/R6 Lua 脚本同文件 `scripts.rs` | Track B 与 D 冲突 | **REV-003i**：归 Track B 唯一 owner |
| WS-9 G9 依赖 WS-1 outbox | 并行错 | **REV-003j**：WS-9 G9 排到 WS-1 outbox 合并后 |

### REV-004：原则违反修正（meta §C）

| Meta-C 项 | v2 修订 |
|---|---|
| S1.10 G3 outbox TTL 是 band-aid | **REV-004a**：删除 TTL 方案；改为 PG `pg_xact_commit_timestamp` 列 + relay 按该列 ORDER（详见 §4 WS-1 修订） |
| S5.7 G11 6 位码限速是 band-aid | **REV-004b**：除限速外，6 位码升 **8 位 alphanumeric**（base32），entropy 从 20bit 升至 40bit |
| S5.18 R25 错误码归并是 band-aid | **REV-004c**：HTTP 层返单一 `GW_AUTH_FAILED`；但 server-log 仍记录 outcome；并新增 INVESTIGATE-002：审计现有错误码集合，是否还有其他 oracle |
| S5.11 R1 jwt_validator panic 杀已有部署 | **REV-004d**：引入 feature flag `[auth] strict_jwt_validator: bool = false`，N+1 版本默认 true，N+2 删除 legacy（WS-15 owner） |
| H7 license 强转 = 客户锁出 | **REV-004e**：feature flag `[license] enforce: enum{disabled, warn, enforce} = warn`；warn 模式记录违例 14 天，再切 enforce（WS-15 + WS-16 owner） |
| P4 零疑点但 v1 无 INVESTIGATE 标记 | **REV-004f**：本 v2 §99 集中列出 INVESTIGATE-N |
| P2 仅 WS-1/2 做了对抗审查 | **REV-004g**：本 v2 已对 WS-3..10 全部做（见 §1.2 表）；WS-11..16 在执行时还要再对抗复审一次 |

### REV-005：DoD 不足修正（meta §D）

为所有 WS DoD 添加**具体阈值与工具**。统一改写在各 §4 WS-N 修订块。范例：

- 旧："RSS 平稳" → 新："RSS P95 在 24h soak 内波动 ≤ ±10% （基线由 `make e2e-soak-smoke` 前置 30s 采样）；`prometheus_remote_write` 上传 `process_resident_memory_bytes`，告警阈值 `+15%`"
- 旧："0 漏扣 0 重扣" → 新："`tests/e2e/wallet/concurrent_deduct_test.go` 1000 并发 × 60s，最终 `wallets.recharge_balance_micros` 变化值 `== Σ(成功响应 cost_micros) ± 0`；用 `pg_xact_commit_timestamp` 验证序列一致；fault injection 通过 `pgmocker` 在第 500 个 op 处主动 `pg_terminate_backend()`"
- 旧："chaos 通过" → 新："使用 `tests/e2e/chaos/` harness（WS-13 交付），按 `chaos-matrix.yaml` 执行 X 个 scenario，每个 scenario PASS/FAIL 二态写入 `.chaos_report/<scenario>.json`，全部 PASS 是合并门禁"

### REV-006：验收门补全（meta §E）

v1 §7 增加：

- **§7 #10**：所有触及 api 层的 PR 必跑 `make test-frontend` 且通过。
- **§7 #11**：所有 DB migration 必有 down 脚本，且 down 脚本在 staging snapshot 上验证可还原至 byte-equivalent。
- **§7 #12**：C4 mixed-version gate 实现：CI 检查 `shared/rustpact/version.go` 内 `RustpactMinSupportedVersion` ≥ Rust HEAD 的 `_pv`，反之上线被拒。
- **§7 #13**：runbook 完整性：每个 §7 #6 列举的 lint 与新告警必有对应 runbook 条目；缺失即拒。
- **§7 #14**：每个 P0 安全披露（R1-R4）必须经过 WS-16 流程；无对应安全告知/客户通告记录的 PR 拒合。

### REV-007：工时风险修正（meta §F）

| 风险 | v1 估值 | v2 修正 |
|---|---|---|
| WS-1 Track A 11d / 单人 / 10 工作日 | 紧 | **REV-007a**：Track A 拆为 A1（M1-M4 + 同表）与 A2（G1-G7 + 同表），双人 |
| T16 跨文件 2d 估值偏低 | 不够 | **REV-007b**：拆 T16a（Rust）+ T16b（Go），各 1.5d |
| Track F (WS-5) 22 项给单人 | 不可能 | **REV-007c**：Track F 拆为 F1（Go fail-closed：S5.1-S5.10）+ F2（Rust fail-closed：S5.11-S5.20），双人 |
| Track G (WS-10 + chaos) 同人 | 不可能 | **REV-007d**：WS-10 给 Track G；WS-13 chaos harness 独立 Track H |
| WS-2 估 5-7d 太紧 | 不够 | **REV-007e**：调整为 8-10d |
| 无对抗复审 buffer | 缺 | **REV-007f**：在里程碑 §6.3 D8-D10 间加专用"对抗复审日" |
| T22 临界路径 | 风险 | **REV-007g**：T22 强排到 D1-D3；任何阻塞升级到全员 |

### REV-008：基建/前置补全（meta §G）

由 WS-12（CI 基建）与 WS-13（test harness）拥有，详见 §4 新工作流。

### REV-009：横切关注补全（meta §H）

- 前端：WS-11 owner
- 监控告警迁移：WS-10 内追加任务（删除 v1 中 R15 高基数 label 的同时迁移 Grafana 面板与告警）
- mixed-version 窗口：WS-15 owner
- license H7 客户锁出风险：WS-15 owner（feature flag）+ WS-16 owner（客户通告）
- 支付回填：WS-1 内追加 T_BACKFILL_PAYMENT（处置 v1 时代滞留 abandoned 的支付）
- 跨团队 SRE 依赖：WS-10 owner 协调
- 部署管道集成：WS-12 owner

### REV-010：攻击者场景补 DoD（meta §I）

| 场景 | 所属 WS DoD |
|---|---|
| Slowloris 跨协议 | WS-3 / WS-6 |
| 跨版本部署窗口攻击 | WS-1 / WS-8 / WS-15 |
| Outbox TOCTOU 多副本 | WS-1（已在 FINAL-M1 修订） |
| Sentinel 分区 Go↔Rust | WS-1 / WS-10 / WS-13 |
| 时钟漂移 | WS-1 / WS-10 |
| Idem-Key 冲撞 | WS-1（已在 FINAL-M11 修订） |
| JWKS/static_pem race | WS-5 |
| 跨实例重放 | WS-5 / WS-13（多 region 测） |

---

## 3. 显式格式范例（v2 §0 的具体化）

所有 TDD 子计划（A 的产物）与 v2 修订条目都按以下范例：

````markdown
### TASK W1-T03：在 `wallet.Deduct` 内注入 `idempotency_keys` 提交

**【是什么 What it IS】**
- 一个 Go 代码改动，文件 `modules/wallet/business/service.go`，函数 `Deduct(...)`，约第 243-336 行。
- 把 `repo.InTx(...)` 内最后一步从直接返回，改为先调用新的 `idem.Commit(tx, ...)`。

**【不是什么 What it is NOT】**
- ❌ 不是新增 `Reserve/Release` 接口（v1 那套已废弃，详见 REV-FINAL-M2）。
- ❌ 不涉及修改 `service.go` 内 `Credit`、`Refund`、`adminDebit`（这些是平行任务 T04/T05/T06）。
- ❌ 不修改 `idem.go` 内的 `Idem` 接口签名（接口定义在 T01 已完成）。

**【为什么 Why this bug exists】**
- 根因：v1 设计的"Reserve 在 tx 外、Release 走 defer"假设 `pgx.Tx` 有 `AfterRollback` hook，**实际 pgx 没有**。
- 引用：原审计 M1（idem 只 Reserve 不 Release）+ verifier `a3dee199ac5df9432` 之 FINAL-M2。
- 后果：闪断 → Reserve 写入 Redis → tx 失败 → idem 键残留 → 后续重试走 `!ok` 分支返成功 shape 零扣费 → 真金白银漏。
- 根本治理：把 idem 写入与余额变更**同 PG tx**，rollback 自然回滚 idem 行。

**【做什么 DO】**
1. 在 `Deduct` 内 `repo.InTx` 的 callback 末尾，组装 `response` struct（包含 `BalanceAfter` 等）。
2. 调用 `s.idem.Commit(ctx, tx, "deduct", req.IdemKey, request_hash, response)`。
3. 根据返回值分支：
   - `didWrite=true` → 继续返回 response 给 caller。
   - `didWrite=false, prior != nil` → **反序列化** prior 为 `DeductionResult` 返回（这是历史响应复现）。
4. 在 PR 描述里链回本 task ID 与 FINAL-M2。

**【不做什么 DON'T】**
- ❌ 不要在 `idem.Commit` 失败时 panic；返回 err，让 `InTx` 整体 rollback。
- ❌ 不要把 `idem.Commit` 放在 `Deduct` 顶层（tx 外）— 那退化为 v1 的错误设计。
- ❌ 不要在 `response` 里塞**敏感字段**（cost basis、internal user_id），因为 prior 复现会回客户端。
- ❌ 不要用 `ON CONFLICT DO NOTHING`（详见 §4 WS-1 REV-FINAL-M2，应用 `ON CONFLICT DO UPDATE SET ... RETURNING xmax, response_jsonb`）。

**【怎么做 HOW (具体)】**

```go
// modules/wallet/business/service.go (around line 280)
result, err := s.repo.InTx(ctx, func(tx pgx.Tx) (*DeductionResult, error) {
    // ... existing balance read + decrement logic ...
    response := &DeductionResult{
        BalanceAfter: walletToBalance(w),
        Allocations:  allocs,
    }
    requestHash := sha256Hex(canonicalize(req))  // helper from shared/canon

    didWrite, priorJSON, ierr := s.idem.Commit(ctx, tx,
        "deduct",
        req.IdemKey,
        requestHash,
        response,
    )
    if ierr != nil {
        return nil, fmt.Errorf("idem.Commit: %w", ierr)
    }
    if !didWrite {
        var prior DeductionResult
        if uerr := json.Unmarshal(priorJSON, &prior); uerr != nil {
            return nil, fmt.Errorf("idem prior decode: %w", uerr)
        }
        return &prior, nil
    }
    return response, nil
})
return result, err
```

`Idem.Commit` 接口（已在 T01 落地，仅作交叉引用，**不要修改**）：
```go
// shared/contracts/wallet.go
type Idem interface {
    Commit(ctx context.Context, tx pgx.Tx,
        op string, key string,
        requestHash string,
        response any,
    ) (didWrite bool, prior json.RawMessage, err error)
}
```

`Idem.Commit` 实现关键 SQL（已在 T02 落地，仅作交叉引用）：
```sql
INSERT INTO idempotency_keys (op, key, request_hash, response_jsonb, created_at)
VALUES ($1, $2, $3, $4, now())
ON CONFLICT (op, key) DO UPDATE SET request_hash = idempotency_keys.request_hash
RETURNING (xmax = 0) AS did_write, response_jsonb
```
（注意：`ON CONFLICT ... DO UPDATE SET request_hash = idempotency_keys.request_hash` 是**无操作 update**，目的是让冲突行也参与 RETURNING；这是规避 READ COMMITTED 可见性的 idiomatic 写法。）

**【如何验证 VERIFY】**

1. 单元：`go test ./modules/wallet/business/ -run TestDeductIdempotency -race -count=10`
   - 期望：`PASS` 全部 10 次 race 检测 + 1000 并发同 IdemKey 仅 1 次成功扣费。
2. 集成：`tests/e2e/wallet/concurrent_deduct_test.go`（WS-13 交付）
   - 期望：1000 并发 × 60s，DB 闪断中点 → 余额 == Σ成功 cost ± 0、idempotency_keys 行数 == 唯一 IdemKey 数、`wallets.recharge_balance_micros - sum(used) == 0`。
3. CI lint：`scripts/check-soft-delete.sh`（WS-12 交付）必须不报警（确保不引入新软删 query 缺谓词）。
4. 输出文件：`logs/W1-T03-verify.txt` 包含 `PASS` 字样并附 race 报告。

**【依赖】** 阻塞于 T01（接口）、T02（migration）；并行于 T04（Credit）、T05（Refund）、T06（adminDebit）。

**【对抗复审】** 完成后派 sonnet 复审，使用 prompt template `tdd-adversarial-review.txt`（WS-12 交付），重点检查：(a) request_hash 计算是否包含所有显著字段；(b) `prior` 反序列化是否处理 schema 演进；(c) `idem.Commit` 在 tx 内的 SQL 是否真用了 DO UPDATE 而非 DO NOTHING。
````

**所有任务必须达到这个详细度。** 不允许 "implement appropriately"、"add error handling"、"write tests"。

---

## 4. WS-1..10 修订条目（仅列对 v1 设计的关键修订；其余文字以 v1 为准）

> 完整 TDD 步骤将由 §6 的 A 阶段并行 sonnet 在 `docs/superpowers/plans/2026-06-17-WS-N-*.md` 落地。本节只记录**修订点**与**必读防误项**。

### WS-1 修订（钱 / 幂等 / 补偿）

| 修订 ID | 应用到 v1 哪里 | 修正内容（必须按 §3 7 段格式展开成 TDD 步骤） |
|---|---|---|
| **REV-FINAL-M1** | v1 §4 WS-1 S1.6 outbox processor | `SELECT ... FROM payment_outbox WHERE status='pending' ...` 必须加 `FOR UPDATE SKIP LOCKED`；同样 `refund_outbox`。Reason: 多副本 worker 会同时 grab 同行，无 SKIP LOCKED 即双扣。DO: 加 SKIP LOCKED；DON'T: 加普通 FOR UPDATE（会让一个 worker 阻塞而非跳过）。 |
| **REV-FINAL-M2** | v1 §4 WS-1 S1.2 Idem.Commit 接口 | `ON CONFLICT DO NOTHING` 在 READ COMMITTED 下 conflict row **看不见**，第二调用者拿不到 prior。改为 `ON CONFLICT (op, key) DO UPDATE SET request_hash = idempotency_keys.request_hash RETURNING (xmax = 0), response_jsonb`。Reason: DO UPDATE 触发返回的可见性扩展。DO: 单 SQL 完成所有路径；DON'T: 用 SELECT 二次查询（仍有可见性窗口）。 |
| **REV-FINAL-M3** | v1 §4 WS-1 S1.16 LeaseGuard 双标志 | 抽样已验证：`lease.rs:117-123` LeaseGuard 字段为 `{uid, reserved, coef_version, released, state}`，**确实没 `settled` 字段**；`settle_actual` (199-206) 仅 `fetch_add(refund)` **确实非幂等**。<br>**候选 A（推荐起点）**：加 `settled: AtomicBool`，`settle_actual` 顶端 `if self.settled.swap(true, AcqRel) { return; }`；`release` 内 `if !self.settled.load(Acquire) { self.settle_actual(0); }`；Drop 仅调 release。<br>**候选 B**：保留两 AtomicBool 但 `release` 走 settle_actual(0) 才置 released。<br>**候选 C**：用一个 `state: AtomicI64` 编码三态。<br>**执行者必须**：核实是否有合法的"先 settle(estimate) 再 settle(final)"调用模式（若有则 A 会丢第二次结算，需切换到 B 或专门 `update_actual`）。先 grep `settle_actual` 所有 caller 再定结构。 |
| **REV-FINAL-M4** | v1 §4 WS-1 S1.9 Refund.Create | wallet 行 lock 锁不住 refund_requests 跨行 SUM。改为 `pg_advisory_xact_lock(hash_int('refund:'||wallet_id))` 在 tx 开头。Reason: PG 无 SUM predicate lock。DO: advisory lock；DON'T: 提升到 SERIALIZABLE（性能炸）。 |
| **REV-FINAL-M5** | v1 §4 WS-1 S1.6 outbox processor 重试 | 必须查 `idempotency_keys.request_hash == sha256(canonical(outbox.payload))`。Reason: 区分"成功但丢 ack"与"被 admin 改过"。DO: 校验 hash 才 mark committed；DON'T: 见到 idem 行就 mark committed。 |
| **REV-FINAL-M6** | v1 §4 WS-1 S1.1 idempotency_keys 表 | 原建议为 PG 声明式分区按 `created_at` 月分区；落地时因 `ON CONFLICT (op,key)` 全局唯一幂等语义优先，WS-14 将首版实现锁定为普通表 + `(op,key)` PK + `created_at` 索引，分区/90 天 retention 后续由 WS-1 owner 在不破坏全局唯一约束的前提下单独 migration。Reason: PG range partition unique/PK 必须包含 partition key，会削弱 `(op,key)` 全局幂等。DO: 保住单 SQL 幂等提交；DON'T: 为分区牺牲跨月份唯一性。 |
| **REV-FINAL-M7** | v1 §4 WS-1 S1.22 Lua | XADD 用 `MAXLEN ~ cap` 可能因配置错被 server 拒导致 Lua abort 半途。改为 `redis.call('XADD', stream, '*', ...)` **不带 MAXLEN**，trim 由独立后台 job 跑。Reason: server-side error 中断 Lua 无 rollback。DO: 拆开 trim；DON'T: 在 Lua 里 trim。 |
| **REV-FINAL-M8** | v1 §4 WS-1 S1.23 sentinel | 取消 `billing:settled:{rid}:{attempt}` 独立 SET。reconciler 改为查 `XRANGE usage:records - +` 在最近 24h 内是否有匹配 `(rid, attempt)` 的条目。Reason: 独立 sentinel 与 Lua-DEL 的时序漏洞无法关闭。DO: 用 usage:records 自身做去重源；DON'T: 用独立 SET（永有时差）。 |
| **REV-FINAL-M9** | v1 §4 WS-1 S1.25 catch_unwind spill | 加 `Semaphore::new(32)` 限并发 spill；spill delete 失败不容忍（escalate restart）；replay-recovery 也走 §S1.24 的 SETNX 去重。Reason: 16+ concurrent spill 在慢 Redis 下塞磁盘。DO: 限并发 + escalate；DON'T: 静默 `let _ = remove_file()`。 |
| **REV-FINAL-M10** | v1 §4 WS-1 S1.14 Upgrade | "单 tx" 仍违反 modules-no-import。改为 subscription_upgrade_outbox 模式：subscription tx 内写 `UPDATE old SET status='upgrading'` + outbox row；processor 异步调 Purchase；Purchase 失败 → outbox retry；连续 5 次失败 → outbox dead，把 old 改回 active 并告警。Reason: outbox 是唯一合规路径。DO: outbox；DON'T: 跨模块同 tx。 |
| **REV-FINAL-M11** | v1 §4 WS-1 S1.5 AdminAdjust | client-supplied Idempotency-Key 可跨 admin 重放；改为：server 端 key = `sha256(adminID || walletID || balanceType || signedAmount || clientNonce)`，clientNonce 走 header。两 admin 同 nonce → 不同 adminID → 不同 key。Reason: 仅 nonce 命名空间太宽。DO: namespace by principal；DON'T: 信任客户端 key 字符串。 |

### WS-2 修订（Worker / Drainer / Tunnel 生命周期）

| 修订 ID | 应用到 v1 哪里 | 修正内容 |
|---|---|---|
| **REV-FINAL-W1** | v1 §4 WS-2 S2.1 Supervised | 必须给具体 RestartPolicy 类型签名：`{ MaxConsecutivePanics: int, BackoffInitial, BackoffMax: Duration, JitterFrac: float, PanicAction: enum{Restart, FatalToErrgroup} }`。+ 每 worker 具体 recipe 表。+ `unhealthy *atomic.Bool` 发布到 `/readyz`。 |
| **REV-FINAL-W2** | v1 §4 WS-2 S2.3 sink offset | 当前 sink 无 batch ID 概念。改为：drainer 用 `(redisID, payload)` 入 sink；sink 内 batch 时分配单调 `batchID`，flush 成功后通过专用 `success chan struct{ batchID uint64, redisIDs []string, n int }` 通知；drainer 仅 XACK 已 success 的 redisIDs。无 oneshot token。 |
| **REV-FINAL-W3** | v1 §4 WS-2 S2.5 CH 去重 | ClickHouse driver 必须设 `wait_for_async_insert=1`；audit_sink 改 `id = sha256(request_id || action || ts_ms_bucket)` 以让 ReplacingMergeTree 真正去重。 |
| **REV-FINAL-W4** | v1 §4 WS-2 S2.9 binaries/web shutdown | 重写 `binaries/web/main.go` 与 `binaries/all-in-one/main.go`：全部 `go func` 改为 `runtime.Supervised(...)` 注册到顶层 `*sync.WaitGroup`；shutdown 顺序：`srv.Shutdown` → ctx cancel → wg.Wait → 然后 pool.Close（移出 defer）。 |
| **REV-FINAL-W5** | v1 §4 WS-2 S2.12 EXTEND_LEASE_LUA | `ZADD GT` 不创建成员；reclaim 后 heartbeat 永久 no-op。改为先 `ZSCORE` 探测；缺 → `ZADD NX`；在 → `ZADD GT`。两步在同 Lua 内。 |
| **REV-FINAL-W6** | v1 §4 WS-2 S2.13 SETTLE_LUA | 当前 SETTLE_LUA `HINCRBY + XADD` 重复调用会双扣 + 双发。Lua 顶部加：`local cur = redis.call('HGET', KEYS[1], 'status'); if cur == 'settled' then return 'noop' end`。 |
| **REV-FINAL-W7** | v1 §4 WS-2 S2.14 cancel drain | 5s 太短/太长都有问题。改为 per-codec 配置：`drain_timeout = { anthropic_messages: 30s, openai_chat: 15s, gemini: 30s }`；超时则 `RST_STREAM` 显式撕掉 HTTP/2 stream。+ 全局 `Semaphore::new(2*global_concurrency)` 限并发 drainer。 |
| **REV-FINAL-W8** | v1 §4 WS-2 S2.15 close().await | bridge 用 `scopeguard::guard(writer, |w| tokio::spawn(async move { let _ = w.close().await; }))`；保证 Ok/Err/panic 三路径都尝试 close。 |
| **REV-FINAL-W9** | v1 §4 WS-2 S2.15 reload_nodes | 添加 grace 周期：`ReconnectingTransport::close()` 仅置软关闭标志阻止新 `open_stream`；存量 stream drain；60s 后 hard close。 |
| **REV-FINAL-W10** | v1 §4 WS-2 S2.16 JoinSet | `relay_stream` 顶部 `scopeguard::defer!(joinset.abort_all())`；不依赖 tokio JoinSet drop 行为。 |
| **REV-FINAL-W11** | v1 §4 WS-2 S2.20 supervised_spawn | 抽样已验证：`gw-runtime/src/supervisor.rs:58` 与 `gw-store/src/managed_loop.rs:35` 已有 `Supervisor` 实现；**所以"undefined"措辞过激**。问题实际是：现有 API 表面是否覆盖 (a) panic 捕获 (`catch_unwind(AssertUnwindSafe(...))`)、(b) RestartPolicy 退避、(c) CancellationToken 传播、(d) `one_shot_spawn` 兄弟函数 —— 这 4 项要 audit。<br>**执行者必须先做**：用 Read 打开 `gw-runtime/src/supervisor.rs` 全文 + `gw-store/src/managed_loop.rs`，列出现有 API 表面与 4 项需求的 diff；再决定 (i) 扩展现 crate、(ii) 在 `gw-runtime` 增加新函数、(iii) 新建 crate。**不要默认新建 crate**。 |
| **REV-FINAL-W12** | v1 §4 WS-2 S2.19 quota_state_outbox | processor 状态机 `pending → emitted_xadd → emitted_del → committed`，分阶段重试。 |
| **REV-FINAL-W13** | v1 §4 WS-2 S2.18 stream_consumer XAUTOCLAIM | 改为：boot 时 `XINFO GROUPS events:entity`，老 instance_id 模式且 >24h 无活跃 → `XGROUP DESTROY`；同时 leader 跑 `XTRIM events:entity MAXLEN ~ N`。 |

### WS-3 修订（内存有界）

| ID | 修正 |
|---|---|
| **REV-FINAL-V1** | BoundedBuf 的 Truncate policy 不适用 `upstream_body_accum`（codec 收到截断 JSON 失败）。policy-per-buffer 强制：`upstream_body_accum` → `RejectRequest`；`sse_leftover` → `Truncate`；`stream_probe.buffer` → `RejectRequest`。配置注入处加 enum 校验，operator 设错即 boot panic。 |
| **REV-FINAL-V2** | Sweepable per-map 谓词具体定义。`InFlightStore` 只能 evict `count==0`（否则计数损坏）；`KeyHealthStore` 用 `last_seen` 时间戳；`RoundRobin/WeightedRr` 加 `last_used: Instant`。**硬 cap** 仅 warn + metric，不强制 LRU 在 `InFlightStore`。 |
| **REV-FINAL-V3** | Codec `max_accumulated_bytes` 触发时：emit SSE `[ERROR]` 帧 + `ctx.usage.aborted=true` + settler refund 本次 attempt 的 output cost。 |
| **REV-FINAL-V4** | 邮件验证限速移入 Redis（已存在 RedisEmailVerificationStore），不在 in-mem。Rust 的 `IpRateLimitGuard.sweep` / `AuthFailureIpLimiter.sweep` 同样问题，要在本 WS 加任务。 |
| **REV-FINAL-V5** | **删除** "Redos 静态估算" 整段。`regex` crate 是 RE2 (linear)，估算不适用。改为：保留 post-hoc tracker，但阈值从 10 → 1，即首次超时即 disable；且 `tracker.is_disabled` 检查在 `check_model_against_permissions` 之前。 |
| **REV-FINAL-V6** | `daily_tokens:*` sweeper 改为**一次性迁移脚本**（PR 部署后 48h 跑一次），非常驻 loop。 |
| **REV-FINAL-V7** | 高基数 label 杀名单具体化：`gw_billing_emit_lag_t0_total{request_id}` → 删 `request_id` 保 `attempt_index`；`gw_redos_timeout_total{api_key_id}` → 改 `{disabled_now: bool}`。+ CI lint：metric label value 必须 enum-allowlist。 |

### WS-4 修订

| ID | 修正 |
|---|---|
| **REV-FINAL-V8** | Go `Supervised` 完整 body 与 Rust `Supervisor` 镜像；带 `unhealthyFlag` 发布到 `/readyz`。 |
| **REV-FINAL-V9** | Lint 用 `go/analysis` AST walker，不用 regex。同 Rust 的 `scripts/check-invariants.sh` 改为 syn parse。 |
| **REV-FINAL-V10** | `binaries/web/main.go` 三裸 `go func` 重写（与 REV-FINAL-W4 合并）。 |
| **REV-FINAL-V11** | `/readyz` 聚合 `SupervisorRegistry.AllHealthy()`；任一 unhealthy → 503。 |
| **REV-FINAL-V12** | Rust `supervised_spawn` vs `one_shot_spawn` 双 API（与 REV-FINAL-W11 合并）；lint 豁免 `#[cfg(test)]`。 |

### WS-5 修订

| ID | 修正 |
|---|---|
| **REV-FINAL-V13** | License middleware 固定 mount 顺序：`requestID → panicRecover → license → auth → authz → handler`。白名单完整列举：`/livez`、`/readyz`、`/api/license/status`、`/api/license/machine`、`/api/license/activate`、`/api/auth/login`、`/api/public/*`。 |
| **REV-FINAL-V14** | sqlc 软删审计脚本重定义：扫 `migrations/` 找带 `deleted_at` 的表；对每表 `git grep "FROM <table>"` + `git grep "JOIN <table>"`，要求 `<table>.deleted_at IS NULL` 在 WHERE/ON，或经过 users.deleted_at 守护的 JOIN 链。**apikey 表本身没有 deleted_at**（hard delete），H9 实际是 users 软删时 apikey 仍解析；要在 `getApiKeyByHash`/`getApiKeyByID` 加 `JOIN users u ON u.id = api_keys.user_id AND u.deleted_at IS NULL`。 |
| **REV-FINAL-V15** | defaultGroupID fail-closed 需配合 bootstrap migration 种子默认 group；并把 users.group_id 改 NOT NULL；否则首用户创建失败。 |
| **REV-FINAL-V16** | DLQ 具体定义：attempt 计数嵌入 stream entry；N=5 → 写 `events:entity:dlq` + XACK 主流；`scripts/drain-cache-dlq.sh` 提供；告警 DLQ size > 1k。 |
| **REV-FINAL-V17** | apikey cache 失效改为 Store decorator 模式：`Store` 接口任何成功 mutation 触发 `cacheInvalidator.OnMutation(keyID)`（同时 local `cache.Delete` + push `events:entity`）；任一失败 → 整个 mutation 返 503。 |
| **REV-FINAL-V18** | OAuth bind-token Redis 失败 → 503 + 提示重试；**禁止 in-mem fallback**。 |
| **REV-FINAL-V19** | legacy `verify_jwt` panic 改为 feature flag `[auth] strict_jwt_validator: bool`，默认 false 一个 release，下个 release 默认 true，再下个 release 删 legacy。 |
| **REV-FINAL-V20** | static_pem fallback 配合 nbf 即可启用；iss/aud 改为推荐不强制；启动期 warn 而非 panic。 |
| **REV-FINAL-V21** | HMAC nonce SETNX 改为可选 `signature_secret.replay_check: enum{none, nonce_redis, nonce_local}`，默认 `none`；选 `nonce_redis` 且 Redis 不可用 → 503（fail-closed by operator choice）。也建议同时把 ±300s 缩到 ±60s 作为更便宜的改进。 |
| **REV-FINAL-V22** | trusted_proxies 启动期校验：`GW_ENV=production` 且 `trusted_proxies=[]` → boot panic。提供 GCP/AWS LB CIDR 模板。 |
| **REV-FINAL-V23** | **删除** Sticky N=3 方案。改为：bound key 不可用时当前请求 fallback `random available` 或 `least loaded available`，不重绑也不计数。简单且无新状态。 |
| **REV-FINAL-V24** | ReservationGate bootstrap-complete = `gateway_uptime > startup_grace_secs (30s) OR ReservationGlobalView.last_event_received_at.is_some()`。 |
| **REV-FINAL-V25** | `DispatchOutcome` 新增 `Malformed` 变体；counter_handler 在 parse 失败时返 Malformed；consumer 见 Malformed 不 XACK，留 PEL；XAUTOCLAIM 兜底。 |
| **REV-CROSS-AUTH** | JWT alg confusion 显式列任务：`JwtValidatorConfig.allowed_algs: Vec<Algorithm>` 必填；启动期校验非空；解 token 前先校验 `header.alg` 在 allowed_algs（在选 key family 之前）。 |

### WS-6 修订

| ID | 修正 |
|---|---|
| **REV-FINAL-H6.1** | 取消"preflight buffer 全场景"。拆三种 probe：
- **Header-only** (egress class / status>=400)：留在 `response_filter`，可 Err。
- **Stream-probe first-N**：维持现状（首字节通过 probe 即可 commit）。
- **Non-stream cross-protocol parse**：buffer 受 Proxy-1 cap 约束；超 cap 时 fail-closed → 502 `GW_UPSTREAM_BODY_TRUNCATED`。
streaming 一旦 commit 200 就无法撤销 → 失败用 SSE error 事件而非翻 status。 |
| **REV-FINAL-H6.2** | `emit_response_surfaces` 内部失败必须**吞掉**（只 metric+log），让外层 `cancel_grace_expired` Err 仍然返回。第二参 `bool` 命名为 `force_flush_partial`。 |
| **REV-FINAL-H6.3** | 写一张完整 ReqCtx 字段 → {persist/attempt-reset/request-end-reset/protocol-only} 表；`reset_response_attempt_state` 由该表派生；CI lint 强制新字段必须分类。 |
| **REV-FINAL-H6.4** | mpsc 改有界 + `try_send`：满则丢 surface + metric `gw_archive_channel_overflow_total{surface}`。channel cap 4096。 |
| **REV-FINAL-H6.5** | spawn 分类配额：billing emit → 无 timeout + spill backlog；counter emit → 小 bounded queue + drop；freeze release → 无 timeout（必须完成，幂等 by CAS）。**删全局 Semaphore**。 |
| **REV-FINAL-H6.6** | 4 处 `block_in_place` 全部废除，不只 archive：让 logging filter 成为单一异步出口。 |
| **REV-FINAL-H6.7** | 测试：cross→same 协议中段切换的客户端字节完整性。 |

### WS-7 修订

| ID | 修正 |
|---|---|
| **REV-FINAL-H7.1** | loader 用 `loaderCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 2*time.Second)`；不只 `WithoutCancel`。 |
| **REV-FINAL-H7.2** | trusted_proxies 默认 `[]`；`GW_ENV=production` 且 `trust_xff=true` 且 `trusted_proxies=[]` → boot panic。 |
| **REV-FINAL-H7.3** | rate limiter cleanup 拆为独立 goroutine（双相 read-lock 收集 → write-lock 删除）；`allow()` 内**禁止 scan**。 |
| **REV-FINAL-H7.4** | singleflight key 加类型 tag：`sfKey := key + "|" + reflect.TypeOf(zero).String()`；`v.(T)` 改为 `v, ok := v.(T); if !ok { return zero, ErrCacheTypeMismatch }`。 |
| **REV-FINAL-H7.5** | `vendor_factor` 量化为 `u32` micro-units（×1e6），全 i128 算术，末除 1e6 clamp 到 i64。 |
| **REV-FINAL-H7.6** | retry_after clamp + 检测 vendor 反复要求 >1h → escalate 状态 `Open(vendor_full_duration)` 或 page。+ metric `gw_health_cooldown_clamp_total{key,vendor}`。 |
| **REV-FINAL-H7.7** | WeightedRr 选项 A（推荐）：health-store 在 transition 到 unavailable 时 hook 设 `current[key]=0`；Phase 1 维持全员 fetch_add；SWRR 数学保持平衡。 |
| **REV-FINAL-H7.8** | `freeze_amount` 字段类型改 `i64`，构造期 `i64::try_from(amount)?`；后续算术全 i64。 |

### WS-8 修订

| ID | 修正 |
|---|---|
| **REV-FINAL-H8.1** | **删除 `DisallowUnknownFields`**。改用 `_pv` 比较：缺省 1；`>supported_max` → DLQ；`≤supported_max` → 默认 unmarshal 容忍未知字段。 |
| **REV-FINAL-H8.2** | provenance ladder 改重排避免 live Redis 值漂移：`upstream_final=10, cli_extracted=8, upstream_partial=6, estimated=4, crash_fallback=2, unknown=1`（留空隙）；存储值带版本 `v3:<rank>:<cost>`，Go 双窗口先读 v2/v3 兼容，后切 v3 only。 |
| **REV-FINAL-H8.3** | 新 8 字段统一 `u64`（Go 镜像 `int64`）；同时把现有 `input_tokens`/`output_tokens` 也升 u64（同 `_pv` bump）。 |
| **REV-FINAL-H8.4** | `ToolCallEnd` 作为信号事件（带 `valid_json: bool`），不是内容。Anthropic→Anthropic 仍 stream deltas；Cross-protocol 在 End 时聚合 emit。 |
| **REV-FINAL-H8.5** | model_pricing 加列分两步：先 nullable + 逐 model 回填；settler 见 NULL → 拒结算 + 告警。 |
| **REV-FINAL-H8.6** | sync-rust-contract.sh 输出 diff → 任何 rename/remove 即 abort，仅 additive 自动接受；PR 描述记 sync commit SHA。 |
| **REV-FINAL-H8.7** | dedup key BNF 明确：`settled:done:{rid}:{attempt_index}` 等四种 key；attempt_index 缺省 0；migration 期允许读旧 `settled:{rid}`。 |
| **REV-FINAL-H8.8** | response header `X-Request-Id` 必须回 Rust 分配值；client 传入的 `X-Request-Id` 只入 trace，**不入 billing dedup**；`x-conversation-id` 不变。 |
| **REV-FINAL-H8.9** | 部署顺序修正：**Rust 先**（开始发 attempt_index）、**Go 后**。Go 读旧 Rust（无 attempt_index）→ 回退 legacy `settled:{rid}` key 形态。CI 阻止反向。 |

### WS-9 修订

- **REV-FINAL-G9**：reconciler 用 Pipeline 批 + partial-error 容忍（不是 first-error-abort）。
- 其余按 v1 §4 WS-9 不变。

### WS-10 修订

- **REV-FINAL-R15-obs**：删除 v1 中 R15 高基数 label 的同时迁移现有 Grafana dashboard 与 alert，避免静默失声。
- **REV-FINAL-LICENSE-COMM**：H7 license 全开前 14 天 warn-only + 客户通告（WS-15+WS-16 协同）。

---

## 5. 新工作流（WS-11..16）

### WS-11：前端契约协同

#### 根因
v1 改了 API 错误码、HTTP 状态码语义、header（R2 nonce、license 402、R7 502 替换 200、R25 错误码归并）。前端 SDK 与 contract test 完全没人维护。

#### 必做
1. 列出 v1+v2 所有 API 行为变更（一张表）。
2. 前端 `frontend/src/api/*.ts` 同步更新错误处理。
3. `tests/contract`（Go 路由契约）补全新路由 + 新 header。
4. `make test-frontend` 通过。

#### 7 段格式 task：所有 task 由 §6 A 阶段 sonnet 在 `docs/superpowers/plans/2026-06-17-WS-11-frontend.md` 落地。

### WS-12：CI / 构建基建

#### 必做
- `scripts/check-goroutines/` Go AST analyzer：禁止裸 `go func`/`g.Go(...)` 在 `binaries/`、`modules/`，除非通过 `runtime.Supervised`/`OneShot`。
- `scripts/check-tokio-spawn.rs` Rust syn analyzer：等价规则。
- `scripts/check-soft-delete.sh`：sqlc 软删谓词审计（REV-FINAL-V14）。
- `scripts/check-prod-compose.sh`：禁止 `ALM_E2E_LICENSE_BOOTSTRAP` 出现在 prod compose。
- `scripts/check-money-as-cast.sh`：禁 money 类型 `as i64`。
- `scripts/check-rustpact-pv.sh`：rustpact 新增字段必有 `_pv` bump。
- `scripts/check-deploy-order.sh`：Go 侧 `shared/rustpact/version.go` 的 `RustpactMinSupportedVersion` ≥ Rust 当前 `_pv`（C4 部署顺序门禁）。
- `scripts/check-metric-labels.sh`：metric label value 必须 enum allowlist。
- 所有挂入 `make check`。

### WS-13：测试 Harness（Chaos）

#### 必做
- `tests/e2e/chaos/` 框架，统一 scenario 描述 YAML。
- Scenario 库：
  - Pingora SIGKILL 中点
  - PG 闪断（pg_terminate_backend / Toxiproxy）
  - Redis Sentinel partition + failover
  - ClickHouse down 30s
  - CROSSSLOT 模拟（小型 cluster 测试用）
  - 跨版本 mixed deploy（同时跑两个 Rust 版本 + 两个 Go 版本）
- 报告：`.chaos_report/<scenario>.json` PASS/FAIL 二态。
- CI gate：合并前 `make chaos-smoke` 通过。
- 长跑：`make chaos-full` 24h 通过为 launch gate。

### WS-14：数据库迁移协调

#### 必做
- 在 `migrations/` 与 `crates/gw-store/migrations/` 内为本 plan 集中分配 goose 编号。
- 写本 plan 所有 migration 的 down 脚本。
- staging snapshot 自动验证 down 可还原。

#### 编号分配表（WS-14 locked）

| WS | 编号 | 表 | 备注 |
|---|---|---|---|
| WS-1 | `00067_idempotency_keys.sql` | idempotency_keys | 普通表 + `(op,key)` PK；分区另行评估 |
| WS-1 | `00068_payment_outbox.sql` | payment_outbox | + processor |
| WS-1 | `00069_refund_outbox.sql` | refund_outbox | + processor |
| WS-1 | `00070_subscription_upgrade_outbox.sql` | subscription_upgrade_outbox | REV-FINAL-M10 |
| WS-1 | `00071_wallet_outbox_commit_ts.sql` | ALTER ... ADD commit_timestamp | REV-004a |
| WS-1 | `00072_user_subscriptions_quota_check.sql` | CHECK constraint | G1 |
| WS-9 | `00073_billing_covering_indexes.sql` | covering indexes | SLQ3 Go hot queries |
| WS-5 | `00074_default_user_group_seed.sql` | seed | REV-FINAL-V15 |
| WS-1 Rust | `0012_task_freeze_pending.sql` | task_freeze_pending | R9 |
| WS-8 Rust | `0013_pricings_extended.sql` | pricings 加新列 nullable | REV-FINAL-H8.5 |
| WS-9 Rust | `0014_billing_query_indexes.sql` | covering indexes | SLQ3 Rust hot queries |
| WS-2 Rust | `0015_quota_state_outbox.sql` | quota_state_outbox | R23 / REV-FINAL-W12 |

### WS-15：回滚 / Feature Flag 策略

#### 必做
- 定义 `flags/` 目录，每个 risky 改动有一份 yaml：
  - `flags/license_enforce.yaml` (`disabled|warn|enforce`)
  - `flags/jwt_strict.yaml` (`bool`)
  - `flags/payment_webhook_5xx.yaml` (`bool`)
  - `flags/r7_502_on_parse_fail.yaml` (`bool`)
- 默认值都是**最保守**（不打开新行为）。
- 每个 PR 必须在描述里说明：feature flag 名 + 默认值 + 计划切换日期 + 回滚步骤。
- runbook 入口。

### WS-16：安全披露 / CVE 流程

#### 必做
- R1-R4 触发安全披露流程：
  1. 走 embargo（48h）。
  2. 通知 affected customers（如有）。
  3. CVE 申请（若 product 公开）。
  4. 补 changelog + 升级建议。
- 部署后强制 14 天 warn-only 模式（与 H7 license 同模式）。

---

## 6. 顺序与并行化（修订 v1 §6）

### 6.1 关键路径（含 REV-007）

```
D0  WS-12 lint 框架   ╲
    WS-13 chaos harness 起步 ╲
    WS-14 migration 编号分配  ╲
    WS-15 flag 框架            ╲
    WS-8 S8.1 sync-rust-contract   ╲   全部 D0 启动
                                     ╲
D1  WS-1 T01 (Idem 接口) + T02 (migration 35) →
    WS-2 supervisor 基础   →
    WS-3 BoundedBuf trait + Sweepable trait →
    WS-4 Supervised/OneShot 实现 →
    WS-5 R1/R2/R3/R4 P0 安全 →
    WS-8 schema 扩展（先发 PR）

D3-D5 各 WS 主体并行；T22+T16b 在 D3 同 PR；T16a 在 D2 同 PR

D6   adversarial 复审日 #1（派 sonnet 复审 D1-D5 落地）

D7-D9  剩余 task；WS-11 前端契约对齐；WS-13 chaos 完整集
D10  adversarial 复审日 #2
D11  24h soak + launch gate 全检
D12  WS-15 灰度（warn-only 14 天 starts）
D13  launch gate 终评
D14  灰度切 enforce / launch
```

### 6.2 8 个并行轨（A1/A2/B/C/D/E/F1/F2/G/H 共 10 轨；7-9 人）

| Track | 负责 | WS |
|---|---|---|
| A1 | Senior Go #1 | WS-1 Go T1-T5 + T22 + T_BACKFILL_PAYMENT |
| A2 | Senior Go #2 | WS-1 Go T6-T12 + WS-9 Go |
| B  | Senior Rust #1 | WS-1 Rust T13-T20 + T16a |
| C  | Senior Rust #2 | WS-3 Rust proxy/codec + WS-6 + WS-8 Rust |
| D  | Senior Rust #3 | WS-2 Rust tunnel/task-runtime + WS-3 routing + WS-5 Routing-5/6 |
| E  | Senior Go #3 | WS-2 Go + WS-3 Go + WS-4 Go + WS-7 Go + WS-10 Go |
| F1 | Senior Sec #1 | WS-5 Go S5.1-S5.10 |
| F2 | Senior Sec #2 | WS-5 Rust S5.11-S5.20 + REV-CROSS-AUTH |
| G  | SRE | WS-10 + WS-14 + WS-15 |
| H  | QA/Test infra | WS-13 + WS-11 |
| WS-12 + WS-16 | shared by team lead | |

---

## 7. 验收门（合并 v1 §7 + REV-006）

完整 14 项，详见 v1 §7 + REV-006。

---

## 8. §8 + §9 + 附录 — 不变（继承 v1）

---

## 99. INVESTIGATE 列表（P4 零疑点强制清零）

合并前必须每条派 sonnet/haiku 调查或本团队确认：

| ID | 待证 |
|---|---|
| **INVESTIGATE-001** | SH1-3 原指代是什么？派 sonnet 取 `a3aa9c0a6443d4772` agent 留档复盘。 |
| **INVESTIGATE-002** | 现有错误码集合除 R25 外是否还有其他状态 oracle（auth/session/wallet）。 |
| **INVESTIGATE-003** | Rust `IpRateLimitGuard.sweep`/`AuthFailureIpLimiter.sweep` 的 O(N log N) 排序是否在生产配置下可被 DoS 触达。 |
| **INVESTIGATE-004** | `wallet.Credit` op-namespace（subscription bonus、refund、payment outbox 共用 "credit"？）。 |
| **INVESTIGATE-005** | TaskFreeze submit 失败后的 sweeper 是否能跨 Rust↔Go 边界协调回收。 |
| **INVESTIGATE-006** | shared/contracts 是否暴露 tx-receiving Purchase 变体（subscription Upgrade 是否真能走 outbox）。 |
| **INVESTIGATE-007** | Proxy-9 block_in_place 替换后 Tokio worker 余量是否足够（runtime worker 数审计）。 |
| **INVESTIGATE-008** | Rust 部署用的是单 shard Redis 还是 Cluster？SETTLE_LUA 跨 key 在 Cluster 下 CROSSSLOT 风险。 |
| **INVESTIGATE-009** | Pingora 0.8 是否暴露 `RST_STREAM` 接口（REV-FINAL-W7 依赖）。 |
| **INVESTIGATE-010** | OPTIMIZE FINAL ClickHouse 调度对正常查询的影响窗口（REV-FINAL-H8.5 依赖）。 |
| **INVESTIGATE-011** | 原审计 H9 已被反证（api_keys 表无 `deleted_at` 列，原描述与源不符）。抽样 10 项原审计的"file:line + 字面代码"声明，核对真实性，记到 `docs/issues/2026-06-17-investigates/INVESTIGATE-011.md`。在执行 A 期间，**每个 sub-plan 作者负责其工作范围内的声明核对**，未核对部分集中提单。 |

每个 INVESTIGATE 闭合记录在 `docs/issues/2026-06-17-investigates/<ID>.md`。

— v2 文档结束 —
