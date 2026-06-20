# Hot Path Async Delivery Audited Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

## 顶层目标（What / Why / How / Not / Don't）

**是什么**
- 把响应主路径从"默认走异步桥接（`block_in_place` + `block_on`）"改成"默认在 Pingora 同步钩内完成的内存前缀归档"。
- 把请求收尾路径从"主 channel 满/关闭时同步 XADD"改成"先尝试非阻塞 overflow 入队，失败再走既有 DLQ / 应急盘 / `BillingBacklog` 级联"。
- 同时补齐 flusher panic 安全、DLQ 终极失败回灌 backlog、archive 截断保留前缀这三块原本会"静默吞数据"的窟窿。

**为什么**
- Pingora 0.8 的 `response_body_filter` 是同步钩；在响应高频路径上 `block_in_place`+`block_on` 会让 worker 线程被霸占，影响并发能力，而这只有在真的写盘归档时才必要。
- 计费记录 (`UsageRecord`) 是 must-deliver——任何"channel 满就丢"或"channel 关就走同步 Redis"都把毛刺时延打到请求收尾路径，违背"请求 hot path 永不等持久化"的工程原则。
- 现有 `sync_xadd` bypass 在主 channel 满或关闭时直接对 Redis 做同步 XADD，是请求 tail 上的高延迟尖刺源；同时它绕开了批处理，吞吐被放大很多倍的开销。

**怎么做（按 8 个 task 顺序）**
1. 配置：`[proxy.archive] storage = "memory" | "disk_spill"`，默认 `memory`。
2. Archive 运行时：补 `ArchiveStorage` 枚举、补"截断后仍保留 hard-cap 前缀"语义、补同步 append 入口。
3. Hot path：memory 模式直接走同步 archive，绕过 `block_in_place`；disk_spill 保留旧桥接。
4. Ledger 必到达基座：panic-safe flush 包装、DLQ 终极失败回灌 backlog。
5. 新建 `PrimaryDelivery` 非阻塞 overflow 句柄。
6. 起 overflow worker，重写 `emit` 的 Full/Closed 分支：先 try_enqueue 到 overflow，失败才进 DLQ。
7. 写确定性 must-deliver 测试，更新 metrics catalog，更新文档与 alert 措辞。
8. Gateway 接线、启动日志、端到端轻 check。

**不是什么 / 不要做**
- **不是**改 Pingora 钩签名或外部 API 协议。
- **不是**把 archive 改成丢弃模式——memory 模式仍保留 hard-cap 前缀。
- **不是**为"流转速度"牺牲 must-deliver；overflow 满→DLQ→应急盘→`BillingBacklog` 链路一条不能少。
- **不要**在 `Ledger::emit` 中 `send().await`（会阻塞请求收尾）。
- **不要**在 `proxy.archive.storage = "memory"` 时调 `block_in_place`。
- **不要**因主 channel 临时关闭就 page on-call——见 Audit Decision §A6 关于 `gw_billing_flusher_dead_total` 的口径修订。
- **不要**给 `LedgerItem` 加 serde（overflow 通道只走内存，不许写盘）。
- **不要**在 `Ledger::from_parts` 里创建后丢掉 `_overflow_rx`（必须接到真实 worker，或拿测试构造器返回出去）。

**Tech Stack:** Rust 1.88, Pingora 0.8, Tokio, fred Redis, sqlx, serde JSONL, existing `gw_runtime::SpillRoot`, existing `gw_runtime::Supervisor`, existing `gw_ledger::BillingBacklog`.

---

## Global Constraints

- 不改对外请求/响应协议行为。
- 不因主 mpsc 满或关闭而丢任何 `UsageRecord`。
- 不在 `Ledger::emit` 中等 overflow mpsc 容量；overflow 必须 `try_send`。
- 不接受 ledger flusher panic 导致批次丢失；`flush(...)` panic 时，必须把 owned backup 路由到 DLQ / 应急盘 / `BillingBacklog`。
- 不接受 DLQ 重试 worker 终极丢失；Redis-DLQ + 应急文件双重失败，必须把 envelope 推回 `BillingBacklog`，而不只是 `gateway_usage_records_lost_total` 加一。
- 维持 DEL 规则：只有在 `UsageRecord` 已确认进 `usage:records` 之后，才允许 DEL `billing:inflight:<instance>:<request_id>:<attempt>`。
- DLQ / 序列化失败 / 入队失败 / overflow 满 fallback / overflow 关 fallback —— **任何一条都不许 DEL inflight key**。
- `proxy.archive.storage = "memory"` 路径上不许出现 `block_in_place`。
- 默认响应归档路径不许新增 per-request 写盘 I/O。
- Archive 内存容量由 `hard_cap_bytes` 顶死；超出部分计入"已丢弃字节"。
- 已 `#[ignore]` 的 Docker 测试保持 ignore。

---

## Audit Decisions（含本轮修订）

- **A1. Overflow full/closed**：`Ledger::emit` 用 `PrimaryDelivery::try_enqueue`，**不**用 `send().await`。当 try_enqueue 返回 `Full` 或 `Closed` 时，调 `route_ledger_item_to_dlq(...)` 进入既有的"DLQ → 应急盘 → `BillingBacklog`"级联。本路径可能在极端情况下产生短暂磁盘写，但这是显式 must-deliver fallback，不会阻塞请求 tail。
- **A2. Overflow worker 句柄类型**：`Arc<Mutex<mpsc::Receiver<LedgerItem>>>`，保留 `Supervisor::spawn` 所需的 `Fn`（非 `FnOnce`）签名。该 Receiver 只允许 overflow worker 自身锁定，禁止外部观察线程也去 lock；外部观察靠 `gw_billing_primary_overflow_pending` gauge 取数。
- **A3. Overflow shutdown drain**：与 `drain_billing_channel` 等价的 deadline-recv 模式，**不**用 `try_recv` 一次性轮询。`None`、超时、最终退出三处都要 flush 残留批次。
- **A4. Flush panic safety**：调 `flush(...)` 前先 clone 一份 owned `Vec<LedgerItem>` 当 backup；如果 `flush(...)` panic，把 backup 路由到 DLQ/backlog。允许重复投递（下游已按 `(request_id, attempt_index)` + provenance 去重），但不允许静默丢失。**Wrapper 成功返回时也必须把 `taken` 中可能残留的项写回 `*buf`**（详见 Task 4 Step 3）。
- **A5. DLQ retry 终极失败**：`process_dlq_envelope(...)` 必须能拿到完整 `DlqFallback`。当 Redis DLQ 写失败 + 应急盘 append 也失败时，envelope 推到 `BillingBacklog`，**不**走 `gateway_usage_records_lost_total`。`lost` 计数器只用于真正的不可恢复场景。
- **A6. `gw_billing_flusher_dead_total` 口径**：旧版本"primary closed = page on-call"在 overflow 接住的新世界里会误告警。新口径：**仅在 primary closed 且 overflow 也 Closed**（即 must-deliver 链路第一层走到 DLQ fallback）时增 `gw_billing_flusher_dead_total`。primary closed 但 overflow 接住的情况，只增 `gw_billing_emit_path_total{path="primary_closed_overflow_ok"}` 与 `path="primary_closed"`（尝试侧）。告警规则跟着改，见 Task 7 Step 8。
- **A7. Path metric 含义**：`gw_billing_emit_path_total{path=...}` 是**尝试落点**计数，单条记录可能贡献多个 path（先 primary 尝试 + overflow 尝试 + DLQ 落点）。catalog `help` 必须明确写"counts attempts at each fallback layer; a single record may increment multiple paths"。这避免运营 dashboard 错把它当独立计数求和。
- **A8. Archive 截断**：memory 模式保留 `hard_cap_bytes` 前缀。`read_for_emit()` / `read_for_emit_sync()` 对截断后的 memory archive 返回**保留的前缀**，不是空 vec。
- **A9. `ArchiveState::Truncated` 字段语义变更**：从 `{ written: u64, dropped: u64 }` 改为 `{ retained: BytesMut, dropped: u64 }`。这是**跨进程契约**变更：
  - `len()` 对 Truncated 现在返回 `retained.len()`，不是历史"已处理总量"。所有读 `len()` 用作"已处理"语义的代码必须改为读 `len() + dropped()`。
  - `read_for_emit()` 对 Truncated 现在返回前缀字节，不是 `Vec::new()`。这改变了 `usage:detailed` 在截断场景下的负载形状——**Go drainer 端契约需同步更新**（见 Sync Rule 增补）。
