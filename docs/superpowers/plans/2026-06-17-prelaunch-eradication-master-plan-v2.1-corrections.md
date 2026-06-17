# Prelaunch Eradication Master Plan — v2.1 Corrections

**Status**: corrections to v2 + 16 WS sub-plans, dated 2026-06-17.
**Scope**: post `plans complete` commit, gateway 7 new commits + Go 1 new commit changed the credit-lease topology from soft-defense (`frozen_micro`-only) to **hard-defense** (per-pod `LeaseGuard` + `OverflowGate` + Go `LeaseScheduler` fanout). This document corrects sonnet-stage misjudgements and locks executable deltas.

**This document is NOT a replacement for v2 or the 16 WS sub-plans.** It is an append-only correction layer. Where this document and a WS sub-plan disagree, this document wins.

---

## §0 五条强制原则（继承自 v2，未变）

执行任何 task 前必须读完本段。

1. **禁止"解决一个问题又因为这个问题导致新的问题的解法"** —— 任何修改都要先核查"会不会引入新风险"，特别是 fail-open ↔ fail-closed 转换的边界。
2. **禁止不完全的修复** —— metric 改名必须同步改所有引用（catalog + 字面量 + alert 文档 + dashboard）。spec/code 漂移修复必须双向闭合（要么改 spec，要么改 code，不能各走各的）。
3. **根本上修复** —— sonnet 给的 fix 建议是假设、必须 Read 源码验证。原始审计的描述也是假设，必须 Read 源码验证。
4. **任何疑点必须用 Read 验证，不接受"差不多对"** —— 凡是写"我猜"、"应该"、"大概"的判断，全部要落到 file:line。
5. **意外发现也要修** —— Read 时如果发现 spec/code 漂移、metric 命名不一致、文档缺口，必须落进本文档（§5 或 §6）。

---

## §1 撤回 — sonnet 错判 5 项

下表中标 **WITHDRAWN** 的 task 不要执行。理由全部基于 self-Read 源码或 Go 仓 docs。

| 原 sonnet 论断 | 状态 | 原因 + 证据 |
|---|---|---|
| **D1 "request.rs:1351-1418 OverflowGate-Allow 是 fail-open 回归"** | **WITHDRAWN** | 这是硬模式的故意设计。<br>**WHY (源码)**: `crates/gw-proxy/src/filters/request.rs:1230-1235` 注释明确 "On success, bypasses step 9-10 entirely and returns"；`request.rs:1237 if protection_cfg.enabled` 与 `1243 if in_protected` 确保只有 protected 用户进入 lease/overflow 路径。非 protected 用户跳过整个 8.5 块，正常走 step 9-10 frozen_micro 软防。 |
| **D3 "新 Redis namespace 未入 go-contracts"** | **WITHDRAWN** | Go 仓三处独立 docs 已登记：<br>1. `AILinkMax-Go/docs/architecture.md:36` — 列 `credit:balance_cache:<uid>` / `credit:overflow_consumed:<uid>` + owner = "Rust overflow gate"；<br>2. `AILinkMax-Go/docs/operations/credit-lease-alerts.md` — 7 条 alert 含 `OverflowCacheUnavailable` (P0) / `OverflowPgFallbackHigh` (P2)；<br>3. `AILinkMax-Go/docs/superpowers/specs/2026-06-16-credit-lease-hard-mode-design.md` — 完整硬模式 spec，与目标拓扑一致。 |
| **WS-1 T27 "OverflowGate.record_actual 对称化（应 record lease-miss 路径的 actual）"** | **WITHDRAWN** | 滚动窗口设计，不需要对称。<br>**WHY (源码)**: `crates/gw-ledger/src/overflow.rs:263-266 record_actual_delta`：只在 `actual > reserved` 时返 Some；`overflow.rs:OVERFLOW_INCR_LUA`：首次 SET 时 `EXPIRE 30s`，INCRBY 不续 TTL —— 30s 自然归零。spec §3.5 行 459："大部分 lease 路径走通,该 key 不存在"。lease-miss 请求已经在 step 8.6 INCRBY estimate 一次，settle 时 actual 接近 estimate，无需二次记录。 |
| **WS-10 T25/T26 "新 alert（cache_unavail / pg_fallback）"** | **WITHDRAWN** | 已存在。<br>**WHY (docs)**: `AILinkMax-Go/docs/operations/credit-lease-alerts.md` 行 11-13：`OverflowCacheUnavailable` (P0)、`OverflowGateHotspot` (P2)、`OverflowPgFallbackHigh` (P2)。 |
| **WS-10 T28 "docs/architecture.md 同步 background-task graph + namespace"** | **WITHDRAWN** | 已同步。<br>**WHY (docs)**: Go `architecture.md:36` 已列；Rust `docs/spec/06-billing-ledger.md:85` 已写 `overflow.rs` 设计；Go `specs/2026-06-16-credit-lease-hard-mode-design.md` 完整。 |

**Do not** revisit these 5 lines unless you find a counter-example in source. **Don't** trust sonnet's reasoning — sonnet did not read the hard-mode spec.

---

## §2 强制阅读 — 执行任何 WS sub-plan 前必读

WS-1..WS-16 的 16 份 sub-plan 是在**硬模式 spec 之前**就以"软防 + outbox"的假设写成的。每个执行 agent **必须** 先读完下列文件，否则会复现 sonnet 阶段的 5 处错判。

| 必读文件 | 在哪个仓 | 为什么必读 |
|---|---|---|
| `docs/superpowers/specs/2026-06-16-credit-lease-hard-mode-design.md` | **Go 仓** | 硬模式拓扑权威，含 §3.5 内存估算、§4.1 错误码表、§4.2 重试矩阵（含 12 行操作 × 重试 × 失败行为）、§4.3 重试原语规格 |
| `docs/spec/06-billing-ledger.md` 行 85 起 | **Rust 仓** | overflow.rs / heartbeat.rs / coef_snapshot.rs 设计描述 |
| `docs/architecture.md` 行 36（namespace 表） | **Go 仓** | 跨进程 Redis namespace 已登记 |
| `docs/operations/credit-lease-alerts.md` | **Go 仓** | 7 条 alert PromQL 与 metric 命名 |
| `crates/gw-ledger/src/lease.rs` 整文件 | Rust 仓 | LeaseGuard / LEASE_POOL / Mode（仅 Protected）— `unprotected` 模式已删除 |
| `crates/gw-ledger/src/overflow.rs` 整文件 | Rust 仓 | OverflowGate / balance_for / consumed_for / record_actual / OVERFLOW_INCR_LUA |
| `crates/gw-proxy/src/filters/request.rs` 行 1230-1422 | Rust 仓 | Step 8.5 lease + step 8.6 overflow（仅 protected 用户路径） |
| `crates/gw-proxy/src/filters/logging.rs` 行 145-200 | Rust 仓 | settle 路径（settle_actual + record_actual） |
| `modules/billing/business/lease_scheduler.go` 整文件 | **Go 仓** | LeaseScheduler.Tick + PushLease（含 balance ≤ 0 跳过的不变量 5 落地） |
| `crates/gw-runtime/src/redis_retry.rs` 整文件 | Rust 仓 | retry primitive，**注意 sleep 无 cancellation** |

**If you have not read these, stop and read them.** Do not start any task in this document until you have.

---

## §3 已敲定的 3 个决策

下面是经用户决策的最终状态。每条都包含 do / don't / why / how / verify。

### §3.1 D2 — Metric 命名统一（决策：A，改 metrics.rs 加 `gw_` 前缀）

#### What to do

把 `crates/gw-ledger/src/metrics.rs` 的 **8 个常量值**全部加 `gw_` 前缀。改 `crates/gw-proxy/src/filters/request.rs` 中 **4 处字面量**为引用 `gw_ledger::metrics::*` 常量。改 `AILinkMax-Go/docs/operations/credit-lease-alerts.md` 中 **4 行** Rust 端 metric 引用。

#### What NOT to do

- **不要**改 Go 端 metric 命名（`shared/platform/metrics/credit_lease.go` 不动）。Go 端没有统一 `gw_*` 前缀公约，混合命名是 by-design。
- **不要**改 metrics.rs 中**常量名本身**（`CREDIT_LEASE_ACQUIRE_TOTAL` 这个 Rust 标识符不改），**只改字符串值**。
- **不要**新增 `gw_freeze_*`、`gw_codec_*` 等已经带前缀的 metric。它们已经合规，不要"统一格式"动它们。

#### What it is / isn't

- **是**：Rust 端 metric 命名公约统一（绝大多数已 `gw_*`，只有 credit-lease 8 个常量漂移）。
- **不是**：Go-Rust 跨端命名统一（Go 端不动）。
- **不是**：metric 含义变更（label 集、value 范围、emit 位点全不变）。

#### Why the problem occurred

3022a3e 引入新子系统时，`metrics.rs` 集中常量值忘了带 `gw_` 前缀，但 `metrics_catalog.rs:679` 已注册 `gw_credit_lease_acquire_total`（带前缀），`request.rs:1295/1335/1374/1909` 也直接写字面量（带前缀）。结果：常量发的 metric 与字面量发的 metric **是两个不同 Prometheus 序列**。任何 dashboard / alert 都会撞 bug。

