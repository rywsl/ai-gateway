# Open Roadmap Decision Packet

- 日期：2026-06-09
- 范围：W2/W3/W4/W6/W7/W8 源码执行前的集中决策确认
- 状态：等待用户确认
- 依据：
  - `docs/plans/2026-06-09-w0-contract-evidence.md`
  - `docs/plans/2026-06-09-w2-stream-probe-failover.md`
  - `docs/plans/2026-06-09-w3-cli-usage-provenance.md`
  - `docs/plans/2026-06-09-w4-async-task-ir-wire-body.md`
  - `docs/plans/2026-06-09-w6-rust-oauth-refresh-writeback.md`
  - `docs/plans/2026-06-09-w7-heavy-transforms-kiro.md`
  - `docs/plans/2026-06-09-w8-tunnel-bridge-e2e.md`
  - `docs/ROADMAP.md`

## 背景

`main` 已经包含 W0-W8 的详细计划，W1/W5 已实现。剩余 W2/W3/W4/W6/W7/W8 的源码执行存在需要用户确认的边界点。本文只汇总已由计划文件提出或支撑的开放决策，不新增计划外架构选择。

## 决策 1：W2 Stream Probe Header/Body Commit 语义

推荐选项：

- 采用 W2 计划的显式流阶段：`Connecting`、`Probing`、`Streaming`。
- `upstream_response_body_filter` 在下游可见提交前检查和缓冲探测字节。
- `response_body_filter` 才是最终客户端 body commit 标记点。
- 只有在合法 SSE event 或非空响应体真正写向客户端后，才进入不可重试语义。
- 上游错误 body、畸形帧、探测超时在客户端可见提交前都可被分类为 probe failure 并触发候选 failover。

替代选项：

- 保持当前 `body_started` 语义：上游首个 body chunk 到达即视为 body started。
- 只允许 header 或 body 完全未开始前 failover，不引入 pre-commit probe 阶段。

影响：

- 推荐选项会修正“上游已有字节但客户端尚未看到有效 SSE event”时无法 failover 的 R-F 缺口，同时保留首个客户端可见 event/body 后不重试的现有安全语义。
- 替代选项实现成本低，但 `docs/ROADMAP.md` 中 R-F first-chunk failover window 仍保持未完成。

会解锁的 wave：

- W2 R-F stream probe failover。

如果不确认会阻塞什么：

- 阻塞 W2 源码实现，尤其是 `ReqCtx` 流阶段字段、`upstream_response_body_filter` 探测、`gw_stream_probe_*` 指标和 R-F roadmap closure。

## 决策 2：W3 `cli_extracted` Provenance Priority

推荐选项：

- 新增 `ProvenanceTag::CliExtracted`，序列化为 `"cli_extracted"`。
- 统一 Rust merge、Go settler、Python soak dedupe 的优先级：

```text
upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown
```

替代选项：

- 用户指定其他 `cli_extracted` 排名，并在实现前同步更新 W3 计划里的 Rust、Go 合同和 soak dedupe rank 表。
- 不新增 `cli_extracted`，继续把 CLI 提取结果归入现有 provenance 标签。

影响：

- 推荐选项把 CLI wire body / stream event 中解析出的 usage 标成比普通 partial IR 更可信，但仍低于标准 codec 的 terminal usage payload。
- 不新增该标签会继续无法区分 CLI 解析 usage 与 partial/estimated usage，`docs/ROADMAP.md` 的 CLI provenance 分歧仍无法关闭。

会解锁的 wave：

- W3 CLI-extracted usage provenance。

如果不确认会阻塞什么：

- 阻塞 `ProvenanceTag` wire enum、`usage:records` provenance 合同、Go settler dedupe priority、`tests/e2e/client/soak/dedupe.py` rank 和相关 spec/ROADMAP closure。

## 决策 3：W4 Async Task Response Envelope

推荐选项：

- 采用 W4 计划的最小 response envelope：

