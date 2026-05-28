# Systemic Fixes — 跨切面根因修复设计（v2026-05-28）

- **日期**：2026-05-28
- **状态**：Draft（待审阅）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - 触发的 gap 来源：
    - `docs/architecture.md` §9.1 G7 follow-up（HMAC 解压复用）
    - `docs/architecture.md` §10.5 "no `fail_to_proxy` envelope emitter"
    - `docs/architecture.md` §10.5 "malformed SSE silent drop"
    - `docs/architecture.md` §10.5 "NOGROUP recovery missing"
    - `docs/architecture.md` §10.5 "ProtocolKind dual wire spelling"
  - 本仓内代码触点：
    - `crates/gw-store/src/stream_consumer.rs`（NOGROUP 自愈缺失）
    - `crates/gw-proxy/src/filters/request.rs:480-595`（HMAC verify + cached_body 分裂语义）
    - `crates/gw-proxy/src/filters/upstream.rs:54,77`（`envelope_for` / `emit_envelope_and_bail`）
    - `crates/gw-proxy/src/gateway.rs:445-1080`（Pingora trait override 当前不含 `fail_to_proxy`）
    - `crates/gw-proxy/src/filters/response.rs:377-712`（parse_stream_chunk warn）
    - `crates/gw-codec/src/openai/codec.rs:96,216-228`（codec warn 散点）
    - `crates/gw-core/src/protocol.rs`（`ProtocolKind` / `ApiShape` — 两种 wire form by-design 给两个跨进程边界使用，缺 CI lock）
    - `crates/gw-proxy/src/ctx.rs:76-200`（`ReqCtx.cached_body` 字段）
    - 14 个 `tokio::spawn` 长循环 worker（gw-store / gw-ledger / gw-gateway）

---

## 1. 背景与动机

最近一次对项目完成度的系统性盘点（见对话记录与 `docs/architecture.md` §9.1 / §10.5）暴露了 5 个高/中优先级 gap：

1. `StreamConsumer` NOGROUP 不自愈（Redis flush 后失效信号永久死亡）
2. HMAC verify 的解压结果未在 codec parse 复用（G7 follow-up）
3. `fail_to_proxy` 没有 envelope 渲染（上游连不上 → Pingora 默认 502 空 body）
4. 畸形 SSE chunk 仅 `tracing::warn!`，无 metric，运维不可观测
5. `ProtocolKind` / `ApiShape` 的 `as_str()` 与 `#[serde(rename_all)]` 是两种 wire form（`"openai"` vs `"open_ai"`），**by design 用于两个不同跨进程边界**（详见 §6.1），但缺 CI lock 防止任一边静默漂移

打补丁视角 5 个独立 PR 可解，但**根因聚类**显示它们对应 4 个跨切面主题：

| 问题 | 根因主题 |
|------|----------|
| 3 + 4 + 13 个 background worker 的散点 warn | **统一错误观测出口** |
| 2（HMAC）+ 现存 `cached_body` 语义分裂 | **请求 body 显式生命周期** |
| 1（NOGROUP）+ 6–7 个长循环 worker 共享相同模式 | **后台 worker 自愈契约** |
| 5（双 wire form 锁基线）| 影响面 = 2 enum + 2 个跨进程边界，**不抽抽象**，CI lock test 锁双形式不漂移 |

本 spec 处理前 3 个主题（**横向系统抽象**），加 1 个 CI lint 锁定主题 4。理由：每个抽象有 ≥3 个独立复用面，避免"打补丁 → 引入新一致性 gap"循环。

## 2. 范围

### 2.1 In scope

- **主题 1 `ErrorSink`**：新出口函数 `observe()` + `ErrorObservation` 类型；接入 `emit_envelope_and_bail`、`fail_to_connect`、**新增 `fail_to_proxy` override**、codec parse warn、background worker（经由主题 3 的 `Supervisor`）。
- **主题 2 `RequestBodyView`**：拆分 `ReqCtx.cached_body` 双重语义，引入 `raw / decoded (OnceCell) / outbound` 三态。删除 `cached_body` 字段。
- **主题 3 `ManagedLoop` + `Supervisor`**：定义 trait + Outcome enum；第一个客户端是 `StreamConsumer`（NOGROUP 自愈自然达成）。
- **主题 4 wire enum CI lock**：`crates/gw-core/tests/wire_enum_lock.rs` 单测锁**双 wire form 各自基线**（serde snake_case + as_str()），既不强行对齐也不"修"两边的差异 — 这是 by-design 跨进程契约（详见 §6.1）。`scripts/check-invariants.sh` 注册。

### 2.2 Out of scope（deliberate）

