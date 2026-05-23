# 完整请求/响应归档(detailed log complete)— v2026-05-23

- **日期**：2026-05-23
- **状态**：Draft(待审阅)
- **作者**：协作设计(Claude + 用户)
- **触发**：用户要求"完整记录上下游请求日志,不再截断",当前 `usage:detailed` 链路是采样 + 4 KiB body 预览 + 64 KiB record 硬丢 + 三级 redaction 的 audit/debug 流,**不是**满足 180 天合规归档的完整流。
- **关联**：
  - [`docs/go-contracts/parts/02-go-as-consumer.md`](../../go-contracts/parts/02-go-as-consumer.md) §2.3(本 PR 重写)
  - [`docs/architecture.md`](../../architecture.md) §3 / §4.1 / §7(本 PR 触发更新)
  - [`docs/workflow.md`](../../workflow.md) §filter chain DetailedLog 段(本 PR 重写)
- **范围**:把 `usage:detailed` 从采样脱敏 debug 流改造成"完整、wire 忠实、按 surface 异步触发"的归档流;新增分级压缩(raw / lz4 / zstd)、per-surface mpsc → flusher pool 解耦、5 个 surface 触发点。Go 侧从"可选 consumer"升级为"必需 consumer + 长期归档负责方"。
- **不在范围**:
  - 长期归档存储选型(S3 / GCS / parquet schema)——Go 端决定
  - 字段级二级脱敏——Rust 不脱敏,需脱敏由 Go drainer 在自己的归档 sink 上处理
  - DLQ / spool / replay——drop-on-outage 策略,与账务 `usage:records` 不同
  - 按用户/key 维度的归档开关——默认全开,不接受细粒度逻辑
  - 多 stream 拆分——5 个 kind 都进同一个 `usage:detailed`
  - cost 计算 / 计费——`usage:records` 不变,本 spec 与计费无交叉

> 一句话定义:`usage:detailed` 上 wire-level 完整、header/body 不脱敏、按 surface 独立异步触发、分级压缩、单向(Rust 写 / Go 读)的归档流;Go 是长期保留(180 天)的最终责任方。

---

## 1. 现状盘点

### 1.1 当前 `usage:detailed` 链路

- `crates/gw-proxy/src/detailed_log.rs`:
  - `DetailedLogRecord` 单条聚合 5 字段元数据 + `request_body_preview` + `response_body_preview`
  - `PREVIEW_MAX_BYTES = 4 * 1024`(4 KiB 截断,尾部 `…(N bytes truncated)`)
- `crates/gw-ledger/src/detailed_stream.rs`:
  - `MAX_DETAILED_LOG_BYTES = 64 * 1024`(整条 64 KiB,超出 drop+metric)
  - 三级 redaction:header denylist(`authorization` / `x-api-key` / `cookie`)+ body 替换为 `<redacted-text-N-chars>` + part-level 媒体替换
- `crates/gw-ledger/src/log_sink.rs::RedisStreamSink`:
  - 单 `payload` 字段 JSON serialize → XADD `usage:detailed` MAXLEN ~ N
  - 失败:metric + drop
- `[debug]` 配置:`sampling_rate`、`body_capture_*` 系列 toggle 决定是否 emit
- Go 契约:[parts/02 §2.3](../../go-contracts/parts/02-go-as-consumer.md) 标的是"可选 consumer"

### 1.2 缺口

| 缺口 | 影响 |
|------|------|
| 4 KiB body 预览截断 | 90% 以上请求 body 不完整 |
| 64 KiB 单条上限 | 多模态 / 大上下文请求被丢弃 |
| body 替换为 `<redacted-text-N-chars>` | 完全失去原始内容 |
| header denylist 移除 Authorization / x-api-key / Cookie | 与"wire 忠实"目标冲突(用户已接受此风险) |
| 采样率门控 | 不是全量 |
| 只有 1 个 `request_body_preview` + 1 个 `response_body_preview` | 看不到 4 个独立面(下游请求 / 上游请求 / 上游响应 / 下游响应) |
| 上游响应已被 codec 解压再回写下游 → 看不到上游原始压缩字节 | wire 不忠实 |
| logging filter 一次性收尾 emit | 请求路径与归档路径耦合;长流式响应得等到 logging 才有 record |
| Go consumer 标为"可选" | 与 180 天合规需求矛盾 |