```json
{
  "body": {"id": "chatcmpl-task", "choices": [{"message": {"role": "assistant", "content": "done"}}]},
  "ir_wire_format": "openai:chat",
  "ir_wire_content_type": "application/json",
  "ir_wire_body": "{\"id\":\"chatcmpl-task\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"done\"}}]}"
}
```

- `body` 保留当前 raw JSON payload 语义。
- `ir_wire_body` 只在 `gw-codec` 能 best-effort parse/render terminal response 时写入。
- response parse/render 失败不使成功的 upstream task 失败，只保留 raw `body`。
- 不新增完整 IR response 存储，不改变 load/watch 的对外投影。

替代选项：

- 只写 raw `output_payload.body`，暂不记录 response-side `ir_wire_body`。
- 在用户另行确认后设计完整 typed IR response envelope 和 load/watch projection。

影响：

- 推荐选项让异步任务响应侧具备最小 wire-fidelity 证据，同时不扩大 `TaskRecord.output_payload` 的外部 API 面。
- 只写 raw body 会让 W4 只能完成 request-side verbatim send，无法记录 normalized response bytes。
- 完整 IR envelope 会扩大合同面，计划文件明确要求没有用户决策前不实现。

会解锁的 wave：

- W4 async task `ir_wire_body` request/response normalization。

如果不确认会阻塞什么：

- 阻塞 `gw-task-runtime` response best-effort parse/render、`output_payload` 文档化、`docs/spec/08-async-tasks.md` 更新和 async task roadmap closure。

## 决策 4：W6 `oauth_failed` 退休或改为 Status-Only

推荐选项：

- 将旧 `oauth_failed` reverse-XADD 改为 status-only 事件，不再作为 Go runtime refresh 命令。
- 事件语义从旧命令：

```text
field oauth_failed reason upstream_401
```

改为状态信号：

```text
field oauth_refresh_status status rust_refresh_failed reason upstream_401 source rust_oauth_refresh
```

- 指标从 `gw_oauth_refresh_failed_xadd_total` 改为 `gw_oauth_refresh_status_xadd_total`。
- Go 合同明确：post-K-C 后 Go 不得消费 Rust OAuth runtime event 来刷新 `vendor_keys.payload.oauth`。

替代选项：

- 完全删除 reverse-XADD 分支，只保留本地 metrics 和 affinity invalidation。

影响：

- 推荐选项保留 operator 和 Go 侧可见的失败状态流，同时消除 Go/Rust 双写 runtime OAuth token 的风险。
- 删除分支更简单，但 Go 将失去 Redis stream 可见的 OAuth refresh failure 状态，只能依赖 Prometheus 或 gateway runtime status。

会解锁的 wave：

- W6 Rust OAuth refresh write-back。

如果不确认会阻塞什么：

- 阻塞 `crates/gw-proxy/src/error/listeners/oauth.rs` 的旧 reverse-XADD 处理、`metrics_catalog.rs` 指标命名、Go OAuth 合同变更，以及 F2 “Rust 是唯一 runtime refresh writer”的源码落地。

## 决策 5：W7 Kiro EventStream-Only Contract

推荐选项：

- 将 Kiro 注册为标准 EventStream upstream protocol，不作为 CLI variant。
- 合同字符串：
  - `ProtocolKind::Kiro`
  - `ApiShape::KiroEventStream`
  - `ApiShape::format_id() == "kiro:eventstream"`
  - `ProtocolKind` JSONB / `as_str()` wire string 都是 `"kiro"`
  - `vendor_keys.payload.accessible_endpoints == ["kiro"]`
- `AnyCodec::Kiro` 在现有 HTTP JSON surfaces 之后检测。
- `WireFormat::KiroEventStream` 至少声明 `supports_thinking = true`，其他 capability 按当前 Kiro wire 实现逐项确定。

替代选项：

- 用户选择不同 Kiro `ProtocolKind`、`ApiShape`、`FORMAT_ID`、codec registration 或 Go contract strings。
- 如果产品需要 Kiro CLI 行为，将其作为未来单独 `ApiShape`，不混入本次 EventStream 注册。

影响：