- 不引入 `GatewayError` 集中 enum — 与 Pingora `Error` 重叠，两套错误系统得不偿失。
- 不抽 `ManagedResource` / `Reconnect` / connection-pool 跨 crate 抽象 — `ensure()` 是 worker-local 语义恢复，统一 model 是过度设计。
- **中流故障**（即 `body_started=true` 之后 upstream 断开）**不**尝试塞 SSE error event。客户端读流逻辑各异，硬塞反而触发 codec 异常；与 `[[feedback_observability_drop_on_outage]]` 一致。
- 不为 wire enum 引入 `strum`/自定义宏 — 当下仅 2 个 enum，10 行单测 + CI lock 收益更高。
- 第一波**只迁移 StreamConsumer** 到 `ManagedLoop`；flusher / inflight_reaper / deduct_gc / detailed_archive / heartbeat / mgmt_blacklist refresher 留接口、按需迁。一次性 `tokio::spawn` 不迁。
- 不改 ErrorIR 形态、不改 envelope 渲染。
- 不重写 logging filter（logging 是 per-request 终结记账，与 ErrorSink 正交）。

## 3. 主题 1: `ErrorSink` — 统一错误观测出口

### 3.1 问题陈述

当前错误事件分散在 4 条不一致的路径：

| 路径 | 当前行为 | gap |
|------|----------|-----|
| Short-circuit（`emit_envelope_and_bail`，66 处） | 渲染 envelope + 客户端可见状态码 | metric 不一致；trace 字段散乱 |
| `fail_to_connect`（Pingora hook） | 标记 `ctx.upstream_error_code` | 无信封；客户端拿到 Pingora 默认 502 |
| `fail_to_proxy`（Pingora hook，**未 override**） | Pingora 默认行为，空 body 502 | **完全无信封 / metric / 自定义日志** |
| 进程内 warn（codec parse / render / background worker） | `tracing::warn!`/`tracing::error!` | 无 metric；告警不可观测 |

### 3.2 设计

新 module `crates/gw-proxy/src/errors/sink.rs`：

```rust
use std::fmt;

#[derive(Copy, Clone, Debug)]
pub enum ErrorKind {
    ShortCircuit,        // request_filter bail
    UpstreamConnect,     // fail_to_connect
    UpstreamMidStream,   // fail_to_proxy 在 body_started=true 之后
    UpstreamProtocol,    // upstream 返回 4xx/5xx body
    CodecParse,          // parse_stream_chunk / parse_response 解析失败
    CodecRender,         // cross-protocol re-render 失败
    BackgroundWorker,    // tokio::spawn 长循环异常
}

impl ErrorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortCircuit => "short_circuit",
            Self::UpstreamConnect => "upstream_connect",
            Self::UpstreamMidStream => "upstream_mid_stream",
            Self::UpstreamProtocol => "upstream_protocol",
            Self::CodecParse => "codec_parse",
            Self::CodecRender => "codec_render",
            Self::BackgroundWorker => "background_worker",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Stage {
    Request, Upstream, Response, Logging, Background,
}

impl Stage {
    pub fn as_str(&self) -> &'static str { /* snake_case */ }
}

pub struct ErrorObservation<'a> {
    pub code: &'static str,                  // codes::GW_* 或 codec error code
    pub kind: ErrorKind,
    pub stage: Stage,
    pub status: Option<u16>,                 // None = in-process warning
    pub trace_id: Option<&'a str>,
    pub extra: Option<&'a dyn fmt::Debug>,   // 结构化上下文（可选）
}

pub fn observe(o: ErrorObservation<'_>) {
    metrics::counter!(
        "gw_errors_total",
        "code" => o.code,
        "kind" => o.kind.as_str(),
        "stage" => o.stage.as_str(),
    ).increment(1);

    tracing::warn!(
        target: "gw_errors",
        code = o.code,
        kind = o.kind.as_str(),
        stage = o.stage.as_str(),
        status = ?o.status,
        trace_id = o.trace_id,
        extra = ?o.extra,
        "error observed",
    );
}
```

### 3.3 集成切入点

| 接入点 | 文件 | 改动 |
|--------|------|------|
| `emit_envelope_and_bail` 内部 | `crates/gw-proxy/src/filters/upstream.rs:77` | 调用前调 `observe(ShortCircuit, Stage::Request_or_Upstream, ...)`；66 处自动覆盖 |
| `fail_to_connect` | `crates/gw-proxy/src/gateway.rs:996` | 现有路径加一行 `observe(UpstreamConnect, Stage::Upstream, ...)` |
| **新 override `fail_to_proxy`** | `crates/gw-proxy/src/gateway.rs`（新 method） | 1) observe(UpstreamMidStream 或 UpstreamConnect)；2) 如未 `body_started`，渲染 `ErrorIR` envelope 到 session response；3) 已 `body_started`，截断流不补 SSE event |
| codec `parse_stream_chunk` 错误处 | `crates/gw-codec/src/*/codec.rs` 多处 | `tracing::warn!` 替换为 `observe(CodecParse, Stage::Response, ...)`；warn 行为不丢失（observe 内部仍 warn） |
| codec render 错误处 | 同上 | `observe(CodecRender, Stage::Response, ...)` |
| background worker 异常 | 由主题 3 的 `Supervisor` 自动调 | `observe(BackgroundWorker, Stage::Background, name=worker_name)` |