---

## 2. 设计决策

### ADR-D-1 单流多 kind,5 个 surface 各自独立 XADD

`usage:detailed` 不拆分,5 种 `kind` 走同一条 Redis stream:`downstream_request` / `upstream_request` / `upstream_response` / `downstream_response` / `summary`。每个 surface 完成时**独立** XADD 一次,不等齐 5 个再发。

**理由**:
- 单流 = Go consumer 单一 XREADGROUP,运维简化
- per-surface 触发 = 上游响应完成时立刻归档,不必等下游 emit 完;请求路径与归档路径解耦,流式长请求不卡在归档上
- 5 个 kind 用同一字段集(`ts_ms / request_id / trace_id / kind / encoding / payload`),Go 按 `kind` 分支处理 payload schema

**否决**:多 stream 拆分(如 `usage:detailed:meta` + `usage:detailed:body`)——增加 consumer 复杂度,且 Go 端如要重建一个请求的完整快照,反而需要跨 stream join,得不偿失。

### ADR-D-2 wire 忠实:header / body 不脱敏,上游响应存原始压缩字节

- header 完整保留(`authorization` / `x-api-key` / `cookie` 不再 denylist)
- body 不做内容替换、不截断、不采样
- 上游响应 body 存 vendor 给出的原始字节(含 `content-encoding: gzip` 等),**不**先 Rust 侧解压

**理由**:用户明确"真完整"。安全风险(180 天存 token)由用户承担,本 spec 不在 Rust 侧加额外脱敏。Go drainer 如需脱敏,在自己的归档 sink 上做,Rust 不知情。

**实现要点**:上游响应捕获点在 `response_body_filter` 的**上游侧**(codec 解压前的 raw bytes),而不是下游 emit 前的解压后字节。`downstream_response` 才是给客户端的 wire bytes。

### ADR-D-3 分级压缩:基于 payload 大小动态选择 raw / lz4 / zstd

| 区间(JSON 序列化后字节) | 算法 | encoding 字段值 |
|---|---|---|
| `< small_threshold_bytes`(默认 1 KiB) | 不压缩 | `raw+json` |
| `< medium_threshold_bytes`(默认 64 KiB) | LZ4 帧 | `lz4+json` |
| `≥ medium_threshold_bytes` | zstd level 3 | `zstd+json` |

**理由**:小载荷压缩头开销 > 收益;中等载荷 LZ4 简单快速(~500 MB/s/core,比率 1.8~2.5x);大载荷 zstd-3 是速度/比率甜区(~300 MB/s/core,比率 3~4x)。三档覆盖典型工作负载,无需更多层级。

**依赖**:Rust 侧 `lz4_flex`(纯 Rust)+ `zstd`(已在 workspace 内)。Go 侧 `pierrec/lz4` + `klauspost/compress/zstd`。

### ADR-D-4 热路径 ⫫ 归档路径,bounded mpsc + flusher pool

Pingora worker(请求路径)只做:克隆 `Arc<Bytes>` body 引用 + `try_send` 进 channel。**不**压缩、**不** XADD、**不**做 Redis I/O。

`DetailedFlusher pool`(独立 tokio tasks,默认 `num_cpus / 4`,最少 1)消费 mpsc:决定 encoding → 压缩 → XADD。

**理由**:多模态请求 body 可达 MB,zstd-3 处理 MB 级 payload 约 2~5 ms。如果在 Pingora worker 同步压缩,会显著影响同 worker 上其他请求的延迟。

**Bounded mpsc**:capacity = 65536 entries(可配置)。满则 `try_send` 失败、drop entry、metric `gw_detailed_log_drop_total{reason=channel_full}`。**不**做 spool / DLQ — drop-on-outage 策略。

### ADR-D-5 base64 装载二进制安全的 body