#### How

**Step 1** — 改 `crates/gw-ledger/src/metrics.rs` 8 个常量值（**只改右侧字符串**）：

```
// 改之前
pub const CREDIT_LEASE_ACQUIRE_TOTAL: &str = "credit_lease_acquire_total";
pub const CREDIT_LEASE_ESTIMATE_CREDITS: &str = "credit_lease_estimate_credits";
pub const CREDIT_OVERFLOW_GATE_TOTAL: &str = "credit_overflow_gate_total";
pub const CREDIT_OVERFLOW_LATENCY_MS: &str = "credit_overflow_latency_ms";
pub const CREDIT_OVERFLOW_CONSUMED_WINDOW: &str = "credit_overflow_consumed_window";
pub const PG_FALLBACK_TOTAL: &str = "pg_fallback_total";
pub const COEF_TABLE_ACTIVE_VERSION: &str = "coef_table_active_version";
pub const HEARTBEAT_SADD_TOTAL: &str = "heartbeat_sadd_total";

// 改之后
pub const CREDIT_LEASE_ACQUIRE_TOTAL: &str = "gw_credit_lease_acquire_total";
pub const CREDIT_LEASE_ESTIMATE_CREDITS: &str = "gw_credit_lease_estimate_credits";
pub const CREDIT_OVERFLOW_GATE_TOTAL: &str = "gw_credit_overflow_gate_total";
pub const CREDIT_OVERFLOW_LATENCY_MS: &str = "gw_credit_overflow_latency_ms";
pub const CREDIT_OVERFLOW_CONSUMED_WINDOW: &str = "gw_credit_overflow_consumed_window";
pub const PG_FALLBACK_TOTAL: &str = "gw_pg_fallback_total";
pub const COEF_TABLE_ACTIVE_VERSION: &str = "gw_coef_table_active_version";
pub const HEARTBEAT_SADD_TOTAL: &str = "gw_heartbeat_sadd_total";
```

**Step 2** — 改 `crates/gw-proxy/src/filters/request.rs` 4 处字面量为常量：

- `request.rs:1295` `"gw_credit_lease_acquire_total"` → `gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL`
- `request.rs:1335` 同上
- `request.rs:1374` 同上
- `request.rs:1909` `"gw_credit_overflow_gate_total"` → `gw_ledger::metrics::CREDIT_OVERFLOW_GATE_TOTAL`

也检查 `crates/gw-proxy/src/filters/logging.rs:155` 的 `"gw_credit_actual_exceeds_reserved_total"` —— 这是字面量但 metrics.rs 没对应常量。两个选择：
- **A**: 留字面量（接受这个孤儿 metric）；
- **B**: 在 metrics.rs 加一行 `pub const CREDIT_ACTUAL_EXCEEDS_RESERVED_TOTAL: &str = "gw_credit_actual_exceeds_reserved_total";` 并替换。**建议 B**（与本次修整对齐）。

**Step 3** — 改 `AILinkMax-Go/docs/operations/credit-lease-alerts.md` 4 行 Rust 端 metric 引用：

| alert 行号 | 旧 metric | 新 metric |
|---|---|---|
| 行 10 | `coef_table_active_version` | `gw_coef_table_active_version` |
| 行 11 | `credit_overflow_gate_total{result="cache_unavail"}` | `gw_credit_overflow_gate_total{result="cache_unavail"}` |
| 行 12 | `credit_overflow_gate_total{result="pass"}` | `gw_credit_overflow_gate_total{result="pass"}` |
| 行 13 | `pg_fallback_total{op="balance_read"}` | `gw_pg_fallback_total{op="balance_read"}` |

行 7 `gw_active_pods_count` 不改（Go 端 metric，已带前缀）。
行 8 `credit_lease_xadd_fanout_total`、行 9 `coef_table_publish_total` 都是 Go 端 metric（`AILinkMax-Go/shared/platform/metrics/credit_lease.go:10,18`），**不改**。

#### Verify

- `grep -rn '"credit_lease_\|"credit_overflow_\|"pg_fallback_total\|"coef_table_active_version\|"heartbeat_sadd_total' crates/` 应返回 0 行（除 metrics.rs 自身）。
- `grep -rn '"gw_credit_lease_acquire_total"\|"gw_credit_overflow_gate_total"' crates/gw-proxy/` 应返回 0 行（已改为常量）。
- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 启动 gateway，`curl :9090/metrics | grep -E "^gw_credit_|^gw_pg_fallback_|^gw_coef_table_|^gw_heartbeat_"` 应能看到全部 8 个 metric。

---

### §3.2 D4 — Ruby validator 命运（决策：B，删除）

#### What to do

删除 `scripts/check-k3s-image-sync-ci.rb`。

#### What NOT to do

- **不要**删之前确认它的逻辑没有别处复用。已确认（grep 全工作目录 0 引用）—— 安全删。
- **不要**删之后又把它的契约约束（"sync-k3s-rust-gateway 不接管 ingress/postgres/redis/clickhouse"、"Go 仓不 force-restart rust-gateway"、"Go 仓保留 live image"）扔掉。把这些约束**转抄进 spec**（见 How Step 2）。

#### What it is / isn't

- **是**：一个未挂入 CI 的 Ruby 双向 contract validator，删它本身不影响 CI 流程。
- **不是**：删去契约本身。Ruby 脚本编码的契约规则要保留为人工 checklist。

#### Why the problem occurred

`795bb04 ci: sync rust gateway image to k3s` 引入脚本但未挂入 .gitlab-ci.yml。CI runner 镜像 (`rust:1.88-bookworm` / `alpine:3.23`) 无 ruby toolchain。脚本是孤儿。

#### How

**Step 1** — 删除 Rust 仓 `scripts/check-k3s-image-sync-ci.rb`。

**Step 2** — 把脚本编码的契约规则归档到 Rust 仓 `docs/spec/` 一处合适位置（如 `docs/spec/ci-deploy-contract.md`）作为**人工 checklist**：

> ### CI deploy contract（Rust ↔ Go 双仓不变量）
> 
> 改 `.gitlab-ci.yml` 时必须保持：
> 
> **Rust 仓 `.gitlab-ci.yml`**：
> - `sync-k3s-rust-gateway` job 存在且 `needs: [push-image]`；
> - sync_script 包含 `kubectl set image` + `kubectl rollout status`；
> - sync_script **不**包含 `kubectl apply -f`、`ingress.yaml`、`statefulset/`、`deployment/go-allinone`、`postgres`、`redis`、`clickhouse`（即不接管这些）；
> 
> **Go 仓 `.gitlab-ci.yml`**：
> - **不**包含 `rollout restart deployment/rust-gateway`；
> - 包含 `current_rust_gateway_image=`、`RUST_GATEWAY_DEPLOY_IMAGE=`、`s#__RUST_GATEWAY_IMAGE__#${RUST_GATEWAY_DEPLOY_IMAGE}#g`（保留 live image）。
> 
> 改任一项前先读这一段。

**Step 3** — Rust `docs/architecture.md` §Invariants 添一行 "scripts/check-k3s-image-sync-ci.rb removed 2026-06-17, contract preserved at docs/spec/ci-deploy-contract.md"（或类似）。

#### Verify

- `git -C ailinkmax-gateway ls-files scripts/check-k3s-image-sync-ci.rb` 应返回空。
- `ls ailinkmax-gateway/docs/spec/ci-deploy-contract.md` 应存在并含上述 5 条约束。

---

### §3.3 D5 — overflow_consumed GET 失败处理（决策：A，按 spec 改 code，fail-open + WARN）

#### What to do

把 `crates/gw-ledger/src/overflow.rs:188-194` 的 `consumed_for` Redis read 失败处理从 **fail-closed (`Err(CacheUnavailable)`)** 改为 **fail-open (`Ok(0) + WARN`)**。

#### What NOT to do

- **不要**改 `overflow.rs:199-231 incr_consumed`（INCRBY 路径）的 fail-closed 行为。INCRBY 失败仍必须返回 `CacheUnavailable` → 503，这是 spec §4.2 行 496 的硬规定（"INCRBY 失败 = 漏账 = 违反 C1"）。
- **不要**把 `balance_for`（balance_cache GET + PG fallback）改成 fail-open。balance read 失败本来就该 fall back PG，PG 失败 → CacheUnavailable 是正确的（balance 是唯一权威）。
- **不要**改 `decide_overflow` 计算逻辑（`balance - consumed >= estimate`）。

#### What it is / isn't

- **是**：让 `consumed_for` 在 Redis 短暂抖动时不阻塞流量，把 consumed 视为 0 让 balance 单独权威决定。
- **不是**：放弃 fail-closed 防线。INCRBY 失败仍 503。
- **不是**：允许漏账。consumed 是 30s 滚动窗口，下次 INCRBY 成功就修正回来，且 balance_micro 是 Go 推 lease 时权威 clamp（不变量 1）。
- **不是**：fail-open 的常态。read 失败应是 P0 报警事件（`OverflowCacheUnavailable` alert 已存在，触发条件不变）。

#### Why the problem occurred

