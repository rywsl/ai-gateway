# P0 Bug 审计与漏洞评级（2026-06-17）

> 本次审计由多 agent 并行扫描 + 二轮对抗式验证完成，仅记录有完整证据链（file:line + 关键代码片段）的发现。**所有"疑似"、"可能"、"看上去"未给出明确证据链的结论一律降级为 `INSUFFICIENT EVIDENCE`，禁止以推测形式列入 P0**。
>
> 审计范围：6 个核心 crate domain
> - `gw-billing` + `gw-ledger`（计费正确性）
> - `gw-proxy` + `gw-gateway`（请求生命周期 / Pingora hot path）
> - `gw-task-runtime` + `gw-task-worker`（后台任务循环）
> - `gw-routing`（路由 / 健康 / 预留）
> - `gw-tunnel-client` + `gw-tunnel-server`（隧道传输）
> - `gw-codec`（协议编解码 / 流式 / 跨协议渲染）
>
> 审计方法：第一轮 6 个 sonnet/haiku agent 并行扫描各 domain，按"硬 P0 标准"（影响正常使用链路 / 内存泄露 / 计费错误）输出候选清单；第二轮 6 个 haiku agent 对每条候选**回读源码**，给出 `CONFIRMED / FALSE POSITIVE / INSUFFICIENT EVIDENCE` 中之一的结论性判定，并附实际代码片段为证。

---

## 严重度评级体系

| 等级 | 标准 | 触发条件 |
|------|------|----------|
| **P0** | 直接金钱损失 / 进程不可用 / 数据丢失 | 单请求即可触发，无需并发或极端配置 |
| **P1** | 资源累积性损耗（FD/permit/内存/PEL）/ 间歇性计费偏差 | 高负载或长时间运行后显现 |
| **P2** | 局部正确性 / 可观测性缺失 / 结构性 RAII 违反但当前路径未触发 | 需要特定输入组合或上游配置 |

---

## 审计统计

候选发现 40 条，对抗式验证后：

| Domain | CONFIRMED | FALSE POSITIVE | 备注 |
|--------|-----------|----------------|------|
| Task-runtime | 2 / 2 | 0 | 全部高置信 |
| Tunnel | 6 / 6 | 0 | 形成耦合缺陷链 |
| Billing | 9 / 10 | 1 | F10 漏看 gw-gateway 高层 dump 调用 |
| Codec | 5 / 5 | 0 | F1 在 proxy 层是 dead code；F2/F5 根因在 schema |
| Proxy | 9 / 10 | 1 | F4 漏看 Wired 引导期对 spill_root 的覆写 |
| Routing | 6 / 7 | 1 | F5 把 CacheAffinity 与 Sticky 混淆 |
| **合计** | **37** | **3** | — |

> 其中 3 条 FALSE POSITIVE 在第一轮 agent 报告中被列为 P0，但经第二轮验证发现：要么忽略了跨 crate 的补救调用，要么把无状态策略与有状态策略混淆。**本文档不再保留它们为 P0**，单独在文末"FALSE POSITIVE"章节记录排除理由，避免未来重复误报。

---

## P0 清单（按金钱 / 可用性影响排序）

### Billing-1：Lease 在 `actual_usage_credits` 返回 Err 时永久泄漏 credit

- **文件**：`crates/gw-proxy/src/filters/logging.rs:163-177` + `crates/gw-ledger/src/lease.rs:196-215`
- **现象**：
  - `lease.rs:208-215` 的 `release()` 仅做 `CAS(released: false → true)`，**不退款**。
  - `lease.rs:196-206` 的 `settle_actual()` 才是唯一退款路径（`state.credits.fetch_add(refund, …)`）。
  - `logging.rs:163-171` 在 `actual_usage_credits` 返回 `Err` 时只打 `warn!` 然后跳过 `settle_actual`；随后 `logging.rs:175-177` 仅调 `release()`。
- **后果**：`try_acquire` 已经 CAS-decrement 的 credit 永久从 lease pool 消耗，`UsageRecord` emit `credits_consumed: None`，Go 端**无法对账**。
- **触发条件**：任意 coef lookup 失败（model 被移除、coef 版本被 `sweep_orphans` 驱逐）。单请求触发，无需并发。
- **置信度**：高。
- **修复方向**：Err 分支也应调用 `settle_actual(0)`，或让 `release()` 在 `settled` 未置位时自动退款。

### Billing-5：`LeaseGuard::Drop` 不退款 — RAII 结构性违反

- **文件**：`crates/gw-ledger/src/lease.rs:218-222`，对照 `crates/gw-ledger/src/freeze.rs:128-141`
- **现象**：
  - `LeaseGuard::Drop` 仅调 `release()`（CAS 翻 marker），无 `fetch_add` 退款。
  - 对比：`RequestFreeze::Drop` 在 `released.swap(true, …)` 为 false 时主动 `fetch_sub(freeze_amount …)`、`enqueue_frozen_delta` 并 `incr gateway_freeze_leak_recovered_total`。
- **后果**：`try_acquire` 与 `settle_actual` 之间任何 panic / cancel / `?` 早返回都会永久泄漏。Billing-1、Billing-6 都是该结构性缺陷的具体表现。
- **置信度**：高。
- **修复方向**：让 `LeaseGuard::Drop` 在未 `settled` 时自动调用 `settle_actual(0)`，与 `RequestFreeze` 对齐。

