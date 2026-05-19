# 业务/支撑分离重构 Plan（v2026-05-19）

> **v2 修订（2026-05-19）**：撤销 `gw-billing` crate 抽出。原因：用户决议「用量 → cost 公式归 Go 端实现，Rust 仅记录用量」（详见 [桶 6 五份 spec](../specs/2026-05-19-billing-c-alpha-usage-record.md)）。Rust 端不再有复杂计费公式，无需独立业务 crate；保留在 `gw-ledger` 内的薄 freeze 估算函数足够。Step 3 整体撤销；Step 5 内的 `gw-billing` 引用清理。其他 step（gw-adapter / FreezeGuard trait 化 / CLAUDE.md 规则）不变。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal**：把目前耦合在 `gw-proxy` 与 `gw-ledger` 里的"业务认知"剥到独立 crate，让支撑层只承载通用机制，为 worker 落地（[2026-05-19-task-worker-design.md](../specs/2026-05-19-task-worker-design.md)）扫清前置依赖。

**Architecture（落地目标，对照 spec v3.3 + 桶 6 v2）**：

1. **新增业务 crate `gw-adapter`**：上游 provider 差异化（OAuth refresh、JWT 签名、kiro EventStream、Vertex SA）的 trait + 注册表。当前 `gw-proxy::filters::upstream` 里写死的 `x-api-key` 注入、auth header 改写这部分逻辑剥出来。gateway 与未来 worker 都 import `gw-adapter`。
2. ~~**新增业务 crate `gw-billing`**~~：**撤销**。用户决议公式归 Go；Rust 仅做用量记录，详见桶 6 五份 spec。`gw-ledger::filters::logging` 的 cost 计算整段删除（由 C-β spec 接管）。
3. **`gw-ledger` FreezeGuard trait 化**：`gw-ledger::freeze::FreezeGuard` 改成 trait + 两个实现（请求级 RAII / 任务级显式 settle）。本 plan 只落 trait + 请求级实现 + 接口预留任务级实现；任务级实现等 worker 落地时填。**v2 同步调整**：`FreezeGuard::settle(actual)` API 改为 `FreezeGuard::release()`（参 C-γ ADR-Cγ-2），不再扣 deducted。
4. **ADR-018 三条新规则进 CLAUDE.md**：业务/支撑分离原则、执行体禁互依、业务-执行体成对存在。

**Tech Stack**：Rust 1.86，沿用 sqlx runtime-checked、fred 9.4、moka 0.12、metrics 0.23 不引新依赖。

**Out of scope**（属于后续 plan，不在本次）：
- `gw-task` / `gw-task-runtime` / `gw-task-worker` 三个 crate 的实现；
- adapter 的 6 个具体实现（claude_code/codex/gemini_cli/vertex_ai/antigravity/kiro），本 plan 只搭骨架；
- ~~billing 的具体新公式（cache_creation 差价、video 时长计费），本 plan 只搭骨架~~ → **归 Go 实施**；
- 桶 6 C-α / C-β / C-γ / C-δ 的具体实施（独立 plan）；
- `gw-engine` 抽出（建议但非硬依赖，单独 plan）。

---

## File Structure

**Modify**：

- `Cargo.toml`（workspace）— 注册 `gw-adapter`（撤销 `gw-billing`）。
- `crates/gw-proxy/Cargo.toml` — 加 `gw-adapter` 依赖；保留 `gw-ledger`。
- `crates/gw-proxy/src/filters/upstream.rs` — 把 `upstream_request_filter` 里写死的 auth 注入挪到 `gw-adapter::Registry::default_for_vendor(vendor_id).rewrite_auth(...)`。
- ~~`crates/gw-proxy/src/filters/logging.rs` — `cost_micro` 计算改调 `gw-billing::compute_cost(...)`~~ → **桶 6 C-β spec 接管**：cost 计算整段删除。
- `crates/gw-proxy/src/filters/request.rs` — `FreezeGuard::freeze(uid, est)` 改调 `gw-ledger::freeze::RequestFreeze::try_freeze(...)` 实现 trait。
- `crates/gw-ledger/src/lib.rs` — 重新导出 `FreezeGuard` trait（取代结构体）；保留 `BillingRecord`（桶 6 C-α 后改名 `UsageRecord`）/ `Ledger`。
- `crates/gw-ledger/src/freeze.rs` — 拆成 `mod trait_def`（trait + FreezeKind 枚举） + `mod request`（RAII 实现，原 `FreezeGuard` 改名 `RequestFreeze`） + `mod task`（trait 接口预留，body 是 `todo!("v1.2 worker plan")`）。
- `CLAUDE.md` — "Things that are intentional" 章节后插入 "Crate layering rules"（ADR-018 三条新规则）。