`overflow.rs:188-194` 实现时把 `consumed_for` Redis read 失败也按 fail-closed 处理（`Err(_) => Err(CacheUnavailable)`），但 spec §4.2 行 495 明确写"视为 0 + WARN"。两边漂移。

**为什么 spec 选 fail-open**：
1. balance_micro 是唯一权威（不变量 3），consumed 只是 30s 滚动窗口的辅助；
2. consumed read 失败 → 视为 0 → balance 单独决定 → 仍受 balance 约束；
3. 短暂 Redis 抖动不应让付费流量被 503 拒；
4. INCRBY 失败仍 503，所以"放行了但记不上账"的窗口很短（最多到下一次 INCRBY 尝试）。

#### How

**Step 1** — 修改 `crates/gw-ledger/src/overflow.rs:182-197 consumed_for`：

```rust
// 改之前 (overflow.rs:182-197)
async fn consumed_for(&self, user_id: &str) -> Result<i64, OverflowError> {
    let key = overflow_consumed_key(user_id);
    match self.redis.get::<Option<i64>, _>(&key).await {
        Ok(Some(consumed)) => Ok(consumed),
        Ok(None) => Ok(0),
        Err(err) => {
            tracing::error!(
                target: "gw_ledger::overflow",
                user_id,
                error = ?err,
                "overflow consumed read failed"
            );
            Err(OverflowError::CacheUnavailable)
        }
    }
}
```

```rust
// 改之后
async fn consumed_for(&self, user_id: &str) -> Result<i64, OverflowError> {
    let key = overflow_consumed_key(user_id);
    match self.redis.get::<Option<i64>, _>(&key).await {
        Ok(Some(consumed)) => Ok(consumed),
        Ok(None) => Ok(0),
        Err(err) => {
            // Spec §4.2 row "GET credit:overflow_consumed:<uid>": fail-open
            // (treat as 0 + WARN). balance_micro remains authoritative; the
            // overflow_consumed is a 30s rolling window only. INCRBY failure
            // path remains fail-closed (incr_consumed below).
            tracing::warn!(
                target: "gw_ledger::overflow",
                user_id,
                error = ?err,
                "overflow consumed read failed; treating as 0 (fail-open per spec §4.2)"
            );
            metrics::counter!(
                CREDIT_OVERFLOW_GATE_TOTAL,
                "result" => "consumed_read_failed_open"
            )
            .increment(1);
            Ok(0)
        }
    }
}
```

**为什么要新增 `result="consumed_read_failed_open"` label**：
- fail-open 是有 spec 背书的，但**不能静默**。新 label 让运维能在 Prometheus 上看到"今天有 N 次 read 失败被 fail-open 放行"。
- 这个 label 值要加进 `AILinkMax-Go/docs/operations/credit-lease-alerts.md` 行 11 的 `OverflowCacheUnavailable` PromQL 集合（建议加 P1 alert: `rate(gw_credit_overflow_gate_total{result="consumed_read_failed_open"}[5m]) > 0` ⇒ "consumed read is failing open; investigate Redis health"）。

**Step 2** — 修改 `crates/gw-ledger/src/overflow.rs` 测试，加一例：

```rust
// 在 mod tests 内追加
#[test]
fn consumed_read_error_fails_open_to_zero() {
    // documentation test: verify the fail-open contract is asserted somewhere.
    // since consumed_for is async + Redis-dependent, integration test in
    // tests/credit_lease_integration.rs is the right home — this stub asserts
    // the doc comment is the source of truth.
    let src = include_str!("overflow.rs");
    assert!(src.contains("fail-open per spec §4.2"), "consumed_for fail-open contract is undocumented");
    assert!(src.contains("consumed_read_failed_open"), "fail-open metric label missing");
}
```

更优的 verify 是在 `crates/gw-ledger/tests/credit_lease_integration.rs` 加 integration test：用 redis-down 场景验证 `OverflowGate::check` 仍返回 `OverflowDecision::Allow`。

**Step 3** — 修改 `AILinkMax-Go/docs/operations/credit-lease-alerts.md`，加新 alert：

```markdown
| `OverflowConsumedReadFailingOpen` | P1 | `rate(gw_credit_overflow_gate_total{result="consumed_read_failed_open"}[5m]) > 0` | Redis consumed-window reads are failing; admission is fail-open per spec. Check Redis health. |
```

**Step 4** — 检查 `AILinkMax-Go/docs/superpowers/specs/2026-06-16-credit-lease-hard-mode-design.md` §4.2 行 495，确认 spec 表述未变（应仍为 "视为 0 + WARN"）。如果 spec 不一致，**不要改 code**，先和 spec owner 对齐。**当前 spec 已确认与新 code 行为一致**。

#### Verify

- `cargo test -p gw-ledger`、`cargo test --workspace -- --ignored`（integration test 需要 Docker）通过。
- 手动验证：启 gateway + Redis，杀掉 Redis 后发请求（protected 用户）。期望：lease 失败 → overflow → consumed_for 返 0（WARN 日志可见）→ balance 决定 → 通过 OR 402。**不应** 503（除非 balance_for 也失败）。
- `curl :9090/metrics | grep consumed_read_failed_open` 应能看到计数器。

---

## §4 真问题清单（撤销错判后剩下的）

下表保留并按优先级排序。每项要落到 16 个 WS sub-plan 的某个 task 上，或新增 task。

| # | 项 | 优先级 | 落在哪个 WS / 新建 |
|---|---|---|---|
| R1 | D2 metric 命名漂移修复（§3.1） | **P0** | 新建 WS-10-T30，跨 WS（metrics.rs / request.rs / alerts.md / metrics_catalog.rs 验证） |
| R2 | D5 spec/code 漂移修复（§3.3） | **P0** | 新建 WS-1-T29 |
| R3 | D4 删 Ruby script + 契约归档（§3.2） | **P1** | 新建 WS-12-T13 |
| R4 | task-runtime 路径 SETTLE_LUA 非幂等（与硬模式无关） | **P1** | 保留 WS-1 T28，**但说明** 这与硬模式 credit-lease 路径无关，是 task-runtime 独立治理 |
| R5 | LeaseGuard.settle_actual 非幂等（防御性） | **P3** | 保留 WS-1 T16，但说明：唯一调用方 `logging.rs:160` 是单 fire 路径，现实风险≈0。是防御性修，不是 launch blocker |
| R6 | redis_retry primitive 缺 cancellation | **P2** | 保留 WS-2 T11b。**WHY 重要**：heartbeat worker 用同 primitive，shutdown 时最多拖 5s；overflow per-request 路径也用 primitive，shutdown 时单请求最多 +5s。worker 路径影响更大 |
| R7 | LeaseGuard::try_acquire 允许负 requested（防御性） | **P3** | 保留 WS-7 NEW-1。`requested < 0` 时 `current < requested` 永远 false，会 CAS 通过给出负 reserved。加 `if requested <= 0 { return Err(LeaseError::Insufficient { requested, available: 0 }) }` |
| R8 | OverflowGate.check 客户端重试双计 estimate | **P3** | 不必修。`overflow_consumed` 30s 自然归零，不漏账，只是偏紧 |

**Do**: 按 R1-R3 (P0/P1) 顺序优先执行。
**Don't**: 把 R4 / R5 当 launch blocker —— 它们都是低现实风险的防御性修。

---

## §5 16 个 WS sub-plan 的处置

| WS | 状态 | 说明 |
|---|---|---|
| WS-1 money/outbox/idem | **保留 + 微调** | 撤回 T27（§1）；T16 / T19 优先级降到 P3 / P1（§4 R5 / R4）；**新增 T29 = §3.3 D5** |
| WS-2 worker/drainer/tunnel | **保留 + 新增** | 新增 T11b cancellation（§4 R6）。原 T11 audit 表追加 3 行：`redis_retry primitive 已存在`、`spawn_heartbeat_task lib.rs:1082 裸 spawn`、`consumer.start lib.rs:1181 未注册 shutdown`（这些是 sonnet 在二次 Read 中发现的真盲区） |
| WS-3 bounded memory | **保留** | 新增可选 T22 = "Redis key TTL CI lint"，可推后 |
| WS-4 panic/supervisor | **保留** | 未受 plans-complete..HEAD 影响 |
| WS-5 fail-closed | **保留 + 一处降级** | S5.17 降级为 "加 `DispatchOutcome::Malformed` 变体 + metric + 留 PEL"，因 `counter_handler.rs` 已部分修了 `InvalidFields` 拒绝路径 |
| WS-6 HTTP integrity | **保留** | 未受影响。R6.1 关于 DetailedArchive 已是 bounded mpsc 的发现已确认 |
| WS-7 concurrency/numeric | **保留 + 新增** | 新增 NEW-1 LeaseGuard 负 requested 防御（§4 R7） |
| WS-8 schema/contract | **保留** | 未受影响 |
| WS-9 query perf | **保留** | 未受影响 |
| WS-10 ops hardening | **保留 + 撤回 + 新增** | 撤回 T25/T26/T28（§1）；新增 T30 = §3.1 D2 metric 命名修复 |
| WS-11 frontend contract | **保留** | 未受影响 |
| WS-12 CI/build infra | **保留 + 新增 + 更新** | 新增 T13 = §3.2 D4 删 Ruby + 契约归档（§4 R3）；T-10 §HOW 改为"增量插入 stages"（gateway 已有 `test`/`deploy`） |
| WS-13 chaos harness | **保留** | 未受影响 |
| WS-14 migration coord | **保留** | 未受影响 |
| WS-15 rollback/flags | **保留** | 未受影响 |
| WS-16 security disclosure | **保留** | 未受影响 |