JSON payload 内部 body 字段统一用 `body_b64`,因为去掉 header denylist 后 body 可能是任意字节(gzip / br 压缩、protobuf、非 UTF-8 文本),JSON 内裸字符串会破坏 schema。

**代价**:base64 序列化前 +33%,实际压缩后差距可忽略(LZ4/zstd 对 base64 仍能找到结构性重复)。

### ADR-D-6 错误路径也要 emit summary

请求异常中断(连接断开、上游超时、ctx 错误)时,前 4 个 surface 可能已经触发了 0~N 个,`summary` 必须**仍然** emit,携带 `error_code` 与不完整的 `output_tokens` / `latency_ms` 字段。

**理由**:Go drainer 通过 `summary` 缺失 vs 存在判定请求是否到达生命周期终点。`summary` 永远存在,缺失即"未到达 logging filter"(Rust 进程崩溃 / panic / mpsc 满)。

---

## 3. Schema

### 3.1 Redis Stream entry fields

```
ts_ms      i64    UTC wall-clock 毫秒
request_id text   r_*
trace_id   text   t_*
kind       enum   downstream_request | upstream_request
                  | upstream_response | downstream_response | summary
encoding   enum   raw+json | lz4+json | zstd+json
payload    bytes  encoding 决定如何解码;解码后是 JSON
```

### 3.2 `payload` 解码后 JSON schema(按 `kind` 分支)

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

- `headers`:数组,保持出现顺序,**不**去重、**不**脱敏。`downstream_request` 是客户端原样;`upstream_request` 是 Rust 转发给 vendor 的最终字节(同协议 passthrough 时与 downstream 一致,跨协议 re-render 时是转换后的形态)。
- `body_b64`:base64-standard(RFC 4648),不含换行。空 body 时为 `""`。

#### `kind = upstream_response` / `downstream_response`

```json
{
  "status": 200,
  "headers": [["content-type", "application/json"], ...],
  "body_b64": "..."
}
```

- `upstream_response.body_b64`:vendor 给的**原始字节**(含 `content-encoding` 未解压形态)
- `downstream_response.body_b64`:发给客户端的最终字节(codec 重渲染后、`content-encoding` 由 Rust 决定的形态)
- 流式响应:**聚合**整个流的全部字节(SSE event 串接、按 wire 顺序)。Rust 进程内累积,响应结束后单次 XADD。**不**做 per-chunk 触发。

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

字段集 = 现 `DetailedLogRecord` 减去 `request_body_preview` 与 `response_body_preview`。可选字段沿用现含义。

### 3.3 与现 `DetailedLogRecord` 的关系

`DetailedLogRecord` 类型保留(`crates/gw-proxy/src/detailed_log.rs`),改造为只承载 `summary` kind 的内容:
- 删除 `request_body_preview` / `response_body_preview` 字段
- 新增/调整字段:无(其它字段不变)

`redacted_preview` 函数 + `PREVIEW_MAX_BYTES` 常量删除。

---

## 4. 触发点(Pingora filter chain)

| Surface | 触发时机 | 钩子 |
|---|---|---|
| `downstream_request` | 下游 body 累积完成 | `request_body_filter` 最后一片处理完 / `request_filter` 收尾 |
| `upstream_request` | 转发字节就绪、即将发给 vendor | `upstream_request_filter` 出口 |
| `upstream_response` | 上游响应全部字节收到(流式则最后一片入网关) | 上游 `response_body_filter` 最后一片 |
| `downstream_response` | 已发完给客户端 | 下游 `response_body_filter` 最后一片 emit 后 |
| `summary` | 请求生命周期结束 | `logging` filter |

**顺序保证**:每个 surface 在它的钩子点 `try_send` 一次,**不**保证到达 Redis 的顺序。Go consumer 按 `(request_id, kind)` 索引,不做顺序假设。

**重复保证**:每个 `(request_id, kind)` Rust 端最多 emit 一次。

**Body 累积**:`ReqCtx` 持有 4 个可选缓冲 `Arc<BytesMut>`(或 `Vec<u8>`),分别对应下游请求 / 上游请求 / 上游响应 / 下游响应。每个 surface 触发时 freeze 成 `Arc<Bytes>` 并 send 进 channel(零拷贝);随后 ReqCtx 释放本地引用。

