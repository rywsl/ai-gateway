# C-δ — Usage Stream DLQ + 持久化保护（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 6 B6）
  - [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md)（usage:records stream）
  - [2026-05-19-billing-c-gamma-balance-split.md](./2026-05-19-billing-c-gamma-balance-split.md)（FrozenDelta 写入路径同样需要 DLQ）
- **范围**：`gw-ledger::flusher` 的工程加固：
  - XADD usage:records 失败时进 DLQ（`usage:records:dlq`）；
  - HINCRBY frozen_micro 失败时进 DLQ（`counter:frozen:dlq`）；
  - 独立 retry loop 周期重试 DLQ；
  - 告警 + 监控；
  - 限流：DLQ 累积达阈值时上报 alert，避免静默吞噬数据。
- **不在范围**：
  - cost 公式（→ Go 侧）；
  - UsageRecord schema（→ C-α）；
  - balance split（→ C-γ）。

> 这是 Rust 端纯工程加固，与公式无关。当前 `gw-ledger::flusher::flush` 写 Redis 失败时数据**永久丢失**——这是已知 v1 缺陷（参 README "DLQ for billing flush failures" gap）。

---

## 1. 背景

`gw-ledger::flusher`（参 `crates/gw-ledger/src/flusher.rs:32-58`）：

```rust
tokio::spawn(async move {
    let mut buf: Vec<BillingRecord> = Vec::with_capacity(batch_size);
    let mut ticker = interval(flush_interval);
    loop {
        tokio::select! {
            maybe_rec = rx.recv() => {
                match maybe_rec {
                    Some(rec) => {
                        buf.push(rec);
                        if buf.len() >= batch_size {
                            flush(&redis, &mut buf).await;   // ← 失败静默
                        }
                    }
                    // ...
                }
            }
            _ = ticker.tick() => {
                if !buf.is_empty() { flush(&redis, &mut buf).await; }   // ← 同上
            }
        }
    }
});
```

`flush` 内部 XADD + HINCRBY 任一失败 → record 丢失 + 无任何重试 + 无告警。生产上不可接受。

C-α 后 stream 是 `usage:records`；C-γ 后还有 FrozenDelta 也走 mpsc → flush。两类消息都需要 DLQ 保护。

## 2. 设计决策

### ADR-Cδ-1 LedgerMsg 与 batch 拆分

```rust
enum LedgerMsg {
    Usage(UsageRecord),
    FrozenDelta { uid: UserId, delta: i64 },
}

struct FlushBatch {
    usage: Vec<UsageRecord>,
    frozen_deltas: Vec<(UserId, i64)>,
}
```

flusher 在 mpsc 收消息时按类型分桶 → batch 满或 ticker 触发时 flush。

### ADR-Cδ-2 flush 分两段 + 各自 DLQ

```rust
async fn flush(redis: &RedisCtx, batch: FlushBatch) -> FlushOutcome {
    // 阶段 1：XADD usage:records
    let usage_failed: Vec<UsageRecord> = match xadd_usage_pipeline(redis, &batch.usage).await {
        Ok(_) => Vec::new(),
        Err(e) => {
            tracing::warn!(error=?e, count=batch.usage.len(), "usage flush failed");
            metrics::counter!("gw_ledger_flush_failed_total", "kind" => "usage").increment(1);
            batch.usage  // 整批失败，进 DLQ
        }
    };

    // 阶段 2：HINCRBY frozen_micro
    let frozen_failed: Vec<(UserId, i64)> = match hincrby_frozen_pipeline(redis, &batch.frozen_deltas).await {
        Ok(_) => Vec::new(),
        Err(e) => {
            tracing::warn!(error=?e, count=batch.frozen_deltas.len(), "frozen delta flush failed");
            metrics::counter!("gw_ledger_flush_failed_total", "kind" => "frozen").increment(1);
            batch.frozen_deltas
        }
    };

    // 阶段 3：进 DLQ
    if !usage_failed.is_empty() {
        write_to_dlq(redis, "usage:records:dlq", &usage_failed).await;
    }
    if !frozen_failed.is_empty() {
        write_to_dlq(redis, "counter:frozen:dlq", &frozen_failed).await;
    }

    FlushOutcome { ... }
}
```