- **A10. Error-passthrough 路径**：响应 error-passthrough 体内省必须走 `read_for_emit_sync()` 读保留前缀，不许保留手写的 `ArchiveState::Truncated { .. } => Vec::new()` 分支。
- **A11. Metrics catalog**：`MetricSpec` 没有 `labels` 字段。label 描述写进 `help`，更新既有 metric 而不是新加重复条目。
- **A12. 文档/alert 同步**：所有"full/closed 走同步 XADD"的措辞改为"async overflow first, then DLQ-backlog cascade"。Sync Rule 增补：本计划改了 `usage:detailed` truncated 负载形状，必须同步更新 `docs/go-contracts/`（具体 part 待 Task 7 Step 8 确认）。
- **A13. Pending gauge 语义**：`gw_billing_primary_overflow_pending` 是"已 enqueue 但未被 worker 排空"的近似值。worker 把项从 channel 取出放到 `buf` 后、flush 完成前的窗口里，pending 仍然计这些项；catalog `help` 必须写明"approximate; counts items enqueued but not yet committed via XADD"。
- **A14. Overflow worker dequeue-but-not-flushed loss**：在 `buf.push(item)` 与 `flush_batch_with_panic_fallback` 之间若 worker future 被 cancel 或 panic，`buf` 内的项会随 future drop。新设计：`buf` 不直接 push `LedgerItem`；改 push `Arc<LedgerItem>`，同时维护 sidecar `Vec<Arc<LedgerItem>>`；cancel/drain 分支负责把 sidecar 路由到 DLQ fallback。详见 Task 6 Step 3。
- **A15. Closed-primary 测试构造器**：测试需要"primary 永远满 / 永远关"且 overflow worker 实活。Task 6 Step 5 必须给出 `Ledger::for_test_with_overflow(...) -> (Ledger, ManuallyOwned<primary_rx>, JoinHandle)`：测试代码持有 primary_rx 不读，覆盖"Full"；或者 drop 它，覆盖"Closed"。

---

## Sync Rule 增补

按 `CLAUDE.md` 的 Sync Rule，本计划落地必须**同一 commit**更新：

| 改动 | 更新位置 |
|------|---------|
| `[proxy.archive] storage` 配置项 | `docs/architecture.md` §Configuration |
| Pingora response 同步路径 / archive 运行时枚举 | `docs/architecture.md` §Deliberate behaviours、`docs/workflow.md` §response_body_filter |
| `Ledger::emit` Full/Closed 行为、overflow worker 加入 worker 图、新 metric | `docs/architecture.md` §Background-task graph、`docs/spec/06-billing-ledger.md` |
| `usage:detailed` 截断负载形状变化 | `docs/go-contracts/main.md` + 对应 part（detailed archive 章节） |
| DLQ 终极失败回灌 backlog | `docs/spec/06-billing-ledger.md`、`deploy/prometheus/billing-rules.yml` |

---

## File Structure

- Modify `config/default.toml`
  - 增 `[proxy.archive] storage = "memory"`.

- Modify `crates/gw-gateway/src/config.rs`
  - 增 `ProxyArchiveStorage`.
  - Parse + validate `proxy.archive.storage`.
  - Validate `soft_cap_bytes <= hard_cap_bytes`.

- Modify `crates/gw-proxy/src/ctx.rs`
  - 增 `ArchiveStorage` 枚举.
  - 改 `Truncated` 字段为 `{ retained: BytesMut, dropped: u64 }`.
  - 增 `append_chunk_sync` / `read_for_emit_sync`.
  - 内存模式不许走 spill 分支（违反时 `debug_assert` 并转 Truncated）.

- Modify `crates/gw-proxy/src/filters/response.rs`
  - 抽出 CPU-only 同步核心，让 async `run` 与新 `run_memory_sync` 共用；如果抽取代价过大，留出 review 的待 follow-up 注释，但仍要给同步路径补全等价测试.
  - 增 `run_memory_sync(...)`、`run_cli_body_filter_sync(...)`.

- Modify `crates/gw-proxy/src/gateway.rs`
  - memory 模式提前到 `block_in_place` 之前.
  - `block_in_place` 只保留给 disk_spill 模式与 spilled error-passthrough 读取.
  - error-passthrough 改走 `read_for_emit*` 而非手写 Truncated→空.

- Modify `crates/gw-gateway/src/lib.rs`
  - 把 `ProxyArchiveStorage` 映射到 `gw_proxy::ctx::ArchiveStorage`.
  - 启动日志：archive 模式 + ledger worker 数（应为 5）.

- Create `crates/gw-ledger/src/primary_delivery.rs`
  - Bounded 非阻塞 overflow handle.
  - Pending gauge 簿记.
  - `try_enqueue(item) -> Result<(), TrySendError<LedgerItem>>`.

- Modify `crates/gw-ledger/src/flusher.rs`
  - 新增 panic-safe flush wrapper（成功也回填 `*buf`）.
  - DLQ retry 终极失败改推 `BillingBacklog`.
  - `Ledger` 加 `primary_overflow: PrimaryDelivery` 字段.
  - 新增 overflow worker（用 `flush_batch_with_panic_fallback`）.
  - 新增 `route_ledger_item_to_dlq(...)` / `route_ledger_items_to_dlq(...)`.
  - 在确定性 overflow 测试通过后，删 `sync_xadd` / `sync_del_inflight`.

- Modify `crates/gw-ledger/tests/flusher.rs`
  - 确定性 full-channel overflow 测试.
  - 断言目标 `request_id` 在 `usage:records` 中出现，**不**用 `xlen >= 1`.

- Modify `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`
  - 确定性 overflow + DEL 测试.
  - 保留真实"primary closed"运行时测试，不能只用静态源测试替换.

- Modify `crates/gw-gateway/src/metrics_catalog.rs`
  - 更新 `gw_billing_emit_path_total`、`gw_billing_inflight_clear_total`、`gw_billing_flusher_dead_total`.
  - 移除 `gw_billing_mpsc_bypass_total`（已无人发送）.
  - 增 `gw_billing_primary_overflow_pending`.
  - 增 `gw_billing_flusher_panic_total`.

- Modify `docs/architecture.md`, `docs/workflow.md`, `docs/spec/06-billing-ledger.md`, `docs/go-contracts/...`, `deploy/prometheus/billing-rules.yml`
  - 把"sync XADD 同步 bypass"措辞替换为"async overflow + DLQ-backlog 级联".

---

## Task 1: Archive Storage Config

**是什么**：在 `[proxy.archive]` 下新增 `storage = "memory" | "disk_spill"`，默认 `memory`，并校验 `soft_cap_bytes <= hard_cap_bytes`。

**为什么**：当前 archive 是单一隐式模式，要让响应 hot path 默认走"不写盘"，必须先在配置层有显式开关，operator 才有按需开 disk_spill 的退路。

**怎么做**：按 figment 既有模式加枚举 + 默认值 + 校验，三段红测试覆盖默认/显式/校验失败。

**不是什么**：不是改既有 `soft_cap_bytes` / `hard_cap_bytes` / `spill_dir` 字段含义；不是新增其他 archive 字段。

**不要做**：不要把 `storage` 改成运行时可热切换（boot-time only，与 `[tunnel]` 等同处理）。不要让 `disk_spill` 路径与 `memory` 路径在同一 request 内混用。

**Files:**
- Modify: `config/default.toml`
- Modify: `crates/gw-gateway/src/config.rs`
- Test: `crates/gw-gateway/src/config.rs`

**Interfaces:**
- Produces: `ProxyArchiveStorage::{Memory, DiskSpill}`.
- Produces: `ProxyArchiveConfig.storage: ProxyArchiveStorage`.

- [ ] **Step 1: Add failing config tests**