---

## §6 INVESTIGATE — 留给执行 agent 的疑点

执行 R1-R7 中任何一项时，如果发现以下疑点，**必须**dispatch sonnet/haiku 或 Read 源码核对，不许凭印象。

1. **INVESTIGATE-013**: §3.1 改完 metrics.rs 后，dashboards / Grafana 配置是否引用旧 metric 名？两个仓 `grep -rn "credit_lease_acquire_total\|credit_overflow_gate_total" --include='*.json' --include='*.yaml'` 应为 0。
2. **INVESTIGATE-014**: §3.3 改完 consumed_for 后，是否还有其它路径假设它返 Err？`grep -rn "consumed_for\|OverflowError::CacheUnavailable" crates/` 看调用方。
3. **INVESTIGATE-015**: §3.2 删 Ruby 之前，必须确认 GitLab CI 的 schedule / pipeline trigger / artifact 配置中无引用（不只是 .gitlab-ci.yml 文件本身，还要看 GitLab UI 端的 schedule）。无法 grep 验证 GitLab 服务端配置 — **需用户在 GitLab UI 上人肉确认**。
4. **INVESTIGATE-016**: WS-1 T19 (flusher 侧 `SETTLE_ATOMIC_LUA`) 与 R4 (task 侧 `SETTLE_LUA` 非幂等修) 是两条独立路径，不能混在一起。Read `crates/gw-task/src/scripts.rs` 与 `crates/gw-ledger/src/flusher.rs` 各自 settle 路径，确认幂等键不互相覆盖。
5. **INVESTIGATE-017**: spec §4.2 行 488-491 涉及 Go 端重试（XADD events:counter:pod:<X>、SMEMBERS gw:active_pods、SELECT FROM wallets），它们的重试参数是否与 spec §4.3 一致？`grep -n "MaxRetries\|InitialBackoff\|max_retries" AILinkMax-Go/modules/billing/business/lease_scheduler.go AILinkMax-Go/shared/runtime/` 验证。
6. **INVESTIGATE-018**: `consumer.start()` 在 `gw-gateway/src/lib.rs:1181` 丢 handle 未注册 shutdown（sonnet 报告 C.2）。这是 plans-complete 之前的旧代码 — 但新 overflow 消费路径继承这个特性。WS-2 T22 落地时**必须**修这个注册缺口。

---

## §7 给执行 agent 的不变量（Do / Don't 总表）

| Do | Don't |
|---|---|
| 改 metrics.rs 时同步改 alerts.md 4 行 + request.rs 4 处字面量 | 改 metrics.rs 但留 request.rs 字面量（会重新漂移） |
| 改 consumed_for 时新增 fail-open metric label + alerts.md 新 alert | 改 consumed_for 但不加 WARN / metric（fail-open 必须可观测） |
| 删 Ruby script 时把契约规则归档到 docs/spec/ci-deploy-contract.md | 删 Ruby script 但丢失契约知识 |
| protected 用户走 lease/overflow → bypass step 9-10 — 这是设计 | 把 protected 路径加进 step 9-10（破坏硬模式拓扑） |
| Rust 单方面消费的 Redis namespace（balance_cache / overflow_consumed）不需要 Go 端契约 | 但 Go 端共享的（active_pods / coef_table）必须在 `shared/rustpact` / `shared/contracts` 里登记 |
| INCRBY credit:overflow_consumed 失败 → 必须 503 | INCRBY 失败 fall through 视为成功（漏账 = 违反 C1） |
| 任何"sonnet 说..." → Read 源码验证才能信 | 把 sonnet 输出当 ground truth |
| Read 时发现 spec/code 漂移 → 落进 INVESTIGATE | 静默"修一边"导致两边继续漂 |

---

## §8 Mirror

本文件必须 byte-identical 镜像到 `AILinkMax-Go/docs/superpowers/plans/2026-06-17-prelaunch-eradication-master-plan-v2.1-corrections.md`。

执行 mirror 后用 `diff -u` 验证两份完全一致。

---

## §9 硬防护栏清单（subagent 改任何代码前必读）

任何 sub-plan task 在执行前，必须先核对其 HOW 目标文件是否触碰下列任一项。若命中，**必须**阅读 §10 对应的修订条目 + §11 的教学矩阵后再动手。

### A. Rust 端不可破坏的文件/函数

| 编号 | 文件 | 不可破坏的语义 |
|---|---|---|
| A1 | `crates/gw-ledger/src/lease.rs` | LeaseGuard `{uid, reserved, coef_version, released: AtomicBool, state}` 字段；`try_acquire` 的 CAS 循环；`settle_actual` 只在 `actual < reserved` 时退款；`release` 仅 idempotent flip；`Drop` 调 release（**不**退款）；`upsert_protected` 入口 |
| A2 | `crates/gw-ledger/src/overflow.rs` | `OverflowGate.check` 调用顺序（balance_for → consumed_for → decide_overflow → incr_consumed）；`OVERFLOW_INCR_LUA` 首次 SET 才 EXPIRE；`record_actual_delta` 只在 actual > reserved 时返 Some；`fetch_balance_micro` 使用 `recharge_balance_micros + gift_balance_micros` |
| A3 | `crates/gw-ledger/src/heartbeat.rs` | active_pod_heartbeat 5s 周期 SADD + 15s TTL；`Supervisor::spawn` 注册 |
| A4 | `crates/gw-ledger/src/coef_snapshot.rs` | `load_latest_coef_snapshot` 冷加载 + version-ordered 安全 |
| A5 | `crates/gw-ledger/src/counter_handler.rs` | `handle_protected_lease`、`apply_coef_commit`；对 `events:counter:pod:<X>` 流的 ACK 行为 |
| A6 | `crates/gw-ledger/src/protection_config.rs` | `enabled` 默认 true、`lease_grace_ms`、`default_output_ceiling_tokens` |
| A7 | `crates/gw-runtime/src/redis_retry.rs` | `RetryConfig` + `retry()` 签名 |
| A8 | `crates/gw-proxy/src/filters/request.rs` 行 1220-1422 | Step 8.5 lease + 8.6 overflow 分支；`if protection_cfg.enabled && in_protected`；lease 成功 + overflow Allow 均 `return Ok(false)` 早 return；no_coefs → 503 fail-closed |
| A9 | `crates/gw-proxy/src/filters/logging.rs` 行 145-200 | `guard.settle_actual(actual) + overflow_gate.record_actual(uid, reserved, actual)` 顺序；`record_actual` 失败仅 warn（不升级到 bail/ERROR） |
| A10 | `crates/gw-gateway/src/lib.rs` 行 1080-1190 | heartbeat / overflow gate / consumer 装配；`Supervisor::spawn` 注册 |
| A11 | `crates/gw-ledger/src/metrics.rs` | 8 个 credit-lease metric 常量（§3.1 D2 修复后值带 `gw_` 前缀） |

### B. Go 端不可破坏的文件

| 编号 | 文件 | 不可破坏的语义 |
|---|---|---|
| B1 | `modules/billing/business/lease_scheduler.go` | `LeaseScheduler.Tick` 1s 周期；`PushLease` 81 行 `balanceMicro <= 0` skip（不变量 5）；ttl=random(10s,30s) clamp（不变量 1） |
| B2 | `shared/contracts/budgetpolicy.go` | `ActivePodsKey = "gw:active_pods"` |
| B3 | `shared/contracts/pricing.go` | `CoefTableLatestEntriesKey` / `CoefTableLatestMetaKey` 常量 |
| B4 | `shared/rustpact/stream.go` | `NsProtectedLease` / `NsCoefTable` / `NsCoefTableCommit` 常量 |
| B5 | `shared/rustpact/usage.go` | `UsageRecord` 的 `pod_id` / `credits_consumed` / `coef_table_version` 字段（**Usage 子结构**可加新字段，**UsageRecord 这 3 个字段绝不动**） |
| B6 | `shared/platform/metrics/credit_lease.go` | 5 个 credit-lease metric（`credit_lease_xadd_fanout_total` 等） |

### C. Redis namespace 不可冲突

| 编号 | namespace | 所有者 / 角色 |
|---|---|---|
| C1 | `credit:balance_cache:<uid>` | Rust overflow gate 单方面 cache PG 余额，TTL 30s |
| C2 | `credit:overflow_consumed:<uid>` | Rust overflow gate 单方面写，**首次 SET 时 EXPIRE 30s 不续**（滚动窗口） |
| C3 | `gw:active_pods` | Rust SADD + Go SMEMBERS |
| C4 | `gw:active_pod:<pod_id>` | Rust 15s TTL heartbeat |
| C5 | `credit:coef_table:latest:entries` / `credit:coef_table:latest:meta` | Go 写、Rust 读 |
| C6 | `events:counter:pod:<pod_id>` | Go LeaseScheduler 推、Rust counter_handler 消费的 per-pod stream |
| C7 | `counter:user:<uid>:balance_micro` / `counter:user:<uid>:deducted_micro` | Go counter_relay 写 |