### 3.4 中流故障策略（deliberate）

`fail_to_proxy` 在 `body_started == true` 后被触发时（mid-stream upstream 断开）：

- **不**尝试塞 `data: {"error": ...}` SSE event。客户端读流逻辑五花八门（OpenAI SDK / Anthropic SDK / 浏览器 EventSource），硬塞反而引发解析异常。
- **行为**：截断 TCP 连接 + `observe(UpstreamMidStream, Stage::Upstream, ...)`。客户端通过流提前结束 + metric 感知。
- 与 `[[feedback_observability_drop_on_outage]]` "drop on observability outage" 一致 — 非阻塞错误就丢，靠 metric 而非补救式 patch。

### 3.5 Deliberate choices

- **不是 trait** — 单实现单出口；trait 化会引入测试 mock 复杂度且无 ≥2 个实现。
- **不接管 envelope body emission** — 与 `bail!`/`fail_to_proxy` 解耦，observe 关注点纯粹（"我看到了一个错误"）。
- **不引入 `GatewayError` enum** — Pingora `Error` 已是错误传递的载体；再加一层会让 `?` 传播路径变得复杂。`ErrorObservation` 是"观测事件"而非"错误值"。
- **不与 logging filter 合并** — logging 是 per-request 终结记账（usage、计费、archive），sink 是 error-anytime 跨阶段观测。两者正交。

### 3.6 新增 metric

- `gw_errors_total{code, kind, stage}` — counter
- 沿用现有 `codes::GW_*` 命名；codec error 复用 codec 内部 error code 字符串。

## 4. 主题 2: `RequestBodyView` — body 显式生命周期

### 4.1 问题陈述

`ReqCtx.cached_body: Option<Bytes>` 当前承担**两种互斥语义**：

- 在 `request_filter` step 3b（HMAC verify 路径），它存"已解压的 body"，供后续 codec parse 复用（仅 `signature_required = true` 时设置）。
- 在 `upstream_request_filter` 中，它被当成"已 mutation、即将发出的出站 body"使用（`crates/gw-proxy/src/filters/upstream.rs:286,728-797`）。

后果：

- HMAC 解压复用是 G7 follow-up gap — 当 `signature_required = false` 且 body 压缩时，codec parse 必须自行解压（重复工作）。
- 任何新增的 filter（例如未来的请求审计、prompt rewriter）都要重新决定它读 / 写哪个语义，靠 review 维护。
- 字段名 `cached_body` 本身误导。

### 4.2 设计

新 module `crates/gw-proxy/src/body/view.rs`：

```rust
use bytes::Bytes;
use gw_codec::compression::Encoding;
use once_cell::unsync::OnceCell;

pub struct RequestBodyView {
    raw: Bytes,
    req_encoding: Encoding,
    decoded: OnceCell<Result<Bytes, DecodeErr>>,
    outbound: Option<Bytes>,
}

impl RequestBodyView {
    pub fn from_raw(raw: Bytes, req_encoding: Encoding) -> Self {
        Self { raw, req_encoding, decoded: OnceCell::new(), outbound: None }
    }

    pub fn raw(&self) -> &Bytes { &self.raw }
    pub fn req_encoding(&self) -> Encoding { self.req_encoding }

    /// 懒解压 + 一次性缓存. max_bytes 防 decompression bomb.
    pub fn decoded(&self, max_bytes: usize) -> Result<&Bytes, &DecodeErr> {
        self.decoded
            .get_or_init(|| decompress(&self.raw, self.req_encoding, max_bytes))
            .as_ref()
    }

    /// upstream_request_filter 把改写后的出站 body 放这里.
    pub fn set_outbound(&mut self, body: Bytes) { self.outbound = Some(body); }

    /// 实际发出的 body. outbound 优先, 否则 raw.
    pub fn outbound_or_raw(&self) -> &Bytes {
        self.outbound.as_ref().unwrap_or(&self.raw)
    }
}

#[derive(Debug)]
pub enum DecodeErr { TooLarge, FormatError(&'static str) }
```

### 4.3 迁移路径

| 当前 | 迁移后 |
|------|--------|
| `ReqCtx.cached_body: Option<Bytes>` | `ReqCtx.body: Option<RequestBodyView>` |
| `crates/gw-proxy/src/filters/request.rs:480-595` (HMAC verify 路径) | `body.decoded(32 * MB)?` 取已解压，不再写 `cached_body` |
| `request_filter` step 4 codec parse | 同样调 `body.decoded(...)` — OnceCell 命中，零二次解压 |
| `crates/gw-proxy/src/filters/upstream.rs:286, 728-797` | 改写时 `body.set_outbound(serialized)`；发出 `body.outbound_or_raw()` |
| 全部 `ctx.cached_body` 引用（≈ 6 处） | 改成对应的 `body.decoded(...)` 或 `body.outbound_or_raw()` 调用 |

### 4.4 CI lint

`scripts/check-invariants.sh` 新增 `no_cached_body` rule（error 模式）：