### Billing-6：Protected-mode CLI 路径 `release()` 后 early-return，跳过 settle 与 UsageRecord emit

- **文件**：`crates/gw-proxy/src/filters/logging.rs:127-148`
- **现象**：当 `usage_snapshot_and_provenance_for_record(ctx, ok)` 返回 `None`（CLI billing protection：CLI-shape + 零 billable usage + 无 `prompt_estimate_tokens`）时，代码 `release()` 后 `return`，**既不退款也不 emit `usage:records`**。
- **后果**：credit 泄漏 + `usage:records + dlq == fake_upstream_hits_total` 等量不变式被打破（under-count）。
- **置信度**：高。
- **修复方向**：early-return 前调 `settle_actual(0)` 退款，并 emit `UsageRecord { credits_consumed: Some(0), vendor_exposure_state: Terminated }` 使 Go 可对账。

### Billing-2：Pipeline RTT 失败时整批 records 路由到 DLQ，inflight key 不 DEL

- **文件**：`crates/gw-ledger/src/flusher.rs:979-998`
- **现象**：`pipe.all()` 返回 `Err` 时，代码把 `queued_indices` 中所有记录 `route_to_dlq` 并显式跳过 DEL（注释：`// No DEL — DLQ path leaves inflight key for reconciler.`）。但 Redis pipeline 失败**不等于**所有 XADD 都未执行 — 部分可能已应用到 server 端。
- **后果**：partial success 的记录被 DLQ 重发 + reconciler 启动后再发一次 `CrashFallback`，单请求最多三重计费。同时打破 soak invariant。
- **置信度**：高。
- **修复方向**：RTT 失败时不主动 DLQ，留给 reconciler 走 CrashFallback；或改用 `MULTI/EXEC` 取得 all-or-nothing 语义。

### Billing-3：Reconciler 不读 `state` 字段就发射 CrashFallback

- **文件**：`crates/gw-ledger/src/inflight_reconciler.rs:44-78`，对照 `crates/gw-ledger/src/inflight.rs:66-79`
- **现象**：`inflight.rs:70` 写入的 hash 含 `state` 字段（`sent`/`settled`/`terminated`），但 `build_fallback_record` 仅做 `hash.is_empty()` 检查，**完全忽略 `state`**。
- **后果**：当上次进程 XADD 成功（`flusher.rs:972`）但后续 DEL pipeline 失败（`flusher.rs:1037` 静默忽略，注释错误地写 "Harmless"），key 残留为 `state=settled`，reconciler 会对**已经计过费的请求再发一条** `CrashFallback`。每次 ungraceful shutdown 都会触发。
- **置信度**：高。
- **修复方向**：`build_fallback_record` 在 `state == "settled"` 时直接 DEL key 不发射；可选地用 `(request_id, attempt_index)` 在 `usage:records` 做去重。

### Billing-4：`replay_file` 用 `let _ = remove_file(…)` 忽略错误

- **文件**：`crates/gw-ledger/src/backlog.rs:238-254`
- **现象**：`replay_file` 读取 DLQ JSONL → `backlog.push(env).await` → `let _ = tokio::fs::remove_file(path).await`。在 push 与 remove 之间崩溃，文件保留；下次启动 `load_into` 重读，envelope 被重新 push。
- **后果**：DLQ 流被消费时产生 duplicate billable 行。
- **置信度**：中-高（窗口窄但实际可触发，特别是 Redis outage 期间）。
- **修复方向**：rename-to-`.replaying` → replay → delete 的两步流程；或对 `(request_id, attempt_index)` 持久化去重集合。

### Billing-7：`u64 as i64` 静默 wrap — `AtomicI64 frozen_micro` 余额可被腐蚀

- **文件**：`crates/gw-ledger/src/freeze.rs:89, 90, 123, 124`
- **现象**：`RequestFreeze::new` 接 `amount: u64`，用 `as i64` 写入 `frozen_micro.fetch_add` 与 `enqueue_frozen_delta`。`amount > i64::MAX`（9.22 × 10¹⁸）时 Rust `as` 静默 wrap 到负数。
- **后果**：`fetch_add(负数)` 实际减小 frozen_micro，用户看到的"已冻结"余额比实际少；release 时同样 wrap，差额传播到 Redis `counter:user:*:frozen_micro`。
- **触发条件**：需要极端配置（`compute_freeze_estimate` 中 `token_count × conservative_per_token_micro × safety × markup > i64::MAX`），当前生产参数远低于阈值。**当前为潜在 P0，无主动触发场景**。
- **置信度**：中（机制实在但当前无触发路径）。对比：`gw-billing/src/formulas.rs:83` 用 `.clamp(0.0, i64::MAX as f64) as i64` 防御了相同问题，证明原作者意识到风险，但 freeze.rs 漏防。
- **修复方向**：`amount.min(i64::MAX as u64) as i64`，或将 `frozen_micro` 改为 `AtomicU64`。

### Billing-8：`catch_unwind` 接住 flusher panic 后 batch 被静默丢弃

- **文件**：`crates/gw-ledger/src/flusher.rs:204-215`
- **现象**：`flush()` panic 后 `taken: Vec<LedgerItem>`（已通过 `std::mem::take` 从 `buf` 取出）在作用域结束时被 drop。日志显式承认 "batch lost"。
- **后果**：inflight key 残留 → reconciler 发 `CrashFallback`，但 CrashFallback `output_tokens = 0`（`inflight_reconciler.rs:117`），相当于**对真实计费数据系统性 under-charge**。
- **置信度**：中-高（panic 罕见但损失彻底）。
- **修复方向**：在 `mem::take` 前 clone 到 panic-recovery 缓冲；或彻底消除 `flush()` 中的 `unwrap/expect`，直接错误传播。