**New**：

- `crates/gw-adapter/Cargo.toml` — 依赖 `gw-core`、`gw-store`、`gw-codec`。
- `crates/gw-adapter/src/lib.rs` — 导出 trait + 注册表 + Noop 默认实现。
- `crates/gw-adapter/src/trait_def.rs` — `UpstreamAdapter` trait（与 adapter-layer spec ADR-A1 对齐）：
  ```rust
  pub trait UpstreamAdapter: Send + Sync + 'static {
      fn name(&self) -> &'static str;
      fn rewrite_auth(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;
      fn rewrite_transport(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;
      fn patch_body<'a>(&self, body: &'a [u8], ctx: &AdapterCtx) -> Option<BodyPatch<'a>>;
  }
  ```
- `crates/gw-adapter/src/ctx.rs` — `AdapterCtx { vendor_id, vendor_key, headers_mut, url_mut, ... }`，**不**持有 Pingora 类型；`gw-proxy` 在 filter 里把 Pingora `RequestHeader` 适配进来。
- `crates/gw-adapter/src/registry.rs` — `Registry::default()` 返回单例；`register(vendor_id, Box<dyn UpstreamAdapter>)`；`for_vendor(&VendorId) -> &dyn UpstreamAdapter`，找不到返回 `Noop`。
- `crates/gw-adapter/src/noop.rs` — Noop 实现：`rewrite_auth` = 注入 `x-api-key: <vendor_key>`（保持当前 gateway 行为）；`rewrite_transport` 空操作；`patch_body` 返回 `None`。本 plan 只交付 Noop；具体 vendor 实现在后续 plan。
- ~~`crates/gw-billing/*`~~ → **撤销**。
- `crates/gw-ledger/src/freeze/trait_def.rs` —
  ```rust
  pub enum FreezeKind { Request, Task }

  pub trait FreezeGuard: Send + Sync {
      fn kind(&self) -> FreezeKind;
      fn freeze_amount(&self) -> u64;
      /// 释放冻结（不扣 deducted；deducted 由 Go 在 settle UsageRecord 时写）。
      /// 幂等：多次调用安全。
      fn release(&self);
  }
  ```
- `crates/gw-ledger/src/freeze/request.rs` — `RequestFreeze`（原 `FreezeGuard` 结构体改名，加 `impl FreezeGuard for RequestFreeze`），含 `try_freeze(uid, amount) -> Option<Self>` 工厂；保留 `Drop` 自动 release 行为；保留 `freeze_leak_recovered_total` metric。
- `crates/gw-ledger/src/freeze/task.rs` — `TaskFreeze`（trait 实现 placeholder）；本 plan 只搭壳，方法体 `todo!("see 2026-05-19-task-worker-design.md ADR-T8 — implement in worker plan")`。

**Delete**：无（保留 `gw-ledger::FreezeGuard` re-export 一段过渡期，标 `#[deprecated]` 指向 trait + RequestFreeze）。

---

## Migration Order

按依赖方向自底向上，每个 step 单独可 commit 可绿单测。

### Step 1: 在 CLAUDE.md 登记 crate layering rules

**Why first**: 后续每个 step 都要遵守这条规则；rule 没落地前实施者会无意识违反。

- [ ] 1.1 在 `CLAUDE.md` 「Things that are intentional, not bugs」之后插入「Crate layering rules」章节，登记三条：
  1. **支撑层 crate**（`gw-core` / `gw-codec` / `gw-codec-macros` / `gw-store` / `gw-routing` / `gw-auth` / `gw-ledger`）禁止引入业务 namespace（如 `task::*`、`billing::video::*`、`adapter::claude_code::*`）。支撑层提供通用机制（IR、Redis 客户端、JWT 验签、Freeze trait），不知道具体业务概念。
  2. **业务层 crate**（`gw-adapter` / `gw-health` / 未来的 `gw-task`）：每个业务独立 crate；可依赖任意支撑层；业务之间互依需 spec 显式登记。**注**：`gw-billing` 在 v2 已撤销，公式归 Go。
  3. **执行体层 crate**（`gw-proxy` / 未来的 `gw-task-runtime`）：承载 runtime/server/binary 入口；可依赖支撑层 + 业务层；执行体之间禁止互依（`gw-proxy ↔ gw-task-runtime` 双向禁，对应 worker spec ADR-T1）。