- 推荐选项与现有 `gw-codec::kiro` 的 `ProtocolCodec` / AWS EventStream 实现一致，并能补齐 Kiro 跨进程协议形状。
- 选择其他形状需要先重写 W7 shared contract lane 的 Tasks 1-3，再进入 Kiro adapter 源码工作。

会解锁的 wave：

- W7 shared contract lane 和 Kiro adapter lane。

如果不确认会阻塞什么：

- 阻塞 `gw-core::ProtocolKind` / `ApiShape` wire enum、`AnyCodec::Kiro` 注册、Kiro cross-render format、Go `accessible_endpoints` 合同、Kiro SigV4/EventStream adapter 和相关 spec/ROADMAP closure。

## 决策 6：W8 Aether-Hub Tunnel Protocol

推荐选项：

- 仅在用户或 aether-hub owner 确认真实远端节点与当前 mock 合同一致时，采用当前 mock contract：
  - `Open` payload 是 raw `host:port`。
  - HTTPS 目标由 gateway 持有 TLS，远端 node 只转发 opaque TCP bytes。
  - `Data` 在同一 `stream_id` 双向承载 opaque bytes。
  - `Close` 表示该 stream EOF。
  - `Error` 承载 UTF-8 diagnostic，并映射为 tunnel upstream error。
  - `ControlAuth` 是 raw token bytes，不要求 ack。
  - 收到 `ControlNodeInfo` 时 gateway 可忽略。
  - `max_streams_per_conn` 是并发 multiplex 上限。

替代选项：

- 如果真实 aether-hub 使用 URL 或结构化 JSON `Open` payload、远端 TLS termination、不同 `Error` payload、auth ack/error、必须等待 `ControlNodeInfo`，或额外 flow-control/multiplex 规则，则先同步更新 `gw-tunnel-client` 测试和 `crates/e2e-fake-upstream/src/tunnel_node.rs`，再实现 proxy bridge。

影响：

- 推荐选项允许 W8 按当前本地 mock 直接实现 raw TCP byte relay over `Open` / `Data` / `Close`。
- 真实协议不同则本仓库 mock 不能作为 e2e 证明，需要先修正测试替身与 client frame/transport 合同。
- W8 期间默认 artifact 仍保持非 tunnel，`latest-tunnel` 继续作为 operator-selected build；默认 artifact 是否升级仍是 post-e2e release decision。

会解锁的 wave：

- W8 HTTP<->WS frame bridge runtime 和 full tunnel e2e round-trip。

如果不确认会阻塞什么：

- 阻塞 `gw-tunnel-client` bidirectional stream API、`gw-proxy` in-process HTTP/1 loopback bridge、live tunnel e2e、`docs/spec/09-tunnel.md` closure。未确认前不得改 Rust 源码实现 W8。

## 推荐一次性确认文本

```text
确认以下 roadmap 决策：

1. W2 stream probe 采用 Connecting/Probing/Streaming 阶段；只有 response_body_filter 选定的客户端可见 body/event 才算 commit，pre-commit 的 upstream error body、malformed frame、timeout 可 failover。
2. W3 新增 cli_extracted provenance，优先级为 upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown。
3. W4 async task response 采用最小 output_payload envelope：body + ir_wire_format + ir_wire_content_type + best-effort ir_wire_body；暂不新增完整 IR response，也不改变 load/watch 投影。
4. W6 将旧 oauth_failed reverse-XADD 改为 status-only oauth_refresh_status 事件，Go post-K-C 不再把任何 Rust OAuth runtime event 当作刷新命令。
5. W7 Kiro 采用 EventStream-only standard protocol：ProtocolKind::Kiro、ApiShape::KiroEventStream、format_id kiro:eventstream、wire string kiro、accessible_endpoints ["kiro"]，不是 CLI variant。
6. W8 若真实 aether-hub 与当前 mock 一致，则采用 raw host:port Open、gateway-owned TLS、Data 双向 opaque bytes、Close EOF、Error UTF-8 diagnostic、ControlAuth raw token no-ack、ControlNodeInfo ignored、max_streams_per_conn 并发上限；如不一致，先按真实协议更新 mock/client 测试再实现 bridge。
```
