# Egress Impersonation — 设计文档（v2026-05-28）

- **日期**：2026-05-28
- **状态**：Draft（待审阅）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - Aether 来源：
    - `src/services/provider/fingerprint.py`（per-key fingerprint lazy persist）
    - `src/services/provider/adapters/claude_code/envelope.py`（`extra_headers` / `prepare_context`）
    - `src/services/provider/adapters/claude_code/constants.py`（`CLAUDE_CODE_DEFAULT_HEADERS` / Beta tokens）
    - `src/clients/curl_cffi_transport.py`（curl_cffi TLS 后端选择）
    - `src/clients/http_client.py:276-325`（`use_curl_cffi_tls` 分支）
  - 本仓内：
    - `crates/gw-proxy/src/filters/upstream.rs`（`pick_peer` + `inject_vendor_headers`）
    - `crates/gw-proxy/src/gateway.rs`（`upstream_peer` 入口、TLS error 分类）
    - `crates/gw-core/src/entity.rs`（`Vendor` / `VendorKey`）
    - `crates/gw-tunnel-client/`（已有 egress 节点机制；与本设计正交）
  - 外部研究：
    - DeepWiki `cloudflare/pingora`（rustls/boringssl 互斥、`PeerOptions` 暴露面、ClientHello 不可定制）
    - DeepWiki `0x676e67/rquest`（Emulation preset、HTTP/2 fingerprint、构建依赖、与 openssl-sys 冲突）
    - DeepWiki `openai/codex`（Codex CLI 头集合、UA 模板、session/turn 头）
    - DeepWiki `google-gemini/gemini-cli`（Gemini CLI UA 模板、x-goog-* 头）
- **范围**：在 `ailinkmax-gateway` 中以**可按 Vendor 切换**的方式提供"出向请求伪装"——同一二进制内同时存在 rustls（默认路径，行为字节级不变）与 BoringSSL（伪装路径），覆盖 Anthropic / OpenAI / Gemini 三家上游的真实 CLI 客户端 TLS JA3/JA4 + HTTP/2 指纹 + Header 伪装。
- **不在范围**：
  - 入向（客户端 → 网关）TLS / 指纹处理（永远是 Pingora 的 rustls server，不变）；
  - aether-proxy / aether-hub 节点的指纹伪装（属于 Aether 项目独立工作）；
  - 现有 tunnel_stack 节点的伪装支持（tunnel 与 impersonation 互斥，本文 §3 明确）；
  - 计费 / 路由 / 权限合并 / rate-limit 任一现有 filter 的语义变更（保持透明）；
  - 入向 / 出向 protocol-level 数据转换（codec 层不动）。

> 这套伪装层不改变请求语义，只改变上游看到的"客户端身份"。它把"被上游因为 client/feature 不匹配而封号"的风险从架构层解决一次，避免每次新上游策略调整都要追着 patch 头。

---

## 1. 背景与问题

### 1.1 风险面

主流上游（Anthropic、OpenAI ChatGPT 后端、Google Vertex）对**非官方客户端**有日益严格的 fingerprint 检测。已观察到的封号 / 限流信号至少包括：

- **TLS JA3 / JA4**：rustls / Go net/http 默认产出的 ClientHello 与 Chrome / Node fetch / undici 在 cipher 顺序、扩展顺序、GREASE、ALPS 维度都可机器区分；
- **HTTP/2 fingerprint**：SETTINGS 帧顺序、`initial_window_size`、`max_concurrent_streams`、pseudo-header 顺序（`:method` `:authority` `:scheme` `:path` 的排列）；
- **HTTP header**：`User-Agent`、`x-stainless-*`（Anthropic Stainless SDK 标记）、`anthropic-beta` / `anthropic-version`、`x-app`、`OpenAI-Beta` / `x-codex-*`、`x-goog-api-client` / `x-gemini-api-privileged-user-id`、header 插入顺序；
- **行为信号**：account 与 fingerprint 的稳定关联（一个账号在不同实例上呈现不同 fingerprint = 强可疑）。

当前 `ailinkmax-gateway` 的出向走 Pingora-rustls，所有上述维度都呈现"显然不是 Claude Code / Codex / Gemini CLI"的特征。

### 1.2 Aether 已有实现（参考）

Aether 在 Python 端通过 `curl_cffi`（BoringSSL fork）+ `extra_headers` 双层伪装，关键设计点：

- **per-ProviderAPIKey 稳定指纹**：用 `key_id` 做种子确定性派生 + lazy persist 到 DB（`ProviderAPIKey.fingerprint` JSON 列），保证同一账号永远呈现同一身份；
- **TLS 层**：`curl_cffi` 的 `impersonate="chrome131"` 等 preset，复用 Session 池 keyed by `(impersonate, proxy)`；
- **Header 层**：`extra_headers` 注入完整 Stainless 头 + `anthropic-beta` 合并 + 按指纹生成的 `User-Agent`；
- **作用范围**：仅对 `claude:cli`（Claude Code OAuth）通道生效。

Aether 的"TLS 层"在 prebuilt Docker 镜像里**没启用**，因为 `curl_cffi` 在 `pyproject.toml` 的 `tls` extra 里、Dockerfile 用 `pip install .` 不带 extra；header 层是默认 on。

本设计要把这个**完整能力**搬到 ailinkmax-gateway，并扩展到 OpenAI Codex 与 Gemini CLI 两家。

### 1.3 ailinkmax-gateway 当前状态

- `Cargo.toml`：`pingora = { version = "0.8", features = ["rustls"] }`，`pingora-proxy` 同；
- 上游 TLS 路径：`crates/gw-proxy/src/filters/upstream.rs::pick_peer` → `HttpPeer::new(addr, tls=true, sni)`，握手由 pingora-rustls 完成；
- Header 注入点：`crates/gw-proxy/src/filters/upstream.rs:854` `inject_vendor_headers`——目前只注 `Authorization`（含 OAuth）、`host`、`traceparent`、`content-length`，**不注**任何客户端身份头；
- credential 模型：`gw-core::entity::VendorKey` 已有 `key_id` / `oauth` / `proxy: Option<Url>` / `tunnel_preference`——per-key fingerprint 自然挂这里；
- tunnel 机制：`crates/gw-proxy/src/tunnel_stack.rs::peer_for_node` 已经实现"egress 走远端节点"，但节点本身（`aether-proxy`）也是 rustls，不解决指纹问题。