- [ ] 1.2 在同一章节加一条「Hard deployment guarantee」（对齐 worker spec ADR-T16）：gateway 配置文件与启动参数不允许出现 worker host/port；反之亦然；唯一耦合面是 Redis DSN（+ KMS KEK / PgSQL DSN）。
- [ ] 1.3 加一条「Billing boundary」（v2 新增）：Rust 网关只记录用量（UsageRecord）；用量 → cost 的转换公式与 markup chain 应用全部在 Go 端。Rust 端不允许出现任何"算 cost"的代码路径。
- [ ] 1.4 `cargo build -p gw-gateway` 仍 pass，无代码改动。

### Step 2: 新增 `gw-adapter` crate（仅骨架 + Noop）

**Why before logging.rs / upstream.rs 改造**: 这两个 filter 要调 adapter trait；trait 必须先存在。

- [ ] 2.1 `cargo new --lib crates/gw-adapter` + `Cargo.toml` 加 `gw-core`、`gw-store`、`gw-codec` 依赖。
- [ ] 2.2 实现 `trait_def.rs` / `ctx.rs` / `registry.rs` / `noop.rs`。`AdapterCtx` 不 import 任何 Pingora 类型。
- [ ] 2.3 `Registry::default()` 注册 `Noop` 为缺省实现（`for_vendor` 找不到 vendor → Noop）。
- [ ] 2.4 单测：构造 `AdapterCtx` → `Noop::rewrite_auth` 应注入 `x-api-key`；`rewrite_transport` / `patch_body` 应 no-op。
- [ ] 2.5 workspace `Cargo.toml` `members` 加入 `crates/gw-adapter`。
- [ ] 2.6 `cargo build --workspace` + `cargo test -p gw-adapter` pass。
- [ ] 2.7 `cargo clippy --workspace --all-targets -- -D warnings` pass。

### ~~Step 3: 新增 `gw-billing` crate~~（v2 **撤销**）

整体撤销。理由：用户决议公式归 Go；Rust 端不实现 cost / markup / 阶梯。桶 6 五份 spec 接管。

### Step 4: `gw-ledger::freeze` trait 化

- [ ] 4.1 把 `crates/gw-ledger/src/freeze.rs` 拆成 `freeze/{mod.rs, trait_def.rs, request.rs, task.rs}`。
- [ ] 4.2 `trait_def.rs` 写 `FreezeKind` 枚举 + `FreezeGuard` trait（method 见 File Structure）。**v2 调整**：trait 方法 `release()`（不再有 `settle(actual)`）。
- [ ] 4.3 `request.rs` 把原 `FreezeGuard` 结构体改名 `RequestFreeze`；`impl FreezeGuard for RequestFreeze`；`try_freeze(uid, amount) -> RequestFreeze` 工厂；保留 `Drop` 与 leak metric。**v2 调整**：内部不再有 `deducted_micro.fetch_add(actual, AcqRel)` 路径。
- [ ] 4.4 `task.rs` 写 `TaskFreeze` 占位：构造函数 `todo!`，方法体 `todo!`。
- [ ] 4.5 `gw-ledger::lib.rs` re-export `pub use freeze::FreezeGuard` (trait) + `pub use freeze::RequestFreeze`；老的 `FreezeGuard` 名字加 `#[deprecated(note = "use RequestFreeze + trait FreezeGuard")]` 指向 RequestFreeze 一个发布周期。
- [ ] 4.6 `cargo build -p gw-ledger` + `cargo test -p gw-ledger` pass（已有 freeze 测试用名旧名也要绿）。

### Step 5: gw-proxy 接入新 crate（v2 简化：去 gw-billing）