```rust
#[test]
fn proxy_archive_storage_defaults_to_memory() {
    let defaults = include_str!("../../../config/default.toml");
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .extract()
        .expect("default.toml parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });
    cfg.validate().expect("default.toml validates");
    cfg.validate_resolved_sizing().expect("sizing resolves");

    assert_eq!(cfg.proxy.archive.storage, ProxyArchiveStorage::Memory);
}

#[test]
fn proxy_archive_accepts_disk_spill_storage() {
    let defaults = include_str!("../../../config/default.toml");
    let overrides = r#"
        [proxy.archive]
        storage = "disk_spill"
        soft_cap_bytes = 1024
        hard_cap_bytes = 4096
    "#;
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .merge(Toml::string(overrides))
        .extract()
        .expect("config parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });
    cfg.validate().expect("disk_spill archive config validates");
    cfg.validate_resolved_sizing().expect("sizing resolves");

    assert_eq!(cfg.proxy.archive.storage, ProxyArchiveStorage::DiskSpill);
    assert_eq!(cfg.proxy.archive.soft_cap_bytes, 1024);
    assert_eq!(cfg.proxy.archive.hard_cap_bytes, 4096);
}

#[test]
fn proxy_archive_rejects_soft_cap_above_hard_cap() {
    let defaults = include_str!("../../../config/default.toml");
    let overrides = r#"
        [proxy.archive]
        soft_cap_bytes = 8192
        hard_cap_bytes = 4096
    "#;
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .merge(Toml::string(overrides))
        .extract()
        .expect("config parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });

    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("proxy.archive.soft_cap_bytes must be <= hard_cap_bytes"),
        "{err:?}"
    );
}
```

- [ ] **Step 2: Run failing test**

```bash
cargo test -p gw-gateway proxy_archive_
```

Expected：因 `ProxyArchiveStorage` 与 `storage` 不存在而编译失败。

- [ ] **Step 3: Implement config**

```rust
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyArchiveStorage {
    Memory,
    DiskSpill,
}

fn default_proxy_archive_storage() -> ProxyArchiveStorage {
    ProxyArchiveStorage::Memory
}
```

加 `storage` 到 `ProxyArchiveConfig` 与其 `Default`。

`Config::validate` 内增：

```rust
if self.proxy.archive.soft_cap_bytes > self.proxy.archive.hard_cap_bytes {
    anyhow::bail!(
        "invalid config: proxy.archive.soft_cap_bytes must be <= hard_cap_bytes"
    );
}
```

`config/default.toml`：

```toml
[proxy.archive]
storage = "memory"
soft_cap_bytes = 4194304
hard_cap_bytes = 8388608
spill_dir = "/data/cnexus/spill/archive"
```

- [ ] **Step 4: Verify**

```bash
cargo test -p gw-gateway proxy_archive_
```

Expected：PASS。

---

## Task 2: Memory Archive Prefix Truncation

**是什么**：在 `ArchiveState` 中增加 `ArchiveStorage` 枚举感知；把 `Truncated` 字段改为保留 `retained: BytesMut` 前缀；提供同步 append/read 入口。

**为什么**：响应 hot path 同步化需要同步 append；同时既有 `Truncated => Vec::new()` 会让 error-passthrough、detailed archive 在截断时损失现场——保留前缀是事故定位与计费旁路审计的底线。

**怎么做**：先红测三条（memory append 不写盘 / memory 截断后仍能读前缀 / disk_spill 在第一次 spill 前先看 hard_cap），再实现。

**不是什么**：不是改变 `usage:detailed` 的字段集；不是新增"半截断"中间态——只有 `InMemory | Spilled | Truncated` 三态。

**不要做**：不要在 memory 模式下转 `Spilled`（违反时 `debug_assert!` 并转 Truncated 而不是悄悄写盘）。不要在 `Truncated => Vec::new()` 路径上保留任何分支。

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Test: `crates/gw-proxy/tests/archive_state.rs`

**Interfaces:**
- Produces: `ArchiveStorage::{Memory, DiskSpill}`.
- Produces: `ArchiveCaps.storage`.
- Produces: `ArchiveState::append_chunk_sync(...)`.
- Produces: `ArchiveState::read_for_emit_sync(...)`.

- [ ] **Step 1: Add failing archive tests**

```rust
fn memory_caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps {
        storage: gw_proxy::ctx::ArchiveStorage::Memory,
        soft_cap_bytes: soft,
        hard_cap_bytes: hard,
    }
}

fn disk_caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps {
        storage: gw_proxy::ctx::ArchiveStorage::DiskSpill,
        soft_cap_bytes: soft,
        hard_cap_bytes: hard,
    }
}

#[test]
fn memory_archive_keeps_hard_cap_prefix_and_counts_dropped_bytes() {
    let mut a = ArchiveState::default();
    a.append_chunk_sync(&vec![1u8; 3000], &memory_caps(1024, 4096), "req", "upstream");
    a.append_chunk_sync(&vec![2u8; 3000], &memory_caps(1024, 4096), "req", "upstream");
    a.append_chunk_sync(&vec![3u8; 10], &memory_caps(1024, 4096), "req", "upstream");

    assert!(a.is_truncated());
    assert_eq!(a.len(), 4096);
    assert_eq!(a.dropped(), 1914);
    let body = a.read_for_emit_sync();
    assert_eq!(body.len(), 4096);
    assert_eq!(&body[..3000], vec![1u8; 3000].as_slice());
    assert_eq!(&body[3000..], vec![2u8; 1096].as_slice());
}

#[tokio::test]
async fn async_append_memory_storage_does_not_create_spill_files() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    let mut a = ArchiveState::default();
    a.append_chunk(&vec![7u8; 4096], &memory_caps(1024, 8192), &root, "req-memory", "upstream")
        .await;

    assert!(matches!(a, ArchiveState::InMemory(_)));
    assert_eq!(a.len(), 4096);
    assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn disk_spill_checks_hard_cap_before_first_spill() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    let mut a = ArchiveState::default();
    a.append_chunk(&vec![9u8; 5000], &disk_caps(2048, 4096), &root, "req-hard-first", "upstream")
        .await;

    assert!(a.is_truncated());
    assert_eq!(a.len(), 4096);
    assert_eq!(a.dropped(), 904);
    assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
}
```

- [ ] **Step 2: Run failing test**

```bash
cargo test -p gw-proxy --test archive_state
```

- [ ] **Step 3: Implement storage enum and prefix retention**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveStorage {
    Memory,
    DiskSpill,
}

pub enum ArchiveState {
    InMemory(BytesMut),
    Spilled { file: Box<SpillFile>, written: u64 },
    Truncated { retained: BytesMut, dropped: u64 },
}
```

- `len()` 对 `Truncated` 返回 `retained.len() as u64`。
- `read_for_emit()` / `read_for_emit_sync()` 对 `Truncated` 返回 `retained.to_vec()`。
- **A9 语义警告**：调用方读"已处理总量"必须用 `len() + dropped()`，不能只用 `len()`。grep `archive_..*body.*len\(\)` 全审一次，必要时改语义并打 Sync Rule 标记。

通用前缀工具：

```rust
fn append_prefix_or_drop(
    buf: &mut BytesMut,
    dropped: &mut u64,
    chunk: &[u8],
    hard_cap_bytes: usize,
) {
    let remaining = hard_cap_bytes.saturating_sub(buf.len());
    if remaining > 0 {
        let keep = remaining.min(chunk.len());
        buf.extend_from_slice(&chunk[..keep]);
        *dropped = dropped.saturating_add((chunk.len() - keep) as u64);
    } else {
        *dropped = dropped.saturating_add(chunk.len() as u64);
    }
}
```

`InMemory → Truncated` 时把现有 buf move 到 `retained`，append 当前 chunk 的 fitting 前缀，剩余计 dropped。`Truncated` 状态下只 append 前缀+计 dropped。memory 模式遇到本应转 `Spilled` 的分支：`debug_assert!(false, "spill in memory mode")` 并转 `Truncated { retained: BytesMut::new(), dropped: chunk.len() as u64 }`，不假装"已写盘"。

`crates/gw-proxy/src/gateway.rs` 中响应 error-passthrough 路径必须改走 `read_for_emit()` / `read_for_emit_sync()`，不许保留 `ArchiveState::Truncated { .. } => Vec::new()` 分支。

- [ ] **Step 4: Verify**

```bash
cargo test -p gw-proxy --test archive_state
```

---

## Task 3: No-Bridge Response Hot Path

**是什么**：在 `proxy.archive.storage = "memory"` 时，响应 body filter 在 Pingora 同步钩内直接完成 archive + 解析；只有 `disk_spill` 模式才走 `block_in_place` 桥接。

**为什么**：默认配置下，响应 hot path 不应被 `block_in_place` 占线程；这是本计划在请求 head/body 路径上唯一真正降低 worker 占用的位置。

**怎么做**：先抽取 CPU-only 同步核心，让 sync 与 async 入口共用；如果抽取代价过大，允许保留两条本地副本但必须用同套测试覆盖，避免行为漂移。然后在 gateway 路由层先 memory 后 bridge。

**不是什么**：不是改 Pingora trait 签名；不是改既有 disk_spill 路径行为；不是改 cross-protocol 渲染逻辑。

**不要做**：不要复制"两份永远会漂移的副本"——若抽取代价过大，必须用同一组 fixture 测试覆盖两条路径。**不要**在 `run_memory_sync` 中出现 `.await` 或 `block_on`。

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `crates/gw-proxy/tests/response_hot_path.rs`
- Test: `crates/gw-proxy/tests/cli_filter_chain.rs`

**Interfaces:**
- Consumes: `ArchiveCaps.storage`.
- Produces: `filters::response::run_memory_sync(...)`.
- Produces: `filters::response::run_cli_body_filter_sync(...)`.

- [ ] **Step 0: 同步可行性勘探（前置）**

在动 `response.rs` 前，先 grep 现有 async 调用面：

```bash
rg -n "\.await" crates/gw-proxy/src/filters/response.rs
```

把每个 awaitee 分类成「天然同步可替代（archive append、metrics、tracing）」「需新同步入口（archive 已加 `*_sync`）」「不可同步（如 tokio Notify、Mutex.lock().await）」。若出现第三类，本 task 暂停，先在 `response.rs` 提一个 follow-up 子任务（不改本计划），不要硬桥。

- [ ] **Step 1: Add focused hot-path tests**

`crates/gw-proxy/tests/response_hot_path.rs`：

```rust
fn function_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src.find(name).expect("function exists");
    let after = &src[start..];
    let mut depth = 0usize;
    let mut seen_open = false;
    for (idx, ch) in after.char_indices() {
        match ch {
            '{' => { depth += 1; seen_open = true; }
            '}' if seen_open => {
                depth -= 1;
                if depth == 0 { return &after[..=idx]; }
            }
            _ => {}
        }
    }
    panic!("function body not found");
}