### D. 关键不变量（spec §4.2 + 用户拓扑）

D1. lease ≤ balance × ttl/period （Go push 前 clamp）
D2. overflow_consumed 滚动窗口（INCRBY 不续 TTL，首次 SET 时 EXPIRE 30s）
D3. balance 唯一权威 = PG（Redis cache 是性能优化）
D4. Redis + PG 双断 → 503，重试 5 次后放弃
D5. balance ≤ 0 → Go 停推 lease
D6. lease 成功 → bypass step 9-10
D7. overflow.check Allow → bypass step 9-10
D8. protected 用户绝不进 frozen_micro 软防路径
D9. INCRBY `credit:overflow_consumed` 失败 → 必须 503（fail-closed）
D10. `GET credit:overflow_consumed` 失败 → 视为 0 + WARN（fail-open，§3.3 D5）
D11. `record_actual` 只追加超额（asymmetric）是设计

### E. metric label allowlist

E1. `gw_credit_overflow_gate_total{result=pass|balance_insuf|cache_unavail|consumed_read_failed_open}`
E2. `gw_credit_lease_acquire_total{result=success|expired|insufficient|nolease|no_coefs}`
E3. `gw_pg_fallback_total{op=balance_read}`
E4. `gw_credit_lease_estimate_credits` (histogram)
E5. `gw_credit_overflow_latency_ms{path=cache|pg_fallback|error}`
E6. `gw_credit_overflow_consumed_window` (gauge)
E7. `gw_coef_table_active_version` (gauge)
E8. `gw_heartbeat_sadd_total{result}`

E9 已存在 alert（**新 alert 不得重名**）：`CreditLeasePodCountUnexpected`、`CreditLeaseFanoutFail`、`CoefTablePublishFail`、`CoefTableVersionSkew`、`OverflowCacheUnavailable`、`OverflowGateHotspot`、`OverflowPgFallbackHigh`。

### F. 已知"看似 bug 实际设计"陷阱（subagent 容易误判）

F1. `LeaseGuard::settle_actual` 看似非幂等是设计（唯一调用方 logging.rs:160 单 fire；**不要**在 release 路径加退款）
F2. `record_actual_delta` 只在 actual > reserved 时返 Some 是设计（滚动窗口归零兜底）
F3. `OVERFLOW_INCR_LUA` 首次 SET 才 EXPIRE 是设计（不续 TTL）
F4. lease 成功 / overflow Allow → bypass step 9-10 是设计（不是 fail-open 回归）
F5. `consumed_for` fail-open + `incr_consumed` fail-closed 不对称是设计（balance 是唯一权威）

---

## §10 sub-plan 修订表（必读 + 严格遵守）

下表每行对应 §9 中触碰硬防的 task，按"撤销 / 限定 / 强化"分类。**未列在表中的 task 不受本节影响**。

### §10.1 撤销并重写（4 项 — 原方案会破坏硬防）

| WS-task | 原方案概述 | 为什么会破坏硬防 | 重写后的方案 |
|---|---|---|---|
| **WS-1 T16** | "lease.rs 加 `settled: AtomicBool` + `release` 时退款 + logging.rs 错误路径" | `release` 退款会和 `settle_actual` 在 logging filter 内**双退款**（settle_actual 已退一次，Drop 触发 release 再退一次）。F1 明确 `Drop`/`release` **只**做 idempotent flip，不退款。 | **降级为 P3 防御性可选改**：仅加 `settled: AtomicBool` 字段，让 `settle_actual` 内部 CAS settled false→true 才执行 refund；**不动** `release` / `Drop` 退款语义。可推迟到 launch 后做。详见 §11 教学矩阵 M1。 |
| **WS-5 S5.17** | "counter_handler 加 `DispatchOutcome::Malformed` 变体 + stream_consumer 对 Malformed **不** XACK，留 PEL" | events:counter:pod:<X> 流（C6）的 PEL 卡住会**阻塞所有 protected_lease push**，硬防失效。新用户、突发用户都被 NoLease + overflow 兜底，余额 cache 还热的话还顶得住，否则 lease push backlog 一直追不上 → 大量 503。 | **限定 Malformed 适用范围**：仅 JSON parse 失败 / schema 字段类型错（真的没法分流）才留 PEL；**lease push InvalidFields（credits<=0 / expires==0 / lease_id 空）必须仍 XACK + WARN**（drop，等下一次 LeaseScheduler tick 重推）。详见 §11 教学矩阵 M2。 |
| **WS-6 T03** | "reset_response_attempt_state 加 17 字段 reset，含 `credits_consumed`, `coef_table_version_at_settle`" | `credits_consumed` 和 `coef_table_version_at_settle` 是 **lease 状态**，跨 retry 必须保留（lease 在 step 8.5 try_acquire 时就锁定了，retry 不该清）。reset 后 logging.rs:151 `actual_usage_credits(coef_version=0, ...)` 必 Err，settle 跳过，**reserved credits 永久泄漏**（直到 LeaseGuard Drop 跑 release，credits 才回到 LEASE_POOL —— 但 INCRBY credit:overflow_consumed 那笔不退）。 | **白名单 reset**：只清 response-only 字段（HTTP status、body buffer、archive_*_response_meta 等）；**禁止**清 `lease_guard`、`freeze_guard`、`credits_consumed`、`coef_table_version_at_settle`、`merged_perm`。详见 §11 教学矩阵 M3。 |
| **WS-6 T05** | "新 `spawn_pool` 替换 gw-proxy 内 ALL `tokio::spawn`，含 lease release / freeze release" | LeaseGuard Drop → release 是 idempotent flip + 无副作用；改成 pool spawn 会引入异步调度延迟，破坏"Drop 后 release 立即完成"的同步假设。更危险：若 pool 满 / 关闭，release 永远不跑 → LEASE_POOL credits 永久泄漏。 | **限定 spawn_pool 范围**：仅替换 billing emit / counter XADD / archive flush 等纯异步路径；**禁止**接管 `LeaseGuard::release` / `RequestFreeze::release` / `Drop` 路径（保留 bare `tokio::spawn` 或直接同步 release）。详见 §11 教学矩阵 M4。 |

### §10.2 限定保留 + 教学矩阵（9 项 — task 本身合理但 subagent 容易顺手破坏硬防）

| WS-task | 原方案 | 必须遵守的限定 | 教学矩阵 ID |
|---|---|---|---|
| WS-1 T19 | task-runtime `SETTLE_LUA` 加幂等 + 外层 XACK | 仅触碰 `crates/gw-task/src/scripts.rs::SETTLE_LUA`；**禁止**改 `crates/gw-ledger/src/lease.rs::settle_actual`（A1） | M5 |
| WS-1 T15 | wallet_counter_outbox 加 commit_timestamp + counter_relay TTL 0→24h | 仅触碰 `counter:user:<uid>:balance_micro/deducted_micro`（C7）；**禁止**触 `credit:balance_cache:<uid>` / `credit:overflow_consumed:<uid>`（C1/C2） | M6 |
| WS-2 W2-T13 | task-runtime SETTLE_LUA + XAUTOCLAIM | 仅触碰 gw-task SETTLE_LUA；**禁止**改 ledger settle 路径（A1/A9） | M5（同 T19） |
| WS-6 T07 | 删 4× `block_in_place` + emit 移入 logging trait | 移入后**必须保证** `settle_actual` + `record_actual` 仍在 `emit_response_surfaces` **之前**完成；**禁止**改它俩相对顺序（F1/F2/A9） | M7 |
| WS-6 T08 | 502 GW_UPSTREAM_PROTOCOL_DECODE_ERROR | parse 失败时若有 LeaseGuard 必须 release（idempotent，不退款）；若 lease 已失败 + overflow Allow，则 record_actual 用 actual=0（让 record_actual_delta 返 None，不写 overflow_consumed） | M8 |
| WS-7 T5 | RequestFreeze.freeze_amount u64→i64 + try_from | 仅触 freeze.rs；**禁止**动 `lease.rs::LeaseGuard.reserved: i64`、`overflow.rs::OverflowConfig.retry`（D8 protected 用户不进 frozen_micro） | M9 |
| WS-8 T02 | `shared/rustpact/usage.go` Usage 加 8 字段 + `_pv` | 仅给嵌套 Usage 加新字段；**禁止**改 `UsageRecord.pod_id` / `credits_consumed` / `coef_table_version`（B5） | M10 |
| WS-8 T09 | `shared/contracts/pricing.go` ChargeInput/CostBreakdown 扩字段 | 仅扩 ChargeInput/CostBreakdown；**禁止**改 `CoefTableLatestEntriesKey` / `CoefTableLatestMetaKey`（B3） | M11 |
| WS-10 T07 | 删 `gw_billing_emit_lag_t0_total` 的 `request_id` label | 仅改 inflight_reaper.rs 的 metric 字面量调用；**禁止**改 `crates/gw-ledger/src/metrics.rs`（D2 修复在 §3.1） | M12 |
| WS-10 T08 / WS-12 T-08 | metric label banned 集合 lint | banned 集合 = `{request_id, api_key_id, user_id, ip, email, token, session_id, jti, refresh_token, access_token, hash}`；**禁止**加 `result` / `op` / `path` / `attempt_index`（E1-E8 用到这些） | M13 |
| WS-12 T-05 | `check-money-as-cast.sh` lint 扫 gw-ledger / gw-billing | lint 只读；**禁止**让 lint 自动改 A1-A11 文件（包括 lease.rs / overflow.rs / counter_handler.rs 的 `as i64`） | M14 |
| WS-15 T15.3 | Rust `[flags]` 配置块加 `gw-gateway/src/config.rs` | 仅追加 `[flags]` block；**禁止**改 `[protection]` block、`crates/gw-ledger/src/protection_config.rs`（A6） | M15 |

