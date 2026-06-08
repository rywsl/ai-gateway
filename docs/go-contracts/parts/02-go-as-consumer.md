# Part 02 — Go-as-Consumer（Rust 写、Go 读）

> 本 part 描述所有 **Rust 主动写入、Go 被动消费**的契约面。读完即可在 Go 端实施 consumer / settler / drainer。索引见 [main.md](../main.md)。
>
> 本文件吸收了原 `docs/go-contracts/main.md` 的契约描述部分（§2 usage:records、§4 base/deducted、§5 events:counter、§6 settled_billing）。原文件随本 PR 删除——历史 commit 内可追溯。

---

## 2.1 `usage:records` Stream（核心契约）

每条 Rust 处理过的请求（成功 / 失败 / 取消）都会落一条 `UsageRecord` 到这条 stream。Go 端在此基础上算 cost、写 deducted、推 events:counter。

### 2.1.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:records` |
| Writer | Rust `gw-ledger::flusher`（`crates/gw-ledger/src/flusher.rs`），mpsc batched XADD |
| Reader | Go billing settler |
| 推荐 consumer group | `go-billing-settler` |
| 序列化 | JSON 单字段 `payload`（XADD `* payload <json>`） |
| 流量预算 | 1000 req/s ≈ 600 KB/s（每条 ~600B JSON） |
| MAXLEN 策略 | Rust **不**自动裁剪。Go 端自定 XTRIM（建议保留 ack 后 7 天） |

### 2.1.2 Payload schema — `UsageRecord`

权威 Rust 类型见 `crates/gw-ledger/src/usage.rs` 与 [C-α](../../spec/06-billing-ledger.md)。JSON 形态：

```json
{
  "ts_ms": 1716345678901,
  "request_id": "req_01HXYZ...",
  "trace_id": "00-...",

  "user_id": "u_abc",
  "child_id": "u_def",
  "api_key_id": "ak_xxx",

  "client_protocol": "openai",
  "upstream_protocol": "anthropic",
  "model": "claude-3-5-sonnet",
  "vendor_id": "v_anthropic",
  "vendor_key_id": "vk_xxx",

  "status": "ok",                  // ok | error | cancelled
  "error_code": null,              // 仅 status=error 时填，如 "GW_UPSTREAM_5XX"
  "http_status": 200,

  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_read_input_tokens": 100,
    "cache_creation_input_tokens": 50,
    "thinking_tokens": null,
    "multimodal_input_tokens": 120
  },

  "attempt_index": 0,              // 0 = 首次，多 key 路由时重试递增
  "vendor_exposure_state": "settled",  // settled | terminated | crash_fallback
  "usage_provenance": {
    "input": "upstream_final",      // upstream_final | upstream_partial | estimated | crash_fallback | unknown
    "output": "upstream_final",
    "cache_read": "upstream_final",
    "cache_creation": "upstream_final",
    "thinking": "unknown",
    "multimodal_input": "upstream_final"
  },

  "latency_ms": 1820,
  "ttft_ms": 380,
  "is_stream": true,
  "operation_id": null,            // 可选;Gemini :predictLongRunning / OpenAI /v1/videos 返回的 operation name

  "freeze_amount_micro": 12340,
  "estimate_pricing_version": 7,

  "markup_chain_snapshot": {
    "user_group_markup": 1.0,
    "api_key_markup": 1.2,
    "vendor_markup": 1.0,
    "model_markup": 1.5,
    "product": 1.8
  }
}
```