---

## 2. 目标与非目标

### 目标

1. **同二进制双 TLS 栈共存**：rustls（Pingora 默认路径）+ BoringSSL（伪装路径），编译期同时进入产物，运行期按 Vendor 切换。
2. **rustls 路径行为字节级不变**：未开 impersonation 的 Vendor，请求/响应在请求头、字节顺序、计时、错误码、计费记录每一维度都与改动前一致——回归测试有 golden diff 卡线。
3. **per-Vendor 切换**：通过 `Vendor.egress_mode` 数据字段开关；admin 可热改，无需重启。
4. **三家上游 day-1 覆盖**：`ClaudeCodeChrome131` / `OpenAiCodex` / `GeminiCli` 三个 profile 首版即完整实现头集合，**不接受"留到下一版"**。
5. **per-key 稳定身份**：同一 `VendorKey.key_id` 永远派生同一身份，DB 持久化（同步 Aether 模式）。
6. **不破坏现有生命周期机器**：billing_lifecycle / inflight / error/listeners / latency_tracker / retry_blacklist 等无须改动。
7. **可观测 + 可回滚**：完整指标暴露、kill-switch 热生效。

### 非目标

- 不实现入向（client → gateway）的 TLS 指纹处理。
- 不为 ChatGPT Web、Claude Web 等非 CLI 客户端写 profile（可扩展，初版不做）。
- 不在 spec 中替换 rustls；rustls 仍是首选，伪装是 opt-in。
- 不实现"per-request 临时改 profile"——profile 是 Vendor 级配置。

---

## 3. 约束（deepwiki 核实）

| # | 约束 | 来源 | 设计含义 |
|---|---|---|---|
| C1 | `pingora-core` 的 `rustls` / `boringssl` feature 互斥（Cargo.toml 明确"Only one tls-providing feature can be selected at a time"） | DeepWiki `cloudflare/pingora` | 伪装的 BoringSSL **必须**在 Pingora 连接路径之外 |
| C2 | Pingora `HttpPeer` / `PeerOptions` 不暴露 ClientHello 的 cipher list / 扩展顺序 / GREASE / 扩展 permutation / ALPS；boringssl backend 的 `CIPHER_LIST` 是 const，rustls backend 更少 | 同上 | "把 Pingora 切到 boringssl"即使做了，也**到不了**浏览器级指纹 → 这条路被排除 |
| C3 | `PeerOptions.custom_l4` 只控 raw TCP；TLS 握手仍由 Pingora 后端做 | 同上 | 不能用 `custom_l4` 把"已经做完 TLS 的 stream"塞进去（scheme=true 时会再叠一层）；scheme=false 配合 custom_l4 需强制 h1 且丧失 h2 指纹，得不偿失 |
| C4 | rquest 与依赖 `openssl-sys` 的 crate 不兼容（README 警告 link 失败 / 段错误） | DeepWiki `0x676e67/rquest` | 必须验证 dep tree 无 openssl-sys（已验：`Cargo.lock` 仅 `aws-lc-rs` + `rustls-native-certs`，无冲突） |
| C5 | rquest 构建依赖：`cmake / perl / pkg-config / libclang-dev / build-essential / ca-certificates`（musl 目标还要 `musl-tools`） | 同上 | Dockerfile / CI 镜像需追加 |
| C6 | rquest proxy 只能 per-client，不支持 per-request 覆盖 | 同上 | 需要按 `(profile, Option<proxy_url>)` 维护 Client 池 |
| C7 | Vendor entity 在 `gw-core::entity`，VendorKey 同；二者均 DB-backed（`store.vendor_or_load` / `EntityStore`），Versioned + moka 热重载 | 本仓代码 | 加字段 = serde + 一行 SQL 加列 + nullable，老数据零迁移 |

---

## 4. 方案抉择

| 方案 | 满足"同二进制双栈" | 满足"全 JA3/JA4" | 复用 Pingora 生命周期 | 工程量 | 结论 |
|---|---|---|---|---|---|
| 切 Pingora 到 boringssl | ❌（C1，与 rustls 互斥） | ❌（C2，没 API） | ✓ | 中 | 排除 |
| `custom_l4` + 自做 TLS | △（scheme=false trick） | △（强制 h1 失 h2 指纹） | ✓ | 高 | 排除 |
| **A**：在 Pingora 内短路、用 rquest 内联完成上游请求 | ✓ | ✓ | ❌（需重写 inflight / billing / streaming / error） | 高 | 候选 |
| **B**：进程内 loopback BoringSSL 出向代理，Pingora 以明文 H1 连过去 | ✓ | ✓ | ✓ | **中** | **采用** |
| C：外部 sidecar 复用 Aether `curl_cffi` | ❌（违反"编译进一个二进制"） | ✓ | ✓ | 低 | 排除（约束冲突） |

**B 胜出的关键**：Pingora 仍然主导生命周期（`upstream_request_filter` / `upstream_response_filter` / `response_body_filter` / inflight / billing / error listeners / streaming），boring 复杂度被限定在**一个独立 crate**。loopback 跳的开销 §3 末尾有量化，对 LLM 流量是百分位看不见的增量。

---

## 5. 架构 / 组件

### 5.1 组件全景

