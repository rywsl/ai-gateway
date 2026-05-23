# v2 Cleanup Overview — v1 标记的全面收口(v2026-05-23)

- **日期**:2026-05-23
- **状态**:Draft(等待 6 个 sub-spec 各自 brainstorming → spec → plan → impl)
- **作者**:协作设计(Claude + 用户)
- **触发**:仓库内仍存在 14 项以 "v1 / wave-X follow-up / TODO(v2) / placeholder / `unimplemented!()`" 形式埋下的未实现点。项目即将切入 v2,不再保留 v1 临时实现路径。详见 §2 的项 ID(F-1/R-1/L-1/...)与对应 file:line。
- **关联(被本 spec 收口的 v1 设计)**:
  - [2026-05-19-codec-s4-response-and-cross-protocol.md](./2026-05-19-codec-s4-response-and-cross-protocol.md)(响应跨协议 v1 留作 `unimplemented!()`)
  - [2026-05-19-routing-r-c-latency-tracker.md](./2026-05-19-routing-r-c-latency-tracker.md)、[r-e-failover.md](./2026-05-19-routing-r-e-failover.md)(latency_first / cost_first / load_balance / retry executor)
  - [2026-05-19-billing-c-delta-stream-dlq.md](./2026-05-19-billing-c-delta-stream-dlq.md)(三层 fallback,本 spec 将其拆为 Rust 半 + Go 半)
  - [2026-05-22-tunnel-t-design.md](./2026-05-22-tunnel-t-design.md)(tunnel 健康探针、`/healthz/tunnel`、`gw_tunnel_transport_alive` 采样器 wave-8 follow-up)
  - [2026-05-19-llm-gateway-design.md](./2026-05-19-llm-gateway-design.md)(CapabilityResolver 注入 LlmGateway,原 T6-A wave-7 follow-up)
- **范围**:本文件不展开任何子系统的实现细节,只负责:
  1. 把 v1→v2 语义切换写定;
  2. 将 14 项清理任务按主 crate 切到 6 个 sub-spec,给出依赖与实施顺序;
  3. 列出跨 sub-spec 共享的不变量(CI gate、sync rule 增补、§Known gaps 清空策略);
  4. 落定 Go vs Rust 划线原则与 4 项关键映射;
  5. 规定 CLAUDE.md 的最小修改点。
- **不在范围**:
  - 每个 sub-spec 内部的架构、数据流、错误处理、测试 — 各自走自己的 brainstorming → spec(见 §2 状态列);
  - Cost computation 的归属(仍归 Go 计费服务,CLAUDE.md §What this gateway does 不变);
  - KEK 轮换流程(无未实现项);
  - Soak / chaos 框架(§C1 已规定 gateway 代码不被该框架触碰);
  - 性能/zero-copy 优化(仅在 N-1 收口范围内做)。

> 一句话定义:把 14 项 "v1 / wave-X / placeholder" 收口成 6 个独立 sub-spec,Rust 侧全部实现,生产代码路径不再保留 `unimplemented!()` / `TODO(v2)` / "for v1" 注释。

---

## §1 目标与边界

### 1.1 v1 → v2 语义切换

v1 阶段允许在 codec、routing、ledger、proxy 等关键路径上保留"先链接桩、后续 wave 接线"的临时实现。v2 推翻这条权宜:

1. **生产代码路径不允许 `unimplemented!()` / `todo!()` / `panic!("TODO …")`**。CI 加 grep gate(见 §6.3)。
2. **不允许 inline 注释里写 "v1 / for v1 / wave-X follow-up / TODO(v2) / placeholder"** 来指代"先不做"。CI 加 grep gate。
3. **不允许"先预留接口、运行期返回 None/Err/no-op"** 这种功能性桩。如果某个能力归 v2 Go,则 Rust 侧根本不要这个接口或字段;如果归 v2 Rust,就在本轮 sub-spec 里实现到位。
4. **CLAUDE.md / docs/architecture.md §Known gaps / docs/superpowers/plans/** 的 "v1 / wave-7 follow-up" 字样在对应 sub-spec 合入时**同 PR 删除**,不留过渡。

### 1.2 适用范围

适用于 `crates/` 下所有 Rust 代码,以及 `docs/superpowers/plans/`、`docs/architecture.md`、`docs/workflow.md`、`docs/go-contracts/`、`CLAUDE.md`。**不适用于** `tests/e2e/client/soak/` 框架代码(§C1 该框架仅消费 gateway 已有 surface,不持有未实现路径)以及测试 fixture 中的 `*Stub` / `Dummy*` / `placeholder_e2e_*` 命名(这些是测试桩,不是生产桩,语义合法)。

### 1.3 复用的已有原则(memory)

- **breaking changes OK**:项目未上线,不为旧 schema / 旧字段 / 旧 stream key 保留兼容路径或灰度期。直接删除被取代的字段、API、stream key,不留 `#[deprecated]` 桩。
- **drop on observability outage**:非账务 sink(`usage:detailed`、tracing 派生 metric)挂了直接丢,不做 spool / DLQ / replay。**账务 sink (`usage:records`)不在此列**,仍要 retry + bounded DLQ(见 §5 G-11 划线)。

---

## §2 Sub-spec 索引

每个 sub-spec 独立走 brainstorming → spec → plan → impl 流程。当前 6 个 sub-spec 状态均为 *未启动 brainstorming*。

### S-codec — `2026-05-23-v2-codec-cleanup-design.md`

- **主 crate**:`gw-codec`
- **收口项**:
  - **C-1** 3 个非流式响应 `unimplemented!()`:`gw-codec/src/openai/codec.rs:175`、`gw-codec/src/anthropic/codec.rs:201`、`gw-codec/src/gemini/codec.rs:273`
  - **C-1 配套** 跨协议响应重渲染接线:`gw-proxy/src/filters/upstream.rs:232`(原 codec-s4 v2 deferred)
  - **C-2** 工具调用块映射:`gw-codec/src/anthropic/via.rs:89`(tool_result)、`gw-codec/src/gemini/via.rs:63`(functionResponse)、`gw-codec/src/openai/wire.rs:13`(explicit tools/tool_choice 字段)、`gw-codec/src/anthropic/wire.rs:37,42`(content array)
  - **G-9** 跨块状态累积器(thinking 签名续行、tool_call delta 合并)— 原 T6-D
  - **N-1 局部** OpenAI extension keys 改 `Cow<'a,str>`:`gw-codec/src/openai/via.rs:24`;Anthropic content 数组形式 wire 端
- **目标态**:三种 wire 形式的非流式响应在 IR 层完成双向映射,跨协议响应通过 `filters/upstream.rs:232` 路径自动重渲染并已纳入 `gw_codec_cross_render_total{from,to}` 指标;工具调用块在三种 wire 间往返不丢字段;SSE 多事件流的 thinking 签名跨块续行 + tool_call delta 合并由 codec 内部累积器处理,filter 层不感知。
- **关键跨 crate 触点**:`gw-proxy::filters::upstream::pick_peer` 之后的 response_body_filter — 此处增加跨协议 re-render 调用。
- **不在范围**:wire→wire 直接 codegen(原 G-8 路径)— v2 仍走 IR 往返,不引入 N² codegen;Kiro EventStream(已有 codec-s5)。

### S-routing — `2026-05-23-v2-routing-cleanup-design.md`

- **主 crate**:`gw-routing`
- **收口项**:
  - **S-1** 三个策略实化:
    - `gw-routing/src/strategy/latency_first.rs:25` — 接 `LatencyTracker` P50 TTFT,移除 "first-available" 桩
    - `gw-routing/src/strategy/cost_first.rs:10` — 同步读取 `Pricing.model_markup` 排序(详见 §5 划线)
    - `gw-routing/src/strategy/load_balance.rs:5` — 错误率输入接 `LatencyTracker`
  - **G-10** 响应侧(5xx/429)retry executor(FailoverExecutor v2)
  - **N-1 局部** `forced_capabilities` 类型 / 命名统一(SmallVec vs Vec,详见 G-13)
- **目标态**:8 个策略全部走"实信号驱动"路径,不再有 "first available" / "stub 错误率" 字眼;失败响应(5xx/429)由 FailoverExecutor 同步切到下一个候选 key,执行层不依赖 retry-by-codec。
- **关键跨 crate 触点**:`gw-capability` 提供同步 `Pricing` 查询(只读 ArcSwap 快照);`gw-proxy` 调用 FailoverExecutor 的入口点。
- **不在范围**:策略级别新增(8 个策略边界不变);LatencyTracker 内部实现(已有 r-c spec 落地)。

### S-ledger — `2026-05-23-v2-ledger-cleanup-design.md`

- **主 crate**:`gw-ledger`
- **收口项**:
  - **F-1** 冻结释放 Phase 6:
    - `gw-ledger/src/freeze.rs:126` — Phase 1b placeholder → 真实 Lua release 脚本(原子释放 + 余额校正)
    - `gw-ledger/src/freeze.rs:60` — DEDUCT 记录周期 GC,消除内存累积
  - **L-1** 详细日志真实 sink:
    - `gw-proxy/src/filters/logging.rs:279` — tracing 桩 → `gw-ledger::detailed_sink` 投递到 `usage:detailed` Redis stream(XADD MAXLEN,三级 redaction 在 codec 侧已就位);**挂了直接丢 + 增加 `gw_detailed_log_drop_total` 指标**(per memory `feedback_observability_drop_on_outage`)
    - `gw-proxy/src/filters/logging.rs:253` — 补 `gw_daily_quota_increment_error_total` metric
    - `gw-proxy/src/filters/logging.rs:383` — 填充 ctx 字段(request_id / api_key_id / vendor_key_id / format_from / format_to / strategy 等),原 T6-B follow-up
  - **G-11 Rust 半** 内存 buffer + bounded `usage:records:dlq` stream(投递失败时入 DLQ,不再丢)— Go 半见 S-contracts
- **目标态**:每条 usage:records / usage:detailed 投递都有明确终态(投递成功 / 入 DLQ / 直接丢 + drop 指标);冻结释放走原子 Lua 脚本,DEDUCT 记录周期 GC,无内存累积;logging.rs 的 ctx 字段填充完整,无 "T6-B follow-up" 字样。
- **关键跨 crate 触点**:`gw-proxy::filters::logging` 调用点;`docs/go-contracts/` 新增 DLQ replay 章节(由 S-contracts 同步起草)。
- **不在范围**:DLQ 的消费 / replay / 启动恢复(归 Go,见 S-contracts);usage:detailed 的 spool 与 replay(per memory 直接丢)。

### S-proxy-core — `2026-05-23-v2-proxy-core-cleanup-design.md`

- **主 crate**:`gw-proxy`
- **收口项**:
  - **R-1** CapabilityResolver 注入 LlmGateway:`gw-proxy/src/filters/request.rs:96-104`、`gw-proxy/src/ctx.rs:140`(原 T6-A wave-7)
  - **H-1** HMAC body_verify 接线(原 G-6 + G-7):
    - `gw-proxy/src/body_verify.rs` 整模块接到 `request_filter` 步骤 7b
    - `VendorKey` 新增独立 `signature_secret` 字段(不再复用 `hash` 作 HMAC 密钥)
    - 全局 `signature_required` 开关(env / TOML)
    - 压缩 body 解析 gap 闭合(content-encoding 感知)
  - **Q-1** TPM 延迟扣费 + response deadline + 变异追踪:
    - `gw-auth/src/ratelimit.rs:46` — TPM 在 token 计数后延迟扣费(原 phase-4)
    - `gw-proxy/src/filters/response.rs:21` — deadline 检查接 `ctx.timing.start`(原 task-4.3)
    - `gw-proxy/src/filters/upstream.rs:482` — `forced_params` 等变异追踪(原 task-4.3)
- **目标态**:每条请求在 request_filter 阶段都经过完整的 (auth → capability resolve → permission merge → ratelimit → HMAC verify → forced_params 应用)链;response_filter 强制 deadline 检查并在超时时同步关流;TPM 在 token 计数后延迟扣费,不再在请求前预扣。
- **关键跨 crate 触点**:`gw-capability::CapabilityResolver` trait(已有);`gw-auth::ratelimit` 模块(同 crate);`gw-ledger` deadline 触发的强制 flush。
- **不在范围**:HMAC 算法选择(沿用 v1 HMAC-SHA256);capability 解析规则本身(已有 capability crate 提供)。

### S-admin-tunnel — `2026-05-23-v2-admin-tunnel-cleanup-design.md`

- **主 crate**:`gw-gateway` + `gw-proxy/src/tunnel_stack.rs`
- **收口项**:
  - **A-1** Admin / management surface:
    - `gw-proxy/src/filters/request.rs:144` — mgmt-routing 真实实现(不再 501)
    - `gw-gateway/src/health.rs:47` — `/admin/*` 改 verify-then-route(替换 blanket-401)
  - **T-1** Tunnel 健康 / 遥测(原 wave-8):
    - `gw-proxy/src/tunnel_stack.rs:42` — 周期健康探针 `TunnelStack::probe_loop`
    - `gw_tunnel_transport_alive` gauge 周期采样器
    - `/healthz/tunnel` 端点(token-gated,与现有 `/healthz/*` 同链)
- **目标态**:`/admin/*` 走管理 JWT 验证后再分发到 mgmt handlers;tunnel 子系统 startup 起即自维护健康探针,`gw_tunnel_transport_alive` 反映真实 ws transport 心跳;`/healthz/tunnel` 暴露每个 proxy node 的 last_pong_age、reconnect_count、in_flight。
- **关键跨 crate 触点**:复用 `gw-auth` 的管理 JWT 验证;`/healthz/*` 现有 token-gate 中间件。
- **不在范围**:管理 UI(由独立服务承担);tunnel 服务端(aether-hub)— tunnel-t-design 已规定。

### S-contracts — `2026-05-23-v2-go-contracts-update-design.md`

- **主目录**:`docs/go-contracts/`(不改 Rust 代码)
- **收口项**:
  - **G-11 Go 半** `usage:records:dlq` 消费 + replay + 启动恢复 — 新增 `parts/04-go-dlq-replay.md`(或扩展现有 `parts/02-go-as-consumer.md`)
  - **C-γ #7 幽灵冻结** Go 侧周期对账修复 — 写入 `parts/02-go-as-consumer.md` 已有的 C-γ §未决问题 #7 解决方案
  - **`exclusive_capability_match` 索引启用** — `parts/03-shared-schema.md:516` 索引接到 Go 查询路径
  - **`forced_capabilities` 命名 / 类型同步**(G-13)— 与 S-routing 的 SmallVec 落地同步
- **目标态**:Rust 侧 XADD 失败 → DLQ 投递 → Go 侧消费 DLQ + replay 主 stream 的完整契约写在 `docs/go-contracts/`;C-γ #7 不再是"v1 follow-up",而是 Go 侧 reconciliation 的明确算法。
- **关键跨进程触点**:`usage:records` / `usage:records:dlq` stream schema;`counter:*` key 不变;ghost-freeze reconciliation cursor。
- **不在范围**:Go 代码本身(仅契约文档,Go 实施由 Go 团队走自己的流程);Cost computation(永远归 Go,不在本 spec)。

---

## §3 依赖图与实施顺序

```
第一波(并行)         第二波           第三波
S-codec   ─────────┐
                   ├──→  S-proxy-core ──→  S-admin-tunnel
S-routing ─────────┘

S-ledger  ────┐
              ├─→ (Go 侧实施独立, 不阻塞 Rust 第一波合入)
S-contracts ──┘
```

- **第一波(可并行)**:S-codec、S-routing、S-ledger。三者之间无源码共享面,只通过 trait 与 metric 名约定耦合。
- **第二波**:S-proxy-core。等 S-codec 的跨协议 re-render 入口稳定 + S-routing 的 FailoverExecutor 接口稳定后,在 proxy filter chain 完成 capability resolver 注入、HMAC 接线、ratelimit / deadline 修补。
- **第三波**:S-admin-tunnel。复用 S-proxy-core 落定的 admin JWT 验证链。
- **S-contracts**:与 S-ledger 同时起草,Go 实施可在 Rust 第一波合入之后开始,不阻塞 Rust 进度。

每个 sub-spec 的合入即"该批 v1 字样从 CLAUDE.md / docs/architecture.md §Known gaps / 注释里同步删除"。

---

## §4 CLAUDE.md 变更清单

最小修改,具体 diff 由 S-* 各自合入时同步:

1. **L30** `(resolver injection into LlmGateway is the wave-7 follow-up T6-A)`:由 S-proxy-core 合入时同 PR 删除该括号说明(此时 resolver 已注入,无须再提及)。本 overview 合入时**不动 L30**。
2. **§What this gateway does 之后**,新增一节 `§v2 cleanup status`(本 overview 合入时同步落地):

   ```markdown
   ## v2 cleanup status

   The repo is mid-transition from v1 (with intentional placeholder paths) to v2 (all Rust-side functionality landed; no `unimplemented!()` / `TODO(v2)` / "wave-X follow-up" in production code paths). Tracking: [v2 cleanup overview](docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md) and six sub-specs (S-codec / S-routing / S-ledger / S-proxy-core / S-admin-tunnel / S-contracts).
   ```
3. 其余 CLAUDE.md 句子保留不动,由各 sub-spec 在 sync rule 下逐项清除其 v1 字样。

---

## §5 Go vs Rust 划线

### 5.1 原则

- **同步且影响业务一致性 → Rust**:位于请求路径关键链上、错放/漏放会立刻导致计费或权限错误的逻辑。
- **可异步且放在 Rust 会拖慢请求 / 增加业务复杂度 → Go**:周期对账、replay、补发、UI 后端、复杂跨实体聚合。

### 5.2 4 项关键映射(本 spec 锁定)

| 项 | 归属 | 理由 |
|---|---|---|
| **F-1** 冻结释放 Phase 6 | **Rust** | 在请求路径上同步原子释放,错放=漏计费 |
| **S-1** cost_first markup 查表 | **Rust** | routing `pick_peer` 同步决策,异步会让请求等查表 |
| **C-γ #7** 幽灵冻结对账 | **Go** | 周期对账修复,可异步,且涉及跨实体聚合 |
| **G-11** billing-c-delta | **拆分**:Rust 内存 buffer + bounded `usage:records:dlq` stream;Go DLQ 消费 / replay / 启动恢复 | XADD 失败的兜底入 stream 是头部同步动作;DLQ 之后的对账/replay 是尾部异步动作 |

### 5.3 边界提醒

- **Cost computation 永远在 Go**(`no_cost_micro` / `no_billing_crate` lints 已就位)。S-1 在 Rust 做的"markup 查表"是 routing 决策用的估算,不是账务计算,与 CLAUDE.md §What this gateway does 不冲突。
- **observability outage drop** 只适用于 `usage:detailed` 与 tracing 派生 metric。`usage:records` 是账务,不在直接丢的范围,仍要 retry + bounded DLQ。

---

## §6 跨 sub-spec 不变量

### 6.1 §Known gaps 清空策略

- 每个 sub-spec 合入时,**同 PR 删除** `docs/architecture.md §Known gaps` 中被该 spec 收口的 G-X 条目。
- 6 个 sub-spec 全部合入后,§Known gaps 至少剩零或仅余明确非 v2 收口的项(若有,需在本 overview §8 追加说明)。

### 6.2 sync rule 增补

`docs/superpowers/plans/` 下任何"T6-A / wave-7 / phase-4 / TODO(v2)"标记的条目,在对应 sub-spec 合入时**同 PR 标记 landed**(或直接删除该 plan 文件,若该 plan 仅服务于已收口的项)。

### 6.3 CI grep gate(分阶段开启)

在 `scripts/check-invariants.sh` 中新增一条 lint(本 overview 合入时同步加上代码,但默认 **dry-run / 仅打印警告不 fail**):

```bash
# v2 cleanup gate: no v1 placeholder markers in production crates
# 状态机: 本 overview 合入 → dry-run; 每个 sub-spec 合入 → 收口范围 token 切到 error-out;
# 6 个 sub-spec 全部合入 → 整体 error-out
v1_markers_in_prod() {
  local mode="${GW_V2_GATE_MODE:-warn}"   # warn | error
  local hits
  hits=$(rg -n --type rust -g 'crates/**' -g '!**/tests/**' \
     -e 'unimplemented!\(' \
     -e 'todo!\(' \
     -e 'TODO\(v2\)' \
     -e 'wave-7' -e 'wave-8' \
     -e 'placeholder for v1' -e 'for v1:' \
     || true)
  if [ -n "$hits" ]; then
    echo "$hits"
    [ "$mode" = "error" ] && { echo "v2 cleanup gate failed"; exit 1; }
    echo "v2 cleanup gate (warn mode) — these tokens must be removed before sub-spec lands"
  fi
}
```

阶段切换:
- 本 overview 合入:`GW_V2_GATE_MODE=warn`(默认),仅打印,不阻塞 CI。
- 每个 sub-spec 合入时:该 sub-spec 范围内的 token **必须全部清零**,在该 sub-spec PR 中验证;脚本可加针对该 spec 收口范围的精确 grep 作为 hard gate。
- 6 个 sub-spec 全部合入后:同 PR 切到 `GW_V2_GATE_MODE=error`,整体硬阻塞 CI。

### 6.4 不允许的形态

- 不允许"先预留接口、运行期返回 None / Err / no-op"作为 v2 落地。要么本 sub-spec 实现到位,要么明确归 Go(并在 S-contracts 中体现)。
- 不允许新增"v2 follow-up / v3"字样指代"先不做"。新引入的延后工作走独立 spec,不与 v2 cleanup 混淆。

---

## §7 测试与验收策略

### 7.1 每个 sub-spec 自带

- 单测 + crate 内集成测,所有"被收口的项"必须有对应测例验证目标态。
- 不允许测例里使用 `#[ignore]` 来跳过新功能的测试(测试性能/资源限制例外,仍按现有约定标 `--ignored`)。

### 7.2 整体验收(6 个 sub-spec 全部合入后)

- **`cargo clippy --workspace --all-targets -- -D warnings`** 与 **`cargo fmt --all -- --check`** 通过。
- **`bash scripts/check-invariants.sh`** 通过(含新增的 v1 marker gate)。
- **`cargo test --workspace`** 通过;**`cargo test --workspace -- --ignored`** 通过。
- **e2e soak** 50k req / 60s × `off | heavy` 跑一次,strict-equality 账务一致(`usage:records + dlq == fake_upstream_hits_total` 在 drain steady state 成立)。

### 7.3 非账务 sink drop 验证(L-1)

L-1 的 sub-spec 必须含一个故意"Redis 下线 → usage:detailed 投递失败"的测试,验证 `gw_detailed_log_drop_total` 计数器递增、主请求路径不受影响、`usage:records` 仍正常投递。

---

## §8 非目标(out of scope)

- **Cost computation 归属**:仍归 Go 计费服务。
- **KEK 轮换流程**:无未实现项,不在本 spec。
- **Soak / chaos 框架**:gateway 代码不被框架触碰(§C1),不在本 spec。
- **新 vendor / 新 codec / 新 strategy**:本 spec 只收口 v1 留下的标记,不引入新能力面。
- **Zero-copy / 性能优化**:仅 N-1 范围内做(OpenAI extension keys `Cow`、Anthropic content 数组)。其它性能优化不在 v2 cleanup 范围。
- **管理后台 UI**:由独立服务承担。
- **Tunnel 服务端 aether-hub**:tunnel-t-design 已明确仅 gateway 客户端侧。

---

## §9 文件落地路径(规划)

本 overview 合入时只创建本文件。6 个 sub-spec 各自走 brainstorming → spec 流程,创建时间点:

| sub-spec | 文件 | 创建时机 |
|---|---|---|
| S-codec | `docs/superpowers/specs/2026-05-23-v2-codec-cleanup-design.md` | 进入 S-codec brainstorming 时 |
| S-routing | `docs/superpowers/specs/2026-05-23-v2-routing-cleanup-design.md` | 同上 |
| S-ledger | `docs/superpowers/specs/2026-05-23-v2-ledger-cleanup-design.md` | 同上 |
| S-proxy-core | `docs/superpowers/specs/2026-05-23-v2-proxy-core-cleanup-design.md` | 第一波合入后 |
| S-admin-tunnel | `docs/superpowers/specs/2026-05-23-v2-admin-tunnel-cleanup-design.md` | 第二波合入后 |
| S-contracts | `docs/superpowers/specs/2026-05-23-v2-go-contracts-update-design.md` | 与 S-ledger 同步 |

对应 plan 文件同名替换 `specs/` → `plans/`、后缀 `-design.md` → `-plan.md`,在各 sub-spec 通过 brainstorming → 进入 writing-plans 时创建。

---

## §10 项 ID 反查表

| 项 ID | 当前位置(file:line) | 归属 sub-spec | Go/Rust |
|---|---|---|---|
| C-1 (3×codec) | `gw-codec/src/{openai,anthropic,gemini}/codec.rs` + `gw-proxy/src/filters/upstream.rs:232` | S-codec | Rust |
| C-2 (tool blocks) | `gw-codec/src/{anthropic/via.rs:89, gemini/via.rs:63, openai/wire.rs:13, anthropic/wire.rs:37,42}` | S-codec | Rust |
| G-9 (cross-block accumulator) | codec SSE 内部 | S-codec | Rust |
| N-1 (codec 部分) | `gw-codec/src/openai/via.rs:24`、`gw-codec/src/anthropic/wire.rs:37,42` | S-codec | Rust |
| S-1 (3×strategy) | `gw-routing/src/strategy/{latency_first.rs:25, cost_first.rs:10, load_balance.rs:5}` | S-routing | Rust(markup 查表也归 Rust) |
| G-10 (retry executor) | `gw-routing` + `gw-proxy` 调用点 | S-routing | Rust |
| N-1 (forced_capabilities 类型) | `gw-routing` + `gw-capability` | S-routing | Rust |
| F-1 (freeze release Phase 6) | `gw-ledger/src/freeze.rs:{60,126}` | S-ledger | Rust |
| L-1 (detailed sink) | `gw-proxy/src/filters/logging.rs:{253,279,383}` + `gw-ledger::detailed_sink` | S-ledger | Rust(挂了直接丢) |
| G-11 (Rust 半) | `gw-ledger` + `gw-proxy/filters/logging.rs` | S-ledger | Rust |
| R-1 (CapabilityResolver) | `gw-proxy/src/filters/request.rs:96-104`、`gw-proxy/src/ctx.rs:140` | S-proxy-core | Rust |
| H-1 (HMAC + signature_secret + signature_required) | `gw-proxy/src/body_verify.rs` 全模块、`VendorKey` 字段 | S-proxy-core | Rust |
| Q-1 (TPM 延迟扣费 + deadline + forced_params) | `gw-auth/src/ratelimit.rs:46`、`gw-proxy/src/filters/response.rs:21`、`gw-proxy/src/filters/upstream.rs:482` | S-proxy-core | Rust |
| A-1 (admin / mgmt routing) | `gw-proxy/src/filters/request.rs:144`、`gw-gateway/src/health.rs:47` | S-admin-tunnel | Rust |
| T-1 (tunnel 健康/遥测/healthz) | `gw-proxy/src/tunnel_stack.rs:42`、`gw_tunnel_transport_alive` 采样器、`/healthz/tunnel` | S-admin-tunnel | Rust |
| G-11 (Go 半) | `docs/go-contracts/parts/04-go-dlq-replay.md`(新增) | S-contracts | Go |
| C-γ #7 (ghost-freeze) | `docs/go-contracts/parts/02-go-as-consumer.md` | S-contracts | Go |
| `exclusive_capability_match` 索引 | `docs/go-contracts/parts/03-shared-schema.md:516` | S-contracts | Go |
| `forced_capabilities` 命名(G-13) | 跨 Rust + Go | S-routing(Rust 侧)+ S-contracts(Go 侧) | 双侧同步 |

---

## §11 状态追踪

- [ ] 本 overview 合入 + CLAUDE.md `§v2 cleanup status` 一节落地 + `scripts/check-invariants.sh` 新增 v1 marker gate 开关(初始关闭,见 §6.3 注)
- [ ] S-codec brainstorming → spec → plan → impl → 合入(同 PR 清 §Known gaps G-x 与 v1 字样)
- [ ] S-routing 同上
- [ ] S-ledger 同上
- [ ] S-proxy-core 同上
- [ ] S-admin-tunnel 同上
- [ ] S-contracts(Go 文档)同上
- [ ] 6 个 sub-spec 全部合入后,开启 §6.3 grep gate(同 PR 验证零命中)
- [ ] e2e soak 50k req / 60s × off+heavy 通过(§7.2 整体验收)

---

> 本 overview 不规定任何具体实现细节。任何把 sub-spec 内部架构 / 字段 / 接口写进本文件的修改都属于越权,应该回到对应 sub-spec。