```bash
if rg -n 'cached_body' --type rust crates/ >/dev/null; then
  echo "ERROR: 'cached_body' is replaced by RequestBodyView; use ctx.body API instead"
  exit 1
fi
```

### 4.5 Deliberate choices

- **`OnceCell` 而非 `Mutex<Option>`** — Pingora `Session` 单线程持有 `ReqCtx`，类型层面保证只解压一次，编译期消除 race。
- **不下沉到 codec API** — codec 仍接收 `&[u8]`；view 活在 ReqCtx 内，不污染 codec trait。
- **不引入 `BodyState` enum 状态机** — `OnceCell` + `Option<outbound>` 已隐式表达 raw → decoded → outbound 三态，加 enum 是过度。
- **decoded 失败也缓存** — `OnceCell<Result<Bytes, DecodeErr>>` 让 HMAC verify 和 codec parse 看到一致的失败（避免不一致诊断）。
- **`decoded()` 返回 `Result<&Bytes, &DecodeErr>`**（而非 `Result<Bytes, DecodeErr>`） — 0 拷贝；上层若需 owned 自己 clone。

## 5. 主题 3: `ManagedLoop` — 后台 worker 自愈契约

### 5.1 问题陈述

仓里有 14 个 `tokio::spawn`，其中**长循环 worker** 至少 7 个：

- `StreamConsumer.consume_loop` — `crates/gw-store/src/stream_consumer.rs:53`
- ledger `flusher` — `crates/gw-ledger/src/flusher.rs:43`
- `inflight_reaper` — `crates/gw-ledger/src/inflight_reaper.rs:18`
- `deduct_gc` — `crates/gw-ledger/src/deduct_gc.rs:25`
- `detailed_archive` worker — `crates/gw-ledger/src/detailed_archive.rs:125`
- `instance_id` heartbeat — `crates/gw-ledger/src/instance_id.rs:94`
- `mgmt_blacklist` refresher — `crates/gw-gateway/src/lib.rs:692/772/893/903`

共享同一种**模式三元组**：

1. **瞬态失败**（网络抖动）→ 应 sleep + 继续
2. **资源消失**（NOGROUP / pg connection dropped / channel closed）→ 应**重建资源**后继续
3. **永久失败**（配置错 / panic）→ 应观测后退出

当前 7 个 worker **各写各的**，多数只 sleep 不重建（NOGROUP 死循环就是这种）。

### 5.2 设计

新 module `crates/gw-store/src/managed_loop.rs`（gw-store 已被 gw-ledger / gw-gateway 间接依赖）：

```rust
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use tokio::task::JoinHandle;

#[async_trait]
pub trait ManagedLoop: Send + Sync + 'static {
    /// metric label + log target.
    fn name(&self) -> &'static str;

    /// 启动时一次 ensure, 资源消失后 supervisor 也会再次调用. 默认 ok.
    async fn ensure(&self) -> Result<(), anyhow::Error> { Ok(()) }

    /// 一次业务 tick. Outcome 表达"下一步".
    async fn tick(&self) -> Outcome;
}

pub enum Outcome {
    Continue,                            // 立刻下一轮
    Sleep(Duration),                     // 显式 backoff
    Recover(RecoverReason),               // 资源消失, supervisor 调 ensure() 后继续
    Fatal(anyhow::Error),                 // 不可恢复, 退出
}

pub enum RecoverReason {
    UpstreamResourceGone(&'static str),  // "NOGROUP" / "pg_disconnect" / ...
    Other(&'static str),
}

pub struct Supervisor;

impl Supervisor {
    pub fn spawn<L: ManagedLoop>(l: Arc<L>) -> JoinHandle<()> {
        let name = l.name();
        tokio::spawn(async move {
            // 启动 ensure
            if let Err(e) = l.ensure().await {
                crate::errors::sink::observe(ErrorObservation {
                    code: "ensure_failed", kind: ErrorKind::BackgroundWorker,
                    stage: Stage::Background, status: None,
                    trace_id: None, extra: Some(&(name, &e)),
                });
                // 继续进 tick 循环；下一次 Recover 仍会触发 ensure
            }
            loop {
                match l.tick().await {
                    Outcome::Continue => {}
                    Outcome::Sleep(d) => tokio::time::sleep(d).await,
                    Outcome::Recover(reason) => {
                        observe(ErrorObservation {
                            code: "recover", kind: ErrorKind::BackgroundWorker,
                            stage: Stage::Background, status: None,
                            trace_id: None, extra: Some(&(name, &reason)),
                        });
                        // ensure() 失败不退出 — 下一轮 Recover 仍会再 ensure.
                        let _ = l.ensure().await;
                    }
                    Outcome::Fatal(e) => {
                        observe(ErrorObservation {
                            code: "fatal", kind: ErrorKind::BackgroundWorker,
                            stage: Stage::Background, status: None,
                            trace_id: None, extra: Some(&(name, &e)),
                        });
                        break;
                    }
                }
            }
        })
    }
}
```

### 5.3 第一个客户端：StreamConsumer（NOGROUP 自愈自然达成）

