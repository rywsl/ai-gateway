# 计费记录持久化(billing record durability)— v2026-05-29

- **日期**:2026-05-29
- **状态**:Draft(待审阅)
- **作者**:协作设计(Claude + 用户)
- **关联**:
  - **扩展**:[`2026-05-24-billing-robustness-design.md`](./2026-05-24-billing-robustness-design.md)(InflightLedger / VendorExposure / must-deliver 基础设施在那里)
  - **正交**:[`2026-05-28-systemic-fixes-design.md`](./2026-05-28-systemic-fixes-design.md)(其附录 B 明确不接管 InflightLedger watchdog;附录 A 把 "G11 follow-up 依赖 Go billing" 列为 out of scope — 本 spec 处理的正是这片被划走的领地)
  - **本仓代码触点**:
    - `crates/gw-ledger/src/flusher.rs`(`Ledger::emit` / `flush` / `sync_xadd` / `dlq_xadd`)
    - `crates/gw-ledger/src/inflight.rs`(`register` / `mark_settled` / `clear` / 24h TTL 安全网)
    - `crates/gw-proxy/src/gateway.rs:1080-1150`(`logging` 方法:`filters::logging::run` → `mark_settled` → `clear` 顺序)
    - `crates/gw-proxy/src/gateway.rs:1028-1058`(`fail_to_connect` 重试 `tokio::spawn`)
    - `crates/gw-proxy/src/billing_lifecycle.rs:248-292`(`on_retry` async 变体)
    - `crates/gw-proxy/src/filters/logging.rs:295`(`ledger.emit(rec)` 调用点)
    - `crates/gw-ledger/src/inflight_reconciler.rs`(启动期 `CrashFallback` 唯一 emit fallback)
    - `crates/gw-ledger/src/inflight_reaper.rs`(T0 emit-lag watchdog)

---

## 1. 背景与动机

### 1.1 现状审计

`2026-05-24-billing-robustness` 落地了 must-deliver、InflightLedger、T0 watchdog、启动 reconciler。该 spec §5.4(line 263)把 `inflight.clear()` 的契约写作 "emit UsageRecord 成功后调用"。审计实际实现发现 **"emit 成功" 的语义比契约预期弱**:

- `crates/gw-ledger/src/flusher.rs:75-99` 的 `emit` 在 fast path 上只把 record `try_send` 进进程内 mpsc 就返回 `Ok(())`,真正 XADD 到 Redis 是后台 flusher 任务异步批量做的(`flusher.rs:43-67`,默认 `flush_interval_ms=200` / `batch_size=200`)。
- `crates/gw-proxy/src/gateway.rs:1101-1149` 的 `logging` 方法在 `emit().await` 返回**之后立刻**对 inflight key 做 `mark_settled` + `clear`。
- 因此存在如下崩溃窗口:emit 入队 → `mark_settled` → **`clear` 已 DEL 掉 inflight key** → 后台 flusher 尚未 XADD → 进程崩。结果:记录死在 mpsc 内存里;inflight key 已删,启动期 reconciler 与 T0 watchdog 都扫不到。**记录与恢复锚点同时消失。**

源代码注释亦认知此矛盾(`gateway.rs:1136-1138`):

> Clear runs unconditionally here. If the UsageRecord emit was dropped due to mpsc-full (the current Ledger try_send drop-on-full behavior), the inflight key is lost too — W2-T7 closes that gap with the must-deliver sync bypass.

W2-T7 关闭的是**背压丢弃**(mpsc full → 静默 drop),但 fast path 的"入队即返回 → clear → XADD"序列并未涉及背压,所以这条注释没有覆盖结构层的崩溃窗口。

### 1.2 本 spec 处理的三个洞

| 编号 | 缺口 | 性质 |
|------|------|------|
| **A1** | `emit` fast path 返回 ≠ 记录已持久化,而 `clear` 紧随其后 DEL 掉恢复锚点;崩溃于窗口内 → 记录丢且不可恢复 | 结构性 |
| **A2** | `fail_to_connect`(`gateway.rs:1035-1057`)把"重试旧 attempt 的 mark_settled + emit + clear"包进 `tokio::spawn` fire-and-forget;若 spawn 未完成即进程关停 → 与 A1 同源的丢失 | 派生 |
| **A3** | flusher 任务 panic 后,后续 `tx.try_send` 走 `Closed` 分支(`flusher.rs:94-97`)只 log 不兜底,既不 sync XADD 也不 DLQ → 后续所有记录静默丢失 | 韧性 |