### Proxy-1：`upstream_body_accum` 非流式响应无上限累积 → OOM

- **文件**：`crates/gw-proxy/src/filters/response.rs:1023-1027` + `crates/gw-proxy/src/ctx.rs:529-532`
- **现象**：`handle_non_stream_chunk` 调用 `ctx.upstream_body_accum.get_or_insert_with(BytesMut::new).extend_from_slice(b)`，**无 `len()` guard、无 `ArchiveCaps` 介入**。请求体 16 MiB 上限不适用于响应体。
- **后果**：单个非流式请求（embeddings 批量、image API 等返回大 body 的接口）即可触发 OOM。
- **置信度**：高 — 最确定性的 OOM 向量，无需特殊上游行为。
- **修复方向**：加可配置硬上限（建议 32-64 MiB），超出时 truncate 标志或返回 `GW_UPSTREAM_BODY_TRUNCATED`。

### Proxy-8：跨协议非流式 pre-EOS chunk suppression 叠加 Proxy-1，峰值 1-2× body

- **文件**：`crates/gw-proxy/src/filters/response.rs:196-219`
- **现象**：跨协议时所有 pre-EOS chunk 被设 `*body = None`，全 body 必须在 `upstream_body_accum` 中累积到 EOS 才能 re-render。EOS 时 `pending_client_body`（re-rendered）与 `upstream_body_accum`（原 body）可能短暂共存，再加 archive 上下游累积器，峰值最多 3 份。
- **后果**：与 Proxy-1 同一缓冲，在跨协议路径放大堆开销。
- **置信度**：高。
- **修复方向**：与 Proxy-1 联动；超出阈值时回退到同协议 passthrough 或直接 502。

### Proxy-2：`sse_leftover: Vec<u8>` 无上限累积

- **文件**：`crates/gw-proxy/src/filters/response.rs:260, 445`
- **现象**：`ctx.sse_leftover.extend_from_slice(chunk)` 后用 `find_event_boundary` 检测 `\n\n`/`\r\n\r\n`；上游若不发分隔符（异常上游或单行超大 `data:` 帧），drain 循环永不前进。
- **后果**：流式路径下持续吃内存到 EOS / 超时。
- **置信度**：高。
- **修复方向**：加 4 MiB 上限，超出时强制 flush 部分帧或返回 `GW_UPSTREAM_BODY_TRUNCATED`。

### Proxy-3：`stream_probe` buffer 仅有 750ms 时间窗口，无 byte cap

- **文件**：`crates/gw-proxy/src/stream_probe.rs:37-43, 67, 98, 129-133`
- **现象**：`StreamProbeState.buffer: BytesMut`，`apply_probe_chunk*` 直接 `extend_from_slice`；`probe_timed_out` 仅做时间比较。
- **后果**：1 Gbps 上游在 750ms 内可填 ~90 MiB。
- **置信度**：高。
- **修复方向**：加 1 MiB byte cap，超出时按 probe 失败处理。

### Proxy-6：Grace-expiry EOS 路径跳过 `emit_response_surfaces` — archive 静默丢失

- **文件**：`crates/gw-proxy/src/gateway.rs:1483-1491` + `:1574-1576`
- **现象**：`GraceStatus::Expired` 分支在 `filters::response::run` 与 `emit_response_surfaces` 之前 `return Err`，`fail_to_proxy` 与 `logging` 都不会补救调用。
- **后果**：详细 archive（`usage:detailed`）丢失；`ArchiveState::Spilled` 的 spill file 句柄保留至 `ReqCtx::Drop`。**违反 "drop-on-outage 之外 archive 必送达" 的不变式**。
- **置信度**：高。
- **修复方向**：grace-expired 且 `end_of_stream=true` 时仍调 `emit_response_surfaces`，再返回 Err。

### Proxy-7：Probe 失败 + failover 耗尽时 `Ok(None)` — client 看到空 body 200

- **文件**：`crates/gw-proxy/src/gateway.rs:1076` + `:1400-1402, 1443-1447`
- **现象**：`response_filter` 在 line 1076 已经把上游 status 提交（200），之后 `upstream_response_body_filter` 探测到错误体，`retry_stream_probe_failure` 返回 `Ok(false)` 时函数返回 `Ok(None)` — header 不可撤销，client 收到 200 + 空 body。
- **后果**：失败被静默吞掉，对客户端表现为"成功的极短响应"。LLM 场景中无法区分。
- **置信度**：高（但在 Pingora 当前协议下无法事后改 status，需要其他兜底）。
- **修复方向**：失败 + 失败转移耗尽时返回 `Err`，让 Pingora 进入 `fail_to_proxy` 产生标准错误响应。

### Tunnel-1：客户端 Drop 不发 `MsgType::Close`，正常 HTTP/1.1 路径 abort 抢在 shutdown 之前