```
                                ┌────────────────────────────────────────────────┐
                                │                gw-gateway (binary)             │
                                │                                                │
client ──TLS──> Pingora (rustls) ──> [pick_peer] ──┐                             │
                                │                  │                             │
                                │                  │ egress_mode = Rustls        │
                                │                  └─────────── HttpPeer ──────TLS──> upstream
                                │                  │                             │     (rustls 握手)
                                │                  │                             │
                                │                  │ egress_mode = Impersonate   │
                                │                  ▼                             │
                                │            HttpPeer (tls=false,                │
                                │              addr = UDS or 127.0.0.1)          │
                                │                  │                             │
                                │     ┌────────────▼────────────┐                │
                                │     │  gw-egress-impersonate  │                │
                                │     │  (loopback h1 listener) │                │
                                │     │  ┌──────────────────┐   │                │
                                │     │  │ rquest ClientPool│   │                │
                                │     │  │ keyed by (profile,│  │                │
                                │     │  │   proxy_url?)     │  │                │
                                │     │  └─────────┬────────┘   │                │
                                │     └────────────┼────────────┘                │
                                │                  │                             │
                                │                  └── BoringSSL ────────────TLS──> upstream
                                │                                                │     (JA3/JA4 + h2 伪装)
                                └────────────────────────────────────────────────┘
```

### 5.2 新 crate：`gw-egress-impersonate`

- **唯一**链接 `boring-sys` / `rquest` 的 crate；其他 crate 全部不感知。
- 内部模块：
  - `server`：loopback h1 listener（hyper 1.x server）；
  - `pool`：`ClientPool` — `DashMap<(ImpersonateProfile, Option<Url>), Entry>` + LRU 驱逐（`GW_EGRESS_MAX_SESSIONS`，默认 64）；
  - `profiles`：profile → `Emulation` / `Http2Config` / `headers_order` 映射；
  - `error`：rquest 错误分类 → `X-Egress-Error-Class` 头；
  - `supervisor`：listener 崩溃后指数退避重启（上限 5/min）；
  - `handle`：`EgressHandle { bind_addr, shutdown() }`——给 gw-gateway 在 main 里持有。

### 5.3 新 crate：`gw-core::impersonate`（**纯 Rust，无 boring 依赖**）

- 类型 `ImpersonateProfile`、`VendorEgressMode`、`IdentitySnapshot`、`IdentityVariable`；
- `derive_identity(profile, key_id) -> ResolvedIdentity`；
- `ensure_identity(&VendorKey, profile) -> ResolvedIdentity`（含 lazy persist 调度）；
- profile-static 常量 + per-key 池 + per-request 生成器；
- 被 `gw-proxy`（写 header）和 `gw-egress-impersonate`（读 profile metadata）共同依赖。

### 5.4 改动到现有 crate

- `gw-core::entity::Vendor`：加 `#[serde(default)] egress_mode: VendorEgressMode`；
- `gw-core::entity::VendorKey`：加 `#[serde(default)] identity_snapshot: Option<IdentitySnapshot>`；
- `gw-store`：vendors / vendor_keys 各加一 JSONB 列（nullable）+ migration；
- `gw-proxy::filters::upstream`：
  - `pick_peer`：新增 `egress_mode` 分支构造 HttpPeer（指向 loopback）；
  - `inject_vendor_headers`：尾部追加 `X-Egress-Target` / `X-Egress-Profile` / `X-Egress-Proxy` + derived identity headers；
- `gw-proxy::filters::response`：响应头解读 `X-Egress-Error-*`，映射进现有 retry / blacklist / listener 通道；
- `gw-proxy::ctx`：`ReqCtx` 加 `egress_profile: Option<ImpersonateProfile>` / `egress_upstream: Option<(String, u16)>`；
- `gw-gateway::main`：启动时构造 `EgressHandle`、注入到 `ProxyHttp`、wire shutdown；
- `crates/gw-proxy/src/detailed_log.rs`：strip 列表加 `X-Egress-*` 系列。

### 5.5 切换点摘要

| 阶段 | rustls path | impersonate path |
|---|---|---|
| `pick_peer` | `HttpPeer::new(host:port, tls=true, sni=host)` | `HttpPeer::new_uds(uds_path, false, sni=host)` 或 `HttpPeer::new(127.0.0.1:port, false, sni=host)` |
| `inject_vendor_headers` 末尾 | 现状 | 追加 `X-Egress-{Target,Profile,Proxy}` + identity 头 |
| 上游 TLS 握手 | pingora-rustls | rquest+BoringSSL（在 egress crate 内）|
| `upstream_response_filter` | 现状 | 读 `X-Egress-Error-*`（如有） + strip 内部头 |
| 其他（计费 / listener / streaming / retry） | 共用，不分叉 | 共用，不分叉 |

### 5.6 默认路径保证（**回归基线**）

- `egress_mode` 缺省 = `Rustls`；所有现存 Vendor 反序列化为默认；
- `pick_peer` 不进 Impersonate 分支；
- `inject_vendor_headers` 不写任何 `X-Egress-*` 或 identity 头；
- loopback listener 启动但**没人连**——`gw_egress_loopback_rtt_seconds` 计数为 0；
- `ctx.upstream_request_meta` 在 rustls path 字节级与改动前一致 → `tests/regression_rustls_unchanged.rs` golden diff 卡线。

---

## 6. 数据模型 / 持久化 / Profile 目录

### 6.1 核心类型

```rust
// gw-core::impersonate
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImpersonateProfile {
    ClaudeCodeChrome131,
    OpenAiCodex,
    GeminiCli,
}

impl ImpersonateProfile {
    pub fn as_static_str(self) -> &'static str { /* "claude_code_chrome131" 等 */ }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VendorEgressMode {
    Rustls,
    Impersonate { profile: ImpersonateProfile },
}

impl Default for VendorEgressMode { fn default() -> Self { Self::Rustls } }

// 给 Vendor 加字段（gw-core::entity::Vendor）
pub struct Vendor {
    // 现有字段不变...
    #[serde(default)]
    pub egress_mode: VendorEgressMode,
}

// 给 VendorKey 加字段（gw-core::entity::VendorKey）
pub struct VendorKey {
    // 现有字段不变...
    #[serde(default)]
    pub identity_snapshot: Option<IdentitySnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySnapshot {
    pub profile_kind: ImpersonateProfile,   // 一致性校验
    pub variable: IdentityVariable,         // per-profile 可变字段
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum IdentityVariable {
    ClaudeCode {
        os: String, arch: String,
        node_version: String, package_version: String, timeout: String,
        chrome_version: String, electron_version: String, user_agent: String,
    },
    OpenAiCodex {
        codex_version: String, os: String, arch: String,
        installation_id: uuid::Uuid, user_agent: String,
    },
    GeminiCli {
        cli_version: String, os_type: String, arch: String,
        installation_id: uuid::Uuid,
    },
}
```