### 1.3 不在本 spec 范围

- **B 家族**(`usage:detailed` 归档保真:下游响应头永空、上游凭证头未捕获、流中断时响应面整段不归档)—— 归档本就是 drop-on-outage,不与本 spec 的 must-deliver 混治。
- **C 家族**(`frozen_micro` 不落 Redis、`rate_limited` / `ipblock` 反向 XADD 缺失、`identity_snapshot` 不 bump version / 不发 events:entity)—— 契约写了 / 跨副本一致性问题,与"账务记录是否到达 Go"正交。
- **Redis 整体宕机的本地磁盘 spool**(C-δ 三层方案)—— `architecture.md` 已 by-design 接受 drop + metric;改动面 + 新增 fsync/回放循环过大。
- **Go-side global reaper** —— `2026-05-24-billing-robustness` §5.4.3 已列为"推荐 Go 侧能力",不在 Rust spec 范围。

---

## 2. 核心不变量

> **I1**:`billing:inflight:<instance>:<request_id>:<attempt>` 这一 hash 的 `DEL` **只由 ledger 子系统在记录确认持久化到 `usage:records` 主流之后**发出。proxy(包括 logging filter、`fail_to_connect`、`on_retry`)**永不 eager-clear**。

由 I1 直接得到:

> **I2**:对任何已 register 过 inflight key 的请求,以下二者**至少有一个**为真:① 对应 UsageRecord 已在 `usage:records` 主流中;② inflight key 仍在 Redis 中(可被启动 reconciler / Go-side global reaper / T0 watchdog 观测到)。

亦即:**记录丢失 ⟹ inflight key 必然还在 ⟹ 必然可被补回**。

副作用:crash-after-XADD-before-DEL 这一窗口可能产生一条 `CrashFallback` 重复,由 `2026-05-24-billing-robustness-design.md` §8.2 既有去重契约(`request_id` + `attempt_index` + provenance 等级)承担(真实 record 的 provenance 高于 `CrashFallback`,Go 端 reconcile 时覆盖)。即 **at-least-once + 既有去重**,不引入新去重逻辑(详见 §附录 C 的论证)。

---

## 3. 设计

### 3.1 channel 载荷:`LedgerItem`

`crates/gw-ledger/src/flusher.rs` 内部 channel 类型从 `mpsc::Sender<UsageRecord>` 改为 `mpsc::Sender<LedgerItem>`:

```rust
pub struct LedgerItem {
    pub record: UsageRecord,
    /// `Some` 表示 record 携带 inflight 恢复锚点,ledger 须在 XADD 主流成功
    /// 之后 DEL 之;`None` 用于不带锚点的兜底场景(如启动 reconciler 自身
    /// 发出的 CrashFallback — 它直接掌管 inflight key 生命周期)。
    pub clear: Option<ClearKey>,
}

pub struct ClearKey {
    pub request_id: String,
    pub attempt_index: u32,
}
```

`Ledger` 结构体新增 `instance_id: String` 字段(从 `gw-ledger::instance_id::resolve` 获得,与 `inflight_reaper` / `inflight_reconciler` 同源),用以拼接 `billing:inflight:<instance_id>:<req>:<attempt>`。

**wire schema 零变化**:XADD 写入 `usage:records` 的 payload 仍是序列化后的 `UsageRecord`;`ClearKey` 与 `instance_id` 都活在进程内,不暴露。Go 端无任何感知。

### 3.2 统一的 "DEL rule"

下文 §3.3 / §3.4 / §3.5 等多处都要遵循同一条规则,提取出来一次定义:

> **DEL rule**:对一个 `LedgerItem`,**当且仅当** `item.clear.is_some()` 且其 `record` 已通过 **§3.3 主 flush XADD 或 §3.4 `sync_xadd` bypass 任一路径**成功写入 `usage:records` 主流时,才发出 `DEL billing:inflight:<instance_id>:<request_id>:<attempt_index>`。
>
> 所有降级(进 DLQ、`gateway_usage_records_lost_total`)路径**一律不 DEL**——把锚点留给 reconciler / T0 watchdog。
>
> DEL 自身失败(网络抖动 / Redis 暂时拒绝)只 warn + 计数,不重试,不阻塞业务——锚点残留无害,reconciler 兜。

### 3.3 主 flush 序列(channel 路径)

