# Roadmap Execution Decision Record

- 日期：2026-06-09
- 状态：已确认
- 范围：W2/W3/W4/W6/W7/W8 源码执行前的最终决策记录
- 上游开放问题包：`docs/design/2026-06-09-open-roadmap-decision-packet.md`
- 约束：本文只记录执行决策；本次提交不包含 Rust 源码实现。

## 背景

`docs/design/2026-06-09-open-roadmap-decision-packet.md` 汇总了 W2/W3/W4/W6/W7/W8 的开放决策。用户已逐项确认或调整默认选择。后续执行应以本文为最终记录；如果计划文件与本文冲突，以本文为准，并先更新计划再进入源码实现。

## W2：Stream Probe Commit 语义

决策：使用推荐选项。

执行要求：

- 引入显式流阶段：`Connecting`、`Probing`、`Streaming`。
- `upstream_response_body_filter` 在下游可见提交前检查和缓冲探测字节。
- `response_body_filter` 是最终客户端 body commit 标记点。
- 只有合法 SSE event 或非空响应体真正写向客户端后，才进入不可重试语义。
- 上游错误 body、畸形帧、探测超时在客户端可见提交前可被分类为 probe failure 并触发候选 failover。

## W3：CLI Usage Provenance 与 Billing Critical 约束

决策：使用推荐选项，并增加 billing-critical 要求。

执行要求：

- 新增 `ProvenanceTag::CliExtracted`，序列化为 `"cli_extracted"`。
- Rust merge、Go settler、Python soak dedupe 的统一优先级为：

```text
upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown
```

- CLI usage extraction 必须有硬测试保证。测试应覆盖 CLI wire body / stream event 中能提取 usage 的路径，不能只靠人工样例、宽松 smoke test 或仅验证 handler 被调用。
- usage 缺失会导致用户白用，因此如果 CLI wire 未给 usage，必须有非零 fallback/settlement 机制。
- W3 不能留下 free usage 路径：任何成功产生上游消耗的 CLI 调用，都必须最终形成可结算的 usage 记录或非零 fallback 记录。
- 如果某个 CLI 协议无法稳定提取 usage，执行者必须先补 settlement fallback 设计，再关闭 W3。

## W4：Async Task Response IR

决策：用户倾向完整 IR。没有完整 IR 会导致新增协议转换困难；如果实现不是很困难就做完整 IR。

执行要求：

- W4 源码执行前必须先评估完整 response-side IR 的实现难度、合同影响和 load/watch 投影影响。
- 如果完整 IR 实现不困难，W4 应直接做完整 IR，而不是只做最小 `body + ir_wire_*` envelope。
- 如果完整 IR 明显困难、会拖大合同面，或需要改变外部 API 投影，必须返回用户做新的执行决策。
- 执行者不能静默退回 minimal envelope，也不能把 minimal envelope 当成默认最终方案。
- 在用户重新确认前，最小 envelope 只能作为 fallback 候选，不是 W4 的自动执行目标。

## W6：`oauth_failed` 退休为 Status-Only

决策：使用推荐选项。

执行要求：

- 旧 `oauth_failed` reverse-XADD 改为 status-only `oauth_refresh_status` 事件。
- Go post-K-C 不得消费 Rust OAuth runtime event 来刷新 `vendor_keys.payload.oauth`。
- Rust 是 runtime OAuth refresh 的唯一写入方。
- 事件应保留 operator 和 Go 侧可见的失败状态，但不得形成 Go/Rust 双写 runtime token 的路径。

## W7：Kiro EventStream 与 Kiro CLI

决策：EventStream-only Kiro 继续做；Kiro CLI 也需要支持，但作为同一 W7 计划里的额外 `ApiShape` / 后续 lane。

EventStream lane 执行要求：

- Kiro 注册为标准 EventStream upstream protocol。
- 合同字符串继续采用：
  - `ProtocolKind::Kiro`
  - `ApiShape::KiroEventStream`
  - `ApiShape::format_id() == "kiro:eventstream"`
  - `ProtocolKind` JSONB / `as_str()` wire string 都是 `"kiro"`
  - `vendor_keys.payload.accessible_endpoints == ["kiro"]`
- `AnyCodec::Kiro` 在现有 HTTP JSON surfaces 之后检测。
- `WireFormat::KiroEventStream` 至少声明 `supports_thinking = true`，其他 capability 按当前 Kiro wire 实现逐项确定。

Kiro CLI lane 执行要求：

- Kiro CLI 行为是确认需求，不能被忽略或记录为产品外范围。
- Kiro CLI 不在本次 EventStream adapter 内混做。
- W7 计划必须更新，增加独立 Kiro CLI lane。
- Kiro CLI lane 至少需要补独立 `ApiShape`、wire behavior、usage extraction/billing 证据，以及与 EventStream codec 的注册边界。

## W8：自研 Tunnel Server / Client

决策：本仓库只是参考 aether，不需要完全兼容；W8 改为自研 tunnel server 和 tunnel client。

执行要求：

- 当前 aether mock 只作为参考，不再作为必须兼容的外部合同。
- W8 计划需要更新：目标从兼容 aether-hub mock 改为自研 tunnel server/client。
- 允许较大改动。协议、server、client、测试替身和 e2e 应围绕本仓库自研 tunnel 合同重新定义。
- 不得继续以“真实 aether-hub 与当前 mock 一致”为实现前提。
- W8 期间默认 artifact 仍保持非 tunnel；是否升级默认 artifact 仍需在 tunnel runtime 与 live round-trip e2e 完成后单独做 release decision。

## 执行纪律

- 后续执行者必须先更新受影响计划，再改源码。
- 若 W4 完整 IR、W7 Kiro CLI 或 W8 自研 tunnel 的计划更新暴露新的合同风险，必须返回用户决策。
- 本记录不授权跳过 billing、usage、settlement、EventStream、CLI 或 tunnel e2e 的硬验证。
- 本次 decision-record 提交只改文档，不改 Rust 源码。