### 6.2 派生与持久化（同步 Aether 模式）

```rust
pub struct ResolvedIdentity {
    pub profile: ImpersonateProfile,
    pub variable: IdentityVariable,
}

impl ResolvedIdentity {
    pub fn headers(
        &self, is_stream: bool, conversation_id: Option<&str>,
    ) -> Vec<(HeaderName, HeaderValue)> { /* static + per-key + per-request */ }
}

pub fn ensure_identity(key: &VendorKey, profile: ImpersonateProfile) -> ResolvedIdentity {
    if let Some(snap) = key.identity_snapshot.as_ref() {
        if snap.profile_kind == profile {
            return resolve(snap.clone());
        }
        // profile mismatch → 派生 + 重新 persist（覆盖旧的）
    }
    let derived = derive(profile, &key.key_id);
    schedule_lazy_persist(key.key_id.clone(), derived.clone());
    derived
}

fn derive(profile: ImpersonateProfile, key_id: &KeyId) -> ResolvedIdentity {
    let mut rng = seeded_rng(key_id);   // SHA-256(key_id) 前 8 字节做种子
    match profile {
        ImpersonateProfile::ClaudeCodeChrome131 => identity::claude_code::derive(&mut rng),
        ImpersonateProfile::OpenAiCodex          => identity::openai_codex::derive(&mut rng),
        ImpersonateProfile::GeminiCli            => identity::gemini_cli::derive(&mut rng),
    }
}
```

**lazy persist**：tokio task + `DashSet<KeyId>` 去重 + `pending_max` 上限（默认 2000，照搬 Aether `_PENDING_LAZY_PERSIST_MAX`），失败仅 warn，永不阻塞请求；写入逻辑：

```rust
// 只在 identity_snapshot IS NULL 时写入（避免覆盖 admin 手动改过的）
UPDATE vendor_keys SET identity_snapshot = $1, updated_at = NOW()
  WHERE id = $2 AND identity_snapshot IS NULL;
```

admin 暴露 `regenerate_identity` 操作（清列 → 下次请求重派生 + 持久化）。

### 6.3 Profile 1：`ClaudeCodeChrome131`

直接复刻 Aether `src/services/provider/fingerprint.py` + `claude_code/constants.py`。

| Header | 类 | 值 |
|---|---|---|
| `X-Stainless-Lang` | static | `js` |
| `X-Stainless-Runtime` | static | `node` |
| `X-Stainless-Retry-Count` | static | `0` |
| `X-App` | static | `cli` |
| `Anthropic-Dangerous-Direct-Browser-Access` | static | `true` |
| `anthropic-version` | static | `2023-06-01` |
| `Accept` | static | `application/json` |
| `X-Stainless-OS` | per-key | 池：`Linux` / `MacOS` / `Windows` |
| `X-Stainless-Arch` | per-key | 池：`x64` / `arm64` |
| `X-Stainless-Runtime-Version` | per-key | 池：`v20.18.1` / `v22.12.0` / `v22.14.0` / `v24.13.0` |
| `X-Stainless-Package-Version` | per-key | 池：`0.68.0` / `0.69.0` / `0.70.0` / `0.71.0` |
| `X-Stainless-Timeout` | per-key | 池：`600` / `900` |
| `User-Agent` | per-key | `Mozilla/5.0 ({platform_token}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{chrome_version} Electron/{electron_version} Safari/537.36` |
| `x-stainless-helper-method` | per-request | `stream` iff `ctx.is_stream` |
| `anthropic-beta` | per-request | 合并 `claude-code-20250219` + `oauth-2025-04-20` + `interleaved-thinking-2025-05-14`（按需 `context-1m-2025-08-07`）|

**池常量**（与 Aether 同步）：
- Chrome version → impersonate 映射：`chrome110/116/119/120/123/124/131/133` ↔ Chrome 版本号；
- Electron 版本池：`35.5.1` / `36.7.1` / `37.3.0` / `38.7.0` / `39.2.3`；
- Platform token：`X11; Linux x86_64` / `X11; Linux arm64` / `Windows NT 10.0; Win64; x64` / `Macintosh; Intel Mac OS X 10_15_7` / `Macintosh; ARM Mac OS X 14_0_0`。

**rquest emulation**：`Emulation::Chrome131`（pool 派出的 chrome_version 与 emulation preset 绑定；初版只选 chrome131 作为单一 TLS profile，pool 字段只在 UA 层变化——简化指纹一致性，未来扩展再考虑多 emulation preset）。

### 6.4 Profile 2：`OpenAiCodex`（基于 openai/codex Rust 实现）

| Header | 类 | 值 |
|---|---|---|
| `Accept` | static | `text/event-stream`（流） / `application/json`（非流） |
| `OpenAI-Beta` | static | `responses=v1`（HTTP `/v1/responses` path；以 codex-rs 当前 source 为准，§11 列为已知校准项） |
| `User-Agent` | per-key | `codex_cli_rs/<version> (<os>; <arch>)`；`<version>` 池：从 codex-rs 已发布版本拉 4-6 个稳定版（首版常量）；`<os>` 池：`linux` / `macos` / `windows`；`<arch>` 池：`x86_64` / `aarch64` |
| `x-codex-installation-id` | per-key | `Uuid::new_v5(NAMESPACE_URL, "codex:install:"+key_id)` |
| `session-id` | per-request | `Uuid::new_v4()` |
| `thread-id` | per-request | `Uuid::new_v4()`（同请求内复用） |
| `x-client-request-id` | per-request | `= thread-id` |
| `x-codex-window-id` | per-request | `Uuid::new_v4()` |
| `x-codex-turn-state` | per-request | **Moka cache** keyed by `(key_id, ctx.conversation_id?)` → 64-char hex token；TTL 5 min；首次随机生成 |
| `OpenAI-Organization` / `OpenAI-Project` / `ChatGPT-Account-Id` | optional | 仅当 Vendor / VendorKey 配置中有显式值时发送 |