- **文件**：`crates/gw-tunnel-client/src/cnexus/transport.rs:160-181`（`TunnelStream::Drop`/`TunnelStreamReader::Drop` 仅移除 routes）、`:183-203`（`TunnelStreamWriter` 完全无 Drop 实现）；唯一发 Close 的路径 `poll_shutdown` 在 `:294-302`；对应 abort 在 `crates/gw-proxy/src/tunnel_bridge.rs:207-214, 233-238, 326-331`
- **现象**：`relay_plain/relay_plain_generic/relay_tls` 中 `request_task = tokio::spawn(... shutdown().await)`，但响应 copy 结束后 **`request_task.abort()` 抢先取消**，shutdown 来不及发 Close。
- **后果**：server 端 `relay_stream` 永远 park 在 `rx.recv()` 上 — 上游 TCP、`streams` map 项、`OwnedSemaphorePermit` 三重泄漏；`max_streams_per_conn` semaphore 在 O(请求数) 后耗尽。
- **置信度**：高。
- **修复方向**：让 `TunnelStreamWriter::drop`（和 `TunnelStreamIo::drop`）在 `out_tx` 上 `try_send` 一个 Close 帧；或重写 bridge 让响应/请求 copy 在 `select!` 中协调，确保 shutdown 完成。

### Tunnel-2：server `handle_ws_conn` 退出时不 abort `relay_stream` task

- **文件**：`crates/gw-tunnel-server/src/lib.rs:289-299` + `:314-316`
- **现象**：`tokio::spawn(relay_stream(...))` 返回的 `JoinHandle` 被丢弃；WS 关闭后 `handle_ws_conn` 直接 `Ok(())` 返回。每个 `relay_stream` 持有 `Arc<Mutex<streams>>` clone（让 Sender 不可释放），`rx.recv()` 永远 park。
- **后果**：每个 WS 连接结束时残留 N 个 `relay_stream` task + N 个上游 TCP + N 个 permit。
- **置信度**：高。
- **修复方向**：用 `JoinSet` 或 `Vec<JoinHandle>` 跟踪，函数退出前 `abort_all`；或为 `relay_stream` 加 `CancellationToken`。

### Tunnel-3：`ReconnectingTransport` 监督任务永生 + `TunnelStack::transports` DashMap 永不剪枝

- **文件**：`crates/gw-tunnel-client/src/cnexus/pool.rs:56-93`（supervisor outer `loop {}` 无 `break`），`crates/gw-proxy/src/tunnel_stack.rs:91-93`（`reload_nodes` 只 swap nodes，不动 transports），`:244-253`（lazy insert 无 remove）
- **现象**：监督任务持有 `Arc<Self>`，外层 `loop {}` 无任何退出路径；`reload_nodes` 不剪枝；项目内对 `self.transports` 无 `remove`/`retain`/`clear` 任何调用。
- **后果**：每次 node 轮换 / 配置变更 / 临时错配都泄漏一个永生重连循环 + 一份对死节点 URL 的 dial。长期运行进程 FD / 连接数无界增长。
- **置信度**：高。
- **修复方向**：supervisor 接 `CancellationToken`，`reload_nodes` diff 节点集合并 abort 已移除节点的 supervisor + 从 DashMap 删除。

### Tunnel-4：reader-task 退出时 routes 未 drain，依赖的 `TunnelStreamReader` 永远 park

- **文件**：`crates/gw-tunnel-client/src/cnexus/transport.rs:344-410`（reader exit 仅 `alive.store(false, …)`）
- **现象**：reader 退出后既不向 routes 中所有 sender 广播 `TunnelEvent::Close`，writer 也不感知 alive=false。`TunnelStreamReader::poll_read` 仅在收到 `Close` 或 `None` 时返回，永远等不到。
- **后果**：半死连接 — bridge 中 `tokio::io::copy` 永 park，stream permit 一直占用，直到请求级超时。
- **置信度**：高。
- **修复方向**：reader 退出时 lock `routes` 并向所有 sender 发 `TunnelEvent::Close`，再清空 map。

### Tunnel-5：`ReconnectingTransport::close()` 不停 supervisor — supervisor 立即重连

- **文件**：`crates/gw-tunnel-client/src/cnexus/pool.rs:127-132` + `:71-82`
- **现象**：`close()` 只调 `WsTransport::close`（置 alive=false）和 `inner.store(None)`；supervisor 内层 200ms poll 检测到 alive=false 后 `break` 内层但继续外层 `loop`，**无条件 reconnect**。
- **后果**：`close()` 名义关闭，实际持续重连。运营端 drain 节点的语义被破坏。
- **置信度**：中（取决于 `close()` 是否在运营 drain 路径调用 — 若仅 process shutdown 调用，runtime cancel 会掩盖；但任何热剔节点场景都会触发）。
- **修复方向**：supervisor 接 `CancellationToken`，`close()` 触发 cancel。

### Tunnel-6（medium）：server `read_task` 在 `send_frame` Err 时静默 return，parent `relay_stream` 永远 park

- **文件**：`crates/gw-tunnel-server/src/lib.rs:370-403`
- **现象**：`read_task` 遇到 `read_sink.send_frame(...).await.is_err()` 时直接 `return`，不发 Close / 不通知 parent。Parent 是裸 `while let Some(bytes) = rx.recv().await`，无 `select!` 监听 `read_task` 状态，`tx` 仍在 `streams` map 里 → `rx.recv()` 永不返回 None。
- **后果**：纯 sink-only failure 场景下每个受影响 stream 泄漏一个 relay。
- **置信度**：中（最常见 sink-failure 模式同时会触发 source 错误并最终走 `handle_ws_conn` 退出 — 但纯 sink failure 不罕见）。
- **修复方向**：`read_task` 通过 oneshot / shared flag 通知 parent，parent `select!` 同时监听 `rx.recv()` 与 task 完成。