两段独立 → 一段失败不连累另一段。

### ADR-Cδ-3 DLQ 写入

DLQ 也是 Redis Stream：

```
usage:records:dlq        # XADD by failed flush
counter:frozen:dlq       # 同上
```

**DLQ 写入也可能失败**（同 Redis 同 connection）。处理：

- 第一次 DLQ 写入失败 → 内存 buffer 重试（`Vec<LedgerMsg>` 进 retry queue，limited capacity）；
- retry queue 满 → 最后兜底：写到本地文件 `${data_dir}/gw-ledger-emergency-dlq-${ts}.jsonl`；
- 文件系统也失败 → metric `gw_ledger_emergency_drop_total` increment + ERROR log + alert。

绝对不能静默吞数据。

### ADR-Cδ-4 DLQ retry loop

独立 tokio task 周期扫 DLQ：

```rust
tokio::spawn(async move {
    let mut ticker = interval(Duration::from_secs(30));
    loop {
        ticker.tick().await;
        retry_dlq(&redis, "usage:records:dlq", &"usage:records").await;
        retry_dlq(&redis, "counter:frozen:dlq", &/* HINCRBY target */).await;
    }
});

async fn retry_dlq(redis: &RedisCtx, dlq_stream: &str, target: &str) {
    // XREAD BLOCK 0 COUNT 100 STREAMS dlq_stream 0
    // 对每个 record 尝试重写到 target
    // 成功 → XDEL from dlq
    // 失败 → 留在 dlq + metric increment
}
```

重试策略：
- 每 30 秒扫一次 DLQ；
- 一次最多取 100 条；
- 重试失败 5 次（attempt counter 存 in payload）→ alert + 移到 `usage:records:dlq:permanent_failed`（人工介入）。

### ADR-Cδ-5 启动期恢复

Rust 网关启动时：

1. 检查 `usage:records:dlq` / `counter:frozen:dlq` 是否非空；
2. 非空 → log warning + 启动 DLQ retry loop 立即工作；
3. 不阻塞业务流量（DLQ 是异步处理）。

崩溃前未来得及写出的 mpsc 内存消息丢失（**这是已知不可恢复的损失**，本 spec 不解决；需要 graceful shutdown 保护，详见未决问题 #5）。

### ADR-Cδ-6 监控与告警

| Metric | 类型 | 含义 |
|---|---|---|
| `gw_ledger_flush_total` | counter (kind, result) | 每批 flush 的成功 / 失败计数 |
| `gw_ledger_flush_failed_total` | counter (kind) | 失败次数（按 usage / frozen） |
| `gw_ledger_dlq_size` | gauge (stream) | DLQ 当前长度 |
| `gw_ledger_dlq_retry_total` | counter (stream, result) | DLQ 重试成功 / 失败 |
| `gw_ledger_emergency_dlq_file_total` | counter | 写到本地文件的次数 |
| `gw_ledger_emergency_drop_total` | counter | 完全丢失的次数（含文件失败） |
| `gw_ledger_dlq_record_age_secs` | histogram | DLQ 内 record 滞留时长 |

告警规则（建议）：
- DLQ size > 1000 → warn；
- DLQ size > 10000 → critical；
- `emergency_drop_total` > 0 → critical（数据丢失，必须人工）；
- DLQ record 滞留 > 1h → warn（重试卡住了）；
- flush failed rate > 5% in 5 min → warn。

### ADR-Cδ-7 与 graceful shutdown 集成

`gw-gateway::lib` 的 SIGTERM 处理：

1. 停止接受新请求（pingora server stop）；
2. 等待 in-flight 请求结束（最多 N 秒，配置）；
3. **drain mpsc 队列**：调 `Ledger::drain().await` 把所有未 flush 的 msg 强制 flush 一次；
4. 退出。

drain 失败时所有未 flush 的 msg → DLQ；drain 自身写 Redis 失败 → 本地文件兜底（参 §ADR-Cδ-3）。