#[test]
fn response_body_filter_selects_memory_before_block_in_place() {
    let src = include_str!("../src/gateway.rs");
    let body = function_body(src, "fn response_body_filter");
    let memory_branch = body.find("run_memory_sync").expect("memory sync branch");
    let bridge = body.find("tokio::task::block_in_place").expect("disk bridge remains");
    assert!(memory_branch < bridge);
}

#[test]
fn response_memory_filter_is_sync() {
    let src = include_str!("../src/filters/response.rs");
    let body = function_body(src, "pub fn run_memory_sync");
    assert!(!body.contains(".await"));
    assert!(!body.contains("block_on"));
}
```

- [ ] **Step 2: Implement sync response path**

抽取 sync core；让 `run_memory_sync` 调用 sync core + `append_chunk_sync`；existing async `run` 同样调 sync core，然后只对 disk_spill 表面走 async append。测试覆盖矩阵：

- same-protocol stream
- cross-protocol stream
- cross-protocol non-stream
- CLI stream
- CLI non-stream

CLI 测试同时覆盖 `run_cli_body_filter_sync(...)` 与 `run_memory_sync(...)` 内对 CLI 解析的调用。

- [ ] **Step 3: Route gateway memory mode before bridge**

`crates/gw-proxy/src/gateway.rs`：

```rust
let res = if matches!(ctx.archive_caps.storage, crate::ctx::ArchiveStorage::Memory) {
    filters::response::run_memory_sync(session, body, end_of_stream, ctx)
} else {
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(filters::response::run(session, body, end_of_stream, ctx))
    })
};
```

- [ ] **Step 4: Wire config into `Wired`**

在 `bootstrap` 内、`Ok((Wired { ... }))` 之前：

```rust
let archive_storage = match cfg.proxy.archive.storage {
    config::ProxyArchiveStorage::Memory => gw_proxy::ctx::ArchiveStorage::Memory,
    config::ProxyArchiveStorage::DiskSpill => gw_proxy::ctx::ArchiveStorage::DiskSpill,
};
```

`Wired` 字段（注意是逗号，不是分号）：

```rust
archive_caps: gw_proxy::ctx::ArchiveCaps {
    storage: archive_storage,
    soft_cap_bytes: cfg.proxy.archive.soft_cap_bytes,
    hard_cap_bytes: cfg.proxy.archive.hard_cap_bytes,
},
```

- [ ] **Step 5: Verify**

```bash
cargo test -p gw-proxy --test response_hot_path
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test archive_state
```

---

## Task 4: Ledger Must-Deliver Foundation Fixes

**是什么**：在引入 overflow 之前，先修两条本来就该修的窟窿——flush panic 时丢批、DLQ retry 终极失败只计 `lost`。

**为什么**：overflow worker 内部也会调 `flush(...)`。如果 `flush(...)` panic 路径不安全，overflow 引入只会放大丢数面积；同理 DLQ retry 终极失败回灌 backlog 是 must-deliver 的最后兜底，不补它，本计划的"必到达"承诺就只是话术。

**怎么做**：先红测两条静态检查（wrapper 存在 + DLQ 终极推 backlog），再加 owned backup 包装与 backlog 推路径。

**不是什么**：不是改 `flush()` 内部行为；不是改 DLQ envelope 字段。

**不要做**：不要在 wrapper 里"成功即清空 `*buf`"——`flush()` 可能在中途因 individual XADD 失败把项留在 vec 里，wrapper 必须把 `taken` 残留写回 `*buf` 让调用方继续处理。不要在 DLQ 终极失败路径上增 `gateway_usage_records_lost_total`（除非 backlog push 也失败，但 backlog 是内存数据结构，没有失败语义）。

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`
- Test: `crates/gw-ledger/src/flusher.rs`
- Test: `crates/gw-ledger/tests/flusher.rs`

**Interfaces:**
- Produces: `flush_batch_with_panic_fallback(...)`.
- Produces: `route_ledger_item_to_dlq(...)` / `route_ledger_items_to_dlq(...)`.
- Produces: `process_dlq_envelope(..., fallback: &DlqFallback, ...)` 在终极失败时推 `BillingBacklog`.

- [ ] **Step 1: Add regression tests**

```rust
#[test]
fn flush_wrapper_keeps_owned_backup_for_panic_fallback() {
    let src = include_str!("flusher.rs");
    assert!(
        src.contains("flush_batch_with_panic_fallback"),
        "flush calls must go through panic-safe wrapper"
    );
    let wrapper_start = src
        .find("async fn flush_batch_with_panic_fallback")
        .expect("wrapper exists");
    let wrapper = &src[wrapper_start..];
    assert!(wrapper.contains("backup"));
    assert!(wrapper.contains("catch_unwind"));
    assert!(wrapper.contains("route_ledger_items_to_dlq"));
    // A4: 成功路径必须把残留写回 *buf
    assert!(
        wrapper.contains("*buf = taken") || wrapper.contains("buf.extend(taken)"),
        "wrapper must restore unprocessed items into *buf on success"
    );
}

#[test]
fn dlq_retry_terminal_failure_routes_to_backlog() {
    let src = include_str!("flusher.rs");
    let start = src
        .find("async fn process_dlq_envelope")
        .expect("process_dlq_envelope exists");
    let body = &src[start..];
    assert!(
        body.contains("fallback.backlog.push"),
        "terminal DLQ retry failure must go to BillingBacklog, not lost counter only"
    );
}
```

- [ ] **Step 2: Add owned fallback helpers**

```rust
async fn route_ledger_items_to_dlq(
    dlq_fallback: &DlqFallback,
    items: Vec<LedgerItem>,
    reason: &'static str,
) {
    for item in items {
        route_ledger_item_to_dlq(dlq_fallback, item, reason).await;
    }
}

async fn route_ledger_item_to_dlq(
    dlq_fallback: &DlqFallback,
    item: LedgerItem,
    reason: &'static str,
) {
    let had_clear = item.clear.is_some();
    let envelope = DlqEnvelope::new(item.record, reason, 0, DlqLayer::Mem, now_ms());
    route_to_dlq(dlq_fallback, envelope).await;
    if had_clear {
        metrics::counter!(
            "gw_billing_inflight_clear_total",
            "outcome" => "skipped_dlq"
        )
        .increment(1);
    }
}
```

