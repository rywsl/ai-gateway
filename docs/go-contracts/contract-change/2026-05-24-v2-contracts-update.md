# Contract Change: v2 Cleanup — S-contracts Landing

> **Status**: 契约变更记录(active)
> **日期**: 2026-05-24
> **触发 spec**: [`docs/architecture.md`](../../architecture.md) — S-contracts(Go 侧文档收口)
> **范围**: Documentation-only(零 Rust 代码变更)
>
> **本文件作用**: 索引 S-contracts sub-spec 引入的 3 项 Go 侧契约变更。每条改动标明 **触发来源 + parts 落点 + 受影响 Go 模块**。
>
> **何时同步 parts/**: 本 changelog 可独立合并;`parts/02`、`parts/03` 的实际正文修改 **与 S-contracts spec 实施 PR 同 PR 落地**(对齐 [main.md §7 Sync rule](../main.md))。

---

## 0. 变更索引

| # | 变更类型 | 来源 | parts 落点 | 受影响 Go 模块 |
|---|----------|------|------------|----------------|
| 1 | DLQ Replay 完整契约 | S-contracts G-11(Go 半) | parts/02-go-as-consumer.md §2.2 | billing-drainer, DLQ consumer |
| 2 | Ghost-Freeze 对账算法 | S-contracts C-γ #7 | parts/02-go-as-consumer.md §2.5.5 | billing-reconciler, health-monitor |
| 3 | `exclusive_capability_match` 索引启用 + JSONB shape 契约 | S-contracts `exclusive_capability_match` | parts/03-shared-schema.md §3.2.5.2 | entity-manager, vendor-key-service |

下文给出每条改动在对应 `parts` 文件中的落点摘要。**详细规范**见 S-contracts spec 对应章节。

---

## 1. Go DLQ Replay Specification

- **File**: `parts/02-go-as-consumer.md` §2.2
- **What**: Full DLQ replay contract — consumer group (`go-billing-dlq`), XREADGROUP pattern, retry backoff, startup recovery, idempotency, `dlq_layer` extension to `UsageRecord` schema
- **Affected Go modules**: billing-drainer, DLQ consumer
- **§2.11 #2**: Resolved(was: follow-up → now: specified)
- **Context**: Rust 侧 XADD 失败时投递到 `usage:records:dlq`(S-ledger F-1/G-11 Rust 半);Go 侧消费 DLQ 并 replay 回主 `usage:records` stream 的完整契约。包含:
  - DLQ consumer group 配置与消费语义
  - 重试退避策略(exponential backoff, max 3 attempts)
  - 启动恢复:从 `0-0` 全量消费,确保崩溃前未 XACK 的条目不丢
  - 幂等性:settler 对 `(request_id, vendor_key_id, ts_ms)` 去重
  - `dlq_layer` 字段:区分主路径与 DLQ replay 的 usage record

---

## 2. Ghost-Freeze Reconciliation Algorithm

- **File**: `parts/02-go-as-consumer.md` §2.5.5
- **What**: Periodic reconciliation of orphaned `frozen_micro` from crashed Rust replicas
- **Affected Go modules**: billing-reconciler, health-monitor
- **§2.11 #3**: Resolved(was: follow-up → now: specified)
- **Context**: Rust 进程崩溃时本地 `frozen_micro` 丢失,Go 侧需周期性扫描 `counter:user:*:frozen_micro` 与 `deducted_micro` 的差值,识别并修复"幽灵冻结"(frozen > 0 但对应的 Rust 实例已不存活)。包含:
  - 对账 cursor:基于 `frozen_since_ts_ms` 时间窗口扫描
  - 实例存活判定:Rust 实例心跳(Go 端 `/healthz/*` probe 或 Redis key TTL)
  - 修复动作:将孤儿 frozen 余额回收到 `base_micro`,写 `events:counter` 通知存活 Rust 实例
  - 对账频率与可配置参数

---

## 3. `exclusive_capability_match` Index Activation + JSONB Shape Contract

- **File**: `parts/03-shared-schema.md` §3.2.5.2
- **What**: Go-side partial index activation, dual-write invariant, `forced_capabilities` naming sync
- **Affected Go modules**: entity-manager, vendor-key-service
- **§3.9 #1**: Resolved(was: follow-up → now: specified)
- **Context**: `vendor_keys` 表的 `exclusive_capability_match` 字段已在 Rust 侧使用(S-routing N-1),但 Go 端尚未激活部分索引。此变更包含:
  - Go 端启用 `CREATE INDEX ... WHERE exclusive_capability_match = true` 部分索引
  - Dual-write 不变量:Go 写 `vendor_keys` 时必须设置 `exclusive_capability_match` 字段,与 Rust EntityStore 读取路径一致
  - `forced_capabilities` 命名同步:Go 端 JSONB 中使用 `forced_capabilities`(snake_case),与 Rust 侧 SmallVec 落地对齐
  - 索引迁移:在下一个 migration 中创建部分索引

---

## 4. 后续

本 changelog 记录 S-contracts sub-spec 落地时引入的 Go 侧契约变更。S-contracts 与 S-ledger 同时起草,Go 侧实施可在 Rust 第一波合入之后开始,不阻塞 Rust 进度(参 overview §3 依赖图)。
