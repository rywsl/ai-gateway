# 上游 Adapter 层 — 架构设计文档（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅接口与分层边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **关联**：[2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)（ADR-019 协议 IR + ProtocolCodec）
- **范围**：Rust 端「上游 Adapter」抽象 —— 接入 claude_code / codex / gemini_cli / vertex_ai / antigravity / kiro 这类非标准官方上游；快路径单字段 patch 通道；codegen 边界划分
- **不在范围**：具体 adapter 实现、kiro EventStream 解析器实现、codegen 宏兑现（后续各自单独 plan）

---

## 1. 背景与问题

对标 Aether 的 `services/provider/adapters/`，Rust 网关当前完全没有 adapter 概念：`gw-routing` 只按 vendor 选 key，`gw-codec` 只做 OpenAI/Anthropic/Gemini 协议 codec。Claude Code OAuth、Codex 配额、Kiro EventStream、Vertex JWT 这一整层缺失。

用户诉求：要更快的「自动生成的内联转换」，而非把所有流量都塞进 IR 中间层。

## 2. 关键判断：adapter ≠ 绕过 IR

拆解 Aether 6 个 adapter 的实际改动内容：

| Adapter | 实际改了什么 | 归属层 |
|---|---|---|
| claude_code | Anthropic 协议不变；OAuth bearer 替 `x-api-key`；注入 system；beta header | 鉴权/信封 |
| gemini_cli | Gemini 协议不变；OAuth + quota header | 鉴权/信封 |
| vertex_ai | Gemini 协议不变；GCP SA JWT 签名 + URL 重写 | 鉴权/传输 |
| antigravity | URL 可用性探测 + 签名缓存 | 传输 |
| codex | OpenAI Responses 形状；image_transform + SSE event 改写 | codec 级 body 微调 |
| kiro | AWS EventStream 二进制 ↔ SSE 全量转换 | 真正的新 codec |

**结论**：6 个里 4 个不碰 codec，是 `gw-proxy` 的 upstream filter（换 auth/header/url）+ 可选 body 小补丁；只有 codex 需要 codec body 改写，kiro 需要全新二进制流 codec。adapter 这层不需要绕过 IR，需要的是一个 proxy 层的 hook。

## 3. 性能直觉纠正

- IR hub = **2N** 个映射（N 协议 ×{parse,render}）；直接 A→B 内联 = **N²** 对。N 增长后 N² 只会更慢写、更难维护，且跨协议时更差。保留 hub。
- 真正的零拷贝快路径已存在：`gw-codec/src/passthrough.rs::same_protocol_passthrough` —— 同协议且 IR 未 mutate 时 `Cow::Borrowed` 原始 body，零 parse 零序列化。
- 用户要的「更快内联」不是去掉 IR，而是**扩大快路径覆盖面**：新增「语义保持的 adapter mutation」通道，停在 borrowed body 上做字节级/单字段 JSON patch，不走 parse→IR→render。

## 4. 真正的速度瓶颈

`gw-codec-macros` 的 `IrConvert`/`IrStreamConvert` derive 当前是空壳（`lib.rs` 明说 v1 scaffolded but inert），wire↔IR 全靠 `via.rs` 手写，且 wire 结构仍是纯文本 stub（`anthropic/wire.rs` 是 `Cow<str>`，OpenAI tools 靠 `extra` HashMap 透传）。加多模态 + tool_use = 3 codec ×{parse,render}×{req,resp,stream} 手写，这才是会拖垮 velocity 的地方。codegen 收益最大处在此，且与 IR hub 不冲突 —— hub 模型（2N 生成点）才让 codegen 划算。

## 5. 设计决策（ADR 草案）

### ADR-A1 Adapter trait 放 `gw-proxy`，不放 `gw-codec`

Adapter 作用于 upstream filter 阶段，管 auth / header / url / envelope，不改协议语义。接口草案（仅形状，待评审）：

```rust
/// 作用于 upstream_request_filter 之后、发往上游之前。
/// 不持有 IR；只读 RequestCtx + 改写出站 HttpPeer/headers/body。
pub trait UpstreamAdapter: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// 改写鉴权：替换/注入 auth header（OAuth bearer、SA JWT、x-api-key）。
    /// 可异步（OAuth token 刷新走此处，但刷新结果应来自缓存，不阻塞热路径）。
    fn rewrite_auth(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;

    /// 改写传输：URL/host 重写、可用性探测结果注入。
    fn rewrite_transport(&self, ctx: &mut AdapterCtx) -> Result<(), AdapterError>;

    /// 可选的语义保持 body patch（见 ADR-A2）。返回 None = 不改 body。
    fn patch_body<'a>(&self, body: &'a [u8], ctx: &AdapterCtx)
        -> Option<BodyPatch<'a>>;
}
```

4/6 adapter（claude_code、gemini_cli、vertex_ai、antigravity）只实现前两个方法即可，天然走零拷贝快路径。

### ADR-A2 快路径「语义保持 patch」通道

在 `same_protocol_passthrough` 旁增加一条：mutations 为空但 adapter 声明了 `patch_body` 时，不进 IR，只做字节级/单字段 JSON 补丁（注 system、改 model 名、加 thinking budget、加 beta 字段）。`BodyPatch` 形状（草案）：

- `Splice { json_pointer, value }`：单点写入，基于 borrowed body 增量序列化。
- `HeaderOnly`：body 完全不动，仅 ADR-A1 的 header 改写生效（claude_code/gemini_cli 主场景）。

这是用户「内联转换」诉求的正解：claude_code/codex 换皮不进 IR。

### ADR-A3 Codegen 边界

- 只给真正需要跨协议 + 多模态/tools 的路径兑现 `IrConvert`/`IrStreamConvert` derive。
- kiro 单独作为新 `ProtocolCodec` 实现（AWS EventStream framer + parse_stream_chunk 变体），**不混进通用 derive**。
- codex 的 image_transform / SSE event 改写归类为 codec 级 body 微调，优先尝试 ADR-A2 的 patch 通道，patch 表达不了再降级到 IR。

## 6. 实施顺序（建议，未排期）

1. `gw-proxy` adapter trait + 注册表 + upstream filter 挂载点（ADR-A1）。先落 claude_code（OAuth+header）打通 4/6 场景。
2. 快路径 patch 通道（ADR-A2）。覆盖 claude_code/codex 换皮。
3. 兑现 `IrConvert` codegen（ADR-A3），配合多模态/tools wire 结构扩展（依赖独立的 codec 多模态 spec）。
4. kiro 独立 codec（最后做，隔离风险最高的二进制流）。

## 7. 未决问题

- OAuth token 刷新的缓存位置与失效语义（Aether 有 account-level block 前缀状态机，Rust 侧需对应 entity/counter 协议设计）—— 留待 adapter 状态机 spec。
- `AdapterCtx` 与现有 `gw-proxy::ctx::RequestCtx` 的关系：内嵌还是旁挂，影响热路径内存布局。
- adapter 选择维度：按 vendor、按 vendor_key、还是按 model_group 绑定 —— 与 ADR-021 entity 扩列耦合。
- codex SSE event 改写能否完全用 ADR-A2 patch 表达，决定它是否需要进 IR。

---

> 本文仅记录分层边界与接口形状，供后续拆分 plan。实现、kiro 解析器、codegen 兑现各自单独立项。