### ADR-Cδ-8 batch size 与 latency 权衡

当前默认 `batch_size = 100` / `flush_interval = 200ms`。

- 小 batch + 短 interval = 低 latency，但 Redis QPS 高，单失败影响小批；
- 大 batch + 长 interval = 高 throughput，单失败影响大批。

倾向 v1 沿用现有默认，按 metric 调优。

### ADR-Cδ-9 性能预算

| 操作 | 预算 |
|---|---|
| flush success path（batch 100） | < 5 ms（一次 pipeline） |
| flush + DLQ 写入（失败 path） | < 20 ms（两次 pipeline） |
| DLQ retry loop tick | 30s 间隔 |
| `gw_ledger_dlq_size` gauge 采样 | 异步，每 10s 一次 |

flusher 本身在独立 tokio task，不阻塞业务路径。

## 3. 实施大纲

依赖：C-α（UsageRecord schema） + C-γ（FrozenDelta msg）。

1. 改 `gw-ledger::flusher`：
   - `LedgerMsg` 改 enum；
   - `flush` 拆两阶段；
   - 失败时 write_to_dlq；
   - 三层兜底（DLQ stream → 内存 retry → 文件）；
2. 新增 DLQ retry loop（独立 tokio task）；
3. 启动期检查 DLQ 非空 + log；
4. `Ledger::drain()` API 供 graceful shutdown；
5. 监控接入 + alert 规则；
6. 测试：
   - mock Redis 注入失败 → record 进 DLQ；
   - DLQ retry 成功 → 数据流转 OK；
   - DLQ retry 5 次失败 → permanent_failed；
   - 本地文件兜底（mock Redis + DLQ 都不可达）；
   - graceful shutdown drain。

## 4. 未决问题

1. **DLQ stream 长度上限**：Redis Stream 默认无 MAXLEN，可能无限增长。倾向 v1 用 `XADD MAXLEN ~ 100000`（近似裁剪），超出时丢最老的 + emergency_drop_total++。但裁剪掉的是"重试失败的"——优先级反而最高。两难。倾向 **DLQ 不裁剪**，由 alert 强制人工干预；如确实超 100k 单独 plan 处理。
2. **DLQ retry 频率**：30s 固定 vs 指数退避。Redis 短期不可达时 30s 够；长期不可达时 30s 太频繁（消耗连接）。倾向加退避：base=30s，每次失败 ×2，上限 5min。
3. **本地文件兜底的清理**：写出去的 emergency 文件谁清理？倾向：人工介入 + monitoring alert；不自动删（怕丢数据）。文件名带 ts 与 record_id 便于追溯。
4. **DLQ payload 与原 stream 同 schema**：是否需要附加"失败原因"字段？倾向加 wrapper：
   ```
   { "original": <UsageRecord JSON>, "first_failed_at": <ts>, "attempts": <n>, "last_error": "..." }
   ```
   方便 retry loop 看 attempts。
5. **graceful shutdown drain 超时**：drain 等多久？太短 → 数据丢；太长 → 重启慢。倾向 5s 上限 + 超时后剩余 msg 全部进文件兜底。
6. **mpsc 满时的反压**：当前 mpsc 默认 channel_capacity（参 `Ledger::start`），满了 `try_send` 直接失败 → 该请求的 UsageRecord 永远丢。需要：(a) 加大 channel capacity；(b) 满时本地文件兜底；(c) 满时调用方 block。倾向 (a) + (b)：channel 10k；满时 try_send 失败的 msg 写文件。
7. **多副本 DLQ 协调**：副本 A 写 DLQ，副本 B retry。两个副本同时 retry 同一条 → 双重写到 target stream → 重复（usage 重复 = Go 端会算两次 cost；frozen 重复 = 多冻结）。需要：DLQ 用 consumer group + XCLAIM 保证单消费者处理；或 retry 时用 idempotency key（`request_id`）让 target stream 去重。倾向 consumer group。

---

> C-δ 是工程类硬保护，跨 v1 / v2 都需要。一旦 land，Rust 端 usage / frozen 数据不会因为 Redis 短时故障而丢失。