### Task-Runtime-1：`ExecLoop` 不向 `RunningTaskRegistry` 注册 → heartbeat + cancel 全是 dead code → 重复执行 + 双重计费 + admin cancel 静默 no-op

- **文件**：`crates/gw-task-runtime/src/exec_loop.rs:97-104, 163-172, 207-271`（struct 无 registry 字段、构造不接收、`execute_claimed` 无 register/insert 调用）；`crates/gw-task-worker/src/lib.rs:392-448`（registry 创建后**只**传给 HeartbeatLoop 和 CancelLoop，**没**传给 ExecLoop）
- **现象**：`exec_loop.rs:97-104` 的 struct 无 `registry` 字段；`execute_claimed` 中零 `register/insert` 调用。Worker 在 `lib.rs:392` 创建 `RunningTaskRegistry::default()`，在 `:443` 传给 `HeartbeatLoop`，在 `:417` 传给 `CancelLoop`，但 `:395-412` 构造 `ExecLoop` 时**没有**传入 — `ExecLoopConfig` 中也没有 registry 字段。`heartbeat_loop.rs:62-64` 迭代的是空 map；`cancel_loop.rs:235-237` 的 `self.registry.remove(&event.task_id)` 总是 `None`，silent `Ok(true)` 返回。
- **后果**：
  1. 默认 `lease_seconds = 60s`、`total_timeout = 120s`（`exec_loop.rs:29-31`），任何 >60s 的任务都会 lease 过期。`RECOVER_LEASE_LUA`（`scripts.rs:90-113`）ZREM 后 XADD 回 `tasks:queue`，**第二个 worker 重新 claim 并再执行一次** — `settle` 被调用两次 → 双重计费 + 上游双发。
  2. Admin 取消（`request_cancel`）发到 `tasks:cancel` 流，CancelLoop XACK 后 `registry.remove` 返回 `None`、silently `Ok(true)` 返回 — 取消事件被吞，任务继续执行直到完成。
- **置信度**：高（多点证据：struct 定义 + 构造点 + heartbeat/cancel 调用 + Lua 脚本）。
- **修复方向**：让 `ExecLoop` 持有 `Arc<RunningTaskRegistry>`，`execute_claimed` 开头 `register(task_id, lease_until_ms, token)`、settle 前 `remove`；同时在 `call_upstream` 的 poll 循环中检查 `CancellationToken`。

### Task-Runtime-2：`tasks:queue` 永不 XACK → consumer group PEL 无界增长 → Redis OOM

- **文件**：`crates/gw-task-runtime/src/redis_contract.rs:367-435`（claim 用 `XREADGROUP`），`crates/gw-task/src/scripts.rs:59-88`（SETTLE_LUA 6 个 KEYS 中无任务流的 key）
- **现象**：项目内 `grep -rn -i xack` 仅命中一处 — `cancel_loop.rs:92`（cancel 流的 ack）。`ClaimedTask.stream_id` 字段被 claim 捕获后**完全不再被引用**，是 dead data。`SETTLE_LUA` 完全不处理任务队列流。
- **后果**：consumer group `tasks-workers` 的 PEL（pending entries list）每 claim 一次增加一项，永不消除。每个 worker pod restart 创建新 consumer name，旧 consumer 的 PEL 永不被 reclaim。长期 Redis 内存无界增长。
- **置信度**：高（grep 全局命中 + Lua 脚本 KEYS 列表 + 字段使用追踪）。
- **修复方向**：在 `SETTLE_LUA` 中加 `XACK KEYS[N] group ARGV[stream_id]`；或在 `execute_claimed` 完成 settle 后调 `contract.ack(stream_id)`。同样需要在 cancel/error 路径补 ack。

### Routing-1：`KeyHealthStore.by_key_format` 每次 `is_available` 都 insert，永不 sweep

- **文件**：`crates/gw-health/src/store.rs:68-77` + `crates/gw-routing/src/pipeline/mod.rs:264` + `crates/gw-routing/src/routing_sweep.rs:21-63`
- **现象**：`is_available` 调 `get_or_create` → `entry().or_insert_with(...)`，**读路径产生插入**。每次 pick 对每个候选都触发一次。`routing_sweep` 处理 4 个 map（freq/view/latency/affinity），**KeyHealthStore 不在其中**，对应 crate 也没有 `remove/retain/sweep` 方法。
- **后果**：增长上界 = `|历史 KeyId| × |ApiFormat|`，每项含 `parking_lot::Mutex<SlidingWindow>` + 6 个 atomic，单项成本可观。键轮换 + protocol-conversion 双格式插入 + retry-blacklist 键都会累积。
- **置信度**：高。
- **修复方向**：把 `KeyHealthStore` 加入 `routing_sweep`，按"活跃 KeyId 集合"或时间阈值清理；或改 `is_available` 为 `get(...).is_none_or(...)`，未知键直接返回 true 不插入。

### Routing-2：`InFlightStore.by_key` 首次 acquire 时插入，永不 remove