**Passthrough**:同协议 passthrough 场景下,`upstream_request` 字节 = `downstream_request` 字节,`downstream_response` 字节 = `upstream_response` 字节。本 spec **不**做共享压缩结果的优化——4 个 surface 各自独立压缩并 XADD。语义简单、failure 路径只影响单个 entry。

---

## 5. 数据通路

```
Pingora worker(请求路径,永不阻塞在归档上)
  ├─ request_body_filter 最后一片
  │      → tx.try_send(Captured{kind=downstream_request, body=Arc<Bytes>, headers, request_id, ts})
  ├─ upstream_request_filter
  │      → tx.try_send(Captured{kind=upstream_request, ...})
  ├─ 上游 response_body_filter 收最后一片
  │      → tx.try_send(Captured{kind=upstream_response, ...})
  ├─ 下游 response_body_filter 最后一片 emit 后
  │      → tx.try_send(Captured{kind=downstream_response, ...})
  └─ logging filter
         → tx.try_send(Captured{kind=summary, ...})

      ↓ bounded mpsc channel(capacity 默认 65536)

DetailedFlusher pool(独立 tokio tasks,默认 num_cpus.saturating_div(4).max(1))
  recv → 选 encoding(按 payload 大小)→ 压缩 → XADD usage:detailed MAXLEN ~ N
  XADD 失败:metric + drop
```

**热路径成本**:克隆 `Arc<Bytes>` 引用(原子计数 +1)+ `try_send`(无锁 mpsc,sub-µs 级)。压缩 + Redis I/O 全部在 flusher 一侧。

---

## 6. Go 协同(替换 parts/02 §2.3 整段)

### 6.1 角色

- Rust:writer(单向),按 §4 触发点 XADD
- Go:**必需 consumer**(不再"可选");长期归档(180 天合规)责任方

### 6.2 消费契约

| 字段 | 值 |
|---|---|
| Stream | `usage:detailed` |
| 消费方式 | `XREADGROUP GROUP go-archive-drainer <consumer> COUNT N BLOCK <ms> STREAMS usage:detailed >` |
| 起始位置 | `0`(消费全量) |
| ACK | 处理成功后 `XACK`;失败由 Go 自决重试或落 Go 侧 DLQ |
| 解码 | 按 `encoding` 字段分支:`raw+json` 直 parse;`lz4+json` LZ4 解压后 parse;`zstd+json` zstd 解压后 parse |

### 6.3 索引与重组

Go 按 `(request_id, kind)` 建立索引。一个请求的完整快照 = 5 条 entry(`downstream_request` + `upstream_request` + `upstream_response` + `downstream_response` + `summary`)。

**缺失语义**:
- 某 surface 缺失 = 该 surface 在 Rust 侧未归档成功(mpsc 满 / 压缩失败 / XADD 失败 / 进程崩溃)。**不**表示请求该 surface 不存在。
- `summary` 缺失 = 请求未到达 logging filter(进程崩溃 / mpsc 满 / panic)。建议 Go 设置 fallback 超时(收到首个 surface 后 60 秒内 `summary` 未到 → 标记该 request_id 为不完整快照)。
- 全部 5 个都缺 = 请求未触发任何归档(早期 filter 抛错 / mpsc 全满)。

### 6.4 长期归档

Go drainer 后台批量将 entry 落到长期存储(S3 / GCS / parquet / lakehouse,任选)。`detailed_log.retention_days = 180` 由 Go 端 lifecycle policy 实现,Rust 不持有保留期参数。

### 6.5 监控义务(Go 端)

| Metric | 阈值 |
|---|---|
| `usage_detailed_consume_lag_secs`(XPENDING 最老 entry 年龄) | > 60s 告警 |
| `usage_detailed_xlen`(当前 XLEN) | > MAXLEN × 0.8 告警(滚动窗口预警) |
| `usage_detailed_processed_total` | counter,仅观测 |
| `usage_detailed_decode_failure_total{reason}` | counter,>0 排查 |
| `usage_detailed_archive_failure_total` | counter,> 1% 告警 |