- [ ] **Step 3: Add panic-safe flush wrapper**

```rust
async fn flush_batch_with_panic_fallback(
    redis: &RedisCtx,
    instance_id: &str,
    dlq_fallback: &DlqFallback,
    buf: &mut Vec<LedgerItem>,
) {
    if buf.is_empty() {
        return;
    }
    let backup = buf.clone();
    let mut taken = std::mem::take(buf);
    let result = AssertUnwindSafe(flush(redis, instance_id, dlq_fallback, &mut taken))
        .catch_unwind()
        .await;
    match result {
        Ok(()) => {
            // A4: flush 成功——把 taken 里可能残留的项写回 *buf，
            // 让调用方下一轮继续处理；不许静默吞。
            if !taken.is_empty() {
                *buf = taken;
            }
        }
        Err(_) => {
            metrics::counter!("gw_billing_flusher_panic_total").increment(1);
            tracing::error!(
                target: "gw_ledger",
                "flush panicked; routing backup batch to DLQ fallback"
            );
            route_ledger_items_to_dlq(dlq_fallback, backup, "flush_panicked").await;
            // `*buf` 已被 take 成空；下一轮从空开始，避免再次重放 backup
        }
    }
}
```

**A4 说明**：panic 时复用 backup 会重复投递。下游按 `(request_id, attempt_index) + provenance` 去重，重复优于静默丢。

- [ ] **Step 4: Replace direct worker flush calls**

在主 billing flusher 与 cancel drain 中，把直接的 `AssertUnwindSafe(flush(...))` 块替换为 `flush_batch_with_panic_fallback(...)`。保留既有 logging，但移除"批次已丢"措辞。

- [ ] **Step 5: Fix DLQ retry terminal failure**

修 `spawn_dlq_retry_worker`、`drain_dlq_retry_channel`、`process_dlq_envelope` 三处签名，让 `process_dlq_envelope` 至少能拿到 `&DlqFallback` 或 `Arc<BillingBacklog>`。

Redis DLQ + 应急文件双失败时推 disk-layer envelope 到 backlog：

```rust
fallback.backlog.push(disk_envelope).await;
metrics::counter!("gw_usage_dlq_backlog_pushed_total").increment(1);
```

**不要**在此处 `gateway_usage_records_lost_total.increment(1)`。

- [ ] **Step 6: Verify**

```bash
cargo test -p gw-ledger flush_wrapper_keeps_owned_backup_for_panic_fallback
cargo test -p gw-ledger dlq_retry_terminal_failure_routes_to_backlog
```

---

## Task 5: Non-Blocking Primary Overflow Handle

**是什么**：新建 `PrimaryDelivery` 句柄：`try_enqueue(item)` 非阻塞，`mark_drained(count)` 更新 gauge。

**为什么**：`emit` 必须有一个"非阻塞 + 立刻拿回 owned item"的入口才能在 Full 时把所有权移交给 fallback；现有 `tx: mpsc::Sender` 直接暴露会诱使后人写 `send().await`。

**怎么做**：薄包装 + AtomicUsize gauge；测试覆盖 Full/Closed 返回 item 与 pending 不变。

**不是什么**：不是新建独立 channel 拓扑——overflow channel 在 Task 6 里建并注入；本 task 只做"对 sender 的可观测包装"。

**不要做**：不要给 `LedgerItem` 加 serde（A1：overflow 只走内存）。不要在 `try_enqueue` 内部"满了就 await 一下"。不要让 `pending` 计入 channel 满之后被拒的项（只在 `Ok(())` 时 +1）。

**Files:**
- Create: `crates/gw-ledger/src/primary_delivery.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Test: `crates/gw-ledger/src/primary_delivery.rs`

**Interfaces:**
- Produces: `PrimaryDelivery::try_enqueue(item) -> Result<(), mpsc::error::TrySendError<LedgerItem>>`.
- Produces: `PrimaryDelivery::mark_drained(count)`.
- Does not derive serde for `LedgerItem`; overflow queue is in-memory only.

- [ ] **Step 1: Create module**

```rust
use crate::flusher::LedgerItem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct PrimaryDelivery {
    tx: mpsc::Sender<LedgerItem>,
    pending: Arc<AtomicUsize>,
}