### §10.3 强化保留（5 项 — chaos test 必须显式断言硬防不变量）

| WS-task | 强化点 |
|---|---|
| WS-13 T05 (Redis sentinel failover / cluster CROSSSLOT chaos) | **必须**在 invariant assertion 中包含：`OverflowGate.check` 在 sentinel 切换后 5 秒内恢复 Allow / BalanceInsufficient 决策（不卡 cache_unavail > 5s）；INCRBY consumed 失败必须 503（D9）；GET consumed 失败必须视为 0 + WARN（D10） |
| WS-13 T17 (pg-terminate-mid-tx-deduct chaos) | **必须**断言：PG 闪断后 `OverflowGate.balance_for` retry 5 次内恢复（D4）；wallets 表 balance 在 chaos 后与 settler 累计 cost 守恒（D3） |
| WS-13 T18 (redis-sentinel-failover-during-lease chaos) | **必须**断言：LeaseScheduler 在 sentinel 切换后 1 tick (1s) 内恢复推 lease；活跃 pod 在 15s 内被 SMEMBERS 看到；旧 lease 不被错误延长（D1） |
| WS-13 T26 (concurrent-deduct + pg-blip chaos) | **必须**断言：lease push 在 pg-blip 期间 skip（D5），balance 恢复后立即继续推；idempotency_keys 表无重复 |
| WS-13 T27 (license-warn-to-enforce-flip chaos) | **必须**断言：硬防 `[protection].enabled` 在 license flag 翻转过程中**不被影响**（license_enforce 与 protection 独立） |

### §10.4 安全无触碰（默认状态，不列）

WS-4 全部、WS-9 全部、WS-11 全部、WS-14 大部分、WS-16 大部分、WS-2/WS-3 的非硬防 task、WS-5 的非 S5.17 task —— 这些 task 与硬防无直接关联，按原 sub-plan 执行，但仍应阅读 §9 护栏清单作为通识。

---

## §11 教学矩阵（subagent 在执行任何 §10 列出的 task 前必读对应矩阵）

每个矩阵的格式：**是什么 / 不是什么 / 做什么 / 不做什么 / 为什么**。

### M1 — WS-1 T16 LeaseGuard 防御性 settled（降级 P3）

**是什么**：给 `LeaseGuard` 加 `settled: AtomicBool` 字段，让 `settle_actual` 内部 CAS settled false→true 才执行 refund 计算。

**不是什么**：
- **不是**给 `release` 加退款行为。
- **不是**改 `Drop` 语义。
- **不是**修复一个真实存在的 bug（settle_actual 唯一调用方是 logging.rs:160 单 fire，现实风险≈0）。

**做什么**：
- 在 `LeaseGuard` struct 加一个字段：`settled: AtomicBool`（默认 false）。
- 修改 `settle_actual`：开头 CAS `settled.compare_exchange(false, true, AcqRel, Acquire)`，失败（已 settled）则 return 直接退出。
- 测试覆盖：连续两次调 `settle_actual(100)` 时第二次 noop。

**不做什么**：
- **不**改 `release()` 函数体（保持仅 `compare_exchange(false, true)` flip released）。
- **不**改 `Drop` impl（保持仅调 `release()`）。
- **不**改 logging.rs 调 `settle_actual` 的现有逻辑。
- **不**加任何 release 退款的 metric / counter。

**为什么**：
- F1：`Drop`/`release` 不退款是设计 —— logging filter 是 Pingora 流末 single fire，`settle_actual` 调用一次后即使 `Drop` 跑 `release` 也无副作用。
- 若 release 加退款，会与 settle_actual **双退**（settle_actual 退一次 → ctx 上的 guard Drop → release 再退一次 → credits 多回去），破坏 LEASE_POOL 总量守恒。
- 加 settled CAS 是纯防御性 —— 万一未来某重构让 settle_actual 被双调（如 retry），这个 guard 兜底。

---

### M2 — WS-5 S5.17 DispatchOutcome::Malformed 限定适用

**是什么**：在 `crates/gw-ledger/src/counter_handler.rs::DispatchOutcome` 加 `Malformed` 变体，对**真正非法的输入**（JSON parse 失败、schema 字段类型错）让 stream_consumer **不** XACK，留在 PEL 等 XAUTOCLAIM 收。

**不是什么**：
- **不是**对所有 InvalidFields 都留 PEL。
- **不是**改 lease push 的 InvalidFields 处理路径。
- **不是**改 events:counter:pod:<X>（C6）的消费组 / consumer 名。

**做什么**：
- `DispatchOutcome` 加 `Malformed { reason: &'static str }`。
- `handle_protected_lease` 中：
  - JSON unmarshal 失败 → 返 `Malformed`。
  - schema 字段类型不对 → 返 `Malformed`。
  - **lease 业务字段不合法**（`lease_credits <= 0`、`expires_at_ms == 0`、`lease_id` 空）→ 仍返 `InvalidFields`（保持现有行为，不改）。
- stream_consumer 收到 `Malformed` → log ERROR + metric `gw_counter_malformed_total{reason}` + 不 XACK。
- stream_consumer 收到 `InvalidFields` → log WARN + metric + **正常 XACK**（drop 等下一次推送）。

**不做什么**：
- **不**让 lease push 的 `InvalidFields` 留 PEL。
- **不**改 `apply_coef_commit` 现有行为（coef_table_commit Stale outcome 仍按原方式处理）。
- **不**改 `events:counter:pod:<X>` 的 stream 配置。
- **不**加 banned 的 label（`reason` 是有界枚举，OK；但不要把 task_id / request_id 放进来）。

**为什么**：
- Go LeaseScheduler 每秒 tick 一次 XADD events:counter:pod:<X> 推 lease。如果消费者把 InvalidFields 留 PEL，PEL 会膨胀 → 消费者 lag → 新 lease push 无法被及时消费 → 用户 LEASE_POOL 状态过期 → lease miss → overflow gate 兜底 → 一旦 cache 也冷就 503。
- 真正的 Malformed（schema 错）罕见，留 PEL 可让运维介入修复。但 lease InvalidFields 是高频可恢复事件（Go 推 lease 偶尔字段空），XACK + WARN 即可。

---

### M3 — WS-6 T03 ReqCtx reset 白名单

**是什么**：明确 `reset_response_attempt_state` 只清"本次响应尝试"相关的字段，跨 retry 保留 lease/freeze/perm 状态。

**不是什么**：
- **不是**加 17 字段全清。
- **不是**清 lease 相关字段。

**做什么**：
- 清下列 response-only 字段：HTTP status code、response body buffer、archive_upstream_response_meta、archive_downstream_response_meta、stream probe state、upstream error 信息、SSE leftover、retry counter (incr)。
- 在 ctx.rs 的字段定义旁加注释，标记每个字段的"reset on retry / preserve on retry"分类。
- 新加 CI lint `scripts/check-reqctx-reset.sh` 校验：分类为 preserve 的字段不得出现在 `reset_response_attempt_state` 函数内。

**不做什么**：
- **绝对不**清 `lease_guard`（A1）—— LEASE_POOL 已经 CAS 锁定了 credits，retry 必须复用。
- **绝对不**清 `freeze_guard`（非 protected 用户的 RequestFreeze）—— 同理。
- **绝对不**清 `credits_consumed` / `coef_table_version_at_settle`（A9 read）—— 这俩是 settle 路径读的，retry 不该清。
- **绝对不**清 `merged_perm`（权限合并结果，retry 复用）。

**为什么**：
- A8 lease/overflow 路径在 step 8.5 已经为整个请求生命周期锁定了 credits。retry 是 response 层的，不重新走 step 8.5。
- A9 settle 路径要读 `coef_version` 才能 actual_usage_credits 算出 actual credits。如果 retry 清了 coef_version，settle 必 Err → reserved credits 卡在 LEASE_POOL 直到 Drop → 期间 LEASE_POOL credits 不够供应同用户的其它请求 → lease miss → overflow → 余额 cache 撑不住就 503。

---

### M4 — WS-6 T05 spawn_pool 排除 lease/freeze release

**是什么**：新建 `crates/gw-proxy/src/spawn_pool.rs`，用于替换 billing emit / counter XADD / archive flush 路径的 `tokio::spawn`，提供有界并发 + metrics。

**不是什么**：
- **不是**接管 lease release / freeze release / Drop。
- **不是**替换所有 `tokio::spawn`。