- [ ] 5.1 `crates/gw-proxy/Cargo.toml` 加 `gw-adapter` 依赖（去除原计划的 `gw-billing`）。
- [ ] 5.2 改 `crates/gw-proxy/src/filters/request.rs:404` 区域：`compute_freeze_estimate` 内联在 gw-ledger 或 gw-proxy 本地（不再走 `gw-billing::compute_freeze_estimate`）；freeze 构造改 `gw-ledger::RequestFreeze::try_freeze`。**注**：v2 后该函数将进一步被桶 6 C-β 接管（改读 EstimatePricing entity），本 plan 只保 trait 化迁移。
- [ ] 5.3 ~~改 `crates/gw-proxy/src/filters/logging.rs:88-95` 区域：`cost_micro` 调 `gw-billing::compute_cost`~~ → **v2 改为**：保留现状（两元公式），等桶 6 C-β spec 实施时**整段删除** cost 计算。本 plan 不动 logging。
- [ ] 5.4 改 `crates/gw-proxy/src/filters/upstream.rs` 的 `upstream_request_filter`：
  - 用 `gw-adapter::Registry::default().for_vendor(&ctx.vendor_id)` 拿 adapter；
  - 构造 `AdapterCtx`（从 Pingora 的 `upstream_request` headers/url 借引用）；
  - 调 `adapter.rewrite_auth(&mut ctx)` / `rewrite_transport`；
  - 原本写死的 `upstream_request.remove_header("x-api-key")` + `insert_header("x-api-key", key)` 由 Noop adapter 接管。
- [ ] 5.5 `cargo build -p gw-proxy` + `cargo test -p gw-proxy` pass（已有 e2e_failure_modes 不应回归）。
- [ ] 5.6 `cargo clippy --workspace --all-targets -- -D warnings` pass。

### Step 6: 验收与文档收尾

- [ ] 6.1 跑完整 `cargo test --workspace` + `--workspace -- --ignored`（如果本机 Docker 可用），全部绿。
- [ ] 6.2 跑 `cargo bench --workspace --no-run`（签名不漂）。
- [ ] 6.3 改 `docs/superpowers/specs/2026-05-19-adapter-layer-design.md` ADR-A1 把 trait 归宿从 `gw-proxy` 改为 `gw-adapter`（同步 spec 与现实）。
- [ ] 6.4 `git diff --stat` 复核：本 plan 应只动 `Cargo.toml` × 2（不再 × 3，因为去掉 gw-billing）、`CLAUDE.md`、`gw-adapter/*` 新增、`gw-ledger/src/freeze*` 重构、`gw-proxy/src/filters/{request,upstream}.rs` 接入，不应碰其他 crate。

---

## Risk Register

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| ~~`gw-billing` 移植后 cost_micro 数值漂移~~ | 低 | 中 | **v2 撤销**——cost 算法整体不进 Rust，无此风险 |
| `gw-adapter::Noop` 行为与现有 hard-coded `x-api-key` 注入不完全一致 | 中 | 高 | Step 5.5 跑现有 e2e；Step 5.4 review 时对照 `filters/upstream.rs:357` 的 remove/insert 序列 |
| `FreezeGuard` trait 化破坏 ABI（现有调用方按结构体语法） | 低 | 低 | Step 4.5 通过 re-export + `#[deprecated]` 兼容一个发布周期；调用方 Step 5.2 同步迁 |
| `FreezeGuard::release()` 改名漏改调用方 | 中 | 中 | Step 4.6 + Step 5.5 grep 所有 `settle(` 调用；编译期会报错 |
| `gw-adapter::AdapterCtx` 设计不足以承载所有 vendor 改写诉求 | 中 | 中 | 本 plan 只交付 Noop；具体 vendor 实现时回头扩 ctx |

---

## Acceptance Criteria

- `cargo build --workspace` 在 Rust 1.86 上 pass。
- `cargo clippy --workspace --all-targets -- -D warnings` pass。
- `cargo test --workspace` pass，包含：
  - `gw-adapter` Noop 单测；
  - `gw-ledger` 现有 freeze 测试通过 trait 路径仍绿（`settle(actual)` 调用全部改 `release()`）；
  - `gw-proxy` 现有 filter 测试不回归。
- `git grep -nE 'task::|billing::video' crates/gw-store crates/gw-core crates/gw-codec crates/gw-routing crates/gw-auth crates/gw-ledger` 应为空（支撑层无业务 namespace）。
- `git grep -n 'gw-billing' Cargo.toml crates` 应为空（撤销验证）。
- CLAUDE.md 三条新规则（含 Billing boundary）存在并被 git diff 显示。
- spec `2026-05-19-adapter-layer-design.md` ADR-A1 文字已更新指向 `gw-adapter`。

---

## Follow-ups（不在本 plan，登记到后续）