迁移 `crates/gw-store/src/stream_consumer.rs`：

```rust
#[async_trait]
impl ManagedLoop for StreamConsumer {
    fn name(&self) -> &'static str { "stream_consumer" }

    async fn ensure(&self) -> Result<(), anyhow::Error> {
        let client = self.redis.pool.next();
        // mkstream=true; tolerate BUSYGROUP (already exists).
        let _: Result<(), _> = client
            .xgroup_create(ENTITY_STREAM, &*self.instance_id, "$", true).await;
        let _: Result<(), _> = client
            .xgroup_create(COUNTER_STREAM, &*self.instance_id, "$", true).await;
        Ok(())
    }

    async fn tick(&self) -> Outcome {
        let client = self.redis.pool.next();
        let raw = client.xreadgroup(...).await;
        match raw {
            Err(e) if is_nogroup(&e) => Outcome::Recover(
                RecoverReason::UpstreamResourceGone("NOGROUP"),
            ),
            Err(_) => Outcome::Sleep(Duration::from_millis(500)),
            Ok(value) => {
                // 处理消息 ...
                Outcome::Continue
            }
        }
    }
}
```

启动入口：`Supervisor::spawn(Arc::new(consumer))` 替换原 `tokio::spawn(consumer.consume_loop())`。

NOGROUP 自愈链路：

```
xreadgroup → NOGROUP error
  → tick() 返回 Outcome::Recover(UpstreamResourceGone("NOGROUP"))
  → Supervisor observe + 调 ensure()
  → ensure() 重建 consumer group
  → 下一轮 tick() 正常 poll
```

### 5.4 后续迁移路线（**out of strict scope，本 spec 不强制**）

按业务优先级：

1. ledger `flusher` — 关键路径，最先迁
2. `inflight_reaper` / `deduct_gc` — 计费一致性，紧随其后
3. `detailed_archive` worker — 受 `[[feedback_observability_drop_on_outage]]` 影响最大，单独验证
4. `instance_id` heartbeat / `mgmt_blacklist` refresher — 影响小，最后

每个迁移独立 PR，**不阻塞本 spec 落地**。一次性 spawn（`identity_persist` 等）不迁。

### 5.5 CI lint（按需）

`scripts/check-invariants.sh` 加 advisory（warn 模式）：长循环模块（gw-store / gw-ledger 指定路径）出现 `tokio::spawn(` + `loop {` 而不 `impl ManagedLoop` → 警告。维持 warn 而非 error，避免阻塞合法 short-lived spawn。

### 5.6 Deliberate choices

- **不抽 connection-pool / `Resource` trait** — `ensure()` 是 worker-local 语义恢复（重建 consumer group / 重连 pg / 重打开 file），由 worker 自己决定怎么 ensure；统一 resource model 是过度抽象。
- **`Outcome` 是 enum 不是 Result** — `Recover` 不是错误，是"我需要协作"信号；用 Result 会扭曲语义。
- **Supervisor 不做心跳/全局注册** — 维持简单：每个 worker 独立 spawn，错误经 ErrorSink 上报；可观测性靠 `gw_errors_total{kind=background_worker, code=*}` 自然涌现。
- **第一波只迁 StreamConsumer** — NOGROUP 是已暴露的具体 bug；其他 6 个 worker 没出过事，迁移按优先级排队，避免单次改动面失控。
- **`ensure()` 启动时即调** — 让 worker 自带启动逻辑（消除原 `start()` 中的 `xgroup_create` 散点）。

## 6. 主题 4: wire enum 漂移 lock（CI test，不抽抽象）

### 6.1 现状勘误（重要 — 跨进程契约 audit）

最初的 gap 表述（"`ProtocolKind` 的 `as_str()` 与 `#[serde(rename_all)]` 手写两遍，存在静默漂移风险"）**说法不准确**。审计跨进程契约（`docs/go-contracts/parts/03-shared-schema.md:140,340`, `01-go-as-producer.md:62`, e2e 测试 `test_cross_protocol.py:727-728`, `test_resilience.py:142-215`, `conftest.py:1018`）后的真实情况：

| 边界 | wire form | OpenAi 形式 | 来源依据 |
|------|-----------|-------------|---------|
| **entity payload JSONB**（PG `vendor_keys.payload`、`apikeys.payload` 等 Go 写入、Rust 读取的 JSONB） | `serde` snake_case | `"open_ai"` | e2e fixture 写入 `"accessible_endpoints": ["open_ai"]`, 与 `#[serde(rename_all = "snake_case")]` 行为一致 |
| **events stream payload + `model_mapping` 表 `protocol` 列** | `as_str()` 形式 | `"openai"` | `stream_consumer.rs:211-213` 反向 lookup 用 `"openai"`; migration `0003_model_mapping.sql` 的 protocol 列约定见 contracts `01-go-as-producer.md:62` |

两种形式**都是 Go ↔ Rust 跨进程契约的一部分，且 by design**。Python 测试代码（`test_cross_protocol.py:727-728`）注释明确指出：