**rquest emulation**：`Emulation::Chrome131`（ChatGPT 后端 Web/Electron 客户端基于 Chromium）。

### 6.5 Profile 3：`GeminiCli`（基于 google-gemini/gemini-cli TS 实现）

| Header | 类 | 值 |
|---|---|---|
| `Content-Type` | static | `application/json` |
| UA 模板 | static | `GeminiCLI-{client_name}/{version}/{model} ({os_type}; {arch}; {surface})` |
| `client_name` | static | 空（标准 terminal） |
| `surface` | static | `unknown` |
| `User-Agent` | per-key | 按上模板组装；`<version>` 池：从 gemini-cli releases 拉 4-6 个；`<os_type>` 池：`linux` / `darwin` / `win32`；`<arch>` 池：`x64` / `arm64`；`<model>` 部分**按请求 IR 的 model 字段动态填**（per-request-dynamic 子字段，但不写入 snapshot） |
| `x-gemini-api-privileged-user-id` | per-key | `Uuid::new_v5(NAMESPACE_URL, "gemini:install:"+key_id)` |
| `X-Goog-User-Project` | optional | 从 `VendorKey.oauth` 的 `quota_project_id` 字段取值；如果 OAuth 结构当前不含此字段，由本设计在 `gw-core::entity::OAuthCreds` 上追加 `#[serde(default)] quota_project_id: Option<String>`（nullable，向后兼容） |
| `Authorization` / `x-goog-api-key` | existing | 由 `inject_vendor_headers` 的 `auth_template` 处理（已存在路径） |

**rquest emulation**：`Emulation::Chrome131`（Gemini CLI 实际是 Node + undici，与 Chrome 差异显著；选 Chrome131 是 "对 Google 后端通过率最高的保守选择"，§11 列为已知设计折衷）。

### 6.6 DB schema 迁移

```sql
-- migrations/NNN_egress_impersonation.sql
ALTER TABLE vendors      ADD COLUMN egress_mode       JSONB DEFAULT NULL;
ALTER TABLE vendor_keys  ADD COLUMN identity_snapshot JSONB DEFAULT NULL;
```

两列均 nullable + `#[serde(default)]` → 老数据零迁移。回滚（DROP COLUMN）也是兼容的。

### 6.7 Config（`config/default.toml` 新增段，全 optional）

```toml
[egress]
bind = "uds:///tmp/gw-egress.sock"   # 或 "tcp://127.0.0.1:0"（0 = ephemeral）
max_sessions = 64                    # rquest Client 池上限
idle_timeout_secs = 300              # Client LRU 闲置驱逐
disabled = false                     # 紧急 kill-switch；true 时所有 Impersonate vendor 退回 Rustls
turn_state_ttl_secs = 300            # Codex turn_state Moka cache TTL
```

`disabled = true` 热重载：30s 内 `pick_peer` 跳过 Impersonate 分支，并打 `gw_egress_disabled_total` 计数。

---

## 7. 运行时数据流

### 7.1 `pick_peer` 改动

```rust
// crates/gw-proxy/src/filters/upstream.rs::pick_peer
// （现有的 host / port / scheme_tls 解析之后追加）

let peer = match vendor.value.egress_mode {
    VendorEgressMode::Rustls => {
        HttpPeer::new(format!("{host}:{port}"), scheme_tls, host.clone())   // 现状不变
    }
    VendorEgressMode::Impersonate { profile } => {
        // tunnel + impersonate 互斥校验
        if matches!(resolution, TunnelResolution::Use(_)) {
            return emit_envelope_and_bail(session, ctx,
                envelope_for(codes::GW_TUNNEL_IMPERSONATE_CONFLICT,
                    "vendor cannot enable both tunnel and impersonate", 503)).await;
        }
        ctx.egress_profile = Some(profile);
        ctx.egress_upstream = Some((host.clone(), port));
        match egress.bind_addr() {
            EgressBind::Uds(path) => HttpPeer::new_uds(path, false, host.clone()),
            EgressBind::Tcp(addr) => HttpPeer::new(addr.to_string(), false, host.clone()),
        }
    }
};
```

### 7.2 `inject_vendor_headers` 追加

```rust
// crates/gw-proxy/src/filters/upstream.rs::inject_vendor_headers
// （现有 authorization / host / traceparent / content-length 注入之后）

if let Some(profile) = ctx.egress_profile {
    let (target_host, target_port) = ctx.egress_upstream.as_ref().unwrap();
    let _ = upstream_request.insert_header("X-Egress-Target",
        format!("{target_host}:{target_port}"));
    let _ = upstream_request.insert_header("X-Egress-Profile",
        profile.as_static_str());
    if let Some(proxy) = picked.proxy.as_ref() {
        let _ = upstream_request.insert_header("X-Egress-Proxy", proxy.as_str());
    }

    // 派生 + persist 身份；命中 snapshot 走零拷贝
    let identity = ensure_identity(picked.as_ref(), profile);
    for (name, value) in identity.headers(ctx.is_stream, ctx.conversation_id.as_deref()) {
        let _ = upstream_request.insert_header(name, value);
    }
}
```

### 7.3 Loopback wire 协议

- Transport：UDS（默认）/ TCP 127.0.0.1（fallback），明文 HTTP/1.1，启用 `Connection: keep-alive`（Pingora 默认行为）；
- Gateway → Egress 内部头（egress 立即 strip，不发往上游）：
  - `X-Egress-Target: <host>:<port>` — egress dial 的真实上游；
  - `X-Egress-Profile: <profile_id>` — 选 rquest Client；
  - `X-Egress-Proxy: <proxy_url>?` — 可选 per-key proxy；
- 其他所有头（含 `Authorization` + 派生 identity 头）原样透传，**egress 不修改、不重写、不日志**。

### 7.4 Egress 处理（伪代码）