- **文件**：`crates/gw-routing/src/in_flight.rs:52-57, 116-119`
- **现象**：`get_or_create` 在 `upstream.rs:646` 首次 acquire 时插入；`InFlightGuard::Drop` 只 `fetch_sub(1)` 不 remove 项。`routing_sweep` 不引用 `InFlightStore`。
- **后果**：键轮换长期累积，DashMap 单调增长（虽比 Routing-1 慢，但同质问题）。
- **置信度**：高。
- **修复方向**：加 `sweep_zeroed_unreferenced(active_keys)`，遍历 counter==0 且不在 active 集合的项移除，挂入 `routing_sweep`。

### Routing-4：`WeightedRr.current` 是 `(ModelName, KeyId)` 笛卡尔积，永不 sweep；Phase 3 在缺失时静默 no-op

- **文件**：`crates/gw-routing/src/strategy/weighted_rr.rs:18-20, 68-70, 80-83, 92-95`
- **现象**：DashMap 键为 `(ModelName, KeyId)` — 增长是模型×键的笛卡尔积。Phase 1/2 都用 `or_insert_with`，Phase 3 用裸 `.get()` + `if let Some(cur)`。当前无 remove 路径所以静默 no-op 不触发，但**一旦未来引入 sweep 而没有同步修正 Phase 3，winner 的 `fetch_sub(total)` 会被丢弃，SWRR 不变式即刻破坏**。
- **后果**：内存无界增长（最大问题）+ 潜在 SWRR 严重劣化。
- **置信度**：高（增长）/ 中（latent SWRR break）。
- **修复方向**：实现 `sweep_unreferenced(active_keys)` 挂入 `routing_sweep`；同时 Phase 3 改为 `or_insert_with(|| AtomicI32::new(-total))` 防御未来 race。

### Routing-3：`RoundRobin.counters` 每出现一个新 model 就插入，无 sweep

- **文件**：`crates/gw-routing/src/strategy/round_robin.rs:11-12, 44-47, 64-67`
- **现象**：`pick` 与 `ScoringPolicy::score` 都 `entry().or_insert_with`；无 sweep 接口；`routing_sweep` 不引用。
- **后果**：fine-tuned 模型名（`ft:gpt-4o-2024-...:user-...`）、deprecated 模型名、eval 一次性模型都累积。
- **置信度**：高（机制清晰）/ 影响中（模型基数通常有限）。
- **修复方向**：仿照已有的 `ModelFrequencyTracker::sweep_stale`，加 `last_used_at_ms` 字段 + sweep 方法，挂入 `routing_sweep`。

### Routing-5：`Sticky` 在 candidates 找不到 / 不健康时 `bindings.insert` 覆盖 moka — 永久断绑

- **文件**：`crates/gw-routing/src/strategy/sticky.rs:100-122`
- **现象**：bound key 在候选中不存在 / 不健康时 fall through 到 rebind 分支，**无条件** `bindings.insert(conv_id, new_key_id)` 覆盖 moka。原绑定永久丢失，即使原 key 恢复健康也不回滚。
- **后果**：任何 transient 429 / 短暂 cooldown / `in_flight` 上限触顶都会永久改 sticky 绑定 — prompt-cache 等以"同对话同键"为前提的收益失效。
- **置信度**：高。
- **修复方向**：rebind 路径仅对当前请求生效，不写 moka；或要求连续 N 次不可用才更新绑定。

### Routing-6：`ReservationGate::allows` 在 view 中无 model 行时 return true — cold start fail-open

- **文件**：`crates/gw-routing/src/reservation_gate.rs:27-38`
- **现象**：`view.and_then(|view| view.get(model))` 为 `None` 时 `return true`。冷启动后 `events:reservation` XREAD 尚未 produce 任何事件时，`get(model)` 返回 `None`，限流网关失效。
- **后果**：Pod 冷启动窗口内（直到 Redis 流首次产生 event）限流不生效。
- **范围限定**：仅"冷启动"场景为真。原审计中的"sweep 后失效"与"Redis outage 后失效"经过验证为**不准确** — `sweep_stale_instances` 调用 `recompute_model` 即使 `instance_count == 0` 也会插入 `GlobalModelView { total_rate: 0.0, … }`，事后 `get()` 返回 `Some(0.0)`，gate 正确允许（rate ≤ threshold）。
- **置信度**：中（机制实在但触发窗口窄）。
- **修复方向**：区分"view 未配置"（全局未启用，return true 合理）与"view 配置但 model 缺失"（应 fail-closed 或显式默认值）。

### Codec-2：OpenAI `usage_extractor` 丢失 `audio_tokens` 与 prediction tokens

- **文件**：`crates/gw-codec/src/openai/usage_extractor.rs:56-99`，对照 `crates/gw-codec/src/openai/wire.rs:396-413`
- **现象**：本地 `PromptDetails` 仅 `cached_tokens`，`CompletionDetails` 仅 `reasoning_tokens`。wire 结构带 `audio_tokens`、`accepted_prediction_tokens`、`rejected_prediction_tokens`。extractor 把 `multimodal_input_tokens` 硬编码为 `None`。
- **进一步发现（验证 agent 提示）**：`UsageSnapshot`（`gw-core/usage.rs:12-19`）只有 6 个维度，`response.rs:744-762` 也只从 `StreamEventIR::Usage` 拷 `input/output/cached` — 即便走 IR 路径也无法把 audio/prediction 喂给 billing。**根因在 schema，不在 extractor**。
- **后果**：`gpt-4o-audio-preview` 等带 audio token 的模型在计费侧静默 free request。
- **置信度**：高（结构对比清晰）。
- **修复方向**：扩展 `UsageSnapshot` 增加 audio/prediction 维度；同步 extractor 与 IR 路径；Go 端 cost 表对应 SKU 也要扩展。