**做什么**：
- 定义 `SpawnPool::spawn_billing`、`spawn_counter`、`spawn_archive` 三类。
- 替换：billing_lifecycle.rs:431、gateway.rs:1649 的 spawn 调用、archive flush spawn。
- 加 `gw_proxy_spawn_pool_inflight{pool=billing|counter|archive}` gauge。

**不做什么**：
- **不**替换 `LeaseGuard::release` 的任何调用路径（如果有 `release_async` 之类的辅助 spawn，保留 bare `tokio::spawn`）。
- **不**替换 `RequestFreeze::release` 的任何 spawn 路径。
- **不**让 `LeaseGuard::Drop` / `RequestFreeze::Drop` 走 pool（Drop 是同步路径，spawn pool 满会丢任务，credits 永久泄漏）。

**为什么**：
- A1: `LeaseGuard::release` 是 idempotent CAS flip，必须**同步**完成 —— 走 pool spawn 引入 async 调度，在高负载下 pool 满会丢 spawn，credits 卡在 LEASE_POOL 不归还。
- F1: `Drop` 必须立即 release —— `Drop` 是同步函数，pool spawn 返 future 但不等 await，Drop 跑完 future 没跑，release 永不执行。
- 这条规则的本质：**任何 RAII guard 的 Drop 路径绝不能 async-ify**。

---

### M5 — WS-1 T19 / WS-2 W2-T13 task-runtime SETTLE_LUA 限定

**是什么**：改 `crates/gw-task/src/scripts.rs::SETTLE_LUA`（task-runtime 子系统的脚本）加幂等 guard，让 task-worker 重试 settle 时不会双扣 deducted_micro / 双退 released_micro。

**不是什么**：
- **不是**改 `crates/gw-ledger/src/lease.rs::settle_actual`（A1）。
- **不是**改 logging.rs 的 settle 路径（A9）。
- **不是**改 LeaseGuard 的任何行为。

**做什么**：
- 在 `SETTLE_LUA` 顶部加 `HGET status` 检查：若 status 已经是 terminal（'settled' / 'cancelled' / 'expired'），return 'noop'。
- 调用方（exec_loop.rs）若收到 'noop'，metric `gw_task_settle_idempotent_skip_total` + 正常 XACK。
- 不改原 SETTLE_LUA 的字段操作顺序。

**不做什么**：
- **不**碰 `crates/gw-ledger/src/lease.rs`（lease 路径独立）。
- **不**碰 `crates/gw-proxy/src/filters/logging.rs:160` 那段 `guard.settle_actual + overflow_gate.record_actual`（这是 LLM 请求路径，跟 task-runtime 完全不同）。
- **不**改 `counter:user:<uid>:deducted_micro` 的写入语义（仍由 SETTLE_LUA 内 INCRBY）。

**为什么**：
- task-runtime 和 LLM gateway 是两个独立子系统：task-runtime 处理异步任务（task scheduler / worker），有自己的 lease/freeze 机制；硬模式 credit-lease 只在 LLM 请求路径生效。
- 一个名字相近不代表是同一段代码。`gw-task/src/scripts.rs::SETTLE_LUA` 和 `gw-ledger/src/lease.rs::settle_actual` 是两条独立 settle 路径。

---

### M6 — WS-1 T15 wallet_counter_outbox 限定

**是什么**：给 `wallet_counter_outbox` 加 `commit_timestamp`，让 counter_relay 按 commit 时序写 `counter:user:<uid>:balance_micro/deducted_micro`（C7），并将 TTL 从 0（永久）改为 24h。

**不是什么**：
- **不是**改 Rust 端 overflow gate 的 balance 来源（A2 `balance_for` 走 `credit:balance_cache:<uid>` + PG fallback，**不读** C7）。
- **不是**改 Go LeaseScheduler 的 balance 来源（B1 直接 SELECT FROM wallets）。

**做什么**：
- 新增 PG migration（WS-14 T05 已编 00071）。
- 改 counter_relay.go:135 加 commit_timestamp 排序。
- TTL `cr.rdb.Set(ctx, key, row.BalanceMicro, 0)` → `Set(ctx, key, row.BalanceMicro, 24*time.Hour)`。
- counter_relay 24h 内必须重写所有活跃用户的 counter（24h TTL 不应过期到没写）。

**不做什么**：
- **不**改 `credit:balance_cache:<uid>`（C1）的任何属性。
- **不**改 `credit:overflow_consumed:<uid>`（C2）的任何属性。
- **不**改 Rust overflow.rs:142-180 `balance_for`（仍走 cache + PG fallback）。
- **不**改 LeaseScheduler.go 的 balance 读取方式。

**为什么**：
- 硬模式不依赖 C7。`counter:user:<uid>:balance_micro` 是 Go 内部 cache（给 Go 自己用）；Rust 硬防只看 `credit:balance_cache:<uid>` + PG。
- D3: balance 唯一权威 = PG。Redis cache 都是性能优化，TTL 改 0→24h 不影响权威性。

---

### M7 — WS-6 T07 删 block_in_place 但保 settle 顺序

**是什么**：在 `gateway.rs::response_body_filter` 删除 4 处 `block_in_place`，把 `emit_response_surfaces` / `classify_final_upstream_error` / error-passthrough re-render 移入 Pingora `logging` trait method。

**不是什么**：
- **不是**改 `settle_actual` 和 `record_actual` 的调用顺序。
- **不是**改 logging filter 中已有的 settle 路径。

**做什么**：
- 把 emit_response_surfaces 移入 `async fn logging` 实现。
- 保持原有调用顺序：先 `guard.settle_actual(actual)` → 然后 `overflow_gate.record_actual(uid, reserved, actual)` → 然后 emit_response_surfaces / 其它收尾。
- 在 PR 描述里附 `before` / `after` 调用顺序对照表。

**不做什么**：
- **不**把 `settle_actual` 移到 `record_actual` 之后。
- **不**把 `record_actual` 调用前后插任何能 await/yield 长时间的代码。
- **不**改 `record_actual` 失败时仅 warn 的行为（A9: 不升级到 ERROR 或 bail）。

**为什么**：
- A9: settle_actual 先做 lease 内存退款 → record_actual 才把超额渗漏进 overflow 滚动窗口。倒过来会导致：先 INCRBY overflow_consumed → 又退 lease credits → 同一笔超额计了两次（一次 overflow，一次 lease）。
- F1: settle_actual 看似非幂等的设计基于"调一次"假设，挪窝时绝不能让它被双调用。

---

### M8 — WS-6 T08 502 parse failure + lease release

**是什么**：parse failure 时返 502 GW_UPSTREAM_PROTOCOL_DECODE_ERROR + JSON body；同时保证 LeaseGuard / RequestFreeze 正确收尾。

**不是什么**：
- **不是**让 protected 用户进 frozen_micro 软防（D8）。
- **不是**改 fail_to_proxy 的 lease 收尾逻辑。

**做什么**：
- 在 fail_to_proxy 加 502 + JSON body 渲染。
- 若 ctx.lease_guard 存在：`guard.release()`（idempotent flip，不退款）。Drop trait 也会兜底，写在这里是显式表达意图。
- 若 ctx.freeze_guard 存在：`guard.release()` 同理。
- 若 lease 失败 + overflow Allow 路径（ctx 中无 guard，但 overflow_consumed 已 INCRBY），调 `overflow_gate.record_actual(uid, reserved=overflow_estimate, actual=0)` → `record_actual_delta(overflow_estimate, 0) = None`（不变），即不退还（按 D2 滚动窗口自然归零）。

**不做什么**：
- **不**在 parse failure 时退还 overflow_consumed 已 INCRBY 的部分。
- **不**让 LeaseGuard 走 Drop 之外的额外 release 路径。
- **不**改 record_actual_delta 公式。

**为什么**：
- D11: record_actual asymmetric 是设计 — 滚动窗口 30s 归零，不需要请求级退还。
- F1: LeaseGuard release 仅 flip，credits 不退（settle_actual 已退或可能少退；release 是 noop）。让 release 退款会破坏 LEASE_POOL 总量守恒。

---

### M9 — WS-7 T5 RequestFreeze i64 不动 lease/overflow

**是什么**：`crates/gw-ledger/src/freeze.rs::RequestFreeze.freeze_amount` 从 `u64` 改为 `i64`，构造函数加 `i64::try_from(u64)` 边界检查。

**不是什么**：
- **不是**统一所有 micro 类型为 i64。
- **不是**改 `LeaseGuard::reserved: i64` 字段（已经是 i64）。
- **不是**改 overflow.rs 的任何数值类型。

**做什么**：
- freeze.rs `RequestFreeze.freeze_amount: u64 → i64`。
- `RequestFreeze::new(uid, amount: u64)` 加 `i64::try_from(amount).map_err(|_| FreezeError::Overflow)`。
- 同步改 `TaskFreeze` 同字段。
- 改所有 caller 调用点。

**不做什么**：
- **不**改 `crates/gw-ledger/src/lease.rs::LeaseGuard.reserved: i64`（已 i64）。
- **不**改 `crates/gw-ledger/src/overflow.rs` 的 `i64` 类型。
- **不**改 `frozen_micro: AtomicI64` 计数器的语义（D8 protected 用户不进，但这条计数器仍服务于非 protected 用户）。