> Metric labels 用 `as_str` 形式 (`"openai"/"anthropic"/"gemini"`)，与 entity payload 用的 serde snake_case 形式 (`"open_ai"/"anthropic"/"gemini"`) 不同。

所以"双拼写"不是 latent bug，是**两个不同跨进程边界故意使用的两种 wire form**。

### 6.2 主题 4 的真正目的（修正）

**目标**：把"两种 by-design 形式"用单测**显式锁定**，让任一边的 wire form 漂移都立即 CI 失败。**不**要试图让两边对齐 — 那会破坏 Go 契约。

具体来说要防止的回归：

1. 有人在 `ProtocolKind` 上加 `#[serde(rename = "openai")]` 想"统一" → 会破坏 entity payload 契约 (Go 写的 `"open_ai"` 突然反序列化失败)
2. 有人改 `as_str()` 返回值 → 会破坏 events stream 反向 lookup (`"openai" => OpenAi`)
3. 有人改 `#[serde(rename_all)]` 选项 → 静默影响 entity payload schema
4. 升级 serde crate 后 `rename_all = "snake_case"` 行为对 PascalCase 边界的处理发生变化

### 6.3 设计

`crates/gw-core/tests/wire_enum_lock.rs`：

```rust
//! Locks ProtocolKind / ApiShape wire forms against silent drift.
//!
//! Two wire forms are intentional cross-process contract:
//!   - `serde` snake_case: PG entity payload JSONB ("open_ai" for OpenAi)
//!   - `as_str()`: events stream payload + model_mapping.protocol column ("openai")
//!
//! See `docs/go-contracts/parts/03-shared-schema.md` and
//! `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §6.

use gw_core::protocol::{ApiShape, ProtocolKind};

#[test]
fn protocol_kind_serde_wire_form_locked() {
    // Locks the Go ↔ Rust entity payload contract (vendor_keys.payload etc).
    // DO NOT change these strings — Go writes them into PG JSONB.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi,    "open_ai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini,    "gemini"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        let got_str = got.as_str().expect("serializes as string");
        assert_eq!(
            got_str, *expected,
            "{:?}: serde wire form drift; got '{}', contract baseline '{}'. \
             entity payload JSONB in PG depends on this — do NOT change.",
            v, got_str, expected,
        );
    }
}

#[test]
fn protocol_kind_as_str_wire_form_locked() {
    // Locks the events:entity stream payload + model_mapping.protocol column form.
    // DO NOT change these strings — Go publishes them in stream payloads.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi,    "openai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini,    "gemini"),
    ];
    for (v, expected) in cases {
        assert_eq!(
            v.as_str(), *expected,
            "{:?}: as_str wire form drift; got '{}', contract baseline '{}'. \
             events stream + model_mapping.protocol column depend on this.",
            v, v.as_str(), expected,
        );
    }
}

#[test]
fn api_shape_serde_wire_form_locked() {
    // ApiShape 出现在 entity payload (?) 与 metric labels.
    // 仅 serde 形式当前未被 Go 写入消费, 但锁基线防 serde crate 升级影响.
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat,        "open_ai_chat"),
        (OpenAiResponses,   "open_ai_responses"),
        (AnthropicMessages, "anthropic_messages"),
        (GeminiGenerate,    "gemini_generate"),
        (ClaudeCli,         "claude_cli"),
        (OpenAiCli,         "open_ai_cli"),
        (OpenAiCompact,     "open_ai_compact"),
        (GeminiCli,         "gemini_cli"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        assert_eq!(
            got.as_str().unwrap(), *expected,
            "{:?}: ApiShape serde wire drift",
            v,
        );
    }
}

#[test]
fn api_shape_format_id_wire_form_locked() {
    // format_id 用冒号风格 ("openai:chat") — 与 metric label 一致, 独立 invariant.
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat,        "openai:chat"),
        (OpenAiResponses,   "openai:responses"),
        (AnthropicMessages, "anthropic:messages"),
        (GeminiGenerate,    "gemini:generate"),
        (ClaudeCli,         "claude:cli"),
        (OpenAiCli,         "openai:cli"),
        (OpenAiCompact,     "openai:compact"),
        (GeminiCli,         "gemini:cli"),
    ];
    for (v, expected) in cases {
        assert_eq!(v.format_id(), *expected, "{:?}: format_id drift", v);
    }
}
```

### 6.4 Invariants 注册

`scripts/check-invariants.sh` 加：

```bash
echo "[wire_enum_lock]"
if cargo test -p gw-core --test wire_enum_lock --quiet 2>&1 > /dev/null; then
    echo "  ok"
else
    echo "  FAIL: wire enum drift detected — see docs/superpowers/specs/2026-05-28-systemic-fixes-design.md §6"
    fail=1