```text
flush(buf: Vec<LedgerItem>):
  1. let xadd_pipeline = pipeline { XADD usage:records <json(item.record)>  for item in buf }
  2. let xadd_results = xadd_pipeline.exec().await
  3. let mut to_clear = Vec::new()
     for (item, result) in buf.iter().zip(xadd_results):
         match result:
           Ok(_)  => if let Some(k) = &item.clear { to_clear.push(k) }
           Err(_) =>
             dlq_xadd(item.record, reason="primary_xadd_failed").await
             // 不 push 进 to_clear — DLQ 路径按 DEL rule 不 DEL
             if dlq_xadd fail: gateway_usage_records_lost_total++
  4. if !to_clear.is_empty():
         let del_pipeline = pipeline { DEL <key> for key in to_clear }
         let del_results = del_pipeline.exec().await
         for (key, r) in to_clear.iter().zip(del_results):
             metrics.inc("gw_billing_inflight_clear_total",
                          outcome = if r.is_ok() { "ok" } else { "del_failed" })
```

实现细节:Pipeline 不使用 MULTI/EXEC——XADD 与 DEL 并不需要原子化(去重契约已承担 crash 窗口的 dup),用普通 pipeline 即可,失败分类更直观。

### 3.4 `Ledger::emit` 的 bypass 路径

现状 `flusher.rs:75-99` 的 `emit`:Full → `sync_xadd` → DLQ → lost-counter。改动:

1. 签名:`emit(item: LedgerItem)`。
2. **Full 分支**:`sync_xadd(item.record)` 成功后,**按 DEL rule 立即 DEL**(同一 `Ledger::redis` 句柄,async pipeline);失败走 DLQ;DLQ 失败走 lost-counter。任一降级路径都不 DEL。
3. **Closed 分支**(原仅 log + drop):**新行为**与 Full 对称——走 `sync_xadd` → DLQ → lost-counter,并按 DEL rule 行事。同时 `gw_billing_flusher_dead_total++`(诊断信号,任何非零 = page on-call)。
4. 加诊断 metric `gw_billing_emit_path_total{path="sync_bypass_full" | "sync_bypass_closed" | "channel"}`(扩展现有 label)。

**为什么 Closed 也走 sync_xadd**:flusher 任务死掉(panic 穿透或 channel 异常 close)是真实可能。当前的"只 log 不兜"违背 must-deliver 字面承诺。与 Full 对称是最低代价的修法。

### 3.5 proxy logging 路径

`crates/gw-proxy/src/filters/logging.rs:295` 现状:`let _ = ledger.emit(rec).await;`。改为:

```rust
let clear = Some(ClearKey {
    request_id: ctx.request_id.0.clone(),
    attempt_index: ctx.attempt_index,
});
let _ = ledger.emit(LedgerItem { record: rec, clear }).await;
```

`crates/gw-proxy/src/gateway.rs:1118-1150` 的 `mark_settled` + `clear` 块:

```diff
 if ctx.picked_vendor_key.is_some() {
     // ... settle_state 计算 ...
     if let Err(e) = self.inflight.mark_settled(&req_id, attempt, settle_state).await {
         warn!(...);
     }
-    if let Err(e) = self.inflight.clear(&req_id, attempt).await {
-        warn!(...);
-    }
 }
```

`mark_settled` **保留** —— 它 HSET `ts_settled_ms` 进 inflight hash,T0 watchdog 用此判断"已 settle 但 key 久未 DEL"。在新模型下,T0 触发的语义更精确:"emit pipeline 卡死"(因 DEL 只能由 ledger 在 XADD 成功后发出,所以 settled 后久不 DEL = flusher 真的卡了或挂了)。

### 3.6 A2 重试路径

`fail_to_connect`(`gateway.rs:1028-1058`)与 `on_retry`(`billing_lifecycle.rs:248-292`)的 spawn 体:

```diff
 tokio::spawn(async move {
     if let Err(e) = inflight.mark_settled(&req_id, old_attempt, Terminated).await {
         warn!(...);
     }
-    ledger.emit(rec).await;
-    if let Err(e) = inflight.clear(&req_id, old_attempt).await {
-        warn!(...);
-    }
+    ledger.emit(LedgerItem {
+        record: rec,
+        clear: Some(ClearKey {
+            request_id: req_id.clone(),
+            attempt_index: old_attempt,
+        }),
+    }).await;
 });
```