**为什么**：
- D8: protected 用户走 lease/overflow，非 protected 用户走 frozen_micro 软防 —— 两条路径都要正常工作。改 freeze.rs 是给非 protected 用户加防御，但不能波及 protected 路径的类型。
- A1/A2 已是 i64，再"统一"反而引入不必要的迁移代价。

---

### M10 — WS-8 T02 UsageRecord 字段不动

**是什么**：在 `shared/rustpact/usage.go::Usage` 子结构加 8 个 `*int64` token 字段 + 顶层 `Usage struct` 上加 `PV int32 json:"_pv,omitempty"`。

**不是什么**：
- **不是**改 `UsageRecord` struct 本身（B5）。
- **不是**改 `UsageRecord.pod_id` / `credits_consumed` / `coef_table_version` 三个硬模式专用字段。

**做什么**：
- 给嵌套的 `Usage` struct 加 8 个 `*int64` 字段：`audio_input_tokens`、`audio_output_tokens`、`accepted_prediction_tokens`、`rejected_prediction_tokens`、`cache_creation_5m_tokens`、`cache_creation_1h_tokens`、`server_tool_web_search_count`、`server_tool_web_fetch_count`。
- 加 `PV int32` + 顶层 `MaxSupportedPV = 2` 常量。
- 同步 Rust 端 `crates/gw-core/src/usage.rs::UsageSnapshot` 加 8 字段 + `pv: u32 default 1, CURRENT_PV = 2`。
- 写 4 个新测试验证序列化兼容。

**不做什么**：
- **不**改 `UsageRecord` 的 `pod_id string` 字段。
- **不**改 `UsageRecord` 的 `credits_consumed int64` 字段。
- **不**改 `UsageRecord` 的 `coef_table_version int64` 字段。
- **不**改 JSON tag 中 `omitempty` 的现有标记（特别是 protected 模式字段：`,omitempty` 保持，让非 protected 模式不序列化）。

**为什么**：
- B5: 这三个字段是 Rust→Go 跨进程契约的硬模式专用字段，是 Go billing settler 算 cost 时识别"硬模式 protected 请求"的唯一线索。改它们就破坏 Go settler 的 routing 逻辑。

---

### M11 — WS-8 T09 model pricing 不动 coef contract

**是什么**：扩展 `shared/contracts/pricing.go::ChargeInput` 和 `CostBreakdown` 加 8 个新计费维度（与 M10 的 8 token 字段对应），并添加 `ErrPricingMissingDimension` 错误。

**不是什么**：
- **不是**改 `CoefTableLatestEntriesKey` / `CoefTableLatestMetaKey` 常量（B3）。
- **不是**改 Rust 端 coef_table_version 的语义。

**做什么**：
- ChargeInput 增 8 `int64` 字段。
- CostBreakdown 增 8 `*int64` 字段。
- 新增 `ErrPricingMissingDimension`。
- 同步改 Go settler 在缺失维度时返该 err 进 DLQ。

**不做什么**：
- **不**改 `CoefTableLatestEntriesKey = "credit:coef_table:latest:entries"`。
- **不**改 `CoefTableLatestMetaKey = "credit:coef_table:latest:meta"`。
- **不**改 Rust 端 `crates/gw-ledger/src/coef.rs` 或 `coef_snapshot.rs`（A4）。

**为什么**：
- B3 常量是 Go ↔ Rust 跨进程的 coef 表 Redis namespace 约定，硬模式 lease 全靠这个传 coef snapshot。改名 = 硬模式破。

---

### M12 — WS-10 T07 仅改 metric 字面量，不动 metrics.rs

**是什么**：删除 `crates/gw-ledger/src/inflight_reaper.rs:119-124` 的 `gw_billing_emit_lag_t0_total` metric 调用中的 `request_id` label。

**不是什么**：
- **不是**改 `crates/gw-ledger/src/metrics.rs`（D2 修复属于本 corrections §3.1）。
- **不是**改其它 credit-lease metric（A11）。

**做什么**：
- 编辑 inflight_reaper.rs 删 label。
- 测试改 inflight_reaper_test 移除 label assertion。
- 在 PR 描述里指出"metrics.rs 8 个常量值的 `gw_` 前缀修复在 R1 task，本 task 不动 metrics.rs"。

**不做什么**：
- **不**改 metrics.rs 中任何 `pub const`。
- **不**新加字面量 metric 名（要新加应走 metrics.rs 常量）。

**为什么**：
- §3.1 D2 修复要统一收口在 metrics.rs，分散改字面量会冲突。

---

### M13 — WS-10 T08 / WS-12 T-08 metric label lint allowlist

**是什么**：新加 `scripts/check-metric-labels.sh` 扫所有 `register_*_vec!` / `NewCounterVec` 等声明，banned 集合命中即 fail。

**不是什么**：
- **不是**把 `result` / `op` / `path` 加进 banned。

**做什么**：
- banned 集合 = `{request_id, api_key_id, user_id, ip, ip_address, email, token, session_id, jti, refresh_token, access_token, hash}`（高基数 / PII）。
- 扫描覆盖 Go + Rust。
- lint 失败时打印命中 file:line + label 名。

**不做什么**：
- **不**加 `result` 进 banned（E1/E2/E8 用）。
- **不**加 `op` 进 banned（E3 用）。
- **不**加 `path` 进 banned（E5 用）。
- **不**加 `attempt_index` / `model_id` 进 banned（有界枚举，合理用）。

**为什么**：
- E1-E8 都是硬模式必需的有界枚举 label，加进 banned 会让 lint 阻挡硬防自身。

---

### M14 — WS-12 T-05 money-as-cast lint 只读

**是什么**：新加 `scripts/check-money-as-cast.sh` 扫 `crates/gw-billing/`、`crates/gw-ledger/`、`crates/gw-store/` 中的 `as i64` / `as u64` / `as f64`，匹配钱相关的标识符模式（micro / credits / balance）。

**不是什么**：
- **不是**自动修改源码。
- **不是**重构 A1-A11 文件。

**做什么**：
- 用 grep 找 `\bas (i64|u64|f64)\b` 同行附近含 `micro|credits|balance|cost` 关键字的行。
- lint 失败时打印 file:line + 命中行。
- 允许 allowlist（如 `crates/gw-ledger/src/lease.rs:175` 若 metrics histogram 的 `as f64` 是已知必要）。

**不做什么**：
- **不**自动 patch 任何文件。
- **不**让 lint 把 `lease.rs` / `overflow.rs` / `counter_handler.rs` 的合理 cast 误报为 fail（用 allowlist 处理）。

**为什么**：
- 这些文件是硬防核心，任何"自动修复"都可能改 semantics。lint 只报告，人工 review 决定。

---

### M15 — WS-15 T15.3 配置文件加 [flags] 不动 [protection]

**是什么**：在 `crates/gw-gateway/src/config.rs` 加 `[flags]` block 解析（FlagsCfg struct + LicenseEnforceMode enum），让 license_enforce / jwt_strict / payment_webhook_5xx / r7_502_on_parse_fail 可灰度。

**不是什么**：
- **不是**改 `[protection]` block。
- **不是**改 `crates/gw-ledger/src/protection_config.rs`（A6）。

**做什么**：
- config.rs 加 `pub flags: FlagsCfg` 字段、`FlagsCfg` struct、`LicenseEnforceMode` enum。
- config/default.toml 加 `[flags]` block。
- config.validate() 加空 block。
- 测试模块 flags_cfg_tests。

**不做什么**：
- **不**改 `[protection]` block 内任何字段（`enabled`、`lease_grace_ms`、`default_output_ceiling_tokens`）。
- **不**改 `crates/gw-ledger/src/protection_config.rs`。
- **不**加 `flags.credit_lease_enable` / `flags.overflow_gate_enable` 这种 flag（硬防不灰度，永远开）。

**为什么**：
- I1: `[protection].enabled` 在 mixed-version 部署时 flag-off 等于硬防整个失效，**禁止**灰度。
- 硬防是 emergency launch 的核心机制，不要给它加 kill-switch。

---

## §12 给执行 agent 的最终自检清单

任何 sub-plan task 在交付 PR 之前，**必须**确认以下 4 项：

1. **§9 护栏对照**：HOW 目标文件是否命中 A/B/C/D/E/F 任一项？命中则必须有对应 §10/§11 教学矩阵的处理。
2. **§10 修订表**：本 task 是否在表中？若在"撤销并重写"行 — 不要执行原方案；若在"限定保留" — 严格遵守"不做什么"清单。
3. **§11 教学矩阵**：本 task 对应的矩阵的"为什么"我能完整复述吗？不能则**回头读 spec**。
4. **PR 描述附录**：列出本 task 触碰的硬防元素编号 + 已遵守的教学矩阵 ID。如"本 PR 触碰 A9 (settle 路径) + F1 (settle_actual 设计陷阱)，已遵守 M7 教学矩阵：保持 settle_actual → record_actual 顺序不变。"

任何一项不通过，PR 不应合并。

---

**End of v2.1 corrections.**