### Codec-3：Gemini `ToolCallDelta` 渲染对 partial JSON `unwrap_or(Null)` — 跨协议工具调用 args 静默被吞

- **文件**：`crates/gw-codec/src/gemini/codec.rs:159-163`，对照 `crates/gw-codec/src/anthropic/codec.rs:525-534`
- **现象**：Anthropic 的 `input_json_delta` 以增量碎片 `{"na`、`me":`、`"alice"}` 形式 emit `ToolCallDelta.arguments_chunk`。Gemini render 路径 `serde_json::from_str(arguments_chunk).unwrap_or(Value::Null)` 对每个碎片单独解析，碎片不是完整 JSON 必然失败，写出 `"args": null`。
- **后果**：Anthropic→Gemini 跨协议流式工具调用每帧 args 都被吞为 null — 客户端拿不到工具参数。
- **置信度**：高。
- **修复方向**：Gemini render 维持自己的 buffer 累积碎片，只在 ToolCall 结束时 emit 一次完整 args；或对跨协议来源 emit `partial_json`-style 增量。

### Codec-4：三个 codec 的 state `String` 累积无 cap；Gemini `pending_function_calls` Vec 按 unique name 无界

- **文件**：`crates/gw-codec/src/anthropic/state.rs:39-51`（`OpenBlock::Text/Thinking/ToolUse` 三种 String）；`crates/gw-codec/src/openai/state.rs:25`（`OpenAiToolCallBuilder.arguments`）；`crates/gw-codec/src/gemini/state.rs:37`（`GeminiFunctionCallBuilder.args_json`）+ `gemini/codec.rs:416-433`（`pending_function_calls.push` 无上限）
- **现象**：所有 builder String `push_str` 无 max；Gemini 按 name 唯一性新增 builder。
- **后果**：slow-loris 流 / 上游永不关闭的 block / 上游恶意发独特名函数调用 → 内存吃满。HTTP 层 timeout 部分缓解但不充分。
- **置信度**：中-高（机制清晰，HTTP timeout 减弱但不消除）。
- **修复方向**：每个 builder 加 `max_accumulated_bytes`；`pending_function_calls` 加 cap（例如 128）。

### Codec-5：Anthropic `usage_extractor` 缺 cache_creation 分层与 server_tool_use

- **文件**：`crates/gw-codec/src/anthropic/usage_extractor.rs:10-108`，对照 `crates/gw-codec/src/anthropic/wire.rs:293-330`
- **现象**：本地 `Usage` 只有聚合 `cache_creation_input_tokens`；缺 `cache_creation`（`ephemeral_1h_input_tokens` vs `_5m_input_tokens`）和 `server_tool_use`（`web_search_requests`、`web_fetch_requests`）。
- **进一步发现**：与 Codec-2 同因 — `UsageSnapshot` 与 `StreamEventIR::Usage→ctx.usage` 拷贝都无对应维度。
- **后果**：Anthropic 分层缓存计费按聚合值近似，server tool 调用不计费。
- **置信度**：中。
- **修复方向**：同 Codec-2 — 扩展 `UsageSnapshot` 与 IR 拷贝路径。

---

## P1（资源累积型，长跑显现）

### Proxy-5：`tokio::spawn` JoinHandle 丢弃 + 无 timeout（settle / fail_to_connect）

- **文件**：`crates/gw-proxy/src/billing_lifecycle.rs:431` + `crates/gw-proxy/src/gateway.rs:1649`
- **现象**：spawn 内 `inflight.mark_settled` + `ledger.emit`，无 `tokio::time::timeout` 包裹。捕获仅是 `Arc`（不阻碍 `ReqCtx` 释放），但持续失败时累积。注释 `gateway.rs:1671-1674` 显式说明依赖启动 reconciler 兜底。
- **后果**：Redis 长期不可用时 spawn 堆积 + 连接池压力。**P0 严格性下降，列为 P1**。
- **修复方向**：用 `Semaphore` 限并发，或为 Redis 调用包 `tokio::time::timeout`。

### Proxy-9：`block_in_place` + `block_on` 在同步 Pingora 回调内

- **文件**：`crates/gw-proxy/src/gateway.rs:1495-1497, 1503-1506, 1519-1536, 1574-1577`
- **现象**：四处使用 `tokio::task::block_in_place(|| Handle::current().block_on(...))`。
- **后果**：理论上当所有 Tokio worker 都被 block_in_place 占用，且嵌套 future 又需要 worker 驱动 I/O 时会死锁。**实际严重性取决于 runtime worker 数与 Pingora 线程模型**。
- **置信度**：模式存在为 CONFIRMED，但 deadlock 触发概率需配合 runtime 配置审计。**列为 P1，待验证 worker 数配置**。
- **修复方向**：把 archive I/O 拆成单独 spawn，oneshot 把结果送回；或确保 Tokio runtime 有足够 worker 余量。

### Proxy-10：Daily-quota fire-and-forget spawn + 无 timeout，Redis 慢导致连接池耗尽

- **文件**：`crates/gw-proxy/src/filters/logging.rs:342-351` + `crates/gw-ledger/src/counter_store.rs:50-60`
- **现象**：spawn 内 `incrby_with_expire`，`counter_store` 无 timeout 包裹。
- **修复方向**：spawn 内 `tokio::time::timeout(5s, ...)` + `Semaphore` 限并发。