**A2 由 I1 直接关闭**:即使整个 spawn 因进程关停或 runtime cancel 而丢,旧 attempt 的 inflight key 已 register 且没人清(I1 禁止 proxy clear,且这条路径根本不再有 proxy-side clear)→ 启动 reconciler 在重启时 emit `CrashFallback` 补回。**保留 `tokio::spawn`** —— Pingora 的 `fail_to_connect` 是同步 hook,改成 await 会阻塞连接重试热路径;在新模型下 spawn 不再是丢失源。

### 3.7 A3 flusher 韧性

**3.7.1 单 tick panic 隔离**(`flusher.rs:46-65` 主循环):用 `std::panic::AssertUnwindSafe` + `futures::FutureExt::catch_unwind` 包住 `flush(&worker_redis, &mut buf)` 调用(每次 batch 满和 ticker 触发都是)。panic → `gw_billing_flusher_panic_total++`,warn,**继续主循环**。channel 保持开放,后续 `emit` 仍走 channel 路径。

**3.7.2 `Closed` 分支不再静默丢**:见 §3.4 步骤 3。

**3.7.3 graceful shutdown 最后一次清理**(`flusher.rs:56-58` 的 `None` 分支):rx 收到 `None` 时已经会 flush 残余 buffer;**确保该 flush 也走完 §3.3 的 DEL 阶段**,把 `to_clear` 一次清干净。避免重启被无意义的 CrashFallback dup 风暴(虽然 Go 会去重,但减少噪音 + Redis 写入量)。

### 3.8 reconciler / T0 watchdog 的连带行为

**无接口改动**,但语义略变,需写进 architecture §Deliberate behaviours:

- **T0 watchdog 触发更精确**:`ts_settled_ms` 已设但 key 久不 DEL,**严格等价于** ledger 没把对应 record 送进 `usage:records`(因 DEL 只能由 ledger 在 XADD 成功后发出)。这条诊断语义比改前更强。
- **启动 reconciler 在新模型下会扫到 "已 settled、`ts_settled_ms` 刚被设" 的 key**——即崩溃发生在 `mark_settled` 已跑、ledger 还没把它 XADD 完的窗口。这些 key 的 `CrashFallback` emit 在 Go 端会**被随后真正的 record 覆盖**(若该 record 在崩溃前已 XADD;若没,CrashFallback 即是终态)。一切按 billing-robustness §6 / §8.2 的去重规则进行,无需特别处理。

---

## 4. 崩溃覆盖矩阵

| 崩溃时点 | UsageRecord 状态 | inflight key 状态 | 恢复路径 |
|------|------------------|--------------------|-----------|
| `upstream_request_filter` register 后,emit 入队前 | 不存在 | 在,无 `ts_settled_ms` | reconciler emit `CrashFallback`(`input=prompt_estimate, output=0`) ✅ |
| emit 入队后,XADD 前 | 在 mpsc(进程崩失) | 在,有 `ts_settled_ms` | reconciler emit `CrashFallback` ✅ |
| XADD 后,DEL 前 | **已落 `usage:records`** ✅ | 在,有 `ts_settled_ms` | reconciler emit `CrashFallback` → Go 去重按 provenance 等级保留真实 record(详见附录 C)✅(at-least-once) |
| DEL 后 | 已落 `usage:records` | 已删 | 终态 ✅ |
| XADD 失败 → DLQ 成功 | 在 `usage:records:dlq` | 在(故意保留) | Go 回放 DLQ;若 DLQ 修剪,reconciler 兜 ✅ |
| XADD 失败 → DLQ 也失败 | 丢(`gateway_usage_records_lost_total++`) | 在(故意保留) | reconciler emit `CrashFallback`(降级估算)✅ |
| flusher 单 tick panic | 该 tick 的 batch 残留 buf,catch_unwind 后下一次 flush 重试;若崩在 panic 与下次 flush 之间,落入第 2 行 | 在 | 第 2 行 / 下一 flush ✅ |
| flusher 任务真死(channel `Closed`) | 后续 emit 走 sync XADD / DLQ / lost-counter | 在 / 按 DEL rule | 与 Full 分支同等可达 ✅ |

→ **所有格子都不存在"记录丢 + 锚点丢"的组合。**

---

## 5. Deliberate choices

