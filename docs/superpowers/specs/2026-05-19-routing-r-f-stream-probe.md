# R-F — Stream Probe(首 chunk 前 failover)(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-routing-r-e-failover.md](./2026-05-19-routing-r-e-failover.md)(**硬前置**:FailoverState)
  - [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md)(framer 在 gw-proxy)
  - Aether 来源:`services/task/core/exceptions.py:StreamProbeError` + `candidate/failover.py`
- **范围**：在 gw-proxy 的 stream 桥接层加"首 chunk 前可切 key"窗口期:
  - 上游已收到响应 status + 部分 body chunk,但**未向客户端发送任何字节**;
  - 此期间若上游报错(5xx / 异常 SSE 帧 / 连接断开),按 R-E 切下一 key,**客户端无感**;
  - 一旦首 chunk 通过 framer 解出至少一个有效 IR event 并写到客户端 → 锁定 stream(`bytes_sent_to_client > 0`,后续按 R-E 默认不切);
  - 探测期超时(默认 5 秒)上游仍未发首 chunk → 视为 transient 失败,触发 failover。
- **不在范围**：
  - 流中段重试(R-E 已声明 v1 不允许);
  - 非流响应路径(R-E 默认 path);
  - codec 内部 framer 状态机(→ codec S3)。

> R-F 把"流式响应启动期"从 R-E 的盲区里救回来。它是 gw-proxy 层的薄改造,几乎不涉及业务逻辑。

---

## 1. 背景

R-E 默认 `allow_stream_retry: false`,字面是"`bytes_sent_to_client > 0` 后不切"。但实际上 stream 上游连接成功 ≠ 客户端开始收字节;Pingora `upstream_response_body_filter` 在 gw-proxy 调"send to client"之间还有 codec 解析的窗口。这段时间内:

- 上游可能发了 5xx 错误体(不是 SSE);
- 上游可能发了 malformed SSE 第一帧;
- 上游可能 idle 没发任何字节超过预期 TTFT;
- 上游可能 TLS 握手后立即 RST 连接;

任何一种情况,客户端**还没收到任何 SSE 字节**,gw-proxy 可以安全切到下一 key。Aether 在 `failover.py:_FailoverHandler.handle_stream_attempt` 显式跑这套"probe"逻辑。

Rust 当前完全没有这套窗口。一旦 Pingora 切到 stream mode,任何上游错误都会原样转给客户端(部分情况下连接直接挂)。

## 2. 设计决策

### ADR-RF-1 stream probe 状态机

在 `gw-proxy::stream::StreamRelay`(参 codec S3 §ADR-S3-v2-4)上加一个 probe 阶段:

```rust
pub enum StreamPhase {
    /// 还在等上游响应 status(connect / TLS handshake / 请求发完毕)
    Connecting,
    /// 收到 status + 解码 framer 累计 < 第一个 IR event
    Probing { since: Instant },
    /// 第一个 IR event 已经写到客户端 buffer,锁定该 key
    Streaming,
    /// 上游或客户端断开
    Done,
}

pub struct StreamRelay<S: ProtocolStreamCodec, T: WireStreamTarget> {
    src_codec: S,
    framer: Framer,
    accum: T::Accum,
    builder: T::ChunkBuilder,
    phase: StreamPhase,
    probe_timeout: Duration,
}
```

probe_timeout 默认 5 秒(配置:`GW_STREAM__PROBE_TIMEOUT_MS`,per-ModelGroup 也可覆盖)。

### ADR-RF-2 phase 跃迁规则

| from | to | 触发 |
|---|---|---|
| Connecting | Probing | 上游 response status 收到(无论 200 还是 5xx) |
| Probing | Streaming | framer 解出第一个 IR event 且写入 client out buffer |
| Probing | Done(success path:不算错) | 上游 [DONE] / 终态事件,但 0 个内容 event(罕见,空响应) |
| Probing | Failover trigger | 上游 4xx/5xx body + 未收到任何 IR event |
| Probing | Failover trigger | probe_timeout 超时 |
| Probing | Failover trigger | 上游连接断开 + 0 IR event |
| Streaming | Done | 上游 [DONE] 或客户端断 |

**关键**:Failover trigger 时:

- `ctx.failover_state.bytes_sent_to_client` 仍为 0(因为 phase 还在 Probing,无任何字节写客户端);
- 触发 R-E `next_action(err)` → 走 SwitchKey/SameKey;
- gw-proxy 重新走 upstream_peer(新 key)→ 重发请求 body → 重新建 stream relay → phase 回到 Connecting。

### ADR-RF-3 错误判定

`StreamRelay::on_upstream_chunk` 收到 chunk 时:

```rust
fn on_upstream_chunk(&mut self, bytes: &[u8], status: u16, ctx: &mut StreamProbeCtx) -> ChunkOutcome {
    if self.phase.is_probing() {
        // 上游 status 非 200/206 → 整批是错误 body(JSON 错误响应,非 SSE)
        if !is_success_status(status) {
            return ChunkOutcome::ProbeFailure {
                err: build_error_ir_from_body(status, bytes),
                reason: ProbeFailReason::UpstreamErrorBody,
            };
        }
        // status 200 但 frame 解析全 0 个 IR event(累积了帧不完整)
        let events = self.framer.feed(bytes);
        let mut wrote_any = false;
        for frame in events {
            self.src_codec.parse_stream_chunk_to(&frame, &mut self.accum, &mut self.builder)?;
            // 检查 builder 是否产出任何 event
            if self.builder.has_pending_event() {
                wrote_any = true;
            }
        }
        if wrote_any {
            self.phase = StreamPhase::Streaming;
            return ChunkOutcome::FirstEventEmitted;
        }
        // 仍在 probing,继续等下一 chunk
        ChunkOutcome::Probing
    } else {
        // 已 Streaming,正常处理
        ...
    }
}

pub enum ChunkOutcome {
    Probing,
    FirstEventEmitted,
    ProbeFailure { err: ErrorIR<'static>, reason: ProbeFailReason },
    StreamData,
    StreamEnd,
}

pub enum ProbeFailReason {
    UpstreamErrorBody,
    ProbeTimeout,
    UpstreamDisconnect,
    MalformedFrame,
}
```

### ADR-RF-4 timeout watchdog

probe 不能无限等 — 上游 idle 超过 probe_timeout 视为 failure:

```rust
// gw-proxy 的 stream loop 用 tokio::select!
loop {
    tokio::select! {
        chunk = upstream.next_chunk() => {
            match relay.on_upstream_chunk(chunk?, status, &mut ctx) {
                ChunkOutcome::ProbeFailure { err, .. } => {
                    return ctx.trigger_failover(err);
                }
                ChunkOutcome::StreamData | ChunkOutcome::FirstEventEmitted => {
                    flush_to_client(...);
                }
                ...
            }
        }
        _ = tokio::time::sleep_until(relay.probe_deadline()) => {
            if relay.phase.is_probing() {
                return ctx.trigger_failover(probe_timeout_error());
            }
            // 已 Streaming → idle 是正常,继续等
        }
    }
}
```

`probe_deadline()` = phase Probing 开始时间 + probe_timeout。已进 Streaming 则不再用 deadline。

### ADR-RF-5 与 R-E 的衔接

`ctx.trigger_failover(err)` 在 R-F probe 路径上调用,与 R-E on_upstream_error 走同一条:

```rust
fn trigger_failover(&mut self, err: ErrorIR<'static>) -> ControlFlow {
    let state = self.failover.as_mut().expect("must exist");
    state.last_error = Some(err.clone());

    // R-E 默认 allow_stream_retry=false → bytes_sent_to_client > 0 时 Exhausted。
    // 但 R-F 在 probe 阶段保证 bytes_sent_to_client == 0,所以走 SwitchKey 路径。
    // ⚠️ K-B 已经在 logging 路径上 cooldown 该 vendor_key;next request 自动 filter。
    //    R-F 这里只关心本请求的 fallback。

    match state.next_action(&err, &self.failover_cfg) {
        FailoverAction::SameKey | FailoverAction::NextKey => {
            // 关键:**重置 stream relay**,因为新 key 重新建连接 + 新 framer
            self.reset_stream_relay();
            ControlFlow::Retry
        }
        FailoverAction::Exhausted(reason) => ControlFlow::Bail(build_exhausted_error(reason, Some(err))),
    }
}
```

reset_stream_relay 把 phase 回 Connecting,framer 与 accum 都新建(旧的内部状态废弃)。

### ADR-RF-6 K-B 错误分类影响

`UpstreamErrorBody` 的 err.status 由响应 status 决定(401/403/429/5xx),走 R-E 的 classify_for_failover 映射:

- 401/403 → SwitchKey(账号问题)
- 5xx → SameKey(transient)
- 429 → SwitchKey(rate limit)

`ProbeTimeout` 视同 504(`UpstreamTimeout`),retryable=true → SameKey 或 SwitchKey 看 R-E 配置。

### ADR-RF-7 流式但不带 stream=true 的请求

非流请求(stream=false)走 R-E 主路径,不进 stream relay,本 spec 不适用。

如果 stream 请求收到上游响应是非流的(content-type != text/event-stream),按 codec S3 framer 是空输出 → probe 永远不会"emit first event" → 超时 trigger failover。这是合理行为(上游不支持流应该换 key)。

### ADR-RF-8 性能预算