### 6.6 强度

`usage:detailed` 不是账务流,**没有 DLQ**。Go drainer 跟不上 Redis MAXLEN 滚动 → 永久丢失未消费 entry。180 天合规要求 = Go drainer 必须不挂;挂了的处置是运维 oncall 问题,不在协议保证内。

### 6.7 单向

Rust 写、Go 读。Go **禁止**反向写 `usage:detailed`。

---

## 7. Configuration

### 7.1 `config/default.toml`

```toml
[detailed_log]
enabled = true
stream = "usage:detailed"
maxlen = 10_000_000                      # Redis XADD MAXLEN ~ N

[detailed_log.compression]
small_threshold_bytes  = 1024            # < 1 KiB → raw+json
medium_threshold_bytes = 65536           # < 64 KiB → lz4+json; ≥ → zstd+json
zstd_level = 3

[detailed_log.channel]
capacity = 65536                         # bounded mpsc,满则 drop

[detailed_log.flusher]
workers = 0                              # 0 = num_cpus.saturating_div(4).max(1)

[detailed_log.limits]
single_payload_soft_cap_bytes = 8388608  # 8 MiB 压缩前软上限,超则 drop+metric
```

### 7.2 删除项(`[debug]` 表下)

- `sampling_rate`
- `body_capture_*` 系列 toggle
- `redactor` 配置(若存在)

迁移:用户 PR 内同步移除 `config/default.toml` / `config/local.toml.example` 下相关字段。当前 local 配置如包含已移除字段,启动期 figment 会报错(unknown field),提示用户更新——符合"不保留旧字段"原则。

---

## 8. Metrics

### 8.1 失败/降级

| Metric | 类型 | 标签 |
|---|---|---|
| `gw_detailed_log_drop_total` | counter | `reason="channel_full" \| "oversize" \| "serialize_failed" \| "compress_failed" \| "xadd_failed"`,`kind` |

### 8.2 吞吐与开销

| Metric | 类型 | 标签 |
|---|---|---|
| `gw_detailed_log_emit_total` | counter | `kind`,`encoding` |
| `gw_detailed_log_payload_bytes` | histogram | `kind`,`encoding`(压缩后字节) |
| `gw_detailed_log_pre_compress_bytes` | histogram | `kind`(压缩前字节) |
| `gw_detailed_log_compress_seconds` | histogram | `encoding` |
| `gw_detailed_log_channel_depth` | gauge | — |

### 8.3 已废除指标

- `gw_detailed_log_oversized_total`(合并到 `gw_detailed_log_drop_total{reason="oversize"}`)

---

## 9. 失败语义

| 场景 | Rust 行为 |
|---|---|
| `try_send` 失败(channel 满) | drop entry + metric;请求继续 |
| 单 payload 序列化后超 `single_payload_soft_cap_bytes` | drop entry + metric |
| `serde_json::to_vec` 失败 | drop entry + metric |
| 压缩失败(lz4_flex / zstd 报错) | drop entry + metric |
| XADD 失败(Redis 不可达 / TIMEOUT / network error) | drop entry + metric |
| Rust 进程崩溃 | 已入 channel 未 flush 全丢;Redis 上已 XADD 的 entry 不受影响 |
| Redis MAXLEN 滚动早于 Go drainer 消费 | XADD 时自动 trim;Rust 无感知,Go 侧靠 lag 监控 |
| 上游响应 `content-encoding` 异常 / 不可识别 | 存原始字节(本来就不解码),无影响 |

**通用规则**:任何归档侧的错误**不**反向影响请求路径。请求该返 200 仍返 200;该返错也按错误正常返,不因归档失败掉链子。

---

## 10. 不变量与 lint

### 10.1 现 lint 的调整(`scripts/check-invariants.sh`)

- 保留:`no_cost_in_dlq`、`no_cost_micro`(与本 spec 无关)
- 调整:如有 `redact_*` 函数引用的 lint,需收紧到只在 summary 路径触发(或全部移除,本 spec 不再做 body redaction)