### Routing-7：`routing_sweep` 60s 间隔 + 1h 阈值 + **无 Prometheus 暴露 sweep map size**

- **文件**：`crates/gw-routing/src/routing_sweep.rs:18-19`
- **现象**：常数硬编码，无 gauge 暴露任一 map 的 `len()`，运营端无可观测性发现 Routing-1～4 的泄漏。
- **修复方向**：为每个被 sweep 的 map 注册 `gauge!("gw_routing_<name>_size")`。

---

## P2（潜在 / 当前路径未触发）

### Billing-9：`scaled_micro` 用 f64 算术 — `base_micro > 2^53` 静默丢精度

- **文件**：`crates/gw-billing/src/formulas.rs:71-83`
- **现状**：当前 `base_micro` 量级远低于 `2^53`（~9 × 10¹⁵），不触发；但缺乏文档化的不变式。
- **修复方向**：迁移到 `i128` 中间表示，最后 clamp 到 `i64`；或加 `debug_assert!(base_micro <= (1 << 53))`。

---

## FALSE POSITIVE（已排除，禁止以 P0 形式重复出现）

### FP-1：~~Backlog drainer 关停未 dump 导致丢失~~

- **原始声称**：`crates/gw-ledger/src/flusher.rs:617-624` 关停路径直接 `return Ok(())`，`dump_to_shutdown_file` 不调用 → in-memory backlog 丢失。
- **核验结果**：高层 `crates/gw-gateway/src/lib.rs:549-572` 在所有 worker drain 完成后**显式调用** `billing_backlog.dump_to_shutdown_file(&spill_root).await`，注释明确写 "This MUST run even on Forced drain"；失败时 `std::process::exit(2)`。drainer 的 cancel handler 留在 backlog 中不出栈是正确的、由更高层兜底。
- **排除理由**：跨 crate 兜底完整，**不是缺陷**。

### FP-2：~~`ArchiveState::InMemory` fallback 在生产路径绕过 cap~~

- **原始声称**：`response.rs:168-172` 注释明示 "Cap enforcement is bypassed in this case"；`LlmGateway::new` 默认 `spill_root: None`。
- **核验结果**：构造期间确实 `spill_root: None`，但 `crates/gw-gateway/src/lib.rs:384` 在引导阶段**无条件** `gw.spill_root = Some(wired.spill_root.clone())`，先于任何请求服务。`new_ctx()`（`gateway.rs:823-824`）把 `Some` 拷到每个 `ReqCtx`，生产路径必走 `if let Some(sr)` 分支与 `append_chunk()` 上限。`InMemory` 分支是 unit-test 专用 dead code。
- **排除理由**：生产路径不可达，**不是缺陷**。

### FP-3：~~`CacheAffinity` 策略在 transient blip 后永久重绑~~

- **原始声称**：`Sticky/CacheAffinity` 在 candidates 找不到 bound key 时 `bindings.insert` 覆盖 moka，transient blip 后永久断绑。
- **核验结果**：`Sticky` 部分为真（已列入 Routing-5）。但 `crates/gw-routing/src/strategy/cache_affinity.rs:13` 的 `CacheAffinity` 是 **stateless unit struct**（`pub struct CacheAffinity;`），用 `SHA-256(model || ":" || message_hash) % available.len()` 每请求重算，无任何持久化绑定；transient blip 在下一个请求自动恢复。
- **排除理由**：把无状态策略误判为有状态。原审计可能想说的是 `AffinityManager`（在 `affinity.rs`），但 `AffinityManager` 用 DashMap + TTL，且**已经被** `routing_sweep` 清理 — 仍不构成缺陷。

---

## 修复优先级建议

按"金钱影响 × 触发概率"排序：

1. **立即修**（直接金钱损失，单请求触发）：Billing-1、Billing-5、Billing-6、Billing-2、Billing-3、Task-Runtime-1
2. **立即修**（可用性，单请求 OOM）：Proxy-1、Proxy-8、Proxy-2、Proxy-3
3. **本周修**（资源累积达到容量上限即不可用）：Tunnel-1～5、Task-Runtime-2、Routing-1、Routing-2、Routing-4
4. **本周修**（数据完整性 / archive 静默丢失）：Proxy-6、Proxy-7、Codec-3
5. **本月修**（行为偏差，影响 prompt-cache / 限流冷启动）：Routing-5、Routing-6
6. **结构性，需要 schema 改动**：Codec-2、Codec-5（要先扩展 `UsageSnapshot` 维度）
7. **P1 资源累积**：Proxy-5、Proxy-9、Proxy-10、Routing-7、Codec-4、Tunnel-6
8. **P2 潜伏**：Billing-7、Billing-8、Billing-9

---

## 备注

- 本审计**未修改任何代码**，仅生成本报告。
- 所有 `file:line` 均按 2026-06-17 仓库状态记录；后续修复 PR 应回链本文档对应小节，并在修复时同步更新本文档的小节状态。
- 与现有 `docs/issues/2026-05-29-functional-audit-findings.md`（HIGH/MEDIUM 级正确性审计）无重叠；本文档专注于 P0 计费、内存泄漏与流程阻断类缺陷。
- `docs/issues/2026-05-29-credit-lease-followups.md` 中已记录的 lease followup 应与 Billing-1/5/6 联动评估，可能存在合并修复机会。