1. **vendor adapter 具体实现**：claude_code OAuth、gemini_cli quota、vertex_ai SA JWT、antigravity URL probe、codex image_transform、kiro EventStream。每个 vendor 一个独立 plan。
2. ~~**billing 多维公式扩展**~~ → **撤销**（Go 端实施）。
3. **桶 6 五份 spec 实施 plan**：C-α UsageRecord schema、C-β EstimatePricing、C-γ 余额读写分工、C-δ DLQ。
4. **`gw-task` / `gw-task-runtime` / `gw-task-worker`**：worker spec 实施 plan。本 plan 完成后才能开始。
5. **`gw-engine` 抽出**：可选优化，单独 plan。

---

> 本 plan 仅交付 crate 骨架与依赖图改造；任何业务规则的"具体实现"（vendor adapter、task worker、桶 6 五份 spec）都在 follow-up plan。


**Tech Stack**：Rust 1.86，沿用 sqlx runtime-checked、fred 9.4、moka 0.12、metrics 0.23 不引新依赖。

**Out of scope**（属于后续 plan，不在本次）：
- `gw-task` / `gw-task-runtime` / `gw-task-worker` 三个 crate 的实现；
- adapter 的 6 个具体实现（claude_code/codex/gemini_cli/vertex_ai/antigravity/kiro），本 plan 只搭骨架；
- billing 的具体新公式（cache_creation 差价、video 时长计费），本 plan 只搭骨架；
- `gw-engine` 抽出（建议但非硬依赖，单独 plan）。

---

## File Structure

**Modify**：

- `Cargo.toml`（workspace）— 注册 `gw-adapter` / `gw-billing`。
- `crates/gw-proxy/Cargo.toml` — 加 `gw-adapter` / `gw-billing` 依赖；保留 `gw-ledger`。
- `crates/gw-proxy/src/filters/upstream.rs` — 把 `upstream_request_filter` 里写死的 auth 注入挪到 `gw-adapter::Registry::default_for_vendor(vendor_id).rewrite_auth(...)`。
- `crates/gw-proxy/src/filters/logging.rs` — `cost_micro` 计算改调 `gw-billing::compute_cost(pricing, usage, markup_chain)`。
- `crates/gw-proxy/src/filters/request.rs` — `FreezeGuard::freeze(uid, est)` 改调 `gw-ledger::freeze::RequestFreeze::try_freeze(...)` 实现 trait。
- `crates/gw-ledger/src/lib.rs` — 重新导出 `FreezeGuard` trait（取代结构体）；保留 `BillingRecord` / `Ledger`。
- `crates/gw-ledger/src/freeze.rs` — 拆成 `mod trait_def`（trait + FreezeKind 枚举） + `mod request`（RAII 实现，原 `FreezeGuard` 改名 `RequestFreeze`） + `mod task`（trait 接口预留，body 是 `todo!("v1.2 worker plan")`）。
- `CLAUDE.md` — "Things that are intentional, not bugs" 章节后插入 "Crate layering rules"（ADR-018 三条新规则）。

**New**：

- `crates/gw-adapter/Cargo.toml` — 依赖 `gw-core`、`gw-store`、`gw-codec`。
- `crates/gw-adapter/src/lib.rs` — 导出 trait + 注册表 + Noop 默认实现。
- `crates/gw-adapter/src/trait_def.rs` — `UpstreamAdapter` trait（与 adapter-layer spec ADR-A1 对齐）：
  ```rust
  pub trait UpstreamAdapter: Send + Sync + 'static {
      fn name(&self) -> &'static str;
      fn rewrite_auth(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;
      fn rewrite_transport(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;
      fn patch_body<'a>(&self, body: &'a [u8], ctx: &AdapterCtx) -> Option<BodyPatch<'a>>;
  }
  ```