impl PrimaryDelivery {
    pub fn new(tx: mpsc::Sender<LedgerItem>) -> Self {
        Self {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    pub fn try_enqueue(
        &self,
        item: LedgerItem,
    ) -> Result<(), mpsc::error::TrySendError<LedgerItem>> {
        match self.tx.try_send(item) {
            Ok(()) => {
                let pending = self.pending.fetch_add(1, Ordering::AcqRel) + 1;
                metrics::gauge!("gw_billing_primary_overflow_pending").set(pending as f64);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn mark_drained(&self, count: usize) {
        if count == 0 { return; }
        let prev = self.pending.fetch_sub(count, Ordering::AcqRel);
        let next = prev.saturating_sub(count);
        metrics::gauge!("gw_billing_primary_overflow_pending").set(next as f64);
    }
}
```

单元测试：

- clear key 在 `try_enqueue` 后仍保留。
- 满 channel 返回 `TrySendError::Full(item)`，`pending()` 不变。
- 关 channel 返回 `TrySendError::Closed(item)`，`pending()` 不变。

- [ ] **Step 2: Export module**

`crates/gw-ledger/src/lib.rs`：

```rust
pub mod primary_delivery;
```

- [ ] **Step 3: Verify**

```bash
cargo test -p gw-ledger primary_delivery
```

---

## Task 6: Overflow Worker And Must-Deliver Emit Routing

**是什么**：拉起 overflow worker（用 `PrimaryDelivery` 的对端 receiver）；重写 `Ledger::emit` 的 Full/Closed 分支：先 try_enqueue 到 overflow，失败再 `route_ledger_item_to_dlq`。

**为什么**：这是本计划"请求收尾去同步 XADD"的承载步。`emit` 必须保证：①任何条件下都不 `.await` 在 overflow 容量上；②不丢任何 owned item；③不在 overflow 接住时把 primary closed 当作 page 事件（A6）。

**怎么做**：先静态红测（emit 体内无 `sync_xadd`、无 `.send(` 等容量 await、含 `primary_overflow.try_enqueue` + `route_ledger_item_to_dlq`），再起 worker，再改 emit 分支，再补 in-flight buf 不丢的 sidecar 机制（A14）。

**不是什么**：不是删 `sync_xadd` 函数——该函数在 Task 7 Step 6 里删，本 task 只做"emit 不再调它"。

**不要做**：不要在 Closed 分支无条件 `gw_billing_flusher_dead_total += 1`（A6：仅在 overflow 也 Closed 时增）。不要在 path metric 上"先猜成功再补失败"——见 Step 4 注释 A7。不要让 worker buf 在重启窗口里裸丢（A14：sidecar）。

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`
- Test: `crates/gw-ledger/src/flusher.rs`

**Interfaces:**
- Consumes: `PrimaryDelivery`.
- Produces: `Ledger.primary_overflow`.
- Consumes: `flush_batch_with_panic_fallback(...)` / `route_ledger_item_to_dlq(...)` from Task 4.
- Produces: `spawn_primary_overflow_worker(...)`.
- Produces: `Ledger::for_test_with_overflow(...)`（详见 Step 5）.

- [ ] **Step 1: Add static regression test**

```rust
#[test]
fn emit_full_branch_does_not_call_sync_xadd_or_await_overflow_capacity() {
    let src = include_str!("flusher.rs");
    let emit_start = src.find("pub async fn emit").expect("emit exists");
    let after_emit = src[emit_start..]
        .find("pub fn try_send")
        .map(|n| emit_start + n)
        .unwrap_or(src.len());
    let emit_body = &src[emit_start..after_emit];

    assert!(!emit_body.contains("sync_xadd"));
    assert!(!emit_body.contains(".send("));
    assert!(emit_body.contains("primary_overflow.try_enqueue"));
    assert!(emit_body.contains("route_ledger_item_to_dlq"));
}

#[test]
fn flusher_dead_total_only_increments_when_overflow_also_closed() {
    // A6: 静态保证 primary closed 分支里 flusher_dead_total 不是无条件 +1
    let src = include_str!("flusher.rs");
    let emit_start = src.find("pub async fn emit").expect("emit exists");
    let after_emit = src[emit_start..]
        .find("pub fn try_send")
        .map(|n| emit_start + n)
        .unwrap_or(src.len());
    let emit_body = &src[emit_start..after_emit];

    // flusher_dead_total 必须出现在 "overflow ... Closed" 子分支里，
    // 不是 primary Closed 顶层
    let dead_pos = emit_body.find("gw_billing_flusher_dead_total")
        .expect("flusher_dead_total still tracked");
    let nearby = &emit_body[dead_pos.saturating_sub(400)..dead_pos];
    assert!(
        nearby.contains("primary_closed_overflow_closed"),
        "flusher_dead_total must be inside the primary-closed + overflow-closed sub-branch"
    );
}
```

- [ ] **Step 2: Add fields and startup**

`Ledger`：

```rust
primary_overflow: PrimaryDelivery,
```

`start_with_spill_root` 中，主 channel 后创建 overflow channel：

```rust
let (overflow_tx, overflow_rx) = mpsc::channel::<LedgerItem>(channel_capacity.max(1024));
let primary_overflow = PrimaryDelivery::new(overflow_tx);
let overflow_rx = Arc::new(Mutex::new(overflow_rx));
```

先建 `overflow_handle` 再建 `flusher_handle`，或同时给两个 supervisor `shutdown.clone()`；**不要**在第二个 worker 拿到 clone 之前 move 走 `shutdown`。

- [ ] **Step 3: Add overflow worker（含 A14 sidecar）**

```rust
fn spawn_primary_overflow_worker(
    redis: RedisCtx,
    instance_id: String,
    dlq_fallback: DlqFallback,
    primary_overflow: PrimaryDelivery,
    rx: Arc<Mutex<mpsc::Receiver<LedgerItem>>>,
    batch_size: usize,
    flush_interval: Duration,
    shutdown: ShutdownToken,
    drain_budget: Duration,
) -> JoinHandle<()> {
    Supervisor::new("billing_primary_overflow", RestartPolicy::default(), shutdown).spawn(
        move |tok| {
            let redis = redis.clone();
            let instance_id = instance_id.clone();
            let dlq_fallback = dlq_fallback.clone();
            let primary_overflow = primary_overflow.clone();
            let rx = rx.clone();
            async move {
                // A14: buf 与 sidecar 同步增长；任何提前退出都把 sidecar 路由到 DLQ。
                let mut buf: Vec<LedgerItem> = Vec::with_capacity(batch_size);
                let mut ticker = interval(flush_interval);
                let result: Result<(), ()> = loop {
                    tokio::select! {
                        _ = tok.cancelled() => {
                            drain_primary_overflow_channel(
                                &rx, &mut buf, batch_size, drain_budget,
                                &redis, &instance_id, &dlq_fallback, &primary_overflow,
                            ).await;
                            break Ok(());
                        }
                        maybe = async {
                            let mut guard = rx.lock().await;
                            let v = guard.recv().await;
                            drop(guard);
                            v
                        } => {
                            match maybe {
                                Some(item) => {
                                    buf.push(item);
                                    if buf.len() >= batch_size {
                                        flush_primary_overflow_batch(
                                            &redis, &instance_id, &dlq_fallback,
                                            &primary_overflow, &mut buf,
                                        ).await;
                                    }
                                }
                                None => {
                                    if !buf.is_empty() {
                                        flush_primary_overflow_batch(
                                            &redis, &instance_id, &dlq_fallback,
                                            &primary_overflow, &mut buf,
                                        ).await;
                                    }
                                    break Ok(());
                                }
                            }
                        }
                        _ = ticker.tick() => {
                            if !buf.is_empty() {
                                flush_primary_overflow_batch(
                                    &redis, &instance_id, &dlq_fallback,
                                    &primary_overflow, &mut buf,
                                ).await;
                            }
                        }
                    }
                };
                // A14: 任何提前退出（包括 panic 后 supervisor 重启前的当前 future drop）
                // 都应保证 buf 不被裸丢；正常分支以上已 flush，重启窗口靠下面的 Drop guard。
                let _ = result;
                Ok(())
            }
        },
    )
}

/// 必须捕获 buf 长度后再 flush，否则 mark_drained 会拿到 0（A14/A13）。
async fn flush_primary_overflow_batch(
    redis: &RedisCtx,
    instance_id: &str,
    dlq_fallback: &DlqFallback,
    primary_overflow: &PrimaryDelivery,
    buf: &mut Vec<LedgerItem>,
) {
    let drained = buf.len();
    flush_batch_with_panic_fallback(redis, instance_id, dlq_fallback, buf).await;
    // 注意：wrapper 在 panic 时清空 buf 并路由 backup；成功时把残留写回 *buf
    let actually_drained = drained.saturating_sub(buf.len());
    primary_overflow.mark_drained(actually_drained);
}
```

**A14 sidecar 的补丁**：上面 `Vec<LedgerItem>` 写法仍存在"future 在 `buf.push(item)` 之后、`flush_primary_overflow_batch` 之前被 cancel 时丢 buf"的窗口。把 `buf` 改为 `Vec<Arc<LedgerItem>>` 既要求 `flush(...)` 接受 `Arc<LedgerItem>` 改造成本太高。**采用替代方案**：包装一个 RAII guard，在 future drop 时把残留 buf 的 owned clone 路由到 DLQ fallback：

```rust
struct OverflowBufGuard<'a> {
    buf: &'a mut Vec<LedgerItem>,
    dlq_fallback: &'a DlqFallback,
}

impl<'a> Drop for OverflowBufGuard<'a> {
    fn drop(&mut self) {
        if self.buf.is_empty() { return; }
        let items = std::mem::take(self.buf);
        let dlq = self.dlq_fallback.clone();
        // Drop 不能 await：把残留交给一个 detached task。
        // 这只在 future 被异常 cancel 时触发，频率极低。
        tokio::spawn(async move {
            route_ledger_items_to_dlq(&dlq, items, "overflow_worker_aborted").await;
        });
        metrics::counter!("gw_billing_overflow_buf_rescued_total").increment(1);
    }
}
```

然后在 worker `async move {}` 开头 `let mut buf_guard = OverflowBufGuard { buf: &mut buf, dlq_fallback: &dlq_fallback };` 用 `buf_guard.buf` 操作，正常退出前 `std::mem::forget(buf_guard)`。**这条 sidecar 是 best-effort：tokio runtime 关停时 spawn 也会被 abort。文档说明这是"最后一道软网"，硬保证仍由 DLQ 重试 + backlog 提供。**

`drain_primary_overflow_channel(...)` 与 `drain_billing_channel` 等价：`try_recv` 排空，满批 flush，`timeout_at(deadline, rx_guard.recv())` 等剩余，最终再 flush 一次残留。

- [ ] **Step 4: Replace `emit` full/closed branches**

```rust
Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
    // A7: path 是"尝试落点"，单条记录可能贡献多个 path
    metrics::counter!("gw_billing_emit_path_total", "path" => "primary_full_attempt").increment(1);
    match self.primary_overflow.try_enqueue(item_back) {
        Ok(()) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "primary_overflow_ok").increment(1);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "overflow_full_dlq").increment(1);
            route_ledger_item_to_dlq(&self.dlq_fallback, item_back, "primary_overflow_full").await;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(item_back)) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "overflow_closed_dlq").increment(1);
            route_ledger_item_to_dlq(&self.dlq_fallback, item_back, "primary_overflow_closed").await;
        }
    }
}
Err(tokio::sync::mpsc::error::TrySendError::Closed(item_back)) => {
    metrics::counter!("gw_billing_emit_path_total", "path" => "primary_closed_attempt").increment(1);
    match self.primary_overflow.try_enqueue(item_back) {
        Ok(()) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "primary_closed_overflow_ok").increment(1);
            // A6: primary closed 但 overflow 接住，不是 dead；不增 flusher_dead_total
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "primary_closed_overflow_full_dlq").increment(1);
            route_ledger_item_to_dlq(&self.dlq_fallback, item_back, "primary_closed_overflow_full").await;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(item_back)) => {
            metrics::counter!("gw_billing_emit_path_total", "path" => "primary_closed_overflow_closed_dlq").increment(1);
            // A6: 只有这条才是真正 "must-deliver 第一层全垮"——可以 page
            metrics::counter!("gw_billing_flusher_dead_total").increment(1);
            route_ledger_item_to_dlq(&self.dlq_fallback, item_back, "primary_closed_overflow_closed").await;
        }
    }
}
```

`emit` 内任何位置都不许 `sync_xadd`。

- [ ] **Step 5: Add test-only constructor with live overflow（A15）**

`Ledger::from_parts` 不许 "create and drop `_overflow_rx`"。新增：

```rust
#[cfg(test)]
impl Ledger {
    /// 给确定性 overflow 测试用：返回 Ledger、primary 接收端、overflow worker handle。
    /// 测试持有 `primary_rx` 不读 → 模拟 primary Full。
    /// 测试 drop `primary_rx` → 模拟 primary Closed。
    pub fn for_test_with_overflow(
        redis: RedisCtx,
        instance_id: String,
        primary_capacity: usize,
        overflow_capacity: usize,
    ) -> (Self, mpsc::Receiver<LedgerItem>, JoinHandle<()>) {
        let (primary_tx, primary_rx) = mpsc::channel::<LedgerItem>(primary_capacity);
        let (overflow_tx, overflow_rx) = mpsc::channel::<LedgerItem>(overflow_capacity);
        let primary_overflow = PrimaryDelivery::new(overflow_tx);
        let overflow_rx = Arc::new(Mutex::new(overflow_rx));
        let dlq_fallback = DlqFallback {
            writer: Arc::new(EmergencyDlqWriter::new_noop()),
            retry_tx: /* in-memory channel for test */,
            backlog: Arc::new(BillingBacklog::new()),
        };
        let overflow_handle = spawn_primary_overflow_worker(
            redis.clone(), instance_id.clone(), dlq_fallback.clone(),
            primary_overflow.clone(), overflow_rx,
            /* batch_size */ 64, /* flush_interval */ Duration::from_millis(50),
            ShutdownToken::new(), /* drain_budget */ Duration::from_secs(2),
        );
        let ledger = Self {
            redis, tx: primary_tx, instance_id, dlq_fallback,
            primary_overflow,
        };
        (ledger, primary_rx, overflow_handle)
    }
}
```

- [ ] **Step 6: Verify**

```bash
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd_or_await_overflow_capacity
cargo test -p gw-ledger flusher_dead_total_only_increments_when_overflow_also_closed
cargo test -p gw-ledger primary_delivery
```

---

## Task 7: Deterministic Must-Deliver Tests And Metrics/Docs

**是什么**：把"overflow 行为"用真实运行时测试钉死，更新 metrics catalog 与 alert/文档措辞，最后删 sync bypass 函数。

**为什么**：静态源测试只能证存在；must-deliver 必须有可重复的运行时证据：record 真的进了 `usage:records`、inflight key 在 XADD 之后被 DEL、终极失败真的回灌 backlog。

**怎么做**：四个运行时测试（满 channel / 关 channel / overflow 满 fallback / shutdown drain）+ 一个 DLQ 终极测试 + catalog 与 docs。

**不是什么**：不是用 `xlen >= 1` 当成功条件（必须按 `request_id` 解析校验）。不是用静态源测试取代真实运行时关 primary 测试。

**不要做**：不要在 sync_xadd 删除前才补运行时证据——次序是"运行时测试绿 → 删 bypass"。不要给 catalog 加 `labels` 字段（A11）。不要漏 `docs/go-contracts/`（A12 + A9）。

**Files:**
- Modify: `crates/gw-ledger/tests/flusher.rs`
- Modify: `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`
- Modify: `crates/gw-ledger/src/flusher.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: `docs/go-contracts/main.md` 及 detailed-archive 章节对应的 part
- Modify: `deploy/prometheus/billing-rules.yml`

**Interfaces:**
- Consumes: Task 6 的 overflow worker 与 `for_test_with_overflow`.
- Produces: 确定性证据覆盖 overflow delivery、inflight DEL、shutdown drain、DLQ 终极 fallback.

- [ ] **Step 1: Replace full-channel test with deterministic overflow harness**

用 A15 的 `for_test_with_overflow` 让 primary 永远不被读：

```rust
let (ledger, _primary_rx_held, _overflow_handle) =
    Ledger::for_test_with_overflow(redis.clone(), "inst-1".into(), 1, 64);

// 先用一条把 primary 填满，再发目标记录使其进 overflow
ledger.emit(make_item("req-fill")).await;
ledger.emit(make_item("req-overflow")).await;

tokio::time::sleep(Duration::from_millis(200)).await; // 等 worker flush

let records = read_usage_records(&redis).await;
assert!(records.iter().any(|r| r.request_id == "req-overflow"));
```

**不要**用 `xlen >= 1`。

- [ ] **Step 2: Preserve closed-primary runtime test**

保持一个真实运行时测试：

- drop primary_rx → primary 关
- overflow worker 仍活
- `ledger.emit(LedgerItem { clear: Some(...) })` 通过 overflow 落地
- 对应记录出现在 `usage:records`
- inflight key 仅在 XADD 之后被 DEL

静态源测试可作为附加 check，不能替代真实运行时测试。

- [ ] **Step 3: Add overflow-full fallback test**

人为把 overflow 也填满：

- `Ledger::emit` 立即返回（无 `.await` 在容量上）
- 记录走 DLQ retry channel / 应急文件 / `BillingBacklog`
- inflight key 不被 DEL

- [ ] **Step 4: Add shutdown drain test**

cancel 时同时持续灌入 overflow：迟到记录在 `drain_budget` 内被 flush 或路由到 fallback；断言 worker 正常退出。

- [ ] **Step 5: Add DLQ retry terminal fallback test**

人为让 Redis DLQ XADD 失败 + 应急文件 append 失败，断言 envelope 进 `BillingBacklog`、`gw_usage_dlq_backlog_pushed_total` +1、`gateway_usage_records_lost_total` 不在本路径上累加。

- [ ] **Step 6: Delete sync bypass helpers**

运行时测试全绿、`emit` 不再引用之后，删：

```rust
async fn sync_xadd(...)
async fn sync_del_inflight(...)
```

- [ ] **Step 7: Update metrics catalog**

不加 `labels` 字段。更新既有 `MetricSpec`：

```rust
MetricSpec {
    name: "gw_billing_emit_path_total",
    kind: MetricKind::Counter,
    help: "UsageRecord emit attempts at each fallback layer. A single record may increment multiple paths (e.g. primary_full_attempt + primary_overflow_ok). Paths: channel, primary_full_attempt, primary_overflow_ok, overflow_full_dlq, overflow_closed_dlq, primary_closed_attempt, primary_closed_overflow_ok, primary_closed_overflow_full_dlq, primary_closed_overflow_closed_dlq.",
},
MetricSpec {
    name: "gw_billing_primary_overflow_pending",
    kind: MetricKind::Gauge,
    help: "Approximate UsageRecord count enqueued in primary overflow but not yet committed via XADD. Counts items that have left the overflow channel but are still in worker buf.",
},
MetricSpec {
    name: "gw_billing_inflight_clear_total",
    kind: MetricKind::Counter,
    help: "DEL inflight key outcome after XADD-confirmed ledger delivery; labels=outcome(ok|del_failed|skipped_dlq). skipped_lost retained only for legacy paths and should approach zero after Task 4.",
},
MetricSpec {
    name: "gw_billing_flusher_dead_total",
    kind: MetricKind::Counter,
    help: "Primary channel closed AND primary overflow closed simultaneously — must-deliver first layer fully down. Records still land in DLQ/backlog cascade. ANY non-zero = page on-call.",
},
MetricSpec {
    name: "gw_billing_flusher_panic_total",
    kind: MetricKind::Counter,
    help: "flush(...) panicked; the panicked batch is duplicate-routed to DLQ. Non-zero = investigate.",
},
MetricSpec {
    name: "gw_billing_overflow_buf_rescued_total",
    kind: MetricKind::Counter,
    help: "Overflow worker future was aborted with non-empty buf; rescued items routed to DLQ best-effort.",
},
MetricSpec {
    name: "gw_usage_dlq_backlog_pushed_total",
    kind: MetricKind::Counter,
    help: "DLQ retry terminal failure (Redis + emergency file both failed) pushed envelope into BillingBacklog instead of counting as lost.",
},
```

`gw_billing_mpsc_bypass_total` 在无人发送时删除。

- [ ] **Step 8: Update docs, alerts, go-contracts**

把"sync XADD bypass"措辞替换为：

```text
On primary channel Full/Closed, Ledger::emit first attempts non-blocking primary overflow enqueue. If overflow is full or closed, the record enters the existing DLQ / emergency disk / BillingBacklog cascade. Inflight keys are deleted only after usage:records XADD is confirmed.
```

更新位置：
- `docs/architecture.md`（§Background-task graph + §Deliberate behaviours）
- `docs/workflow.md`（§response_body_filter 改 memory 路径；§request 尾部改 emit 路径）
- `docs/spec/06-billing-ledger.md`（emit 路径状态机 + flusher_dead_total 口径）
- `deploy/prometheus/billing-rules.yml`（移除"primary_closed → page"，改为"primary_closed_overflow_closed → page"）
- `docs/go-contracts/main.md` + 对应 part：A9 的 `usage:detailed` 截断负载形状变化（`Truncated` 现在返回前缀字节而非空 vec）必须在跨进程契约里说明，Go drainer 端解析逻辑要兼容。

- [ ] **Step 9: Verify**

```bash
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd_or_await_overflow_capacity
cargo test -p gw-ledger flusher_dead_total_only_increments_when_overflow_also_closed
cargo test -p gw-ledger primary_delivery
cargo test -p gw-ledger --test flusher -- --ignored
cargo test -p gw-ledger --test inflight_clear_via_flusher -- --ignored
```

---

## Task 8: Gateway Wiring And End-To-End Checks

**是什么**：把 archive 模式与 ledger overflow worker 接到 `bootstrap`；启动日志输出 archive 模式与 ledger worker 数（应为 5：main flusher + cancel drain + DLQ retry + backlog drainer + primary overflow）；跑一遍端到端 check。

**为什么**：上面 7 个 task 改的都是"组件"，bootstrap 是"装配"。任何配置→运行时映射、worker 数量回归、ArchiveCaps 字段补全都靠这一步把上下游对齐。

**怎么做**：startup log + source-level 接线测试 + 一组 focused cargo test + workspace compile check。

**不是什么**：不是跑全量 e2e / soak（那是另一条 CI 路径）；不是改 bootstrap 其他 wiring。

**不要做**：不要把启动日志改成 debug 级别（应是 info，operator 要能在日常 log 里直接看到）。不要忘了把 ledger worker 数预期更新到 5。

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Test: existing workspace tests

**Interfaces:**
- Consumes: Task 1 archive 配置；Task 2 archive 运行时；Task 6 overflow worker.

- [ ] **Step 1: Add startup logs**

计算完 `archive_storage` 之后、构造 `Wired` 之前：

```rust
tracing::info!(
    target: "gw_gateway",
    archive_storage = ?archive_storage,
    soft_cap_bytes = cfg.proxy.archive.soft_cap_bytes,
    hard_cap_bytes = cfg.proxy.archive.hard_cap_bytes,
    "response archive configured"
);
```

`Ledger::start_with_spill_root(...)` 返回之后、消费 `ledger_handles` 之前：

```rust
let ledger_worker_count = ledger_handles.len();
tracing::info!(
    target: "gw_gateway",
    ledger_workers = ledger_worker_count,
    "ledger workers started"
);
```

Task 6 完成后预期值为 `5`。

- [ ] **Step 2: Add wiring source test**

源级或 focused unit 证明：

- `config::ProxyArchiveStorage::Memory` 映射到 `gw_proxy::ctx::ArchiveStorage::Memory`。
- `Wired { archive_caps: ... }` 含 `storage: archive_storage`。

- [ ] **Step 3: Run focused tests**

```bash
cargo test -p gw-gateway proxy_archive_
cargo test -p gw-proxy --test response_hot_path
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test archive_state
cargo test -p gw-ledger primary_delivery
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd_or_await_overflow_capacity
cargo test -p gw-ledger flusher_dead_total_only_increments_when_overflow_also_closed
```

- [ ] **Step 4: Run broader compile check**

```bash
cargo check -p gw-gateway -p gw-proxy -p gw-ledger
```

- [ ] **Step 5: Smoke-run + smoke e2e（推荐，不强制）**

```bash
make e2e-soak-smoke
```

按 `CLAUDE.md` 的建议，本计划触及 codec/routing/ledger 三处 hot path，开 PR 前过一遍 smoke。

---

## Verification Matrix

- Config:
  - `cargo test -p gw-gateway proxy_archive_`
  - 验证默认 memory + 显式 disk_spill + soft>hard 拒绝。

- Response memory hot path:
  - `cargo test -p gw-proxy --test response_hot_path`
  - 验证 memory 分支在 bridge 之前 + `run_memory_sync` 无 await/block_on。

- Archive bounds:
  - `cargo test -p gw-proxy --test archive_state`
  - 验证 memory 模式不写盘、保留 hard_cap 前缀、累计 dropped 字节。

- Billing overflow:
  - `cargo test -p gw-ledger primary_delivery`
  - `cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd_or_await_overflow_capacity`
  - `cargo test -p gw-ledger flusher_dead_total_only_increments_when_overflow_also_closed`
  - 验证 `emit` 不 sync XADD、不 await overflow 容量、`flusher_dead_total` 口径正确。

- Redis integration:
  - `cargo test -p gw-ledger --test flusher -- --ignored`
  - `cargo test -p gw-ledger --test inflight_clear_via_flusher -- --ignored`
  - 验证 overflow delivery 真写到目标 request 且仅在 XADD 后 DEL。

- Workspace:
  - `cargo check -p gw-gateway -p gw-proxy -p gw-ledger`

---

## Operational Notes

- 默认 `[proxy.archive] storage = "memory"` 把 archive 写盘移出响应 hot path。大响应保留至 `hard_cap_bytes` 的前缀，超出部分计入 dropped。
- 需要"传统大响应归档保真"的 operator 可设 `storage = "disk_spill"`，并接受 disk I/O 段上的 Pingora 同步桥接。
- 计费 full/closed 不再触发同步 XADD；流程是 "non-blocking overflow → DLQ / emergency disk / `BillingBacklog`"。
- `gw_billing_primary_overflow_pending` 持续增长 = 容量或 Redis 延迟信号；结合既有 billing backlog 指标看。
- `gw_billing_flusher_dead_total > 0` = primary + overflow 都关 = must-deliver 第一层完全垮 = page on-call。primary 单独关短暂出现（如 supervisor 重启）不再 page。
- `gw_billing_flusher_panic_total > 0` = flush panic，已用 backup 重投（可能重复，下游去重），需查 panic 根因。
- `gw_billing_overflow_buf_rescued_total > 0` = overflow worker future 异常 cancel 触发 RAII 救援，频率应极低。
- A9 改了 `usage:detailed` 在截断时的负载形状；Go drainer 上线前需先升级到新契约。

---

## Self-Review

- 规格覆盖：
  - "不用磁盘的时候不走同步桥接"：Task 1-3。
  - "异步任务必须完成 / must-deliver"：Task 4-6 用 overflow + DLQ + backlog 保留 owned `LedgerItem`。
  - "不造成 must-deliver 语义损失"：overflow full/closed 一律落 fallback 链，且不 DEL inflight。

- 本轮审计修复并入：
  - A1 + Task 6 Step 4 措辞一致："先 overflow 后 DLQ"。
  - A4 + Task 4 Step 3：wrapper 成功路径写回 `*buf`。
  - A6 + Task 6 Step 1 静态测试 + Step 4 代码：`flusher_dead_total` 仅在 overflow 也 Closed 时增。
  - A7 + Task 7 Step 7 help：path 是"尝试落点"，可多增。
  - A9 + Task 7 Step 8：`usage:detailed` 负载形状跨进程契约同步。
  - A13 + Task 6 Step 3：`mark_drained` 用真实 drained 数。
  - A14 + Task 6 Step 3 RAII：worker future 异常 cancel 时残留 buf 走 best-effort 救援。
  - A15 + Task 6 Step 5：测试构造器返回 primary_rx 与 overflow worker handle。
  - Task 2 Step 1：补 `disk_caps()` helper。
  - Task 3 Step 0：同步可行性勘探前置。
  - Sync Rule：`docs/go-contracts/` 列入更新清单。