- **at-least-once + 去重而非 exactly-once**:exactly-once 在分布式语境下要求两阶段提交或事务 outbox,Redis Streams 不是事务边界。利用既有 Go 去重契约更省事,且 dup 仅在崩溃窗口内出现,量级小。
- **DEL 用 pipeline 而非 MULTI/EXEC**:MULTI 让 XADD 与 DEL 原子化能消除 crash-after-XADD-before-DEL 的 dup,但会牺牲 batch 内 XADD 部分失败的分类(MULTI 不支持"成功者继续、失败者降级"),且 EVAL/Lua 会让逻辑复杂化。重复由现有去重承担更简单。
- **`fail_to_connect` 保留 `tokio::spawn`**:同步 Pingora hook 内做 async await 会阻塞连接处理。spawn 在新模型下不再是丢失源——锚点不被 spawn 内 clear,所以丢 spawn = 锚点保留 = reconciler 补。
- **`LedgerItem` 而非把 `clear_key` 加进 `UsageRecord`**:wire schema 不承担进程内调度状态。`UsageRecord` 序列化产物对 Go 端 byte-for-byte 不变,无契约变动。
- **flusher 不对 DEL 失败做 retry**:DEL 失败的"惩罚"已被 reconciler / T0 接管,内联 retry 增加复杂度无收益。
- **`mark_settled` 留在 proxy 而非下沉 ledger**:T0 watchdog 需要 `ts_settled_ms` 标记"应该 emit 了";在 proxy 同步设置最准。它的 HSET 与后续 DEL 不再竞争(中间隔着 ledger XADD)。
- **Closed 分支与 Full 对称**:must-deliver 的语义不应区分"channel 满了"和"channel 死了"——两者都不是"丢"的理由。
- **不引入新去重逻辑**:Go 端 §8.2 的 `(request_id, attempt_index)` + provenance 等级足以承担本 spec 引入的 dup 窗口(详见附录 C)。
- **不改 `usage:records` / `usage:records:dlq` wire schema**:Go 端零感知。

---

## 6. 新增 / 复用 metric

新增(`crates/gw-gateway/src/metrics_catalog.rs`):

- `gw_billing_flusher_panic_total` — counter。flusher 单 tick panic 次数(catch_unwind 捕获)。**任何非零值 = page on-call**(等同 T0 incident)。
- `gw_billing_flusher_dead_total` — counter。`emit` 走 Closed 分支的次数(flusher 任务真的死了,channel 已 close)。任何非零 = page on-call。
- `gw_billing_inflight_clear_total{outcome="ok"|"del_failed"|"skipped_dlq"|"skipped_lost"}` — counter。DEL inflight key 的去向。`ok` 是健康主路;`del_failed` 是无害噪音(reconciler 会兜);`skipped_dlq` / `skipped_lost` 出现频度反映 Redis 健康度。

扩展现有 metric:

- `gw_billing_emit_path_total{path}` — 新增 label 值 `sync_bypass_closed`(原有 `channel` / `closed`;`closed` 含义改为"被 sync_bypass_closed 处理过的总数",与 `sync_bypass_closed` 一一对应——上下兼容)。

复用(无改动):`gateway_usage_records_lost_total`、`gw_billing_emit_lag_t0_total`、`gw_billing_inflight_recovered_total`、`gw_billing_mpsc_bypass_total`、`gw_billing_emit_total`。

---

## 7. PR 拓扑

```
PR-1: LedgerItem 引入 + flusher 内部签名变化
       ├─ channel 载荷类型变更
       ├─ flush() 实现 §3.3 的 XADD → DEL 序列
       ├─ emit() 接受 LedgerItem (Full/Closed bypass §3.4)
       ├─ Ledger 持 instance_id 字段
       ├─ 新 metric: gw_billing_inflight_clear_total
       └─ proxy / billing_lifecycle 暂时构造 LedgerItem { clear: None } 保兼容

PR-2: proxy logging 切换到 clear: Some(..)
       ├─ filters/logging.rs:295 改 LedgerItem 构造
       ├─ gateway.rs:1118-1150 删除 inflight.clear 调用
       └─ A1 主路径闭合

PR-3: A2 重试路径切换
       ├─ gateway.rs:1028-1058 fail_to_connect spawn 体改 LedgerItem
       ├─ billing_lifecycle.rs:248-292 on_retry 同步改造
       └─ A2 闭合

PR-4: flusher 韧性 §3.7
       ├─ catch_unwind 包 flush
       ├─ Closed 分支走 sync_xadd / DLQ / lost-counter
       ├─ graceful shutdown 路径带 DEL
       ├─ 新 metric: gw_billing_flusher_panic_total, gw_billing_flusher_dead_total
       └─ A3 闭合

PR-5: 测试 §8
       ├─ 单测扩展(flusher / emit bypass / panic isolation)
       ├─ 崩溃注入集成测(testcontainers + process::abort)
       ├─ soak harness 新 mode --kill-during-flight (make e2e-soak-kill 新 target)
       └─ docs sync §10
```