- `crates/gw-adapter/src/ctx.rs` — `AdapterCtx { vendor_id, vendor_key, headers_mut, url_mut, ... }`，**不**持有 Pingora 类型；`gw-proxy` 在 filter 里把 Pingora `RequestHeader` 适配进来。
- `crates/gw-adapter/src/registry.rs` — `Registry::default()` 返回单例；`register(vendor_id, Box<dyn UpstreamAdapter>)`；`for_vendor(&VendorId) -> &dyn UpstreamAdapter`，找不到返回 `Noop`。
- `crates/gw-adapter/src/noop.rs` — Noop 实现：`rewrite_auth` = 注入 `x-api-key: <vendor_key>`（保持当前 gateway 行为）；`rewrite_transport` 空操作；`patch_body` 返回 `None`。本 plan 只交付 Noop；具体 vendor 实现在后续 plan。
- `crates/gw-billing/Cargo.toml` — 依赖 `gw-core`（Pricing struct、UsageIR）；不依赖 `gw-store`。
- `crates/gw-billing/src/lib.rs` — 导出 `compute_cost(pricing, usage, markup_chain) -> u64` + `compute_freeze_estimate(pricing, est_in, est_out) -> u64` + `MarkupChain`（user_group / api_key / vendor / model 四级倍率连乘的封装）。
- `crates/gw-billing/src/cost.rs` — 当前两元公式实现（in*p_in + out*p_out）；公开 trait `CostFormula` 留给后续扩展（多维定价、video 时长）。
- `crates/gw-ledger/src/freeze/trait_def.rs` —
  ```rust
  pub enum FreezeKind { Request, Task }

  pub trait FreezeGuard: Send + Sync {
      fn kind(&self) -> FreezeKind;
      fn freeze_amount(&self) -> u64;
      fn settle(&self, actual_micro: u64);
      fn unfreeze(&self);  // 显式释放，幂等
  }
  ```
- `crates/gw-ledger/src/freeze/request.rs` — `RequestFreeze`（原 `FreezeGuard` 结构体改名，加 `impl FreezeGuard for RequestFreeze`），含 `try_freeze(uid, amount) -> Option<Self>` 工厂；保留 `Drop` 自动 unfreeze 行为；保留 `freeze_leak_recovered_total` metric。
- `crates/gw-ledger/src/freeze/task.rs` — `TaskFreeze`（trait 实现 placeholder）；本 plan 只搭壳，方法体 `todo!("see 2026-05-19-task-worker-design.md ADR-T8 — implement in worker plan")`。

**Delete**：无（保留 `gw-ledger::FreezeGuard` re-export 一段过渡期，标 `#[deprecated]` 指向 trait + RequestFreeze）。

---

## Migration Order

按依赖方向自底向上，每个 step 单独可 commit 可绿单测。

### Step 1: 在 CLAUDE.md 登记 crate layering rules

**Why first**: 后续每个 step 都要遵守这条规则；rule 没落地前实施者会无意识违反。

- [ ] 1.1 在 `CLAUDE.md` 「Things that are intentional, not bugs」之后插入「Crate layering rules」章节，登记三条：
  1. **支撑层 crate**（`gw-core` / `gw-codec` / `gw-codec-macros` / `gw-store` / `gw-routing` / `gw-auth` / `gw-ledger`）禁止引入业务 namespace（如 `task::*`、`billing::video::*`、`adapter::claude_code::*`）。支撑层提供通用机制（IR、Redis 客户端、JWT 验签、Freeze trait），不知道具体业务概念。
  2. **业务层 crate**（`gw-adapter` / `gw-billing` / 未来的 `gw-task`）：每个业务独立 crate；可依赖任意支撑层；业务之间互依需 spec 显式登记。
  3. **执行体层 crate**（`gw-proxy` / 未来的 `gw-task-runtime`）：承载 runtime/server/binary 入口；可依赖支撑层 + 业务层；执行体之间禁止互依（`gw-proxy ↔ gw-task-runtime` 双向禁，对应 worker spec ADR-T1）。
- [ ] 1.2 在同一章节加一条「Hard deployment guarantee」（对齐 worker spec ADR-T16）：gateway 配置文件与启动参数不允许出现 worker host/port；反之亦然；唯一耦合面是 Redis DSN（+ KMS KEK / PgSQL DSN）。
- [ ] 1.3 `cargo build -p gw-gateway` 仍 pass，无代码改动。

### Step 2: 新增 `gw-adapter` crate（仅骨架 + Noop）

**Why before logging.rs / upstream.rs 改造**: 这两个 filter 要调 adapter trait；trait 必须先存在。