```rust
// crates/gw-egress-impersonate/src/server.rs
async fn handle(req: HyperRequest<Body>) -> HyperResponse<Body> {
    let target = require_header(&req, "X-Egress-Target")?;     // 缺失 → 400 + class=bad_request
    let profile = require_header(&req, "X-Egress-Profile")?.parse()?;
    let proxy   = req.headers().get("X-Egress-Proxy").and_then(parse_url);

    let client = pool.get_or_create(profile, proxy);            // (profile, proxy) → Arc<Client>

    // 重建上游 URL：scheme=https + target + 原 path/query
    let url = format!("https://{}{}", target,
        req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/"));

    // 透传 method + headers（strip 内部头 + Host / Content-Length 让 rquest 重算）
    let mut rreq = client.request(req.method().clone(), url);
    for (k, v) in req.headers() {
        if !is_internal_or_hop_by_hop(k) { rreq = rreq.header(k, v); }
    }
    // 请求体：hyper Body → Stream<Bytes> → rquest Body
    let rresp = rreq
        .body(rquest::Body::wrap_stream(req.into_body().into_data_stream()))
        .send().await
        .map_err(map_rquest_error)?;     // map → X-Egress-Error-Class response

    // 响应体：rquest bytes_stream → hyper Body
    let mut resp = HyperResponse::builder()
        .status(rresp.status());
    for (k, v) in rresp.headers() {
        if !is_hop_by_hop(k) { resp = resp.header(k, v); }
    }
    resp.body(hyper::Body::wrap_stream(rresp.bytes_stream())).unwrap()
}
```

### 7.5 ClientPool 行为

- Key：`(ImpersonateProfile, Option<Url>)`；URL 归一化（lowercase scheme/host、strip trailing `/`、port 规范化）后再作为 key；
- 容量：`GW_EGRESS_MAX_SESSIONS`（默认 64）；超过 LRU 驱逐；
- 每个 Client 自带连接池（rquest）：到上游的 h2 多路复用连接，跨请求复用；
- 闲置驱逐：`idle_timeout_secs`（默认 300s）；
- 驱逐时 spawn `client.shutdown()` graceful；
- metrics：`gw_egress_client_pool_total{event=hit|miss|evicted}`。

### 7.6 流式与背压

请求体：
```
Pingora.read_request_body() → h1 chunked over loopback → egress.body_stream
  → rquest::Body::wrap_stream → h2 DATA frames over BoringSSL TLS → 上游
```

响应体（SSE 主战场）：
```
上游 h2 DATA → rquest.bytes_stream() → h1 chunked over loopback
  → Pingora.write_response_body() → 客户端
```

每段都是 `Stream<Item=Bytes>`；下游 `poll_next` 才向上游要数据，无中间缓冲。rquest h2 window 由上游驱动，loopback h1 由 Pingora 写速率驱动，反压链完整闭合。

### 7.7 性能预算（量化）

| 项 | 单请求增量 | 备注 |
|---|---|---|
| Loopback 跳（UDS） | +20–50 µs RTT | UDS 是 in-kernel mem-copy，无 checksum/CRC |
| Loopback 跳（TCP 127.0.0.1） | +100–300 µs RTT | 全 TCP stack，但 kernel shortcircuit |
| 额外 h1 parse+serialize（双向 各一次） | +5–15 µs | hyper h1 SIMD-加速 |
| Header 派生（命中 snapshot） | <1 µs | HashMap 拷贝 |
| Header 派生（snapshot miss + 派生） | +10–20 µs，**仅首次** | SHA-256 + 几次 pool pick |
| Body 多一次 zero-copy 转手 | 0（`Bytes::clone` = Arc bump） | |

总额外：非流请求 ~50 µs；流请求每 chunk ~20 µs。对 LLM first-token 200ms-2s、整请求秒级延迟，**百分位看不见**。CPU 主要由 TLS 握手占用——这是必付成本，与栈选择无关。

性能 SLO：`gw_egress_loopback_rtt_seconds` p99 < 200 µs（UDS）/ < 500 µs（TCP）。CI 跑 microbench 卡线。

### 7.8 关闭路径

`gw-gateway::main` shutdown 序列：
1. 停 Pingora accept（不收新请求）；
2. 等所有 inflight 排空（按现有 grace timeout）；
3. 调 `EgressHandle::shutdown(drain_timeout=15s)`；
4. egress 关 listener；通知所有 rquest Client `idle_drain_timeout`；
5. 进程退出。

---

## 8. 错误处理 / 重试 / 可观测性

### 8.1 Egress → Pingora 错误通道

Egress 永远以 HTTP 响应回给 Pingora（不在 loopback 层关连接），分类信息通过两个内部头携带：

```
X-Egress-Error-Class:  <class>          # 机器读，枚举
X-Egress-Error-Detail: <truncated msg>  # 256B 上限，纯诊断
```

### 8.2 错误分类映射表

| rquest 错误 | class | egress 返回 status | gw error code | retryable | 等同 native 行为 |
|---|---|---|---|---|---|
| TLS handshake / cert | `tls` | 502 | `GW_UPSTREAM_TLS_ERROR` | ❌ 短路 | rustls TLS 错（`gateway.rs:842`） |
| connect timeout | `connect_timeout` | 504 | `GW_UPSTREAM_CONNECT_TIMEOUT` | ✅ blacklist + 换 key | `fail_to_connect` 路径 |
| connection refused / DNS | `connect_refused` | 502 | `GW_UPSTREAM_CONNECT_REFUSED` | ✅ | 同上 |
| read timeout | `read_timeout` | 504 | `GW_UPSTREAM_READ_TIMEOUT` | ✅（非流）/ ❌（流已发数据） | Pingora read_timeout |
| h2 protocol error | `h2_protocol` | 502 | `GW_UPSTREAM_H2_PROTOCOL` | ✅ | |
| body stream broken（流中途断） | `body_truncated` | — | `GW_UPSTREAM_BODY_TRUNCATED` | ❌（流不可重试） | |
| proxy error（per-key proxy 故障） | `proxy_error` | 502 | `GW_UPSTREAM_PROXY_ERROR` | ✅ | |
| panic（不该有） | `panic` | 502 | `GW_UPSTREAM_PANIC` | ❌ | |
| 其他 | `unknown` | 502 | `GW_UPSTREAM_UNKNOWN` | ❌ | |