fi
```

### 6.5 Deliberate choices（修正后）

- **不抽 strum 或自定义 `#[wire_enum]` 派生宏** — 当前仅 2 个 wire enum，10 行单测胜出。
- **不"修复"双 wire form** — 它是 by-design 跨进程契约的一部分；强行对齐会破坏 Go 端已经持久化的 entity payload JSONB。
- **测试明确锁定双形式各自的基线** — 第一次跑应当**通过**（不是失败）；只有真正发生回归时才失败。
- **每条 assertion 包含 contract 说明** — `panic!` 消息直接告诉读者"为什么不能改"，避免后人误以为是 latent bug 去"修"。
- 文档同步（§10）必须在 `docs/architecture.md` §Deliberate behaviours 新增一条："ProtocolKind 故意维持两种 wire form — serde snake_case 给 entity payload, as_str 给 events stream + model_mapping.protocol 列"。

## 7. PR 拓扑

```
PR-A: ErrorSink 基础设施 (sink.rs + emit_envelope_and_bail 接入 + fail_to_connect 接入)
       └─ 不含 fail_to_proxy override（避免一次改动面过大）
       
PR-B: RequestBodyView 引入 + cached_body 拆迁 + CI lint no_cached_body
       └─ 独立于 ErrorSink（不依赖）

PR-C: ManagedLoop + Supervisor + StreamConsumer 迁移
       └─ 依赖 PR-A（Supervisor 内部调 observe）

PR-D: wire_enum_lock test + scripts/check-invariants.sh 注册
       └─ 独立

PR-E: fail_to_proxy override + 渲染 envelope + observe
       └─ 依赖 PR-A
       
PR-F: codec / background worker 散点 tracing::warn 批量改 observe
       └─ 依赖 PR-A
```

依赖图：
```
PR-A ──┬─→ PR-C
       ├─→ PR-E
       └─→ PR-F
PR-B (独立)
PR-D (独立)
```

并行机会：A、B、D 同时开工；C/E/F 在 A 落地后并行。

## 8. 测试策略

### 8.1 ErrorSink (PR-A, PR-E, PR-F)

- **Unit**：`observe(...)` 调用后 `gw_errors_total{code=X, kind=Y, stage=Z}` +1（用 Prometheus registry mock）。
- **E2E**：
  - 触发 `fail_to_proxy`（上游 fake-upstream 关闭连接）→ 验证响应体含 `gateway_error` envelope（PR-E）。
  - 触发 mid-stream 断开（fake-upstream 半流截断）→ 验证 metric +1 + 流被截断 + 客户端拿到 0-byte chunk 结束（PR-E）。
  - 注入畸形 SSE chunk → `gw_errors_total{kind=codec_parse}` +1（PR-F）。

### 8.2 RequestBodyView (PR-B)

- **Unit**：构造 `RequestBodyView`，连续两次调 `decoded(...)` → 验证内部 `decompress` 只调用一次（通过 mock 或计数）。
- **E2E**：发送 gzip 压缩的签名请求 → HMAC verify + codec parse 共用解压结果（用 tracing 校验只有一次 "decompress" 事件）。

### 8.3 ManagedLoop (PR-C)

- **Unit**：mock 一个 ManagedLoop，`tick` 第 3 次返回 `Recover(NOGROUP)`，验证 `ensure` 被再调用一次。
- **E2E**：
  - 启动网关，连一个 entity event → `apply_entity_event` 被调用，cache invalidate。
  - Redis `FLUSHALL` → 下一轮 xreadgroup 失败 NOGROUP。
  - 等 ~500ms，再发一个 event → 验证它仍能被消费（说明 group 重建成功）。
  - `gw_errors_total{kind=background_worker, code=recover}` ≥ 1。

### 8.4 wire_enum_lock (PR-D)

- `cargo test -p gw-core --test wire_enum_lock` 自身即验证。
- 手动故意改 ProtocolKind 的 serde rename 或 as_str 引入漂移 → 测试失败 + 错误消息明确指向哪个 wire form 漂移及其 Go 契约后果。

### 8.5 Soak 回归

`make e2e-soak-smoke` 在 A/B/C/E/F 各 PR 都跑一次。重点指标：

- `gw_errors_total{kind=*}` 出现合理分布（heavy chaos 下 mid_stream / upstream_connect 占多数）
- 无 `cached_body` 引用（B 后）
- `gw_errors_total{kind=background_worker, code=recover}` 在长时间运行后非零（NOGROUP 偶发恢复）

## 9. 风险与已知限制