- [ ] 2.1 `cargo new --lib crates/gw-adapter` + `Cargo.toml` 加 `gw-core`、`gw-store`、`gw-codec` 依赖。
- [ ] 2.2 实现 `trait_def.rs` / `ctx.rs` / `registry.rs` / `noop.rs`。`AdapterCtx` 不 import 任何 Pingora 类型。
- [ ] 2.3 `Registry::default()` 注册 `Noop` 为缺省实现（`for_vendor` 找不到 vendor → Noop）。
- [ ] 2.4 单测：构造 `AdapterCtx` → `Noop::rewrite_auth` 应注入 `x-api-key`；`rewrite_transport` / `patch_body` 应 no-op。
- [ ] 2.5 workspace `Cargo.toml` `members` 加入 `crates/gw-adapter`。
- [ ] 2.6 `cargo build --workspace` + `cargo test -p gw-adapter` pass。
- [ ] 2.7 `cargo clippy --workspace --all-targets -- -D warnings` pass。

### Step 3: 新增 `gw-billing` crate（仅骨架 + 移植两元公式）

- [ ] 3.1 `cargo new --lib crates/gw-billing` + 加 `gw-core` 依赖（用 `Pricing` / `UsageIR`）。
- [ ] 3.2 `src/cost.rs` 移植 `gw-proxy::filters::logging.rs` 现有的 `in_cost + out_cost` 公式到 `fn compute_cost(pricing: &Pricing, usage: &UsageIR, markup: &MarkupChain) -> u64`。
- [ ] 3.3 `src/freeze.rs` 移植 `gw-proxy::filters::request.rs:404` 的 `compute_freeze_estimate(pricing, est_in, est_out) -> u64`。
- [ ] 3.4 `MarkupChain` 暂只承载 `model_markup * vendor_markup * group_markup * key_markup`，全 1.0 默认；后续 plan 接入用户分组倍率。
- [ ] 3.5 单测：与现有 logging.rs 用同一组输入应得同一个 cost_micro（回归保护）。
- [ ] 3.6 workspace members 加入 `crates/gw-billing`。
- [ ] 3.7 `cargo build --workspace` + `cargo test -p gw-billing` + clippy pass。

### Step 4: `gw-ledger::freeze` trait 化

- [ ] 4.1 把 `crates/gw-ledger/src/freeze.rs` 拆成 `freeze/{mod.rs, trait_def.rs, request.rs, task.rs}`。
- [ ] 4.2 `trait_def.rs` 写 `FreezeKind` 枚举 + `FreezeGuard` trait（method 见 File Structure）。
- [ ] 4.3 `request.rs` 把原 `FreezeGuard` 结构体改名 `RequestFreeze`；`impl FreezeGuard for RequestFreeze`；`try_freeze(uid, amount) -> RequestFreeze` 工厂；保留 `Drop` 与 leak metric。
- [ ] 4.4 `task.rs` 写 `TaskFreeze` 占位：构造函数 `todo!`，方法体 `todo!`。
- [ ] 4.5 `gw-ledger::lib.rs` re-export `pub use freeze::FreezeGuard` (trait) + `pub use freeze::RequestFreeze`；老的 `FreezeGuard` 名字加 `#[deprecated(note = "use RequestFreeze + trait FreezeGuard")]` 指向 RequestFreeze 一个发布周期。
- [ ] 4.6 `cargo build -p gw-ledger` + `cargo test -p gw-ledger` pass（已有 freeze 测试用名旧名也要绿）。

### Step 5: gw-proxy 接入新 crate

- [ ] 5.1 `crates/gw-proxy/Cargo.toml` 加 `gw-adapter`、`gw-billing` 依赖。
- [ ] 5.2 改 `crates/gw-proxy/src/filters/request.rs:404` 区域：`compute_freeze_estimate` 调 `gw-billing::compute_freeze_estimate`；freeze 构造改 `gw-ledger::RequestFreeze::try_freeze`。
- [ ] 5.3 改 `crates/gw-proxy/src/filters/logging.rs:88-95` 区域：`cost_micro` 调 `gw-billing::compute_cost`。
- [ ] 5.4 改 `crates/gw-proxy/src/filters/upstream.rs` 的 `upstream_request_filter`：
  - 用 `gw-adapter::Registry::default().for_vendor(&ctx.vendor_id)` 拿 adapter；
  - 构造 `AdapterCtx`（从 Pingora 的 `upstream_request` headers/url 借引用）；
  - 调 `adapter.rewrite_auth(&mut ctx)` / `rewrite_transport`；
  - 原本写死的 `upstream_request.remove_header("x-api-key")` + `insert_header("x-api-key", key)` 由 Noop adapter 接管。