### 8.3 Pingora 侧解读（`crates/gw-proxy/src/filters/response.rs`）

```rust
if let Some(class) = upstream_response.headers.get(HDR_CLASS) {
    let (code, retryable, kind) = map_egress_class(class);
    ctx.upstream_error_code = Some(code);
    if kind == ConnectErrorKind::Tls {
        metrics::counter!("gw_retry_attempt_total", "outcome" => "tls_blocked").increment(1);
        return Err(short_circuit(502));
    }
    if retryable {
        ctx.retry_blacklist.push(picked.key_id.clone());
        return Err(retry_signal());
    }
    // 不可重试：X-Egress-Error-Detail 喂给 error_emit
}
strip_internal_headers(upstream_response);   // X-Egress-* 全 strip
```

### 8.4 现有 listener 兼容性

`crates/gw-proxy/src/error/listeners/` 全部按 `ctx.upstream_status` + `upstream_error_code` 驱动，对 egress path **透明**：

| Listener | 触发 | egress path 表现 |
|---|---|---|
| `oauth` | 401 + OAuth key | 上游 401 原样转发 → 监听器触发 token refresh；下次请求重新 `inject_vendor_headers` 已支持（行 870-893） |
| `key_health` | 重试次数 / 错误类型 | `retry_blacklist` + `upstream_error_code` 直接生效 |
| `rate_limit` | 429 + Retry-After | 上游 429 原样转发 → 监听器照常读 Retry-After |
| `ledger` (inflight) | RAII guard | `InFlightGuard` 是 ctx Drop，路径无关 |
| `metrics` | 各 phase counter | egress path 多发 `gw_egress_*`，老 counter 不变 |
| `cache_invalidate` | 5xx | 透明 |
| `thinking` | response body | 透明 |

### 8.5 Egress 自身的故障

| 故障 | 处理 |
|---|---|
| Listener 启动失败（UDS 冲突等） | `EgressHandle::start()` 返回 Err，gw-gateway main fast fail |
| Accept loop panic | supervisor 捕获 + log critical + counter + 指数退避重启（≤5/min） |
| rquest 任意 panic | handle 套 `tokio::task::JoinHandle::catch_unwind` → 返回 `502 X-Egress-Error-Class: panic` |
| Shutdown 时 inflight 未完 | `shutdown(drain_timeout=15s)`；超时强关，inflight 收到 `body_truncated` |
| 紧急止血 | `[egress].disabled = true` 热重载，30s 生效 |

### 8.6 观测指标

```
gw_egress_requests_total{profile, result}            counter   # success/failure
gw_egress_error_total{profile, class}                counter
gw_egress_client_pool_total{event}                   counter   # hit/miss/evicted
gw_egress_handshake_seconds{profile}                 histogram # rquest 首次握手
gw_egress_loopback_rtt_seconds                       histogram # SLO 指标
gw_egress_lazy_persist_total{result}                 counter   # ok/failed/deferred/dropped
gw_egress_identity_cache_total{kind, event}          counter   # turn_state hit/miss
gw_egress_disabled_total                             counter   # kill-switch
gw_egress_supervisor_restart_total{reason}           counter
```

### 8.7 Detailed-log 脱敏

`crates/gw-proxy/src/detailed_log.rs` strip 列表加：
- `X-Egress-Target` / `X-Egress-Profile` / `X-Egress-Proxy`（内部信号，不入审计）
- `X-Egress-Error-Class` / `X-Egress-Error-Detail`（同上）
- `Authorization` 现有脱敏不变（loopback 跳前后 header 值是同一个）

---

## 9. 测试 / 构建 / 上线

### 9.1 测试矩阵

| 层 | 覆盖 | 位置 |
|---|---|---|
| **单元** `gw-core::impersonate` | `derive` 确定性（同 key_id 必同结果）；不同 key_id 必异；三 profile 池命中分布；`IdentityVariable` serde roundtrip；profile mismatch 触发 re-derive | `cargo test -p gw-core` |
| **单元** `gw-egress-impersonate` | rquest ClientPool LRU + 键归一化（proxy URL 大小写/末尾 `/` 不分裂池）；错误映射表逐项（每个 rquest 错误变体 → 期望 class）；UDS 路径冲突清理；supervisor 重启计数 | `cargo test -p gw-egress-impersonate` |
| **集成** loopback wire | 模拟 Pingora 发 h1 + `X-Egress-*` → egress；断言上游收到的 headers 不含 `X-Egress-*`；断言失败场景出现 `X-Egress-Error-Class` | `cargo test -p gw-egress-impersonate --test loopback` |
| **E2E** 全链路 | 复用 `crates/e2e-fake-upstream`：Pingora → egress → fake upstream；断言到达 upstream 的 headers 含 forged identity；断言同 VendorKey 多次请求 identity 字节一致；断言 401/429/5xx 触发 listener 联动 | `tests/e2e_impersonate.rs` |
| **指纹验证**（"真的伪装生效了吗"） | 跑 JA3/JA4 echo 服务（自托管 mitm-fingerprint 等）；为每个 profile 断言 rquest 输出的 JA4 hash 与对应真实客户端一致 | `tests/fingerprint_verify.rs`，feature `fingerprint-verify` 启用，CI nightly |
| **真实客户端头校准**（防 spec 漂移） | `scripts/capture_real_client_headers/{claude-code,codex,gemini-cli}/`：跑真客户端打 mock recorder，JSON-diff spec 静态头；CI 周跑 + release gate | `.github/workflows/header-drift.yml` |
| **回归基线** | 所有 vendor `egress_mode=Rustls` 时跑现有 E2E suite；逐请求 diff `ctx.upstream_request_meta` 与 改动前 golden；任意字节差异 fail | `tests/regression_rustls_unchanged.rs` |
| **性能微基准** | UDS / TCP loopback 各 1k req/s × 50k；断言 `loopback_rtt_seconds` p99 < 200µs (UDS) / < 500µs (TCP)；egress vs rustls 路径 fake upstream TPS 降幅 < 5% | `cargo bench -p gw-egress-impersonate` |