依赖图:

```
PR-1 ──┬─→ PR-2 ─┐
       └─→ PR-3 ─┤
PR-4 (独立) ─────┴─→ PR-5
```

并行机会:PR-1 与 PR-4 同时开工;PR-2/PR-3 在 PR-1 后并行;PR-5 收尾。每个 PR 单独可 merge 不破坏既有行为(PR-1 之后 `clear: None` 即旧语义)。

---

## 8. 测试策略

### 8.1 Unit(`crates/gw-ledger/src/flusher.rs` 自带 mod tests)

- `flush_success_then_del`:构造 batch 全 `clear: Some`,mock Redis XADD 全成功 → 断言后续 DEL pipeline 被发出,`gw_billing_inflight_clear_total{outcome=ok}` += batch.len()。
- `flush_xadd_fail_no_del`:mock XADD 返回 Err → 断言走 DLQ,**对应 inflight key 不被 DEL**;`{outcome=skipped_dlq}` += 1。
- `flush_del_fail_no_record_resend`:XADD 成功、DEL Err → 仅 warn,record 不被重发;`{outcome=del_failed}` += 1。
- `emit_full_bypass_dels_on_success`:强制 channel 满 → emit 走 sync_xadd → 成功后 DEL 发出。
- `emit_closed_bypass`:强制 channel close → emit 走 sync_xadd → `gw_billing_flusher_dead_total++` 且记录到达 mock Redis。
- `flusher_panic_isolation`:注入 flush 内 panic → channel 仍开,下一次 emit 仍走 channel,`gw_billing_flusher_panic_total++`。

### 8.2 Integration(`crates/gw-proxy/tests/` + testcontainers Redis)

- `crash_loses_no_record`:
  1. 启 gateway,发请求至 `logging` 完成(record 已入 mpsc;通过 trace 或 metric 确认)。
  2. 在 flusher tick 触发**之前**(把 `flush_interval_ms` 临时设大 + 通过 admin 信号控制)用 `std::process::abort()` 杀进程。
  3. 重启 gateway → 启动 reconciler 扫到 inflight key → emit `CrashFallback` 进 `usage:records`。
  4. 断言:`XLEN usage:records ≥ N`(N=发起请求数);`gw_billing_inflight_recovered_total ≥ N`。
- `retry_crash_loses_no_record`:连续两次 connect failure(用 fake-upstream 拒连模式)→ 各 attempt 的 spawn 已提交 emit → 在 flusher 处理前 abort → 重启后 reconciler 补回所有 attempt 的 `CrashFallback`(用 `attempt_index` 计数验证)。
- `at_least_once_dup_deduped`:模拟 XADD 后 abort → 重启,验证 `usage:records` 既包含 ① 原 record 又包含 ② 同 `(req_id, attempt_index)` 的 `CrashFallback`;断言 fake-web drain 端按 provenance 等级保留 ①(模拟 Go 去重行为)。

### 8.3 Soak / chaos 扩展

`tests/e2e/client/soak/` 加 `--kill-during-flight` 模式:运行中按 5–20% 概率随机 `SIGKILL` 进程并由 docker-compose 自动重启。运行 60s 后断言:

```
records_delta + dlq_delta + crashfallback_dedupes_inferred == upstream_hits_total
```

其中 `crashfallback_dedupes_inferred` = 同 `(request_id, attempt_index)` 在流中出现 ≥2 次时只计 1(去重模拟 Go 端行为)。

新 make target:`make e2e-soak-kill`。**非默认 CI**,作为本 spec 的验证 gate(spec §11 verification);开发本 spec 期间手跑,后续可加入夜间 nightly 套件。

---

## 9. 风险与已知限制