### 10.2 新增 lint(本 PR 同步落地)

- `no_detailed_redaction`:禁止 `redact_body` / `redact_headers` / `redact_message_content` 等命名重新出现在 `crates/gw-ledger/` 或 `crates/gw-proxy/` 内
- `no_detailed_truncation`:禁止 `PREVIEW_MAX_BYTES` / `MAX_DETAILED_LOG_BYTES` 常量名重新出现

两项均加入 `scripts/check-invariants.sh`,失败即 CI 红。

---

## 11. 文档同步(按 CLAUDE.md Sync rule)

落地 PR 内同 commit 更新:

| 文件 | 改动 |
|---|---|
| `docs/architecture.md` §3 | `Redactor` trait 段落:如该 trait 仅服务 detailed log,删除;若服务其它链路(如 `usage:records` 的 ext 字段)则保留并标注"不再用于 detailed log" |
| `docs/architecture.md` §4.1 | 表格中 `usage:detailed` 行重写为新行为(完整 / wire 忠实 / 分级压缩 / per-surface 触发 / Go 必需 consumer / 无 DLQ) |
| `docs/architecture.md` §7 | 若 §10.2 lint 落地,加入 invariants 表 |
| `docs/workflow.md` §filter chain DetailedLog 段 | 整段重写:5 个触发点 + mpsc + flusher pool,删除"`MAXLEN` trim"、"body preview"、"sampling rate"等过时描述 |
| `docs/go-contracts/parts/02-go-as-consumer.md` §2.3 | 整段重写,采用本 spec §6 内容 |
| `docs/go-contracts/main.md` §3.3 | 表格中 `usage:detailed` 行:reader 改为 `Go drainer(必需)`,consumer group 列改为 `go-archive-drainer`(推荐) |
| `CLAUDE.md` | "Observability" 一行的 `usage:detailed`(verbose, three-level redaction, XADD MAXLEN) 描述更新为新行为 |

---

## 12. 不在范围(明确)

- 长期归档存储选型(S3 / GCS / 文件 / parquet) — Go 端决定
- 字段级二级脱敏 — Go drainer 自决
- DLQ / spool / replay — drop-on-outage
- 用户/key 维度归档开关 — 不接受
- 多 stream 拆分 — 单流
- cost / 计费 — `usage:records` 不变
- 与 `usage:records` 的统一(归档 vs 计费)— 两条流职责不同,各自演进

---

## 13. 验证锚点

实施期由 implementation plan 落具体步骤;本 spec 落地后必须满足:

1. **完整性**:`downstream_request` 与 `upstream_request` 解码后 body 大小 == 原 wire bytes(对 1 KiB / 1 MiB / 4 MiB 三档抽样,均落于 `single_payload_soft_cap_bytes` 之下);`downstream_response` 同;`upstream_response` 解码后字节 == vendor 给出的原始字节(含 `content-encoding`)。
   - 边界用例:body == 8 MiB 时验证 entry 被 drop 且 `gw_detailed_log_drop_total{reason="oversize"}` +1,请求自身正常返回
2. **不脱敏**:`Authorization` / `x-api-key` / `Cookie` 在解码 headers 中可见(本 spec 明确决策)。
3. **per-surface 触发**:对一个 100-event SSE 流式请求,5 条 entry 全部到达 Redis 且 `summary.is_stream=true`、`output_tokens` 与 `upstream_response.body_b64` 解码后 token 数对应。
4. **错误路径 summary**:连接断开 / 上游 503 / panic 试验下,`summary` 仍 emit,`error_code` 字段非空。
5. **解耦**:压缩 worker artificially 拖慢(注入 1s sleep),请求 P99 延迟无回归(归档 P99 升高,验证)。
6. **drop 计量**:channel 容量调到 8 + 高并发压测,`gw_detailed_log_drop_total{reason="channel_full"}` > 0;请求 5xx 比例不上升。
7. **分级压缩**:200 B / 4 KiB / 200 KiB 三档 body 各跑一次,`encoding` 字段对应 `raw+json` / `lz4+json` / `zstd+json`。