- [ ] 5.5 `cargo build -p gw-proxy` + `cargo test -p gw-proxy` pass（已有 e2e_failure_modes 不应回归）。
- [ ] 5.6 `cargo clippy --workspace --all-targets -- -D warnings` pass。

### Step 6: 验收与文档收尾

- [ ] 6.1 跑完整 `cargo test --workspace` + `--workspace -- --ignored`（如果本机 Docker 可用），全部绿。
- [ ] 6.2 跑 `cargo bench --workspace --no-run`（签名不漂）。
- [ ] 6.3 改 `docs/superpowers/specs/2026-05-19-adapter-layer-design.md` ADR-A1 把 trait 归宿从 `gw-proxy` 改为 `gw-adapter`（同步 spec 与现实）。
- [ ] 6.4 `git diff --stat` 复核：本 plan 应只动 `Cargo.toml` × 3、`CLAUDE.md`、`gw-adapter/*` 新增、`gw-billing/*` 新增、`gw-ledger/src/freeze*` 重构、`gw-proxy/src/filters/{request,logging,upstream}.rs` 接入，不应碰其他 crate。

---

## Risk Register

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| `gw-billing` 移植后 cost_micro 数值漂移 | 低 | 中 | Step 3.5 强制对比单测；用 e2e_failure_modes 现有 fixture 跑一遍人工 sanity |
| `gw-adapter::Noop` 行为与现有 hard-coded `x-api-key` 注入不完全一致 | 中 | 高 | Step 5.5 跑现有 e2e；Step 5.4 review 时对照 `filters/upstream.rs:357` 的 remove/insert 序列 |
| `FreezeGuard` trait 化破坏 ABI（现有调用方按结构体语法） | 低 | 低 | Step 4.5 通过 re-export + `#[deprecated]` 兼容一个发布周期；调用方 Step 5.2 同步迁 |
| `gw-adapter::AdapterCtx` 设计不足以承载所有 vendor 改写诉求（OAuth refresh 异步性、kiro EventStream 流式） | 中 | 中 | 本 plan 只交付 Noop；具体 vendor 实现时回头扩 ctx，spec ADR-A1 已经留了 patch_body / Future 异步钩子 |
| 业务 crate 之间隐式互依（gw-billing 用了 gw-adapter） | 低 | 低 | spec 已禁；CI 加 `cargo deny check bans` 规则或人工 review |

---

## Acceptance Criteria

- `cargo build --workspace` 在 Rust 1.86 上 pass。
- `cargo clippy --workspace --all-targets -- -D warnings` pass。
- `cargo test --workspace` pass，包含：
  - `gw-adapter` Noop 单测；
  - `gw-billing` cost_micro 与历史值一致的回归测试；
  - `gw-ledger` 现有 freeze 测试通过 trait 路径仍绿；
  - `gw-proxy` 现有 filter 测试不回归。
- `git grep -n 'crate gw-store' crates/gw-adapter crates/gw-billing` 应为空（业务层不依赖 store 命名空间内的业务表）。**注**：gw-adapter 可依赖 gw-store 作为通用 Redis 客户端（OAuth token 缓存），但禁止引用 gw-store 内的 task/billing/adapter 命名空间——只用 generic Entity/Counter helper。
- `git grep -nE 'task::|billing::video' crates/gw-store crates/gw-core crates/gw-codec crates/gw-routing crates/gw-auth crates/gw-ledger` 应为空（支撑层无业务 namespace）。
- CLAUDE.md "Crate layering rules" 章节存在并被 git diff 显示。
- spec `2026-05-19-adapter-layer-design.md` ADR-A1 文字已更新指向 `gw-adapter`。

---

## Follow-ups（不在本 plan，登记到后续）

1. **vendor adapter 具体实现**：claude_code OAuth、gemini_cli quota、vertex_ai SA JWT、antigravity URL probe、codex image_transform、kiro EventStream。每个 vendor 一个独立 plan。
2. **billing 多维公式扩展**：cache_creation / cache_read / thinking / audio / image / video duration / 阶梯 / markup 链。每个公式族一个独立 plan。
3. **`gw-task` / `gw-task-runtime` / `gw-task-worker`**：worker spec 实施 plan。本 plan 完成后才能开始。
4. **`gw-engine` 抽出**：可选优化，单独 plan。

---

> 本 plan 仅交付 crate 骨架与依赖图改造；任何业务规则的"具体实现"（vendor adapter、billing 公式、task worker）都在 follow-up plan。