| 风险 | 缓解 |
|------|------|
| `LedgerItem` 改动是 mpsc 类型签名变更,跨 crate ripple 较大(gw-ledger / gw-proxy / billing_lifecycle / on_retry / fail_to_connect / 测试) | PR-1 引入时让 `emit(rec: UsageRecord)` 临时仍存在并 wrap 成 `LedgerItem { clear: None }`,作为 deprecated shim;PR-2/PR-3 后再删 |
| `catch_unwind` 包 async flush 需要 `AssertUnwindSafe`,验证 `Bytes` / `RedisPool` clone 满足 `UnwindSafe` | 单测注入 panic 验证;`bytes::Bytes` 内部为 `Arc`,`fred::clients::RedisPool` 是 `Arc` clone,均 `UnwindSafe`(实测在 PR-4 加 trait bound 验证) |
| crash-after-XADD-before-DEL 让 `CrashFallback` dup 比例上升 | 窗口大小 ≤ `flush_interval_ms`(默认 200 ms);按 5000 rps 估算 dup ≤ 1000/重启,Go 去重承受得起;由 `gw_billing_inflight_recovered_total` 监控 |
| graceful shutdown 时 last-flush 增加退出时间 | flush 是 pipeline batch,最坏 1 个 RTT;在 graceful-shutdown 30s 缓冲内远远够 |
| `Ledger` 持 instance_id 字段——若有人在 boot 时未传或传错,DEL 路径会指向不存在的 key | bootstrap 启动顺序保证 `instance_id::resolve` 在 `Ledger::start` 之前;PR-1 加 `#[must_use]` / 构造期校验非空 |
| Closed 分支走 sync_xadd 会让请求热路径在 flusher 死时被阻塞一次 Redis RTT | flusher 死是异常事件(`gw_billing_flusher_dead_total` 任何非零都告警),阻塞代价远小于丢账务记录;不引入熔断 |
| 文档同步遗漏(Sync rule) | §10 列表逐项 checkbox |

---

## 10. 文档同步(按 CLAUDE.md Sync rule)

| 文件 | 改动 |
|------|------|
| `docs/architecture.md` §Deliberate behaviours | + 条目:"inflight key 的 `DEL` 由 ledger 在 XADD 成功之后发起(主 flush 或 `sync_xadd` bypass);proxy 永不 eager-clear。crash-after-XADD-before-DEL 由 Go 去重(`request_id` + `attempt_index` + provenance)承担,at-least-once。" |
| `docs/architecture.md` §Known gaps | A1/A2/A3 三项加 `closed-by 2026-05-29-billing-record-durability`(若 §10.5 已有相关条目,合并;无则在 §9.x 新增) |
| `docs/workflow.md` §logging filter | 更新顺序图:`mark_settled` 仍在 proxy;`emit(LedgerItem)` 走 ledger;**flusher 在 XADD 后才 DEL**;proxy 侧不再有 `inflight.clear` 调用 |
| `docs/workflow.md` §error fanout / retry path | `fail_to_connect` 重试 spawn 内容更新为不调 clear |
| `docs/superpowers/specs/2026-05-24-billing-robustness-design.md` §5.4 末尾 | 加 cross-ref:"`clear` 调用时机契约由 [`2026-05-29-billing-record-durability`](./2026-05-29-billing-record-durability-design.md) 收紧为'flusher XADD 成功之后';本 spec 内 §5.4 文字保留(描述接口),实际调用点已下沉。" |
| `docs/superpowers/specs/2026-05-24-billing-robustness-design.md` §13 Known gaps | 标记 "emit fast path 持久化语义" 为 closed-by 2026-05-29 |
| `docs/go-contracts/parts/02-go-as-consumer.md` §8.2 / §2.5.x | 加注:"Rust 侧 at-least-once 的主要来源是 crash-after-XADD-before-DEL 窗口(典型大小 ≤ flush_interval_ms 200 ms);与 §2.5.x ghost-freeze 共用既有 `(request_id, attempt_index)` + provenance 去重契约,Go 侧无新增。" |
| `CLAUDE.md` | 不动(已被 architecture / workflow / go-contracts 覆盖) |

---

## 11. 验证清单(Plan 阶段检查表)

- [ ] PR-1 落地后,`cargo build --workspace --release` 干净;`cargo test --workspace` 通过;`gw_billing_inflight_clear_total` 在单测中可见
- [ ] PR-2 落地后,正常请求路径下 inflight key 在 ≤ `flush_interval_ms × 2` 内被清(integration test)
- [ ] PR-2 落地后,`rg 'inflight\.clear' crates/gw-proxy/` 仅在测试代码中出现(prod 全清零)
- [ ] PR-3 落地后,connect retry × N 不再触发 proxy 侧 clear;`gw_retry_attempt_total{outcome=recovered}` 与 `gw_billing_emit_total` 仍 1:1
- [ ] PR-4 落地后,单测注入 flusher 内 panic,channel 不断,`gw_billing_flusher_panic_total ≥ 1`,后续 emit 仍走 channel
- [ ] PR-4 落地后,强制 channel close,emit 走 sync_xadd,`gw_billing_flusher_dead_total ≥ 1`,记录到达 Redis
- [ ] PR-5 `make e2e-soak-kill` 通过:`records_delta + dlq_delta + crashfallback_dedupes_inferred == upstream_hits_total`
- [ ] CI invariants:`scripts/check-invariants.sh` 通过(本 spec 不新增 lint;§10 文档同步即可)
- [ ] architecture.md / workflow.md / billing-robustness §5.4 / billing-robustness §13 / go-contracts §8.2 全部同步
- [ ] `make e2e-soak-smoke` 通过(无回归)