**禁字段**：`cost_micro` —— 由 [`no_cost_micro` invariant](../../architecture.md#7-invariants-scriptscheck-invariantssh) 保证 Rust 永远不写。Go 自己算。

**`operation_id`** 含义:

- 来源:Rust `response_body_filter` 在解析上游响应 body 时,若发现顶层 `name: "operations/..."` 字段(Gemini long-running operation / OpenAI video 等),抽取并填入;
- 普通 chat / completions 请求该字段为 `null`;
- Go task-worker 用 `operation_id` 关联 `video_tasks` / `async_tasks` 表与 usage records;
- 后续轮询 `/v1beta/operations/{operation_id}` 等由 Go task-worker 直接打上游,**Rust gateway 不再介入**;
- 同一 `operation_id` 可能产生**多条** `UsageRecord`(初始 POST + 轮询完成的 finalize 写入),Go settler 自行 dedup / aggregate。

`serde(default)` / Go 端 omitempty 兼容:不存在 / 缺失视为 `null`。

### 2.1.3 Go consumer 义务

每收一条 UsageRecord 必须：

1. **消费**：`XREADGROUP GROUP go-billing-settler <consumer_id> COUNT <N> BLOCK <ms> STREAMS usage:records >`。
2. **算 cost**：基于 Go 端 `BillingPricing` + `markup_chain_snapshot` + `usage` 多维。本契约不规定公式。
3. **写 deducted**：
   ```
   INCRBY counter:user:<user_id>:deducted_micro <cost_micro>
   ```
   若 `child_id` 存在，按业务规则决定扣谁的 counter（本契约不规定）。
4. **通知 Rust**：
   ```
   XADD events:counter * ns user_balance uid <user_id> op incrby
        field deducted_micro delta <cost_micro> value_after <new_value> ts_ms <now>
   ```
   （详见 [parts/01 §1.3](01-go-as-producer.md#13-eventscounter-stream)）。
5. **XACK**：成功后 ack。
6. **可选**：另写一条到 `settled_billing` 内部 stream 用于报表 / Pg 持久化（详见 §2.1.6）。

### 2.1.4 幂等性义务

`request_id` 是天然主键。同一 `request_id` 重复消费时（XREADGROUP 重投 / consumer 重启）**不得重复扣 deducted**。建议实现：

- PgSQL `usage_records(request_id PK)` + `ON CONFLICT DO NOTHING`；或
- Redis `SET NX settled:<request_id> "1" EX 86400`，命中即跳过。

### 2.1.5 错误回路

Go 算 cost 失败（公式异常 / DB 挂 / network 抖）时：

- **不 XACK**；
- 等待重试（pending 重投）；
- 多次失败 → 写 Go 自己的 DLQ stream，与 Rust DLQ（§2.2）**互不相关**；
- 告警。

### 2.1.6 `settled_billing` Stream（Go 内部 / 可选）

| 字段 | 值 |
|---|---|
| Stream name | `settled_billing` |
| Writer | Go（settle 完一条 UsageRecord 后） |
| Reader | Go 内部（报表 / Pg 持久化 / dashboard） |
| Rust 角色 | **完全不读不写** |

目的：留一条"最终结算结果"审计流，schema 由 Go 自定，允许包含 cost_breakdown / pricing_version_used / markup_chain_applied 等。Rust 不感知，schema 变更不需要通知 Rust。

非强制；Rust 期望 Go 至少有"幂等去重"机制（§2.1.4），未必走 settled_billing 实现。

### 2.1.7 流量管理

`usage:records` 是 Rust 高频写、Go 消费的核心通路。Go 端必须保证消费速率 ≥ Rust 写入速率：

- 实测：Rust 1000 req/s → 1000 record/s；
- 单 consumer 每条 < 1 ms（含 PgSQL 写）→ 1000/s 单 consumer 可达；
- 高 QPS 需多 consumer 并行（consumer group 内多 consumer name）。

Rust **不**自动裁剪 stream，避免数据丢失。Go 消费 + XACK + XTRIM 由 Go 自决，建议保留 ack 过的部分 7 天用于排查。

### 2.1.8 去重契约（新增，billing-robustness W4-T1）

Rust 可能 emit **多条** `usage:records` 共享同一主键 `(request_id, attempt_index)`（例如崩溃恢复 reconciler 先 emit `CrashFallback`，后续真实 emit 又抵达）。Go settler **必须** 按主键去重，同主键多条按 `usage_provenance.output` 优先级择高：

```
优先级（从高到低）：
  upstream_final > upstream_partial > estimated > crash_fallback > unknown
```

**算法**：
1. 维护短期 dedup 表 `(request_id, attempt_index) → 已 settle 的 output provenance level`，TTL 24h（与 InflightLedger EXPIRE 对齐）。
2. 收到一条 record：
   - 主键不存在 → settle，记入 dedup 表。
   - 主键存在 + 新 record 的 provenance 更高 → 应用 delta（新值 - 旧值）更新 balance，表更新到新 level。
   - 主键存在 + 新 record 的 provenance ≤ 已存 → 丢弃。
3. CrashFallback record 永远会被同主键的真实 emit 覆盖。Rust 侧不做幂等。

> **Rust-side at-least-once source (added 2026-05-29).** The primary dup window in current Rust gateway is `XADD-confirmed → flusher process death → restart → reconciler emits CrashFallback for the same (request_id, attempt_index)`. The window equals one `flush_interval_ms` (default 200 ms). Go's existing dedup rule (highest provenance level + highest `vendor_exposure_state` wins) covers this. No Go-side change required. See [`docs/spec/06-billing-ledger.md`](../../spec/06-billing-ledger.md) for the formal argument.

### 2.1.9 Orphan instance 兜底（推荐 Go 侧能力，新增）

Rust 启动期 reconciler 只扫自己 `instance_id` 的残留。被永久下线的 replica（`gateway:instance:<id>:heartbeat` 已 expire）的 inflight key 由 Rust 侧 24h `EXPIRE` 兜底，但在 expire 之前的窗口里不会被恢复。

**推荐 Go 侧**实现 global reaper，周期扫 `billing:inflight:*` 与 `gateway:instance:*:heartbeat`，对 heartbeat 已 expire 的 `instance_id` 的 inflight key emit CrashFallback record（语义等同 Rust 的 §5.4.3 reconciler）。该 reaper 不强制，但对长期生产稳定性推荐。

## 2.2 `usage:records:dlq` Stream（G11 主路径兜底）

### 2.2.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:records:dlq` |
| Writer | Rust `gw-ledger::flusher`，仅当主路径 `XADD usage:records` 失败时（`flusher.rs:73-105`） |
| Reader | Go drainer（可选） |
| 推荐 consumer group | `go-billing-dlq` |
| MAXLEN | Rust 端写时 `MAXLEN ~ 100_000`（粗略限制；溢出 → metric `gateway_usage_records_lost_total`） |

### 2.2.2 Payload schema — `dlq_meta` 包裹

DLQ 条目在 UsageRecord 之外再加一层元数据：

```json
{
  "dlq_meta": {
    "first_failure_ts_ms": 1716345678901,
    "primary_failure_reason": "<error string>",
    "retry_count_before_dlq": 3,
    "dlq_layer": "redis"
  },
  "usage_record": { /* 完整 UsageRecord，schema 同 §2.1.2 */ }
}
```

> Rust 当前实现为 C-δ 三层兜底：Redis DLQ → 内存 retry queue → emergency JSONL，并在不可恢复时尝试 `usage:records:dlq:permanent_failed`。Go drainer 处理统一的 `dlq_meta` + `usage_record` 包裹。

### 2.2.3 Go drainer 义务（必需）

> Go drainer **必需**：Rust 三层兜底（C-δ）保证 record 最终落到 `usage:records:dlq` 或 emergency/permanent-failed surface，Go 端不实现 drainer = 资金泄漏。

#### Consumer group 创建

```
XGROUP CREATE usage:records:dlq go-billing-dlq $ MKSTREAM
```

- 首次部署时执行一次（Go 启动代码或运维脚本均可）；
- `$` 表示只消费创建后新写入的 entry；历史积压由下方 §启动恢复 处理。

#### XREADGROUP 消费模式

```
XREADGROUP GROUP go-billing-dlq <consumer_name> COUNT 100 BLOCK 5000 STREAMS usage:records:dlq >
```

- `consumer_name`：建议 `<hostname>:<pid>`，便于 XPENDING 排查；
- `COUNT 100`：单批上限，按实际吞吐调整；
- `BLOCK 5000`：5 秒轮询周期。

#### 处理管线

每批 entry 按以下顺序处理：

1. **解包**：从 XADD 的 `payload` 字段取 JSON → 解析顶层 `dlq_meta` + `usage_record`（schema 见 §2.2.2）；
2. **去重**：以 `request_id` 查询 billing DB（PgSQL `usage_records(request_id PK)` + `ON CONFLICT DO NOTHING`，或 Redis `SET NX settled:<request_id> "1" EX 86400`）。命中即跳过，直接 XACK；
3. **算 cost**：同 §2.1.3 流程，基于 `usage_record` 内 `usage` + `markup_chain_snapshot` + Go 端 `BillingPricing`；
4. **写 deducted**：`INCRBY counter:user:<user_id>:deducted_micro <cost_micro>`；
5. **通知 Rust**：`XADD events:counter * ns user_balance uid <user_id> op incrby field deducted_micro delta <cost_micro> value_after <new_value> ts_ms <now>`；
6. **XACK**：`XACK usage:records:dlq go-billing-dlq <entry_id>`。

#### 幂等性

`request_id` 是唯一幂等键。DLQ record 与主流 `usage:records` record 共用同一个 `request_id`，所以 Go drainer 的去重索引必须与 §2.1.4 settler 共享。不论 record 从哪条 stream 来，同一 `request_id` 只结算一次。

#### 重试退避

DLQ 处理自身失败（DB 挂、网络抖）时：

- **不 XACK** 失败的 entry；
- 指数退避重试：`1s → 2s → 4s → 8s → 16s`（上限 16s）；
- 连续失败 10 次 → 写 Go 侧告警日志 + `dlq_drain_failure_total{reason}` metric；
- 不设 permanent-fail 语义（DLQ 本身就是末端，必须最终处理成功）。

#### 启动恢复

Go drainer 启动时执行以下顺序：

1. **先 drain DLQ**：用 XREADGROUP + `0`（而非 `>`）读取 pending entries，逐条处理 + XACK，直到 pending == 0；
2. **再 drain 积压**：切换到 `>` 读新 entry，处理到 `usage:records:dlq` 长度为 0 或无新 entry（BLOCK 超时返回 nil）；
3. **最后启动主流消费**：开始消费 `usage:records`。

这保证 DLQ 中滞留的 record 优先于新请求结算，避免 FIFO 倒序。

#### 乱序容忍

Rust 三层兜底可能导致同一批请求的 DLQ record 以 mem → disk → redis 不同路径到达，**不保证写入顺序与请求顺序一致**。Go drainer 不得依赖 entry 的 stream ID 排序来推断业务时序。结算逻辑必须只依赖 `request_id` 幂等性，不依赖处理顺序。

#### 监控（Go 端）

| 指标 | 类型 | 阈值 |
|---|---|---|
| `dlq_drain_total{result}` | counter | success rate < 95% 告警 |
| `dlq_drain_failure_total{reason}` | counter | > 0 排查 |
| `dlq_drain_lag_secs`（XPENDING 最老 entry） | gauge | > 300s 告警 |
| `dlq_drain_dedup_hit_total` | counter | 仅观测（高值说明主流 + DLQ 重叠正常运作） |

### 2.2.4 不变量

- DLQ 条目**不**包含 `cost_micro`（由 `no_cost_in_dlq` invariant 强制）。
- DLQ 主路径失败的 Rust 行为：`tracing::warn!` + metric +（v1）drop。

### 2.2.5 `dlq_layer` 扩展（v2）

v2 三层兜底（C-δ）下，record 落入 DLQ 之前可能经过不同的缓冲层。Rust 在 `dlq_meta` 中新增 `dlq_layer` 字段标识来源层：

| `dlq_layer` 值 | 含义 |
|---|---|
| `mem` | 内存环形缓冲区（mpsc retry queue）溢出后写入 DLQ stream |
| `disk` | 本地磁盘 WAL（`${data_dir}/gw-ledger-emergency-dlq-*.jsonl`）恢复后写入 DLQ stream |
| `redis` | 直接 XADD 到 `usage:records:dlq`（v1 唯一路径，v2 仍为主路径） |

扩展后的 `dlq_meta` 示例：

```json
{
  "dlq_meta": {
    "first_failure_ts_ms": 1716345678901,
    "primary_failure_reason": "redis connection refused",
    "retry_count_before_dlq": 3,
    "dlq_layer": "mem"
  },
  "usage_record": { /* 完整 UsageRecord，schema 同 §2.1.2 */ }
}
```

**Go drainer 义务**：

- 必须接受全部三个 `dlq_layer` 值，处理逻辑完全相同（不按层区分结算行为）；
- `dlq_layer` 缺失时视为 `"redis"`（向后兼容 v1 写入的无此字段的 entry）；
- 不得假设不同 `dlq_layer` 的 entry 之间存在任何顺序关系；
- 建议在监控中按 `dlq_layer` 分维度统计：`dlq_drain_total{result, dlq_layer}`。

## 2.3 `usage:detailed` Stream（必需 consumer / 完整归档）

### 2.3.1 角色

- Rust：writer（单向），按 [`docs/workflow.md`](../../workflow.md) §filter chain 的 5 个触发点 XADD
- Go：**必需 consumer**（不再"可选"）；长期归档（180 天合规）责任方

### 2.3.2 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:detailed` |
| Writer | Rust `gw-ledger::flusher`（`crates/gw-ledger/src/flusher.rs`），独立 flusher pool 异步 XADD |
| Reader | Go drainer（**必需**）|
| 推荐 consumer group | `go-archive-drainer` |
| MAXLEN | Rust 写时 `MAXLEN ~ <配置值>`（参 `[detailed_log]` 表，详见 [`docs/architecture.md`](../../architecture.md) §4.1） |
| 保留期 | Go 端实现 180 天（法律合规）；Rust 无保留期参数 |

### 2.3.3 Redis Stream entry 字段集

```
ts_ms      i64    UTC wall-clock 毫秒
request_id text   r_*
trace_id   text   t_*
kind       enum   downstream_request | upstream_request | upstream_response | downstream_response | summary
encoding   enum   raw+json | lz4+json | zstd+json
payload    bytes  encoding 决定如何解码；解码后是 JSON
```

### 2.3.4 Encoding 解码规则

| encoding 值 | 解码方式 |
|---|---|
| `raw+json` | 直接解析 JSON（payload 字段即 UTF-8 JSON 文本） |
| `lz4+json` | LZ4 帧解压后解析 JSON |
| `zstd+json` | zstd 解压后解析 JSON |

选择策略（由 Rust 侧自动判定）：
- JSON 序列化后 < 1 KiB → `raw+json`
- 1 KiB ≤ 字节 < 64 KiB → `lz4+json`
- ≥ 64 KiB → `zstd+json`

### 2.3.5 Payload 解码后 JSON Schema（按 kind 分支）

#### `kind = downstream_request` / `upstream_request`

```json
{
  "method": "POST",
  "url": "/v1/chat/completions",
  "http_version": "HTTP/1.1",
  "headers": [["host", "..."], ["authorization", "..."], ...],
  "body_b64": "..."
}
```

- `headers`：数组，保持出现顺序，**不**去重、**不**脱敏。`Authorization` / `x-api-key` / `Cookie` 完整保留。
- `body_b64`：base64-standard（RFC 4648），不含换行。空 body 时为 `""`。

#### `kind = upstream_response` / `downstream_response`

```json
{
  "status": 200,
  "headers": [["content-type", "application/json"], ...],
  "body_b64": "..."
}
```

- `upstream_response.body_b64`：vendor 给的**原始字节**（含 `content-encoding` 未解压形态）
- `downstream_response.body_b64`：发给客户端的最终字节（codec 重渲染后形态）
- 流式响应：聚合整个流的全部字节（SSE event 串接、按 wire 顺序）

#### `kind = summary`

```json
{
  "user_id": "u_...",
  "api_key_id": "ak_...",
  "model": "claude-sonnet-4.6",
  "client_protocol": "openai_chat",
  "upstream_protocol": "anthropic_messages",
  "http_status": 200,
  "error_code": null,
  "is_stream": true,
  "input_tokens": 1234,
  "output_tokens": 567,
  "latency_ms": 8901,
  "ttft_ms": 234,
  "tunnel_resolution": "direct"
}
```

### 2.3.6 Go consumer 义务

1. **消费**：`XREADGROUP GROUP go-archive-drainer <consumer_id> COUNT N BLOCK <ms> STREAMS usage:detailed >`，起始位置 `0`（消费全量）
2. **按 kind 分支解码**：根据 `encoding` 字段选择解压算法，再解析 JSON
3. **索引与重组**：按 `(request_id, kind)` 建立索引。一个请求的完整快照 = 5 条 entry：`downstream_request` + `upstream_request` + `upstream_response` + `downstream_response` + `summary`
4. **缺失语义**：
   - 某 surface 缺失 = 该 surface 在 Rust 侧未归档成功（mpsc 满 / 压缩失败 / XADD 失败 / 进程崩溃）。**不**表示请求该 surface 不存在
   - `summary` 缺失 = 请求未到达 logging filter（进程崩溃 / mpsc 满）。建议 Go 设置 fallback 超时（收到首个 surface 后 60 秒内 `summary` 未到 → 标记该 request_id 为不完整快照）
   - 全部 5 个都缺 = 请求未触发任何归档（早期 filter 抛错 / mpsc 全满）
5. **长期归档**：批量将 entry 落到长期存储（S3 / GCS / parquet / lakehouse，任选），保留期 180 天
6. **XACK**：成功归档后 ack；失败由 Go 自决重试或落 Go 侧 DLQ（**不走 Rust DLQ**）

### 2.3.7 监控义务（Go 端）

| 指标 | 类型 | 阈值 |
|---|---|---|
| `usage_detailed_consume_lag_secs` | gauge | > 60s 告警 |
| `usage_detailed_xlen` | gauge | > MAXLEN × 0.8 告警 |
| `usage_detailed_processed_total` | counter | 仅观测 |
| `usage_detailed_decode_failure_total{reason}` | counter | > 0 排查 |
| `usage_detailed_archive_failure_total` | counter | > 1% 告警 |

### 2.3.8 强度与 SLA

- **无 DLQ**：`usage:detailed` 不是账务流。Go drainer 跟不上 Redis MAXLEN 滚动 → 永久丢失未消费 entry。180 天合规要求 = Go drainer 必须不挂；挂了的处置是运维 oncall 问题，不在协议保证内
- **单向**：Rust 写、Go 读。Go **禁止**反向写 `usage:detailed`
- **完整性**：body 不截断、不脱敏；header 包含 `Authorization` / `x-api-key` / `Cookie` 等敏感字段。Go drainer 如需脱敏，在自己的归档 sink 上处理，Rust 不知情

## 2.4 `events:entity` 反向写入（Rust XADD）

绝大多数 `events:entity` 写入由 Go 完成（[parts/01 §1.2](01-go-as-producer.md#12-eventsentity-stream)）。Rust 仅在两类受控场景下反向 XADD：

### 2.4.1 `vendor_key.status_change`（rate_limited 自动设置）

| 触发 | Rust 探活 / 健康统计判定某 vendor_key 速率受限（spec §1302-1303） |
|---|---|
| Payload | `ns=vendor_key id=<key_id> op=status_change field=status from=active to=rate_limited reason=rate_limited ts_ms=<now>` |
| Go 接收行为 | 更新 PgSQL `vendor_keys.payload.status` 为 `rate_limited` + 自增 version；可选写审计表 |
| Rust 后续 | Rust 自己**不**改 PgSQL；仅本地 Moka `vendor_key` 失效（下次冷加载若 Go 已写新 status 即生效，否则保留 active） |

### 2.4.2 `ipblock.upsert`（异常流量自动封禁）

| 触发 | 单 IP QPS > 100 持续 1 min（spec §1830-1831, §1885）或其他异常检测命中 |
|---|---|
| Payload | `ns=ipblock id=<cidr> op=upsert field=expires_at value=<now+3600s> reason=abnormal_traffic ts_ms=<now>` |
| Go 接收行为 | 写 `ip_blocks.payload`，包含 `cidr / scope=global / reason=abnormal_traffic / expires_at`；自增 version |
| Rust 后续 | Rust 通过 events:entity 自然消费此条 → 失效 `ipblock` cache → 下次请求拒服 |

### 2.4.3 `vendor_key.oauth_failed`(OAuth 失败通知)

| 触发 | 上游返回 401 且 `vendor_key.oauth.is_some()`,经 `OAuthListener` 判定为 `AccountBlocked(OAuthRefreshFailed)` |
|---|---|
| Payload | `ns=vendor_key id=<key_id> op=status_change field=oauth_failed reason=upstream_401 ts_ms=<now>` |
| Go 接收行为 | task-worker 检查 `refresh_token` 有效性 → 尝试 refresh:<br/>① 成功:加密新 access_token + 整体替换 `payload.oauth` 子对象 + `version + 1` + XADD `events:entity ns=vendor_key op=upsert`;<br/>② 失败:`status = disabled` + XADD `events:entity ns=vendor_key op=upsert` |
| Rust 后续 | **不**主动 refresh;等 Go 写新 vendor_key payload 后自然冷加载到新 token |

不变量:与 §2.4.1(`rate_limited`)、§2.4.2(`ipblock`)并列,均属"Rust 反向写,Go 接收"的白名单 op。

### 2.4.4 不变量

- Rust 反向写**仅限**上述两类 op；其他实体（user / apikey / vendor / model_group / pricing / jwks / model / auth_config）的写权 Rust 一律没有。
- Rust 写时 `v` 字段**不**自行分配（Rust 不知道 Go 的下一个 version），传 `0` 或省略。Go 收到后应当：
  1. PgSQL `UPDATE ... SET version = version + 1 RETURNING version`；
  2. 把 `version` 写回 events:entity 的 `v` 字段（再 XADD 一次"权威 upsert"）；
  3. 这避免了 Rust 反向写 `v=0` 让其他 Rust 副本的 versioned upsert 误判。
- §2.4.3 oauth_failed:Rust 反向写,Go task-worker 端必须实现 refresh 处理;未实现的代价 = 该 vendor_key 失效后无法自愈,等同 §2.4.4 表中"Go 未实现接收 → vendor_key 状态不会同步到 PgSQL"。

> v1 实现：Rust 端可以省略 `v`，Go 端按收到的 `op=status_change` / `reason=abnormal_traffic` 字段识别"Rust 反向写"。Go 端实施完成后，可将此约定显式化为 stream 字段 `source=rust`。

### 2.4.5 失败模式

| 故障 | 结果 |
|---|---|
| Rust XADD 失败 | 仅 metric warn；rate_limited / abnormal_traffic 检测下次再触发 |
| Go 未实现接收 | Rust 反向写丢失；vendor_key 状态不会同步到 PgSQL，但 Rust 本地 health store 仍能挡（[`gw-health::KeyHealthStore`](../../architecture.md#21-crate-responsibilities)） |

## 2.5 `counter:*:frozen_micro`（Rust 写、Go 读）

| Key | `counter:user:<uid>:frozen_micro` |
|---|---|
| Writer | Rust（freeze 时 +est, release 时 -est），异步 INCRBY |
| Reader | Rust（available 计算 + 启动期对齐） + Go（balance 审计 / 透支检测） |
| TTL | 无（永久 key，Rust 不设 EXPIRE） |

### 2.5.1 Rust 写入路径

```
请求到达：
  1. compute estimate (Part 01 §1.5)
  2. local: state.frozen_micro.fetch_add(estimate)        // 本地 AtomicI64，实时
  3. async: INCRBY counter:user:<uid>:frozen_micro +est   // 异步刷 Redis（mpsc 队列）

请求结束：
  1. local: state.frozen_micro.fetch_sub(estimate)
  2. async: INCRBY counter:user:<uid>:frozen_micro -est
```

详见 [C-γ](../../spec/06-billing-ledger.md)。

### 2.5.2 Go 读取语义

Go 在做 balance check（如 settle 前验证 user 不透支）时可以：

```
GET counter:user:<uid>:base_micro       → base
GET counter:user:<uid>:deducted_micro   → deducted  (Go 自己写过的)
GET counter:user:<uid>:frozen_micro     → frozen    (Rust 异步写)
available = base - deducted - frozen
```

`frozen` 是**全局视图**：所有 Rust 副本对同一 user 的 freeze 都 HINCRBY 到同一 key，累加自然多副本汇总。

### 2.5.3 一致性窗口

- Rust 本地 fetch_add 先于 HINCRBY 异步推送，存在 ~几十 ms 延迟。Go 在此窗口内看到的 frozen 偏低 → balance 可能误判为可用。**v1 接受此不一致**；Go settle 时如果检测到透支，可走"拒 settle + 告警"路径，由人工或后台对账修正。
- Rust 进程崩溃 → 本地未 flush 的 frozen delta 丢失。Redis 上已 flush 的 frozen 仍存（"幽灵冻结"问题）。Go 端回收算法见 §2.5.5。

### 2.5.4 写权独占

- Go **禁止**直接写 `frozen_micro`（会破坏 Rust 本地 AtomicI64 与 Redis 一致性）。
- Rust **禁止**直接写 `base_micro` / `deducted_micro`。

### 2.5.5 Ghost-freeze reconciliation（幽灵冻结回收）

Rust 进程崩溃后，已 flush 到 Redis 的 `frozen_micro` 增量不会自动释放（§2.5.3 所述"幽灵冻结"）。本节规定 Go 端的回收算法。

#### 触发条件

Go 端周期任务，建议 **5 min** 间隔。额外触发点：Go 检测到某 Rust pod 健康检查（`/readyz`）连续失败时可立即触发一次针对该 replica 的回收。

#### Rust 侧前置条件

当前 Rust 代码（`gw-store/src/deduction.rs`）使用单个 `frozen_micro` AtomicI64，所有副本共享同一 Redis key。要支持按副本回收，Rust 需改为写 **副本级子键**：

```
INCRBY counter:user:{user:<uid>}:frozen_micro:<replica_id> <delta>
```

其中 `replica_id` 是 Rust pod 唯一标识（如 Kubernetes Pod UID 或 `HOSTNAME`）。Go 读 frozen 总量时需 SUM 所有 `frozen_micro:*` 子键。

此 Rust 侧改动归入 S-ledger spec，本契约仅文档化 Go 端接口契约和算法。Rust 未完成副本级拆分前，本节算法无法生效。

#### 算法（4 步）

1. **Scan**：Go 遍历活跃用户列表，对每个 `uid` 执行 `SCAN` 匹配 `counter:user:{user:<uid>}:frozen_micro:*`，收集所有 `replica_id` 及其对应 frozen 值。
2. **Compare**：将收集到的 `replica_id` 集合与当前活跃 Rust 副本列表比对。活跃副本列表来源二选一：Kubernetes API（list pods with label selector）或轮询各 Rust pod 的 `/readyz` 端点。
3. **Release**：对判定为 dead 的 `replica_id`：
   ```
   GET  counter:user:{user:<uid>}:frozen_micro:<replica_id>  → amount
   DEL  counter:user:{user:<uid>}:frozen_micro:<replica_id>
   ```
   两条命令放在同一 `MULTI/EXEC` 事务内，保证原子性。`amount` 即为释放量，无需反向 HINCRBY。
4. **Audit**：每次释放写审计日志（结构化日志或专用 audit 表）：
   ```json
   {
     "event": "ghost_freeze_released",
     "uid": "<uid>",
     "replica_id": "<replica_id>",
     "released_amount": <amount>,
     "timestamp": "<ISO8601>"
   }
   ```

#### 风险与缓解

误判活跃 replica 为 dead 会释放有效冻结，用户可在短窗口内超额消费。缓解措施：

- **Grace period**：replica 连续 **3 次**健康检查失败（即 ≥ 15 min 不可达）才判定 dead。
- **上限保护**：单次回收释放的 `amount` 超过阈值（如 10,000,000 micro）时，跳过并告警，由人工确认。
- **对账兜底**：Go settle 时检测到 `balance < 0` 仍走"拒 settle + 告警"路径（§2.5.3 已有机制），保证最终一致。

## 2.6 `counter:*:daily_tokens:*`（Rust 写、Go 报表可读）

| Key | `counter:user:<uid>:daily_tokens:<YYYY-MM-DD>` |
|---|---|
| Writer | Rust `logging` filter，`counter_store.rs::incrby_with_expire` |
| Reader | Rust（request_filter 前置 quota 检查） + Go（报表 / 分析，**可选**） |
| TTL | 48 小时（Rust 写入时 EXPIRE） |

### 2.6.1 Rust 写入路径

`logging` filter 在请求结束时：

```
INCRBY counter:user:<uid>:daily_tokens:<UTC_today> <input + output tokens>
EXPIRE counter:user:<uid>:daily_tokens:<UTC_today> 172800   // 48h
```

详见 G3 + [`docs/architecture.md`](../../architecture.md) §9 Known gaps。Rust **不**写 `events:counter` 通知 Go（这是 observational 计数，不是权威结算）。

### 2.6.2 Go 报表可读

Go 端如需"今日 token 消耗排行"等报表，直接 GET 即可。**禁止**写入。

> 注意：UTC midnight 边界存在 < 0.001% 的请求错日（[`docs/architecture.md`](../../architecture.md) §8 Deliberate behaviours）；Go 端报表可接受此误差。

## 2.7 监控义务

Go 端必须暴露：

| 指标 | 类型 | 阈值 |
|---|---|---|
| `usage_records_consume_lag_secs`（XPENDING 最老 entry） | gauge | > 60 |
| `usage_records_processed_total` | counter | — |
| `usage_records_settle_failure_total` | counter | rate > 1% |
| `dlq_records_pending`（XLEN usage:records:dlq） | gauge | > 1000 |
| `dlq_drain_total{result}` | counter | drain success rate < 95% |
| `events_entity_rust_inbound_total{op}` | counter | （反向 XADD 监控） |
| `events_entity_rust_inbound_unprocessed_total` | counter | rate > 0 → 告警（Go 未处理 Rust 反向写） |
| `balance_overdraft_detected_total` | counter | 出现即告警（Go settle 时检测到 balance < 0） |

## 2.8 不变量

| 不变量 | 强制方 |
|---|---|
| Rust 永远不写 `cost_micro` | Rust `no_cost_micro` invariant（`scripts/check-invariants.sh`） |
| `usage:records` 是唯一权威 cost 输入流 | 双方约定 + `no_billing_records_stream` invariant（禁旧名 `billing:records`） |
| DLQ 条目不引入 `cost_micro` | `no_cost_in_dlq` invariant |
| `markup_chain` 是数组，不是单一 scalar `markup_product` | `no_markup_product_scalar` invariant |
| `FreezeGuard::release` 唯一释放方式 | `no_freezeguard_settle` invariant |

## 2.9 `mgmt_token:blacklist:<token_id>` Redis Key

| Key 格式 | `mgmt_token:blacklist:<token_id>` |
|---|---|
| Type | STRING |
| Writer | Go（用户主动撤销 / 管理员强制 revoke） |
| Reader | Rust（`/healthz/*` 与 management-path `request_filter` 在 `verify_mgmt_token_secret` 之前查 `EXISTS`） |
| TTL | Go 设为 token 的剩余有效期 |

写入语义与 [`jwt:blacklist:<jti>`](01-go-as-producer.md#16-jwtblacklistjti-redis-key) 一致。Rust 端 `RedisJwtBlacklist::check_mgmt`（`crates/gw-auth/src/jwt_blacklist.rs`）做一次 `EXISTS`：

- 命中 → 401 `GW_AUTH_MGMT_TOKEN_REVOKED`；
- Redis 不可达 → 默认 `BlacklistFailMode::FailClosed`（仍 401），与 JWT 黑名单共用 `AuthConfig.blacklist_fail_mode` 开关。

Go 写入示例：

```
SET mgmt_token:blacklist:<token_id> "1" EX <remaining_seconds>
```

`token_id` 即 `ManagementTokenEntry.token_id`（前缀 `mt_`，详见 [parts/03 §3.2.13](03-shared-schema.md#3213-management_tokenspayload--managementtokenentrymigration-0007)）。

## 2.10 Redis L1 失效一致性责任

Rust 端在消费 `events:entity` 时会 `DEL entity:{ns}:{id}`(详见 [parts/01 §1.2.4](../parts/01-go-as-producer.md#124-写顺序与重试) 补口段)。Go 端**无需**主动 DEL `entity:{ns}:{id}` 来配合;Go 写 entity 的三步流程(PgSQL UPDATE → 可选 Redis HSET write-through → XADD `events:entity`)不变。

**不变量**:

- 任意 Rust pod 收到 `events:entity` → DEL Redis L1 是幂等的;
- 多 pod 同时收同一事件多次 DEL 无害;
- Rust DEL 失败 → 仅 warn;下次该 entity 被读时,Moka miss → Redis L1 命中陈旧值(若 Go 未 write-through 则不存在)→ 写回。Go 写顺序保证 PgSQL 已经更新,所以即便 Redis L1 命中陈旧,后续 Moka TTL 到期后冷加载仍能拿到 PgSQL 新值。

**例外**:启动期 Redis L1 陈旧(Rust 启动用 `$` 锚点,启动前事件不消费)不在本节范围,见 [parts/01 §1.2.4 补口段](../parts/01-go-as-producer.md#124-写顺序与重试) 末段。

## 2.11 未决问题

1. **`settled_billing` 是否要 Rust 强制要求 Go 保留**：本契约不强制（Rust 不读）。Go 自决保留与否。
2. **`usage:records:dlq` 在 v2 三层兜底（C-δ）后的 payload 扩展**：~~已规定~~。`dlq_layer` 字段（`mem` / `disk` / `redis`）及 Go drainer 处理义务见 §2.2.5；DLQ replay 完整规格见 §2.2.3。
3. **`counter:*:frozen_micro` 副本级子键**（C-γ §未决问题 #7）：Go 端 ghost-freeze 回收算法已规定，见 §2.5.5。Rust 侧副本级子键拆分归入 S-ledger spec。
4. **Rust 反向写 `events:entity` 的 source 标记**：v1 用 `reason` 字段隐式识别；v2 加显式 `source=rust` 字段，Go 端读时可显式分流"Rust 反向写" vs "Go 自身写"。