| 风险 | 缓解 |
|------|------|
| `cached_body` 拆迁触及 5+ filter, rebase 风险 | PR-B 独立先落地, 后续 PR 在 view API 上 build |
| `fail_to_proxy` override 影响所有上游路径, 可能误吞 Pingora 自带语义 | PR-E 灰度: 先实现仅 `body_started=false` 渲染 envelope, mid-stream 仍走 Pingora 默认; e2e soak 验证后扩到 mid-stream 截断 |
| ManagedLoop 第一波只迁 StreamConsumer, 其他 worker 维持现状, 双轨期较长 | spec §5.4 列出迁移优先级; 双轨期允许 — Supervisor 与现有 `tokio::spawn` 共存无冲突 |
| `ErrorSink` 单实现而非 trait, 测试要靠 Prometheus registry mock | 在 sink.rs 内部直接暴露 `#[cfg(test)] fn last_observation_for_testing()` 接口供 unit test 用 |
| 中流故障"不补 SSE error event" 决策可能被未来需求挑战（如客户要求标准化的 stream error） | spec §3.4 明确这是 deliberate choice; 真正要补的时候必须修改 codec 而非 ErrorSink — 责任边界清晰 |
| OnceCell 在 ReqCtx 内 — Pingora 是 Send？ | OnceCell 非 Send; 若 ReqCtx 需要跨 await 在不同 task 持有, 改为 `tokio::sync::OnceCell`. 实测确认 Pingora session 是单线程持有 ReqCtx, std OnceCell 够用 |

## 10. 文档同步（按 CLAUDE.md Sync rule）

| 文件 | 改动 |
|------|------|
| `docs/architecture.md` §Deliberate behaviours | +条目: "Mid-stream upstream failure does not emit SSE error event (drop on outage)"; +条目: "`RequestBodyView` 替代 `cached_body`; 三态显式 (raw / decoded OnceCell / outbound)"; **+条目: "`ProtocolKind` 故意维持两种 wire form — serde snake_case (`open_ai`) 给 entity payload JSONB, `as_str` (`openai`) 给 events stream + model_mapping.protocol 列。两边各自由 `wire_enum_lock` invariant 锁基线"** |
| `docs/architecture.md` §Known gaps | 从 §9.1 移除 G7 follow-up (HMAC body reuse); 从 §10.5 移除 fail_to_proxy / malformed SSE / NOGROUP 3 项; **ProtocolKind 项措辞从"双拼写"改为"双 wire form by-design + 已锁基线"** |
| `docs/architecture.md` §Invariants | +`no_cached_body`, +`wire_enum_lock`, +`managed_loop_advisory`(warn) |
| `docs/workflow.md` §request_filter step 3b/4 | 描述更新为"body.decoded(max) 懒解压, OnceCell 复用"; cached_body 字眼全删 |
| `docs/workflow.md` §error fanout | +新章节"Error observability sink"; 列出 4 路径 + observe 接入点 + 中流策略 |
| `CLAUDE.md` §Sync rule | 不动 (已被 architecture/workflow 覆盖) |

## 11. 验证清单 (Plan 阶段检查表)

- [ ] PR-A 落地后, `cargo build` 干净, `cargo test --workspace` 通过
- [ ] PR-A 落地后, `gw_errors_total{kind=short_circuit}` 在 e2e soak 中出现非零
- [ ] PR-B 落地后, `rg cached_body crates/` 返回空
- [ ] PR-B 落地后, gzip 压缩请求 + HMAC verify, tracing 显示 decompress 只发生一次
- [ ] PR-C 落地后, `FLUSHALL` redis 测试: NOGROUP recovery e2e 通过
- [ ] PR-D 落地后, `scripts/check-invariants.sh` 显示 wire_enum_lock ok（第一次跑应当**通过**，因双 wire form 已与现状一致；如果失败说明 spec §6.1 表格已经过时，先查现状再决定）
- [ ] PR-E 落地后, fake-upstream 关闭连接 → 客户端收到 502 + JSON envelope
- [ ] PR-F 落地后, codec / worker 关键 warn 在 `gw_errors_total` 中可见
- [ ] 所有 PR 合并后, architecture.md §9.1 / §10.5 4 项 gap 全部勾掉
- [ ] `make e2e-soak-smoke` 通过, soak report 显示无新 invariant 违规

---

## 附录 A: 不被处理的相关 gap

以下 gap 与本 spec 主题相关但**故意不在范围内**（详见 §2.2）：

- **G11 follow-up**（三层 DLQ 回退） — 依赖 Go billing 支持，不属于网关跨切面问题。
- **G3 follow-up**（日配额时区） — 单点功能补全，无横向影响。
- **G13 follow-up**（capability 字段命名统一） — 破坏性变更，留待大版本。
- 其他 5 个 long-loop worker 的迁移 — §5.4 列出优先级，独立 PR 跟进。

## 附录 B: 与现有 spec 的关系

- 与 `2026-05-23-v2-cleanup-overview-design.md` 互补：v2 cleanup 关注的是"v1 placeholder → v2 实现"，本 spec 关注的是"v2 已实现但横向不一致"。
- 与 `2026-05-24-billing-robustness-design.md` 解耦：billing robustness 关注计费状态机；本 spec 的 ManagedLoop 不接管 InflightLedger 的 watchdog（那是 billing 内部状态，非通用 worker 模式）。
- 与 `2026-05-28-egress-impersonation-design.md` 同日：egress impersonation 的 EgressHandle / Supervisor 在 §8.6 自带观测 metric，本 spec 的 ErrorSink 不接管 egress 内部错误（egress 已自管），仅在 egress 拒服时通过 `inject_vendor_headers` 的常规错误路径流入 sink。