---

## 附录 A:与 `2026-05-24-billing-robustness` 的关系

billing-robustness 设计了 InflightLedger 的所有原语(`register` / `mark_settled` / `clear` / reconciler / T0 watchdog),并在 §5.5 把 "must-deliver" 作为 emit 的命名契约。但 §line263 把 `clear()` 描述成 "emit UsageRecord 成功后调用",**没有指明 "成功" 的精度**——在当时的实现里 "成功 = 入 mpsc",事后回看是不充分的。

本 spec 不替换 billing-robustness 的任何部件,只把 "emit 成功" 的语义从 "入 mpsc" 收紧为 "已 XADD 进 `usage:records` 主流(经任意 bypass 亦可)",并把 clear 的执行点从 proxy 移到 ledger。InflightLedger 的 API 保持不变,只是 `clear` 的调用点和触发时机变。

## 附录 B:与 `2026-05-28-systemic-fixes-design` 的关系

systemic-fixes 在附录 B 明确不接管 InflightLedger watchdog;在附录 A 把 "G11 follow-up(三层 DLQ)依赖 Go billing" 列为 out of scope。本 spec 处理的正是这片被划走的领地。两份 spec 完全正交,可并行落地:

- systemic-fixes 的 ErrorSink(主题 1)不接管 flusher 内部错误(那是 metric driven);本 spec 的新 metric 不进 ErrorSink。
- systemic-fixes 的 ManagedLoop §5.4 把 flusher 列为可选迁移项 #1。如果 ManagedLoop 在本 spec 之前落地,本 spec §3.7.1 的 catch_unwind 可改为依赖 ManagedLoop 的 panic 处理(但 ManagedLoop 当前设计不 catch panic,只处理返回的 Outcome,需要扩展)。这是协调机会,不是依赖。

## 附录 C:不引入新去重逻辑的论证

新模型下可能产生 dup 的唯一窗口是 "XADD 后、DEL 前的崩溃"。重启后 reconciler 扫到 `ts_settled_ms` 已设且 key 未删的 inflight 项,会按现状 emit 一条 `CrashFallback`(`vendor_exposure_state = CrashFallback`,`usage_provenance.*` 全部 `CrashFallback`)。Go 端按 `2026-05-24-billing-robustness-design.md` §8.2 的规则:

```
同 (request_id, attempt_index) 同时存在多条时,
取 provenance level 最高 / vendor_exposure_state 最具体的一条。
provenance level: UpstreamFinal > UpstreamPartial > Estimated > CrashFallback > Unknown
vendor_exposure_state: Settled > Terminated > CrashFallback
```

崩溃发生在 XADD 之后 ⟹ 真实 record 已在 `usage:records` 中,其 `vendor_exposure_state` 为 `Settled` 或 `Terminated`(均高于 `CrashFallback`),其 `provenance` 至少有一维是 `UpstreamFinal` / `UpstreamPartial` / `Estimated`(均高于 `CrashFallback`)⟹ Go 必然丢弃 reconciler 后补的 `CrashFallback`。**无需 Rust 侧新增任何去重逻辑。**

## 附录 D:`emit_path_total` label 兼容性

现有 label `path ∈ {channel, closed}` 中 `closed` 原义是 "channel 死了,记录被丢弃"。改动后 Closed 分支不再丢——`closed` label 计数仍可保留(表示"经过 Closed 分支处理过"),但 PR-4 同时新增 `sync_bypass_closed` 表示"被 sync_bypass 真正持久化的"。仪表盘可继续看 `closed` 趋势(任何非零 = flusher 死过),并交叉看 `sync_bypass_closed / closed ≈ 1`(must-deliver 兜底命中)、`gateway_usage_records_lost_total{} == 0`(终极保证)。`channel` label 含义不变。**无 dashboard 破坏性改动。**