| 操作 | 预算 |
|---|---|
| phase transition Connecting → Probing | < 50 ns(原子写) |
| `on_upstream_chunk` probe path 检查 | < 1 µs(包含 framer.feed) |
| watchdog tokio sleep_until | < 50 ns 唤醒 |
| reset_stream_relay | < 1 µs(SmallVec / VecDeque clear) |

probe 不在 hot path 上;hot path 是 stream 已开始(Streaming phase),与现状性能一致。

### ADR-RF-9 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_stream_probe_outcome_total` | result | counter:probe_pass / upstream_error / timeout / disconnect / malformed |
| `gw_stream_probe_duration_ms` | result | histogram:从 connect 到 first_event(成功)或 trigger(失败) |
| `gw_stream_probe_failover_total` | reason | counter:probe 触发 failover 次数 |
| `gw_stream_first_event_after_chunks` | - | histogram:首 event 出来前累积了几个 chunk(粘包度) |

### ADR-RF-10 测试

- 模拟上游 200 + 立即 [DONE](空响应) → probe 不 fail(允许空响应);
- 上游 500 + JSON 错误体 → probe failure UpstreamErrorBody → R-E SwitchKey;
- 上游 200 + 5 秒不发 chunk → ProbeTimeout → failover;
- 上游 200 + 发 corrupt 字节 → framer feed 解出 0 event + 累积 → 累积超过阈值仍 0 event 时是否触发?**v1 不在此处主动触发**(由 timeout 兜底);
- 上游 200 + 首 frame 解出有效 event → phase Streaming → 之后上游 5xx → R-E Exhausted::StreamAlreadyStarted;
- failover 后新 key 200 + 流正常 → 客户端只看到一次连续 SSE(无两个 message_start);
- per_key_max_retries:probe failure → SameKey retry × max → NextKey;
- probe_timeout 在 ModelGroup 配置覆盖默认。

## 3. 实施大纲

依赖:R-E land(FailoverState),codec S3 land(framer / WireStreamTarget),codec S2 land(builder.has_pending_event 接口)。

1. 改 `gw-proxy::stream::StreamRelay` 加 phase + probe_timeout 字段;
2. on_upstream_chunk 加 probe 分支;
3. stream loop 用 tokio::select! 加 watchdog 分支;
4. trigger_failover + reset_stream_relay 集成 R-E;
5. ModelGroup config 加 `stream_probe_timeout_ms`(可选);
6. metrics + 测试。

## 4. 未决问题

1. **probe_timeout 默认值 5s 是否合理**: 视频/长任务可能需要更长(上游真要思考几秒才发 thinking 块)。倾向 per-vendor 配置,Anthropic/OpenAI 默认 5s,Vertex Claude 用 10s。
2. **空响应判定**: 上游 200 + [DONE] 不发任何 content event — 应该算成功还是失败?(用户付了费没拿到内容。)倾向**算成功**(R-F 不判断业务对错;返回空 stream 给客户端,客户端自己判断要不要重试)。但 metric `gw_stream_probe_outcome_total{result="empty_response"}` 单独统计。
3. **malformed frame 累积阈值**: codec S3 ADR-S3-v2-5 决定单帧解析失败 `Ok(SmallVec::new())` + warn,不短路。但若所有帧都 malformed(累积 10+ chunk 无 event),应触发 failover 还是相信会恢复?倾向加上限:`malformed_chunk_max = 10` 或 `accumulated_bytes > 64KB` 仍无 event → probe failure。
4. **probe 期间 first event 是否真的写到客户端**: codec S3 `T::finalize_chunk` 写到 buf;buf 之后才 `session.write_response_body(buf)`. 这两步之间能精确感知"已发出"吗?Pingora API 应该支持(返回写入字节数)。倾向"finalize 写 buf + body filter return Ok" 后视为已发——细微差异(可能 buf 还没 flush 到 TCP)v1 接受。
5. **首个 event 含 error 的情况**: codec parse 第一个 stream chunk 解出 StreamEventIR::Error 而非 Delta — 是 probe success(转给客户端)还是 failure(走 failover)?倾向 **failure**:首 event 就是 error 说明上游本质失败,切换有意义。但 first_event_emit 判断要排除 Error variant。
6. **客户端先断的处理**: 客户端在 probe 阶段就断(罕见)— gw-proxy 直接 Done,不触发 failover(浪费 attempt)。倾向加 metric `gw_stream_probe_client_disconnect_total`。
7. **`builder.has_pending_event()` 接口**: codec S3 当前 trait 没有这个方法。本 spec 需要 codec S3 加 — 标 follow-up,与 R-F 同 PR 落。

---

> R-F 是 gw-proxy 薄改造,无新模块。完成后 stream 路径在首 chunk 前的失败可优雅切换,客户端无感。