### 9.2 构建依赖

```dockerfile
# Dockerfile —— cargo build 之前
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake perl pkg-config libclang-dev build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
```

- Base layer 体积 +~150 MB；冷构建时间 +~2 min（boring-sys 编 BoringSSL）；BuildKit cache 命中无感。
- workspace `Cargo.toml` 新增成员 `crates/gw-egress-impersonate`；`rquest` 按"实现时 crates.io 当前 stable"锁定（Cargo.lock 提交），`boring-sys` 间接依赖。版本升级走 §11 #4 评估流程。
- musl 目标：rquest 要 `musl-tools`，release pipeline 跑 musl 时 Dockerfile 同步加。
- Feature `egress-disabled`（默认 off）：紧急 hotfix 时启用 → `gw-egress-impersonate` 退化为空实现，Impersonate 分支 fail-fast。

### 9.3 灰度上线

**Stage 0 — 部署不启用**

- DB 迁移先跑（两列 nullable，无破坏性）；
- 新 binary 部署；所有 vendor `egress_mode` 保持空 → 默认 Rustls → 行为字节级不变；
- 观察 24h：`gw_egress_loopback_rtt_seconds` count = 0；`gw_egress_supervisor_restart_total` = 0。

**Stage 1 — 单 vendor 试点**

- admin 把一个**非关键** test vendor 切到 `Impersonate{ClaudeCodeChrome131}`；
- 用一组 test key 跑 100 个真实请求；
- 通过条件：
  - `gw_egress_requests_total{result=success}` ≥ 99%；
  - `gw_egress_error_total` 无 `tls` / `h2_protocol` 集中爆发；
  - 目标 vendor 上游不返封号信号（403 / `account_disabled`）。

**Stage 2 — 按 profile 推全**

- 顺序：Anthropic → OpenAI → Gemini，间隔 ≥ 48h；
- 每步前后对比对应 vendor 的 5xx ratio + key cooldown 速率，无显著恶化才推进。

**回滚**

- 任一步触发：
  - (a) `[egress].disabled = true` 配置热重载 → 30s 内全部 Impersonate vendor 退回 Rustls（不改 DB）；
  - (b) 单 vendor 撤回（admin 改 `egress_mode` 字段），其他不受影响；
  - (c) Binary 回滚：上一版 binary 不识别新字段也无副作用（serde tolerant + 列 nullable）。

### 9.4 文档更新

- `CLAUDE.md`：source layout 节加 `crates/gw-egress-impersonate`；build pitfalls 节追加 boring-sys 系统依赖；
- `docs/architecture.md`：upstream connection 段加 egress 分支图 + 三 profile 简介；
- `docs/workflow.md`：`upstream_peer` / `upstream_request_filter` 段补充 Impersonate 分支；
- `docs/issues/egress-impersonate-known-gaps.md`：本 spec §11 的未决问题搬过去做运行手册。

---

## 10. 安全考虑

- **Loopback 绑定**：UDS 缺省，文件权限 `0700` + 仅 service user owner；TCP fallback 时硬绑 127.0.0.1，启动时主动检测 `0.0.0.0` 绑定立即 fast fail；
- **明文跳的 Authorization**：loopback 不出本机，安全模型与"同进程访问内存"等价；脱敏在 `detailed_log` 层处理，确保不漏；
- **rquest panic 隔离**：`catch_unwind` 防止单请求 panic 拖垮 listener task；
- **profile 升级**：每个 profile 的 emulation preset / pool 常量是代码改动 + Stage 1-2 灰度走完整流程，不通过 DB 热改（避免运行时 profile 与代码常量不同步）；
- **identity_snapshot 注入**：admin 端 update vendor_keys.identity_snapshot 要做 schema validation（profile_kind 必须 match Vendor.egress_mode）；
- **kill-switch 信任路径**：`[egress].disabled` 配置文件来源走标准 figment 加载，与现有 admin 权限等同。

---

## 11. 未决问题 / 已知设计折衷

| # | 问题 | 影响 | 处理 |
|---|------|------|------|
| 1 | OpenAI Codex `/v1/responses` 的真实 `OpenAI-Beta` 值（spec 暂用 `responses=v1`），需对 codex-rs 当前 source 校准 | Codex profile 静态头准确性 | Phase 1 开始前 grep codex-rs source 落定 |
| 2 | Codex `x-codex-turn-state` Moka cache TTL 5 min 是凭经验估，需对真实 Codex CLI 行为校准 | turn 粘性精度 | Stage 1 试点时抓真流量对比 |
| 3 | Gemini CLI 实际 fingerprint 是 Node + undici，与 Chrome 差异大；我们选 Chrome131 是"对 Google 通过率最高"折衷 | Gemini 伪装可能过度 / 不精确 | Stage 1 监控 Google 后端封号率；如果差，新增 `GeminiNodeUndici` profile |
| 4 | rquest 版本升级策略：跟 latest（profile 池常量随浏览器版本动） vs 固定（稳定但易漂移） | 长期维护成本 | 首版固定，季度评估；header-drift CI 触发评估 |
| 5 | CLI 客户端版本池（Claude Code / Codex / Gemini）刷新机制：手动 vs 周自动 PR | 池常量陈旧 → 伪装暴露 | 初版手动，配 `.github/workflows/header-drift.yml` 提醒；后续考虑自动 PR |
| 6 | 性能 SLO `loopback_rtt_seconds` p99 超线（>200µs UDS）时的升级路径 | 性能回归 | 初版仅告警；持续超线触发 issue 评估（缓存 ClientPool 重构 / hyper 调优 / 是否回到 Approach A） |
| 7 | `identity_snapshot` 列被运营手改后是否仍触发 lazy persist（spec 当前规则：列 IS NULL 时才写） | 手改保留 vs 一致性 | 规则为"列存在则信任"，admin UI 暴露 "regenerate" 显式覆盖 |
| 8 | tunnel + impersonate 互斥校验目前在 `pick_peer` 运行时；是否上提到 admin update 时校验 | UX 一致性 | Phase 后补，配 `crates/gw-store` 的 vendor update 路径 |
