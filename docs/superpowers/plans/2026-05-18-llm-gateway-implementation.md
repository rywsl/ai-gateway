# LLM 网关实施计划（v2026-05-18）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 spec `2026-05-18-llm-gateway-design.md` 中 ADR-018 的 9-crate DAG，实现高性能 Rust LLM 网关，覆盖协议归一化、号池路由、JWT/APIKey 鉴权、估算冻结计费、多级限流、双级日志与 Debug 模式。

**Architecture:** Pingora 单进程 + 9 个 crate 的 cargo workspace；`gw-core`（类型层）→ `gw-store`（缓存/IO）→ `{gw-codec, gw-auth, gw-routing, gw-ledger}`（业务层，可并行）→ `gw-proxy`（编排层）→ `gw-gateway`（bin）。`gw-codec-macros` 是 proc-macro crate，仅被 `gw-codec` 使用。

**Tech Stack:** Rust 1.78+ / pingora 0.8 / fred / sqlx + PgSQL / moka / sonic-rs / governor / tikv-jemallocator / aes-gcm / tracing。

**测试策略：** 不采用严格 TDD —— 每个 crate **先实现核心代码，再补单元测试 + 集成测试**。集成测试以 `crates/<name>/tests/*.rs` 形式放在各 crate 的 `tests/` 目录。涉及 Redis / PgSQL 的测试使用 `testcontainers-rs` 启动临时实例。

**并行策略：** 按 Phase 划分，同 Phase 内任务可由不同 agent 并行执行；跨 Phase 严格遵循 DAG 依赖。

---

## 阶段总览

| Phase | 任务 | 是否可并行 |
|---|---|---|
| 0 | Workspace 骨架 + 公共配置 | 串行（前置） |
| 1 | `gw-core` 类型层 | 串行（所有 crate 依赖） |
| 2 | `gw-codec-macros` + `gw-codec` ＿＿｜＿＿ `gw-store` | **可并行**（2 路） |
| 3 | `gw-auth` ＿｜＿ `gw-routing` ＿｜＿ `gw-ledger` | **可并行**（3 路） |
| 4 | `gw-proxy` 编排 + filter | 串行 |
| 5 | `gw-gateway` bin 装配 | 串行 |
| 6 | 端到端集成测试 + 性能基准 | 串行 |

---

## Phase 0：Workspace 骨架

### Task 0.1：创建 cargo workspace

**Files:**
- Create: `Cargo.toml`（workspace 根）
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `crates/.gitkeep`

- [ ] **Step 1：创建 workspace 根 Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "crates/gw-core",
    "crates/gw-codec-macros",
    "crates/gw-codec",
    "crates/gw-store",
    "crates/gw-auth",
    "crates/gw-routing",
    "crates/gw-ledger",
    "crates/gw-proxy",
    "crates/gw-gateway",
]

[workspace.package]
edition = "2021"
rust-version = "1.78"
license = "Apache-2.0"

[workspace.dependencies]
# 基础类型
bytes = "1.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sonic-rs = "0.3"
thiserror = "1"
anyhow = "1"
secrecy = { version = "0.10", features = ["serde"] }
time = { version = "0.3", features = ["serde", "macros"] }
url = "2.5"
smallvec = { version = "1.13", features = ["serde"] }

# 异步
tokio = { version = "1.40", features = ["full"] }
async-trait = "0.1"
futures = "0.3"

# proc-macro
proc-macro2 = "1"
syn = { version = "2", features = ["full"] }
quote = "1"

# 缓存 / 并发
moka = { version = "0.12", features = ["future"] }
dashmap = "6"
parking_lot = "0.12"
arc-swap = "1"

# 加密
aes-gcm = "0.10"
hmac = "0.12"
sha2 = "0.10"
subtle = "2.6"
rand = "0.8"

# 网关 / 网络
pingora = "0.8"
pingora-proxy = "0.8"
fred = { version = "9", features = ["dynamic-pool", "partial-tracing", "metrics", "subscriber-client"] }
sqlx = { version = "0.8", features = ["postgres", "runtime-tokio-rustls", "macros", "migrate"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
jsonwebtoken = "9"
governor = "0.7"

# 可观测
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
tracing-appender = "0.2"
metrics = "0.23"
metrics-exporter-prometheus = "0.15"

# 配置
figment = { version = "0.10", features = ["toml", "env"] }

# 内存
tikv-jemallocator = "0.6"
tikv-jemalloc-ctl = "0.6"

# 测试
testcontainers = "0.23"
testcontainers-modules = { version = "0.11", features = ["postgres", "redis"] }
wiremock = "0.6"
criterion = { version = "0.5", features = ["async_tokio"] }
proptest = "1"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
debug = "line-tables-only"

[profile.bench]
inherits = "release"
debug = true
```

- [ ] **Step 2：rust-toolchain.toml 锁定版本**

```toml
[toolchain]
channel = "1.82"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3：.gitignore 追加 Rust 标准条目**

```
target/
**/*.rs.bk
.env
.env.local
.sqlx/
```

- [ ] **Step 4：验证 workspace 解析**

Run: `cargo check --workspace`
Expected: 因为 crate 尚未创建，会报错 "could not find ... crates/gw-core" 等；此步仅校验 `Cargo.toml` 语法可解析（无 toml parse error）。

- [ ] **Step 5：提交**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore
git commit -m "chore: bootstrap cargo workspace with 9 crates and shared deps"
```

---

## Phase 1：`gw-core` 类型层

### Task 1.1：创建 crate + ID newtype + 基础 enum

**Files:**
- Create: `crates/gw-core/Cargo.toml`
- Create: `crates/gw-core/src/lib.rs`
- Create: `crates/gw-core/src/ids.rs`
- Create: `crates/gw-core/src/protocol.rs`
- Create: `crates/gw-core/src/error.rs`

- [ ] **Step 1：crates/gw-core/Cargo.toml**

```toml
[package]
name = "gw-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
bytes.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
secrecy.workspace = true
time.workspace = true
url.workspace = true
smallvec.workspace = true
```

- [ ] **Step 2：lib.rs 注册模块**

```rust
//! gw-core: 纯类型层（无 IO / 无 async）。所有上层 crate 共享的 IR / ErrorIR / Versioned / ID newtype 集中于此。

pub mod debug;
pub mod entity;
pub mod error;
pub mod ids;
pub mod ir;
pub mod permissions;
pub mod protocol;
pub mod traits;
pub mod versioned;
```

- [ ] **Step 3：ids.rs ID newtype**

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Clone, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        pub struct $name(pub String);
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}({})", stringify!($name), self.0) }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
        }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
        impl From<&str>  for $name { fn from(s: &str) -> Self  { Self(s.to_owned()) } }
        impl AsRef<str>  for $name { fn as_ref(&self) -> &str   { &self.0 } }
    };
}

id_newtype!(UserId);
id_newtype!(UserGroupId);
id_newtype!(ApiKeyId);
id_newtype!(VendorId);
id_newtype!(KeyId);
id_newtype!(ModelName);
id_newtype!(Group);
id_newtype!(Kid);
id_newtype!(TraceId);
id_newtype!(RequestId);
```

- [ ] **Step 4：protocol.rs ProtocolKind**

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind { OpenAi, Anthropic, Gemini }

impl ProtocolKind {
    pub fn as_str(&self) -> &'static str {
        match self { Self::OpenAi => "openai", Self::Anthropic => "anthropic", Self::Gemini => "gemini" }
    }
}
```

- [ ] **Step 5：error.rs ErrorIR + 稳定错误码（ADR-020）**

完整列出 23+ 个稳定错误码常量：

```rust
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ClientBadRequest, ClientAuth, ClientQuota,
    UpstreamError, UpstreamTimeout, UpstreamOverloaded,
    GatewayInternal, Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorIR<'a> {
    pub category: ErrorCategory,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub code: Cow<'a, str>,
    pub message: Cow<'a, str>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_raw: Option<&'a serde_json::value::RawValue>,
    pub request_id: &'a str,
    pub retry_after_secs: Option<u32>,
}

pub mod codes {
    pub const GW_AUTH_INVALID_JWT: &str = "GW_AUTH_INVALID_JWT";
    pub const GW_AUTH_INVALID_APIKEY: &str = "GW_AUTH_INVALID_APIKEY";
    pub const GW_AUTH_NO_KEY_PERMISSION: &str = "GW_AUTH_NO_KEY_PERMISSION";
    pub const GW_AUTH_IP_BLOCKED: &str = "GW_AUTH_IP_BLOCKED";
    pub const GW_AUTH_IP_NOT_ALLOWED: &str = "GW_AUTH_IP_NOT_ALLOWED";
    pub const GW_AUTH_SIGNATURE_INVALID: &str = "GW_AUTH_SIGNATURE_INVALID";
    pub const GW_QUOTA_BALANCE_ZERO: &str = "GW_QUOTA_BALANCE_ZERO";
    pub const GW_QUOTA_RPM_EXCEEDED: &str = "GW_QUOTA_RPM_EXCEEDED";
    pub const GW_QUOTA_TPM_EXCEEDED: &str = "GW_QUOTA_TPM_EXCEEDED";
    pub const GW_QUOTA_CONCURRENT_EXCEEDED: &str = "GW_QUOTA_CONCURRENT_EXCEEDED";
    pub const GW_REQUEST_PROTOCOL_UNKNOWN: &str = "GW_REQUEST_PROTOCOL_UNKNOWN";
    pub const GW_REQUEST_INPUT_TOO_LARGE: &str = "GW_REQUEST_INPUT_TOO_LARGE";
    pub const GW_REQUEST_COST_TOO_HIGH: &str = "GW_REQUEST_COST_TOO_HIGH";
    pub const GW_REQUEST_CONTENT_REJECTED: &str = "GW_REQUEST_CONTENT_REJECTED";
    pub const GW_REQUEST_MODEL_NOT_FOUND: &str = "GW_REQUEST_MODEL_NOT_FOUND";
    pub const GW_ROUTING_NO_AVAILABLE_CHANNEL: &str = "GW_ROUTING_NO_AVAILABLE_CHANNEL";
    pub const GW_UPSTREAM_TIMEOUT: &str = "GW_UPSTREAM_TIMEOUT";
    pub const GW_UPSTREAM_5XX: &str = "GW_UPSTREAM_5XX";
    pub const GW_UPSTREAM_4XX: &str = "GW_UPSTREAM_4XX";
    pub const GW_UPSTREAM_OVERLOADED: &str = "GW_UPSTREAM_OVERLOADED";
    pub const GW_UPSTREAM_CANCELLED: &str = "GW_UPSTREAM_CANCELLED";
    pub const GW_BACKEND_UNAVAILABLE: &str = "GW_BACKEND_UNAVAILABLE";
    pub const GW_GATEWAY_INTERNAL: &str = "GW_GATEWAY_INTERNAL";
}
```

- [ ] **Step 6：编译通过**

Run: `cargo check -p gw-core`
Expected: 编译通过，无 warning。

- [ ] **Step 7：提交**

```bash
git add crates/gw-core
git commit -m "feat(gw-core): scaffold crate with ID newtypes, protocol, error codes"
```

---

### Task 1.2：IR 类型（RequestIR / ResponseIR / StreamEventIR / MessageIR）

**Files:**
- Create: `crates/gw-core/src/ir.rs`

- [ ] **Step 1：完整 IR 定义（按 ADR-019）**

```rust
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;
use crate::error::ErrorIR;

#[derive(Clone, Debug, Default)]
pub struct UntouchedFlags(pub u32);
impl UntouchedFlags { pub const ALL_UNTOUCHED: Self = Self(u32::MAX); }

#[derive(Clone, Debug)]
pub struct RequestIR<'a> {
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub stop: Option<Vec<Cow<'a, str>>>,
    pub seed: Option<u64>,
    pub tools: Option<Vec<ToolIR<'a>>>,
    pub tool_choice: Option<ToolChoiceIR<'a>>,
    pub response_format: Option<ResponseFormatIR>,
    pub cache_control: Option<CacheControlIR>,
    pub metadata: MetadataIR<'a>,
    pub extensions: Option<HashMap<Cow<'a, str>, Box<RawValue>>>,
    pub untouched: UntouchedFlags,
}

#[derive(Clone, Debug)]
pub enum MessageIR<'a> {
    System    { content: ContentIR<'a> },
    User      { content: ContentIR<'a> },
    Assistant { content: ContentIR<'a>, tool_calls: Option<Vec<ToolCallIR<'a>>> },
    Tool      { tool_call_id: Cow<'a, str>, content: ContentIR<'a> },
}

#[derive(Clone, Debug)]
pub enum ContentIR<'a> {
    Text(Cow<'a, str>),
    Parts(SmallVec<[ContentPart<'a>; 4]>),
}

#[derive(Clone, Debug)]
pub enum ContentPart<'a> {
    Text(Cow<'a, str>),
    Image { source: ImageSource<'a> },
    Audio { source: MediaSource<'a> },
    File  { source: MediaSource<'a> },
}

#[derive(Clone, Debug)]
pub enum ImageSource<'a> {
    Url(Cow<'a, str>),
    B64 { media_type: Cow<'a, str>, data: Cow<'a, str> },
}

#[derive(Clone, Debug)]
pub enum MediaSource<'a> {
    Url(Cow<'a, str>),
    B64 { media_type: Cow<'a, str>, data: Cow<'a, str> },
}

#[derive(Clone, Debug)]
pub struct ToolIR<'a> {
    pub name: Cow<'a, str>,
    pub description: Option<Cow<'a, str>>,
    pub parameters_jsonschema: Cow<'a, RawValue>,
}

#[derive(Clone, Debug)]
pub struct ToolCallIR<'a> {
    pub id: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub arguments_json: Cow<'a, RawValue>,
}

#[derive(Clone, Debug)]
pub enum ToolChoiceIR<'a> {
    Auto, None, Required,
    Function { name: Cow<'a, str> },
}

#[derive(Clone, Debug)]
pub enum ResponseFormatIR { Text, JsonObject, JsonSchema(Box<RawValue>) }

#[derive(Clone, Debug)]
pub struct CacheControlIR { pub kind: Cow<'static, str> }

#[derive(Clone, Debug, Default)]
pub struct MetadataIR<'a> {
    pub openai_logit_bias: Option<HashMap<Cow<'a, str>, f32>>,
    pub openai_logprobs: Option<bool>,
    pub anthropic_thinking: Option<AnthropicThinking<'a>>,
    pub gemini_safety_settings: Option<Vec<GeminiSafetySetting<'a>>>,
}

#[derive(Clone, Debug)]
pub struct AnthropicThinking<'a> { pub kind: Cow<'a, str>, pub budget_tokens: Option<u32> }

#[derive(Clone, Debug)]
pub struct GeminiSafetySetting<'a> { pub category: Cow<'a, str>, pub threshold: Cow<'a, str> }

#[derive(Clone, Debug)]
pub struct ResponseIR<'a> {
    pub id: Cow<'a, str>,
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub finish_reason: Option<FinishReasonIR>,
    pub usage: Option<UsageIR>,
    pub metadata: MetadataIR<'a>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReasonIR { Stop, Length, ToolCalls, ContentFilter, Other }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UsageIR {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum StreamEventIR<'a> {
    Start { request_id: Cow<'a, str> },
    Delta { text: Option<Cow<'a, str>>, tool_call: Option<ToolCallIR<'a>>, thinking: Option<Cow<'a, str>> },
    Usage(UsageIR),
    End   { finish_reason: FinishReasonIR },
    Error(ErrorIR<'a>),
}

/// 收集业务层对 IR 的改动（max_tokens cap、forced_params 等），用于同协议透传判定空 mutation。
#[derive(Clone, Debug, Default)]
pub struct Mutations { pub fields: u32 }
impl Mutations {
    pub const NONE: Self = Self { fields: 0 };
    pub fn is_empty(&self) -> bool { self.fields == 0 }
    pub fn set_max_tokens(&mut self) { self.fields |= 1 << 0; }
    pub fn set_temperature(&mut self) { self.fields |= 1 << 1; }
    pub fn set_metadata(&mut self) { self.fields |= 1 << 2; }
    pub fn set_tools(&mut self) { self.fields |= 1 << 3; }
}
```

- [ ] **Step 2：编译通过**

Run: `cargo check -p gw-core`
Expected: 无错误。

- [ ] **Step 3：提交**

```bash
git add crates/gw-core/src/ir.rs
git commit -m "feat(gw-core): add RequestIR/ResponseIR/StreamEventIR types"
```

---

### Task 1.3：Versioned / KeyStatus / 业务实体 / trait 扩展点

**Files:**
- Create: `crates/gw-core/src/versioned.rs`
- Create: `crates/gw-core/src/entity.rs`
- Create: `crates/gw-core/src/permissions.rs`
- Create: `crates/gw-core/src/traits.rs`
- Create: `crates/gw-core/src/debug.rs`

- [ ] **Step 1：versioned.rs**

```rust
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Versioned<T> {
    pub version: u64,
    pub value: Arc<T>,
}

impl<T> Versioned<T> {
    pub fn new(version: u64, value: T) -> Self { Self { version, value: Arc::new(value) } }
}

/// 严格大于才覆盖（ADR-006）。
pub fn upsert_if_newer<T>(current: &mut Option<Versioned<T>>, candidate: Versioned<T>) -> bool {
    match current {
        Some(c) if c.version >= candidate.version => false,
        _ => { *current = Some(candidate); true }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding { Json, Msgpack }
```

- [ ] **Step 2：entity.rs（UserEntry / UserGroup / ApiKeyEntry / Vendor / VendorKey / ModelGroup / Pricing / JwksEntry / IpBlockEntry / KeyStatus 7 状态机）**

```rust
use crate::ids::*;
use crate::protocol::ProtocolKind;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::OffsetDateTime;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus { Active, Disabled, Expired, Depleted, RateLimited, Rotated, Deleted }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus { Active, Banned }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyStatus { Active, Disabled }

#[derive(Clone, Debug, Deserialize)]
pub struct UserEntry {
    pub user_id: UserId,
    pub parent_id: Option<UserId>,
    pub tier: String,
    pub status: UserStatus,
    #[serde(default)] pub allowed_models: Vec<ModelName>,
    #[serde(default)] pub allowed_channels: Vec<KeyId>,
    #[serde(default)] pub allowed_groups: Vec<Group>,
    #[serde(default)] pub rpm_limit: u32,
    #[serde(default)] pub tpm_limit: u32,
    #[serde(default = "default_concurrent")] pub max_concurrent_streams: u32,
    #[serde(default)] pub daily_quota_tokens: u64,
    #[serde(default)] pub user_group_ids: Vec<UserGroupId>,
    #[serde(default = "default_markup")] pub markup: f32,
    #[serde(default)] pub debug_enabled: bool,
}
fn default_concurrent() -> u32 { 100 }
fn default_markup() -> f32 { 1.0 }

#[derive(Clone, Debug, Deserialize)]
pub struct UserGroup {
    pub id: UserGroupId,
    pub display_name: String,
    #[serde(default)] pub priority: i32,
    #[serde(default)] pub allowed_models: Vec<ModelName>,
    #[serde(default)] pub allowed_channels: Vec<KeyId>,
    #[serde(default)] pub model_group_binding: Option<Group>,
    #[serde(default)] pub rpm_limit: u32,
    #[serde(default)] pub tpm_limit: u32,
    #[serde(default)] pub max_concurrent_streams: u32,
    #[serde(default = "default_markup")] pub markup: f32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiKeyEntry {
    pub key_id: ApiKeyId,
    pub user_id: UserId,
    pub status: ApiKeyStatus,
    pub hash: Vec<u8>,
    #[serde(default)] pub allowed_models: Vec<ModelName>,
    #[serde(default)] pub allowed_channels: Vec<KeyId>,
    #[serde(default)] pub rpm_limit: u32,
    #[serde(default)] pub tpm_limit: u32,
    #[serde(default = "default_markup")] pub markup: f32,
    #[serde(default)] pub ip_allowlist: Option<Vec<String>>,
    #[serde(default)] pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Vendor {
    pub id: VendorId,
    pub display_name: String,
    pub base_urls: Vec<Url>,
    pub auth_template: String,
    pub timeout_overrides: Option<TimeoutSpec>,
    #[serde(default)] pub default_proxy: Option<Url>,
    #[serde(default)] pub contact: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimeoutSpec {
    pub connect_ms: Option<u32>,
    pub read_ms: Option<u32>,
    pub total_ms: Option<u32>,
    pub stream_total_ms: Option<u32>,
}

/// Vendor Key —— api_key 是解密后的 SecretString
pub struct VendorKey {
    pub key_id: KeyId,
    pub vendor_id: VendorId,
    pub api_key: SecretString,
    pub accessible_models: Vec<ModelName>,
    pub model_mapping: HashMap<ModelName, String>,
    pub accessible_endpoints: Vec<ProtocolKind>,
    pub protocol_conversion: bool,
    pub priority: i32,
    pub weight: u32,
    pub rpm_limit: u32,
    pub tpm_limit: u32,
    pub max_usage_count: Option<u64>,
    pub proxy: Option<Url>,
    pub ip_allowlist: Option<Vec<String>>,
    pub expires_at: Option<OffsetDateTime>,
    pub status: KeyStatus,
    pub tags: Vec<String>,
}

impl std::fmt::Debug for VendorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VendorKey")
            .field("key_id", &self.key_id)
            .field("vendor_id", &self.vendor_id)
            .field("api_key", &"[REDACTED]")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyKind {
    CacheAffinity, FixedOrder, RoundRobin, LoadBalance,
    CostFirst, LatencyFirst, WeightedRr, Sticky,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelGroup {
    pub group: Group,
    pub strategy: StrategyKind,
    #[serde(default)] pub cross_vendor_fallback: bool,
    pub max_tokens_cap: Option<u32>,
    #[serde(default)] pub default_params: DefaultParams,
    #[serde(default)] pub forced_params: ForcedParams,
    pub vendor_key_ids: Vec<KeyId>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DefaultParams { pub temperature: Option<f32>, pub top_p: Option<f32> }

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ForcedParams { pub temperature: Option<f32>, pub top_p: Option<f32> }

#[derive(Clone, Debug, Deserialize)]
pub struct Pricing {
    pub model: ModelName,
    pub input_price_micro_per_token: u64,
    pub output_price_micro_per_token: u64,
    pub max_output: u32,
    pub model_markup: f32,
    pub connect_timeout_ms: Option<u32>,
    pub read_timeout_ms: Option<u32>,
    pub total_timeout_ms: Option<u32>,
    pub stream_total_timeout_ms: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct JwksEntry { pub kid: Kid, pub pem: String }

#[derive(Clone, Debug, Deserialize)]
pub struct IpBlockEntry {
    pub cidr: String,
    pub scope: IpBlockScope,
    pub reason: IpBlockReason,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpBlockScope { Global, User(UserId), ApiKey(ApiKeyId) }

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpBlockReason { Manual, AbnormalTraffic, Compliance }
```

- [ ] **Step 3：permissions.rs（MergedPermissions）**

```rust
use crate::ids::*;
use std::collections::HashSet;
use std::net::IpAddr;

#[derive(Clone, Debug)]
pub struct MergedPermissions {
    pub user_id: UserId,
    pub effective_user_id: UserId,
    pub client_ip: IpAddr,
    pub allowed_models: HashSet<ModelName>,
    pub allowed_channels: HashSet<KeyId>,
    pub allowed_groups: HashSet<Group>,
    pub forced_model_group: Option<Group>,
    pub rpm_limit: u32,
    pub tpm_limit: u32,
    pub max_concurrent_streams: u32,
    pub markup_product: f32,
}
```

- [ ] **Step 4：traits.rs 扩展点（ProtocolCodec 占位 / ContentFilter / TokenEstimator / Redactor）**

`ProtocolCodec` 完整 trait 在 `gw-codec-macros` 编写后由 `gw-codec` 实现，这里仅定义其余扩展点。

```rust
use crate::error::ErrorIR;
use crate::ir::*;

pub trait ContentFilter: Send + Sync {
    fn check_request(&self, ir: &RequestIR<'_>) -> ContentVerdict;
    fn check_stream_chunk(&self, chunk: &StreamEventIR<'_>) -> ContentVerdict;
    fn check_response(&self, ir: &ResponseIR<'_>) -> ContentVerdict;
}

pub enum ContentVerdict { Pass, Tag(String), Reject(ErrorIR<'static>) }

pub struct NoopContentFilter;
impl ContentFilter for NoopContentFilter {
    fn check_request(&self, _: &RequestIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
    fn check_stream_chunk(&self, _: &StreamEventIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
    fn check_response(&self, _: &ResponseIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
}

pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, text: &str, model: &str) -> u32;
}

pub struct NoopTokenEstimator;
impl TokenEstimator for NoopTokenEstimator {
    fn estimate(&self, text: &str, _: &str) -> u32 {
        // ADR-004 保守上限：char/4 经验系数
        (text.chars().count() as u32 / 4).max(1)
    }
}

pub trait Redactor: Send + Sync {
    fn redact(&self, s: &mut String);
}
```

- [ ] **Step 5：debug.rs TraceBuffer / DebugMode（ADR-027）**

```rust
use smallvec::SmallVec;
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct PhaseRecord {
    pub name: &'static str,
    pub start_ns: u64,
    pub end_ns: u64,
    pub alloc_at_start: u64,
    pub alloc_at_end: u64,
}

impl Default for PhaseRecord {
    fn default() -> Self {
        Self { name: "", start_ns: 0, end_ns: 0, alloc_at_start: 0, alloc_at_end: 0 }
    }
}

#[derive(Clone, Debug)]
pub struct ChunkRecord { pub idx: u32, pub size: u32, pub parse_ns: u64, pub render_ns: u64, pub at_ns: u64 }

#[derive(Clone, Debug)]
pub struct MutationRecord { pub field: &'static str, pub reason: &'static str, pub at_ns: u64 }

pub struct TraceBuffer {
    pub phases: [PhaseRecord; 32],
    pub phase_count: u8,
    pub stream_chunks: Vec<ChunkRecord>,
    pub ir_mutations: Vec<MutationRecord>,
    pub filter_drop_reasons: SmallVec<[(String, &'static str); 8]>,
    base: Instant,
}

impl TraceBuffer {
    pub fn new() -> Self {
        Self {
            phases: [PhaseRecord::default(); 32],
            phase_count: 0,
            stream_chunks: Vec::new(),
            ir_mutations: Vec::new(),
            filter_drop_reasons: SmallVec::new(),
            base: Instant::now(),
        }
    }

    #[inline(always)]
    pub fn now_ns(&self) -> u64 { self.base.elapsed().as_nanos() as u64 }

    #[inline(always)]
    pub fn phase_start(&mut self, name: &'static str, alloc: u64) {
        let idx = self.phase_count as usize;
        if idx >= self.phases.len() { return; }
        self.phases[idx] = PhaseRecord { name, start_ns: self.now_ns(), end_ns: 0, alloc_at_start: alloc, alloc_at_end: 0 };
        self.phase_count += 1;
    }

    #[inline(always)]
    pub fn phase_end(&mut self, alloc: u64) {
        let idx = (self.phase_count as usize).saturating_sub(1);
        if idx >= self.phases.len() { return; }
        let p = &mut self.phases[idx];
        p.end_ns = self.now_ns();
        p.alloc_at_end = alloc;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DebugTrigger { None, Header, UserFlag, Sampling, TraceSuffix }

#[derive(Clone, Copy, Debug)]
pub struct DebugMode {
    pub enabled: bool,
    pub trigger: DebugTrigger,
    pub include_request_body: bool,
    pub include_response_body: bool,
}

impl DebugMode {
    pub const DISABLED: Self = Self { enabled: false, trigger: DebugTrigger::None, include_request_body: false, include_response_body: false };
    #[inline] pub fn upgrade_to_enabled(&mut self, trigger: DebugTrigger) {
        if self.enabled { return; }
        self.enabled = true;
        self.trigger = trigger;
        self.include_request_body = true;
        self.include_response_body = true;
    }
}

pub trait DebugSink: Send + Sync { fn emit(&self, trace: &TraceBuffer); }
pub struct NoopDebugSink;
impl DebugSink for NoopDebugSink { fn emit(&self, _: &TraceBuffer) {} }
```

- [ ] **Step 6：编译通过**

Run: `cargo check -p gw-core`
Expected: 无错误。

- [ ] **Step 7：提交**

```bash
git add crates/gw-core
git commit -m "feat(gw-core): add entities, permissions, traits, debug TraceBuffer"
```

---

### Task 1.4：`gw-core` 单元测试

**Files:**
- Create: `crates/gw-core/tests/ids.rs`
- Create: `crates/gw-core/tests/versioned.rs`
- Create: `crates/gw-core/tests/debug.rs`

- [ ] **Step 1：ids.rs 序列化/反序列化测试**

```rust
use gw_core::ids::*;

#[test]
fn user_id_roundtrip_json() {
    let id = UserId::from("u123");
    let s = serde_json::to_string(&id).unwrap();
    assert_eq!(s, "\"u123\"");
    let back: UserId = serde_json::from_str(&s).unwrap();
    assert_eq!(back, id);
}

#[test]
fn debug_format_includes_type_name() {
    let id = UserId::from("u1");
    let s = format!("{:?}", id);
    assert!(s.contains("UserId"));
    assert!(s.contains("u1"));
}
```

- [ ] **Step 2：versioned.rs upsert_if_newer 测试**

```rust
use gw_core::versioned::{upsert_if_newer, Versioned};

#[test]
fn newer_version_overwrites() {
    let mut cur = Some(Versioned::new(1, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(upsert_if_newer(&mut cur, upd));
    assert_eq!(cur.as_ref().unwrap().version, 2);
    assert_eq!(&**cur.as_ref().unwrap().value, "b");
}

#[test]
fn equal_version_rejected() {
    let mut cur = Some(Versioned::new(2, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(!upsert_if_newer(&mut cur, upd));
    assert_eq!(&**cur.as_ref().unwrap().value, "a");
}

#[test]
fn older_version_rejected() {
    let mut cur = Some(Versioned::new(3, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(!upsert_if_newer(&mut cur, upd));
    assert_eq!(&**cur.as_ref().unwrap().value, "a");
}

#[test]
fn empty_cur_accepts() {
    let mut cur: Option<Versioned<String>> = None;
    assert!(upsert_if_newer(&mut cur, Versioned::new(1, "x".into())));
}
```

- [ ] **Step 3：debug.rs phase 记录测试**

```rust
use gw_core::debug::TraceBuffer;

#[test]
fn phase_start_end_records_alloc_and_time() {
    let mut tb = TraceBuffer::new();
    tb.phase_start("p1", 100);
    std::thread::sleep(std::time::Duration::from_micros(50));
    tb.phase_end(200);
    assert_eq!(tb.phase_count, 1);
    let p = &tb.phases[0];
    assert_eq!(p.name, "p1");
    assert!(p.end_ns > p.start_ns);
    assert_eq!(p.alloc_at_start, 100);
    assert_eq!(p.alloc_at_end, 200);
}

#[test]
fn phase_overflow_truncates_silently() {
    let mut tb = TraceBuffer::new();
    for i in 0..40 {
        tb.phase_start("p", i);
        tb.phase_end(i);
    }
    assert_eq!(tb.phase_count, 32);
}
```

- [ ] **Step 4：运行所有测试**

Run: `cargo test -p gw-core`
Expected: 全部通过。

- [ ] **Step 5：提交**

```bash
git add crates/gw-core/tests
git commit -m "test(gw-core): unit tests for ids, versioned upsert, TraceBuffer"
```

---

## Phase 2：协议层 + 存储层（**两路并行**）

> Phase 2 包含两条独立路径：A（codec macros + codec）和 B（store）。可分配给两个 agent 同时执行；两路最终都依赖 `gw-core`，互不依赖。

### Path A：`gw-codec-macros` + `gw-codec`

---

### Task 2A.1：`gw-codec-macros` 派生宏 crate

**Files:**
- Create: `crates/gw-codec-macros/Cargo.toml`
- Create: `crates/gw-codec-macros/src/lib.rs`
- Create: `crates/gw-codec-macros/src/ir_convert.rs`
- Create: `crates/gw-codec-macros/src/ir_stream.rs`
- Create: `crates/gw-codec-macros/src/ir_error.rs`
- Create: `crates/gw-codec-macros/src/detect.rs`

- [ ] **Step 1：Cargo.toml（proc-macro=true）**

```toml
[package]
name = "gw-codec-macros"
version = "0.1.0"
edition.workspace = true

[lib]
proc-macro = true

[dependencies]
proc-macro2.workspace = true
syn = { workspace = true, features = ["full", "extra-traits"] }
quote.workspace = true
```

- [ ] **Step 2：lib.rs 注册 4 个 derive**

```rust
use proc_macro::TokenStream;

mod ir_convert;
mod ir_stream;
mod ir_error;
mod detect;

#[proc_macro_derive(IrConvert, attributes(ir))]
pub fn derive_ir_convert(input: TokenStream) -> TokenStream { ir_convert::expand(input) }

#[proc_macro_derive(IrStreamConvert, attributes(ir))]
pub fn derive_ir_stream_convert(input: TokenStream) -> TokenStream { ir_stream::expand(input) }

#[proc_macro_derive(IrErrorConvert, attributes(ir))]
pub fn derive_ir_error_convert(input: TokenStream) -> TokenStream { ir_error::expand(input) }

#[proc_macro_derive(DetectRequest, attributes(detect))]
pub fn derive_detect_request(input: TokenStream) -> TokenStream { detect::expand(input) }
```

- [ ] **Step 3：ir_convert.rs 解析 `#[ir(...)]` 属性 + 生成 `From` / `TryFrom` impl**

实现要点（按 ADR-019 derive 宏行为）：

- 解析 `#[ir(target = "RequestIR", direction = "both")]` 顶层属性
- 解析每个字段的 `#[ir(map = "...", via = "...", default = "...", extensions)]`
- 生成 `impl<'a> From<X<'a>> for RequestIR<'a>` 与 `impl<'a> TryFrom<&RequestIR<'a>> for X<'a>`
- 所有方法体 `#[inline(always)]`
- IR 上未被映射的字段填 `None` / `Default::default()`
- `#[ir(extensions)]` 字段映射到 `RequestIR.extensions`
- `via = "fn"` 在转换时调用该函数

```rust
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr, Meta};

struct IrTargetAttr { target: String, direction: String }

struct FieldAttr {
    map: Option<String>,
    via: Option<syn::Path>,
    default: Option<String>,
    extensions: bool,
}

pub fn expand(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();

    let target = parse_target_attr(&ast.attrs);
    let target_ident = format_ident!("{}", target.target);

    let fields = match &ast.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => panic!("IrConvert requires named fields"),
        },
        _ => panic!("IrConvert requires struct"),
    };

    let from_body = gen_from_body(&target_ident, fields);
    let try_from_body = gen_try_from_body(&target_ident, fields);

    let expanded = quote! {
        impl #impl_generics ::core::convert::From<#name #ty_generics> for ::gw_core::ir::#target_ident<'a> #where_clause {
            #[inline(always)]
            fn from(src: #name #ty_generics) -> Self { #from_body }
        }

        impl #impl_generics ::core::convert::TryFrom<&::gw_core::ir::#target_ident<'a>> for #name #ty_generics #where_clause {
            type Error = ::gw_core::error::ErrorIR<'static>;
            #[inline(always)]
            fn try_from(ir: &::gw_core::ir::#target_ident<'a>) -> ::core::result::Result<Self, Self::Error> { #try_from_body }
        }
    };
    expanded.into()
}

fn parse_target_attr(attrs: &[syn::Attribute]) -> IrTargetAttr {
    for a in attrs {
        if !a.path().is_ident("ir") { continue; }
        let mut target = String::new();
        let mut direction = String::from("both");
        a.parse_nested_meta(|m| {
            if m.path.is_ident("target") { target = m.value()?.parse::<LitStr>()?.value(); }
            if m.path.is_ident("direction") { direction = m.value()?.parse::<LitStr>()?.value(); }
            Ok(())
        }).ok();
        if !target.is_empty() { return IrTargetAttr { target, direction }; }
    }
    panic!("IrConvert: missing #[ir(target = \"...\")]")
}

fn parse_field_attr(attrs: &[syn::Attribute]) -> FieldAttr {
    let mut fa = FieldAttr { map: None, via: None, default: None, extensions: false };
    for a in attrs {
        if !a.path().is_ident("ir") { continue; }
        a.parse_nested_meta(|m| {
            if m.path.is_ident("map") { fa.map = Some(m.value()?.parse::<LitStr>()?.value()); }
            if m.path.is_ident("via") { let s: LitStr = m.value()?.parse()?; fa.via = Some(syn::parse_str(&s.value())?); }
            if m.path.is_ident("default") { fa.default = Some(m.value()?.parse::<LitStr>()?.value()); }
            if m.path.is_ident("extensions") { fa.extensions = true; }
            Ok(())
        }).ok();
    }
    fa
}

fn gen_from_body(target: &syn::Ident, fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>) -> TokenStream2 {
    // 按字段聚合：构造 IR struct literal；未被 map 的目标字段填 None / Default
    // 完整实现需枚举 RequestIR / ResponseIR 字段集合；此处给出生成模板
    let mut assigns: Vec<TokenStream2> = Vec::new();
    for f in fields {
        let fa = parse_field_attr(&f.attrs);
        let ident = f.ident.as_ref().unwrap();
        if fa.extensions {
            assigns.push(quote!( extensions: if src.#ident.is_empty() { None } else { Some(src.#ident) }, ));
            continue;
        }
        let map = match fa.map { Some(m) => m, None => continue };
        let map_path: Vec<&str> = map.split('.').collect();
        let target_field = format_ident!("{}", map_path[0]);
        if let Some(via) = fa.via {
            assigns.push(quote!( #target_field: #via(src.#ident), ));
        } else {
            assigns.push(quote!( #target_field: src.#ident, ));
        }
    }
    quote! {
        ::gw_core::ir::#target {
            #(#assigns)*
            ..::core::default::Default::default()
        }
    }
}

fn gen_try_from_body(target: &syn::Ident, fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>) -> TokenStream2 {
    let mut assigns: Vec<TokenStream2> = Vec::new();
    for f in fields {
        let fa = parse_field_attr(&f.attrs);
        let ident = f.ident.as_ref().unwrap();
        if fa.extensions {
            assigns.push(quote!( #ident: ir.extensions.clone().unwrap_or_default(), ));
            continue;
        }
        let map = match fa.map { Some(m) => m, None => continue };
        let map_path: Vec<&str> = map.split('.').collect();
        let src_field = format_ident!("{}", map_path[0]);
        if let Some(via) = fa.via {
            let via_rev = format_ident!("{}_rev", via.segments.last().unwrap().ident);
            assigns.push(quote!( #ident: #via_rev(&ir.#src_field)?, ));
        } else {
            assigns.push(quote!( #ident: ir.#src_field.clone(), ));
        }
    }
    quote! {
        Ok(Self { #(#assigns)* })
    }
}
```

> 注意：因 IR 的 `Default` 实现尚未完成，需要在 `gw-core/src/ir.rs` 补 `impl Default for RequestIR / ResponseIR / MetadataIR`。同步在该步骤回写 gw-core。

- [ ] **Step 4：ir_stream.rs / ir_error.rs / detect.rs 给出框架**

`ir_stream.rs` 与 `ir_error.rs` 复用 `ir_convert.rs` 的逻辑，仅切换 target 类型；
`detect.rs` 解析 `#[detect(path_prefix = "/v1/chat/completions", header_contains = "X-")]`，生成 `pub fn detect(headers: &HeaderMap, body_head: &[u8]) -> Option<Self>`。

```rust
// ir_stream.rs（最小实现：复用 ir_convert 逻辑，仅 target enum 不同）
pub fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    crate::ir_convert::expand(input)
}

// ir_error.rs（同上）
pub fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    crate::ir_convert::expand(input)
}

// detect.rs
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, LitStr};

pub fn expand(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let mut path_prefix = String::new();
    let mut header_contains: Option<String> = None;
    for a in &ast.attrs {
        if !a.path().is_ident("detect") { continue; }
        a.parse_nested_meta(|m| {
            if m.path.is_ident("path_prefix") { path_prefix = m.value()?.parse::<LitStr>()?.value(); }
            if m.path.is_ident("header_contains") { header_contains = Some(m.value()?.parse::<LitStr>()?.value()); }
            Ok(())
        }).ok();
    }
    let header_check: TokenStream2 = match header_contains {
        Some(h) => quote!( headers.iter().any(|(k, _)| k.as_str().contains(#h)) ),
        None => quote!( true ),
    };
    let expanded = quote! {
        impl #name {
            pub fn detect(path: &str, headers: &::pingora::http::HeaderMap, _body_head: &[u8]) -> Option<Self> {
                if path.starts_with(#path_prefix) && #header_check { Some(Self) } else { None }
            }
        }
    };
    expanded.into()
}
```

- [ ] **Step 5：编译通过**

Run: `cargo check -p gw-codec-macros`
Expected: 无错误。

- [ ] **Step 6：提交**

```bash
git add crates/gw-codec-macros
git commit -m "feat(gw-codec-macros): scaffold IrConvert/IrStream/IrError/Detect derive macros"
```

---

### Task 2A.2：`gw-codec` crate + OpenAI / Anthropic / Gemini wire structs

**Files:**
- Create: `crates/gw-codec/Cargo.toml`
- Create: `crates/gw-codec/src/lib.rs`
- Create: `crates/gw-codec/src/trait_def.rs`
- Create: `crates/gw-codec/src/openai/mod.rs`
- Create: `crates/gw-codec/src/openai/wire.rs`
- Create: `crates/gw-codec/src/openai/via.rs`
- Create: `crates/gw-codec/src/openai/codec.rs`
- Create: `crates/gw-codec/src/anthropic/mod.rs`
- Create: `crates/gw-codec/src/anthropic/wire.rs`
- Create: `crates/gw-codec/src/anthropic/via.rs`
- Create: `crates/gw-codec/src/anthropic/codec.rs`
- Create: `crates/gw-codec/src/gemini/mod.rs`
- Create: `crates/gw-codec/src/gemini/wire.rs`
- Create: `crates/gw-codec/src/gemini/via.rs`
- Create: `crates/gw-codec/src/gemini/codec.rs`
- Create: `crates/gw-codec/src/any.rs`
- Create: `crates/gw-codec/src/passthrough.rs`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-codec"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-codec-macros = { path = "../gw-codec-macros" }
bytes.workspace = true
serde.workspace = true
serde_json.workspace = true
sonic-rs.workspace = true
thiserror.workspace = true
smallvec.workspace = true
pingora.workspace = true

[dev-dependencies]
proptest.workspace = true
```

- [ ] **Step 2：lib.rs**

```rust
pub mod trait_def;
pub mod any;
pub mod passthrough;
pub mod openai;
pub mod anthropic;
pub mod gemini;

pub use trait_def::ProtocolCodec;
pub use any::AnyCodec;
```

- [ ] **Step 3：trait_def.rs（ADR-019 完整 ProtocolCodec）**

```rust
use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::*;
use pingora::http::HeaderMap;

pub trait ProtocolCodec: Send + Sync + 'static {
    fn detect_request(path: &str, headers: &HeaderMap, body_head: &[u8]) -> Option<Self> where Self: Sized;

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>>;
    fn render_request(&self, ir: &RequestIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;

    fn parse_stream_chunk<'a>(&self, chunk: &'a [u8]) -> Result<Option<StreamEventIR<'a>>, ErrorIR<'static>>;
    fn render_stream_chunk(&self, ir: &StreamEventIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>>;
    fn render_response(&self, ir: &ResponseIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a>;
    fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut);

    fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32;
}
```

- [ ] **Step 4：openai/wire.rs OpenAI wire-format struct + `#[derive(IrConvert)]`**

```rust
use gw_codec_macros::IrConvert;
use gw_core::ir::*;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, IrConvert)]
#[ir(target = "RequestIR", direction = "both")]
#[serde(deny_unknown_fields = false)]
pub struct OpenAiRequest<'a> {
    #[ir(map = "model")]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "openai_msg_vec")]
    pub messages: Vec<OpenAiMessage<'a>>,

    #[ir(map = "stream", default = "false")]
    #[serde(default)]
    pub stream: bool,

    #[ir(map = "max_tokens")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[ir(map = "temperature")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[ir(map = "top_p")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[ir(map = "stop")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<Cow<'a, str>>>,

    #[ir(map = "metadata.openai_logit_bias")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<Cow<'a, str>, f32>>,

    #[ir(extensions)]
    #[serde(flatten, borrow)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum OpenAiMessage<'a> {
    System    { content: Cow<'a, str> },
    User      { content: Cow<'a, str> },
    Assistant { content: Option<Cow<'a, str>>, #[serde(default)] tool_calls: Option<Vec<OpenAiToolCall<'a>>> },
    Tool      { tool_call_id: Cow<'a, str>, content: Cow<'a, str> },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiToolCall<'a> {
    pub id: Cow<'a, str>,
    pub r#type: Cow<'a, str>,
    pub function: OpenAiFunctionCall<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiFunctionCall<'a> {
    pub name: Cow<'a, str>,
    pub arguments: Cow<'a, str>,
}
```

- [ ] **Step 5：openai/via.rs（手写复合字段转换函数，由 derive 调用）**

```rust
use gw_core::ir::*;
use std::borrow::Cow;
use super::wire::*;

#[inline(always)]
pub fn openai_msg_vec<'a>(msgs: Vec<OpenAiMessage<'a>>) -> Vec<MessageIR<'a>> {
    msgs.into_iter().map(|m| match m {
        OpenAiMessage::System { content }    => MessageIR::System    { content: ContentIR::Text(content) },
        OpenAiMessage::User { content }      => MessageIR::User      { content: ContentIR::Text(content) },
        OpenAiMessage::Assistant { content, tool_calls } => MessageIR::Assistant {
            content: ContentIR::Text(content.unwrap_or(Cow::Borrowed(""))),
            tool_calls: tool_calls.map(|tcs| tcs.into_iter().map(|tc| ToolCallIR {
                id: tc.id, name: tc.function.name,
                arguments_json: Cow::Owned(serde_json::value::RawValue::from_string(tc.function.arguments.into_owned()).unwrap()),
            }).collect()),
        },
        OpenAiMessage::Tool { tool_call_id, content } => MessageIR::Tool { tool_call_id, content: ContentIR::Text(content) },
    }).collect()
}

#[inline(always)]
pub fn openai_msg_vec_rev<'a>(msgs: &Vec<MessageIR<'a>>) -> Result<Vec<OpenAiMessage<'a>>, gw_core::error::ErrorIR<'static>> {
    Ok(msgs.iter().map(|m| match m {
        MessageIR::System    { content } => OpenAiMessage::System { content: content_to_text(content) },
        MessageIR::User      { content } => OpenAiMessage::User { content: content_to_text(content) },
        MessageIR::Assistant { content, tool_calls } => OpenAiMessage::Assistant {
            content: Some(content_to_text(content)),
            tool_calls: tool_calls.clone().map(|tcs| tcs.into_iter().map(|tc| OpenAiToolCall {
                id: tc.id, r#type: Cow::Borrowed("function"),
                function: OpenAiFunctionCall { name: tc.name, arguments: Cow::Owned(tc.arguments_json.get().to_owned()) },
            }).collect()),
        },
        MessageIR::Tool { tool_call_id, content } => OpenAiMessage::Tool { tool_call_id: tool_call_id.clone(), content: content_to_text(content) },
    }).collect())
}

fn content_to_text<'a>(c: &ContentIR<'a>) -> Cow<'a, str> {
    match c {
        ContentIR::Text(t) => t.clone(),
        ContentIR::Parts(parts) => Cow::Owned(parts.iter().filter_map(|p| match p {
            ContentPart::Text(t) => Some(t.as_ref()), _ => None,
        }).collect::<Vec<_>>().join("")),
    }
}
```

- [ ] **Step 6：openai/codec.rs（ProtocolCodec 实现）**

```rust
use bytes::BytesMut;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::*;
use pingora::http::HeaderMap;
use std::borrow::Cow;
use crate::trait_def::ProtocolCodec;
use super::wire::OpenAiRequest;

pub struct OpenAi;

impl ProtocolCodec for OpenAi {
    fn detect_request(path: &str, _h: &HeaderMap, _body_head: &[u8]) -> Option<Self> {
        if path.starts_with("/v1/chat/completions") { Some(OpenAi) } else { None }
    }

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let req: OpenAiRequest<'a> = sonic_rs::from_slice(body).map_err(|e| ErrorIR {
            category: ErrorCategory::ClientBadRequest, status: 400,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_REQUEST_PROTOCOL_UNKNOWN),
            message: Cow::Owned(format!("openai parse: {e}")),
            retryable: false, upstream_raw: None, request_id: "", retry_after_secs: None,
        })?;
        Ok(req.into())
    }

    fn render_request(&self, ir: &RequestIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>> {
        let req = OpenAiRequest::try_from(ir).map_err(|_| ErrorIR {
            category: ErrorCategory::GatewayInternal, status: 500,
            upstream_status: None, code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
            message: Cow::Borrowed("openai render failed"), retryable: false,
            upstream_raw: None, request_id: "", retry_after_secs: None,
        })?;
        let mut writer = buf.writer();
        sonic_rs::to_writer(&mut writer, &req).map_err(|_| ErrorIR {
            category: ErrorCategory::GatewayInternal, status: 500,
            upstream_status: None, code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
            message: Cow::Borrowed("openai serialize failed"), retryable: false,
            upstream_raw: None, request_id: "", retry_after_secs: None,
        })?;
        Ok(())
    }

    fn parse_stream_chunk<'a>(&self, chunk: &'a [u8]) -> Result<Option<StreamEventIR<'a>>, ErrorIR<'static>> {
        // OpenAI SSE: lines like "data: {...}" / "data: [DONE]"
        let s = std::str::from_utf8(chunk).map_err(|_| internal_err("invalid utf8"))?;
        for line in s.split('\n') {
            let line = line.trim_start();
            if let Some(payload) = line.strip_prefix("data: ") {
                if payload.trim() == "[DONE]" {
                    return Ok(Some(StreamEventIR::End { finish_reason: FinishReasonIR::Stop }));
                }
                // 简化：仅抽 delta.content
                if let Ok(val) = sonic_rs::from_str::<sonic_rs::Value>(payload) {
                    let txt = val.get("choices").and_then(|c| c.get(0))
                                 .and_then(|c| c.get("delta")).and_then(|d| d.get("content"))
                                 .and_then(|c| c.as_str()).map(|s| Cow::Owned(s.to_owned()));
                    return Ok(Some(StreamEventIR::Delta { text: txt, tool_call: None, thinking: None }));
                }
            }
        }
        Ok(None)
    }

    fn render_stream_chunk(&self, ir: &StreamEventIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>> {
        use std::io::Write;
        match ir {
            StreamEventIR::Delta { text, .. } => {
                let payload = serde_json::json!({
                    "choices": [{ "delta": { "content": text.as_deref().unwrap_or("") }, "index": 0 }]
                });
                write!(buf.writer(), "data: {payload}\n\n").ok();
            }
            StreamEventIR::End { .. } => { buf.extend_from_slice(b"data: [DONE]\n\n"); }
            _ => {}
        }
        Ok(())
    }

    fn parse_response<'a>(&self, _body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> { unimplemented!("v1: 仅用于错误体；2xx 走 stream") }
    fn render_response(&self, _ir: &ResponseIR<'_>, _buf: &mut BytesMut) -> Result<(), ErrorIR<'static>> { unimplemented!() }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        // OpenAI: {"error": {"message": "...", "type": "...", "code": "..."}}
        let msg = sonic_rs::from_slice::<sonic_rs::Value>(body).ok()
            .and_then(|v| v.get("error").and_then(|e| e.get("message"))
                .and_then(|m| m.as_str()).map(|s| s.to_owned()))
            .unwrap_or_default();
        ErrorIR {
            category: classify(status),
            status, upstream_status: Some(status),
            code: Cow::Borrowed(map_code(status)),
            message: Cow::Owned(msg),
            retryable: matches!(status, 500..=599 | 429),
            upstream_raw: None, request_id: "", retry_after_secs: None,
        }
    }

    fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut) {
        let body = serde_json::json!({
            "error": { "type": err.category, "message": err.message, "code": err.code }
        });
        buf.extend_from_slice(body.to_string().as_bytes());
    }

    fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32 {
        ir.messages.iter().map(|m| match m {
            MessageIR::System { content } | MessageIR::User { content }
            | MessageIR::Tool { content, .. } | MessageIR::Assistant { content, .. } => approx_tokens(content),
        }).sum()
    }
}

fn approx_tokens(c: &ContentIR<'_>) -> u32 {
    match c {
        ContentIR::Text(t) => (t.chars().count() as u32 / 4).max(1),
        ContentIR::Parts(parts) => parts.iter().map(|p| match p {
            ContentPart::Text(t) => (t.chars().count() as u32 / 4).max(1), _ => 0,
        }).sum(),
    }
}

fn classify(status: u16) -> ErrorCategory {
    match status {
        429 => ErrorCategory::ClientQuota,
        400..=499 => ErrorCategory::ClientBadRequest,
        500..=599 => ErrorCategory::UpstreamError,
        _ => ErrorCategory::GatewayInternal,
    }
}

fn map_code(status: u16) -> &'static str {
    match status {
        429 => codes::GW_QUOTA_RPM_EXCEEDED,
        400..=499 => codes::GW_UPSTREAM_4XX,
        500..=599 => codes::GW_UPSTREAM_5XX,
        _ => codes::GW_GATEWAY_INTERNAL,
    }
}

fn internal_err(msg: &'static str) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal, status: 500,
        upstream_status: None, code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
        message: Cow::Borrowed(msg), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None,
    }
}
```

- [ ] **Step 7：anthropic / gemini 三 mod 重复同样结构**

每个 mod 内部包含：
- `wire.rs`：定义 wire-format struct + `#[derive(IrConvert)]`
- `via.rs`：手写 via 函数
- `codec.rs`：`pub struct Anthropic;` / `pub struct Gemini;` 实现 `ProtocolCodec`
- `mod.rs`：`pub mod codec; pub mod via; pub mod wire; pub use codec::*;`

简化的 wire 仅覆盖以下字段（v1）：
- **Anthropic**：`model / messages / system / max_tokens / temperature / stream`，stream 事件支持 `content_block_delta` / `message_stop`
- **Gemini**：`contents / systemInstruction / generationConfig.maxOutputTokens`，stream 事件 chunk JSON

- [ ] **Step 8：any.rs AnyCodec enum**

```rust
use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::*;
use gw_core::protocol::ProtocolKind;
use pingora::http::HeaderMap;
use crate::trait_def::ProtocolCodec;
use crate::openai::OpenAi;
use crate::anthropic::Anthropic;
use crate::gemini::Gemini;

pub enum AnyCodec { OpenAi(OpenAi), Anthropic(Anthropic), Gemini(Gemini) }

impl AnyCodec {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            AnyCodec::OpenAi(_)    => ProtocolKind::OpenAi,
            AnyCodec::Anthropic(_) => ProtocolKind::Anthropic,
            AnyCodec::Gemini(_)    => ProtocolKind::Gemini,
        }
    }
    pub fn detect(path: &str, h: &HeaderMap, body_head: &[u8]) -> Option<Self> {
        if let Some(c) = OpenAi::detect_request(path, h, body_head) { return Some(AnyCodec::OpenAi(c)); }
        if let Some(c) = Anthropic::detect_request(path, h, body_head) { return Some(AnyCodec::Anthropic(c)); }
        if let Some(c) = Gemini::detect_request(path, h, body_head) { return Some(AnyCodec::Gemini(c)); }
        None
    }

    #[inline] pub fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.parse_request(body),
            AnyCodec::Anthropic(c) => c.parse_request(body),
            AnyCodec::Gemini(c) => c.parse_request(body),
        }
    }
    // 同模式 inline 转发其余方法…略
}
```

- [ ] **Step 9：passthrough.rs（ADR-019 同协议透传 fast path）**

```rust
use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::Mutations;
use std::borrow::Cow;
use crate::trait_def::ProtocolCodec;

#[inline(always)]
pub fn same_protocol_passthrough<'a, C: ProtocolCodec>(
    codec: &C, body: &'a [u8], mutations: &Mutations,
) -> Result<Cow<'a, [u8]>, ErrorIR<'static>> {
    if mutations.is_empty() {
        return Ok(Cow::Borrowed(body));
    }
    let ir = codec.parse_request(body)?;
    let mut buf = BytesMut::with_capacity(body.len());
    codec.render_request(&ir, &mut buf)?;
    Ok(Cow::Owned(buf.to_vec()))
}
```

- [ ] **Step 10：编译通过**

Run: `cargo check -p gw-codec`
Expected: 无错误。可能因 derive 宏生成的 IR Default impl 缺失需回 gw-core 补 `impl<'a> Default for RequestIR<'a>` 等。

- [ ] **Step 11：提交**

```bash
git add crates/gw-codec crates/gw-core/src/ir.rs
git commit -m "feat(gw-codec): impl ProtocolCodec for OpenAI/Anthropic/Gemini + AnyCodec + passthrough"
```

---

### Task 2A.3：`gw-codec` 单元 + 集成测试

**Files:**
- Create: `crates/gw-codec/tests/openai_roundtrip.rs`
- Create: `crates/gw-codec/tests/anthropic_roundtrip.rs`
- Create: `crates/gw-codec/tests/gemini_roundtrip.rs`
- Create: `crates/gw-codec/tests/cross_protocol.rs`
- Create: `crates/gw-codec/tests/passthrough_bench.rs`
- Create: `crates/gw-codec/tests/fixtures/openai_basic.json`
- Create: `crates/gw-codec/tests/fixtures/anthropic_basic.json`
- Create: `crates/gw-codec/tests/fixtures/gemini_basic.json`

- [ ] **Step 1：openai_roundtrip.rs**

```rust
use gw_codec::openai::OpenAi;
use gw_codec::ProtocolCodec;

const BASIC: &str = include_str!("fixtures/openai_basic.json");

#[test]
fn parse_basic_request() {
    let codec = OpenAi;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert_eq!(ir.model.as_ref(), "gpt-4");
    assert!(!ir.stream);
    assert_eq!(ir.messages.len(), 2);
}

#[test]
fn render_roundtrip_preserves_model() {
    let codec = OpenAi;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir.model, ir2.model);
    assert_eq!(ir.messages.len(), ir2.messages.len());
}

#[test]
fn parse_stream_chunk_delta() {
    let codec = OpenAi;
    let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
    let e = codec.parse_stream_chunk(chunk).unwrap().unwrap();
    match e {
        gw_core::ir::StreamEventIR::Delta { text, .. } => assert_eq!(text.unwrap(), "hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_done() {
    let codec = OpenAi;
    let e = codec.parse_stream_chunk(b"data: [DONE]\n\n").unwrap().unwrap();
    assert!(matches!(e, gw_core::ir::StreamEventIR::End { .. }));
}
```

`fixtures/openai_basic.json`：

```json
{
  "model": "gpt-4",
  "messages": [
    {"role":"system","content":"You are helpful"},
    {"role":"user","content":"Hello"}
  ],
  "max_tokens": 100,
  "temperature": 0.7
}
```

- [ ] **Step 2：anthropic_roundtrip.rs / gemini_roundtrip.rs 同样结构**

每个测试文件至少包含：`parse_basic_request` / `render_roundtrip` / `parse_stream_chunk_delta` / `estimate_input_tokens` / `parse_error_400`。

- [ ] **Step 3：cross_protocol.rs（IR 中转）**

```rust
use gw_codec::{openai::OpenAi, anthropic::Anthropic, ProtocolCodec};

#[test]
fn openai_to_anthropic_via_ir() {
    let openai = OpenAi;
    let anthropic = Anthropic;
    let body = include_bytes!("fixtures/openai_basic.json");
    let ir = openai.parse_request(body).unwrap();
    let mut buf = bytes::BytesMut::new();
    anthropic.render_request(&ir, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(v["model"], "gpt-4");
    assert!(v.get("messages").is_some());
}
```

- [ ] **Step 4：passthrough_bench.rs**

```rust
use gw_codec::{openai::OpenAi, passthrough::same_protocol_passthrough};
use gw_core::ir::Mutations;

#[test]
fn empty_mutation_returns_borrowed() {
    let codec = OpenAi;
    let body = b"{\"model\":\"gpt-4\",\"messages\":[]}";
    let out = same_protocol_passthrough(&codec, body, &Mutations::NONE).unwrap();
    assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn non_empty_mutation_returns_owned() {
    let codec = OpenAi;
    let body = b"{\"model\":\"gpt-4\",\"messages\":[]}";
    let mut m = Mutations::NONE;
    m.set_max_tokens();
    let out = same_protocol_passthrough(&codec, body, &m).unwrap();
    assert!(matches!(out, std::borrow::Cow::Owned(_)));
}
```

- [ ] **Step 5：运行测试**

Run: `cargo test -p gw-codec`
Expected: 全部通过。

- [ ] **Step 6：提交**

```bash
git add crates/gw-codec/tests
git commit -m "test(gw-codec): integration tests for OpenAI/Anthropic/Gemini codec + passthrough"
```

---

### Path B：`gw-store`

> 与 Path A 并行。仅依赖 `gw-core`。

### Task 2B.1：`gw-store` crate 骨架 + Redis / PgSQL 客户端封装

**Files:**
- Create: `crates/gw-store/Cargo.toml`
- Create: `crates/gw-store/src/lib.rs`
- Create: `crates/gw-store/src/redis_client.rs`
- Create: `crates/gw-store/src/pg_client.rs`
- Create: `crates/gw-store/src/entity_store.rs`
- Create: `crates/gw-store/src/counter_store.rs`
- Create: `crates/gw-store/src/loader.rs`
- Create: `crates/gw-store/src/stream_consumer.rs`
- Create: `crates/gw-store/src/deduction.rs`
- Create: `crates/gw-store/src/encrypt.rs`
- Create: `crates/gw-store/migrations/0001_initial.sql`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-store"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
bytes.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
anyhow.workspace = true
time.workspace = true
secrecy.workspace = true
url.workspace = true
tokio.workspace = true
async-trait.workspace = true
futures.workspace = true
moka.workspace = true
dashmap.workspace = true
parking_lot.workspace = true
fred.workspace = true
sqlx.workspace = true
aes-gcm.workspace = true
rand.workspace = true
tracing.workspace = true
metrics.workspace = true

[dev-dependencies]
testcontainers.workspace = true
testcontainers-modules.workspace = true
tokio = { workspace = true, features = ["macros"] }
```

- [ ] **Step 2：lib.rs**

```rust
pub mod redis_client;
pub mod pg_client;
pub mod entity_store;
pub mod counter_store;
pub mod loader;
pub mod stream_consumer;
pub mod deduction;
pub mod encrypt;

pub use entity_store::EntityStore;
pub use counter_store::CounterStore;
pub use deduction::{DeductionState, DEDUCT, USER_CIRCUIT, UserCircuit};
```

- [ ] **Step 3：redis_client.rs**

```rust
use fred::clients::{RedisPool, SubscriberClient};
use fred::types::{RedisConfig, PerformanceConfig, ConnectionConfig, ReconnectPolicy};
use std::sync::Arc;

#[derive(Clone)]
pub struct RedisCtx {
    pub pool: Arc<RedisPool>,
}

impl RedisCtx {
    pub async fn connect(url: &str, pool_size: usize) -> anyhow::Result<Self> {
        let cfg = RedisConfig::from_url(url)?;
        let pool = RedisPool::new(cfg, Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::default()), pool_size)?;
        pool.init().await?;
        Ok(Self { pool: Arc::new(pool) })
    }
}
```

- [ ] **Step 4：pg_client.rs**

```rust
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(url: &str, max: u32) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .min_connections(2)
        .acquire_timeout(Duration::from_millis(200))
        .idle_timeout(Some(Duration::from_secs(600)))
        .connect(url).await?;
    Ok(pool)
}
```

- [ ] **Step 5：encrypt.rs（AES-256-GCM + KEK 管理，ADR-017）**

```rust
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct KekCache(pub Arc<HashMap<String, [u8; 32]>>);

#[derive(Debug, serde::Deserialize)]
pub struct EncryptedKey {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub auth_tag: Vec<u8>,
    pub kek_kid: String,
}

pub fn decrypt(enc: &EncryptedKey, cache: &KekCache) -> anyhow::Result<SecretString> {
    let kek = cache.0.get(&enc.kek_kid).ok_or_else(|| anyhow::anyhow!("kek not found: {}", enc.kek_kid))?;
    let cipher = Aes256Gcm::new(kek.into());
    let nonce = Nonce::from_slice(&enc.nonce);
    let mut combined = enc.ciphertext.clone();
    combined.extend_from_slice(&enc.auth_tag);
    let pt = cipher.decrypt(nonce, combined.as_ref())
        .map_err(|_| anyhow::anyhow!("decrypt failed"))?;
    let s = String::from_utf8(pt).map_err(|_| anyhow::anyhow!("utf8"))?;
    Ok(SecretString::from(s))
}

#[cfg(test)]
pub fn encrypt_for_test(plain: &str, kek: [u8; 32]) -> EncryptedKey {
    use aes_gcm::aead::AeadCore;
    let cipher = Aes256Gcm::new(&kek.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plain.as_bytes()).unwrap();
    let (cipher_only, tag) = ct.split_at(ct.len() - 16);
    EncryptedKey {
        ciphertext: cipher_only.to_vec(),
        nonce: nonce.to_vec(),
        auth_tag: tag.to_vec(),
        kek_kid: "test".to_string(),
    }
}
```

- [ ] **Step 6：counter_store.rs（Counter 协议读写）**

```rust
use fred::interfaces::*;
use super::redis_client::RedisCtx;

#[derive(Clone)]
pub struct CounterStore { pub redis: RedisCtx }

impl CounterStore {
    pub async fn get(&self, scope: &str, id: &str, field: &str) -> anyhow::Result<i64> {
        let key = format!("counter:{scope}:{id}:{field}");
        let v: Option<i64> = self.redis.pool.next().get(&key).await?;
        Ok(v.unwrap_or(0))
    }
    pub async fn incrby(&self, scope: &str, id: &str, field: &str, delta: i64) -> anyhow::Result<i64> {
        let key = format!("counter:{scope}:{id}:{field}");
        let v: i64 = self.redis.pool.next().incr_by(&key, delta).await?;
        Ok(v)
    }
}
```

- [ ] **Step 7：deduction.rs（三层余额 + UserCircuit）**

```rust
use dashmap::DashMap;
use gw_core::ids::UserId;
use once_cell::sync::Lazy;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::time::Instant;

pub struct DeductionState {
    pub base_micro:     AtomicI64,
    pub deducted_micro: AtomicI64,
    pub frozen_micro:   AtomicI64,
}

impl DeductionState {
    pub fn real(&self) -> i64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.base_micro.load(Relaxed) - self.deducted_micro.load(Relaxed) - self.frozen_micro.load(Relaxed)
    }
}

pub static DEDUCT: Lazy<DashMap<UserId, Arc<DeductionState>>> = Lazy::new(DashMap::new);

#[derive(Clone, Copy)]
pub enum UserCircuit { Closed, Open { until: Instant, reason: CircuitReason } }

#[derive(Clone, Copy)]
pub enum CircuitReason { BalanceZero, BannedUser }

pub static USER_CIRCUIT: Lazy<DashMap<UserId, UserCircuit>> = Lazy::new(DashMap::new);
```

> 注：需在 Cargo.toml 添加 `once_cell = "1.20"`，或改用 `std::sync::OnceLock`。

- [ ] **Step 8：entity_store.rs（9 个 Moka cache）**

```rust
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

pub struct EntityStore {
    pub user: Cache<UserId, Arc<Versioned<UserEntry>>>,
    pub user_group: Cache<UserGroupId, Arc<Versioned<UserGroup>>>,
    pub apikey: Cache<ApiKeyId, Arc<Versioned<ApiKeyEntry>>>,
    pub vendor: Cache<VendorId, Arc<Versioned<Vendor>>>,
    pub vendor_key: Cache<KeyId, Arc<Versioned<VendorKey>>>,
    pub model_group: Cache<Group, Arc<Versioned<ModelGroup>>>,
    pub pricing: Cache<ModelName, Arc<Versioned<Pricing>>>,
    pub jwks: Cache<Kid, Arc<Versioned<JwksEntry>>>,
    pub ipblock: Cache<String, Arc<Versioned<IpBlockEntry>>>,
}

impl EntityStore {
    pub fn new() -> Self {
        Self {
            user: Cache::builder().max_capacity(1_000_000).time_to_live(Duration::from_secs(300)).build(),
            user_group: Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(300)).build(),
            apikey: Cache::builder().max_capacity(100_000).time_to_live(Duration::from_secs(300)).build(),
            vendor: Cache::builder().max_capacity(100).time_to_live(Duration::from_secs(1800)).build(),
            vendor_key: Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(1800)).build(),
            model_group: Cache::builder().max_capacity(1_000).time_to_live(Duration::from_secs(1800)).build(),
            pricing: Cache::builder().max_capacity(1_000).time_to_live(Duration::from_secs(1800)).build(),
            jwks: Cache::builder().max_capacity(100).time_to_live(Duration::from_secs(3600)).build(),
            ipblock: Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(300)).build(),
        }
    }
}
```

- [ ] **Step 9：loader.rs（ADR-016 冷加载链）**

```rust
use fred::interfaces::HashesInterface;
use gw_core::versioned::Versioned;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use std::sync::Arc;
use super::redis_client::RedisCtx;
use super::encrypt::KekCache;

pub struct LoaderCtx {
    pub redis: RedisCtx,
    pub pg: PgPool,
    pub kek: KekCache,
}

/// Generic Entity 加载链。`namespace` 决定 Redis key / SQL table / TTL。
pub async fn load_entity<T>(
    ctx: &LoaderCtx,
    namespace: &str,
    id: &str,
    sql: &str,
    decode_fn: impl Fn(&serde_json::Value) -> anyhow::Result<T>,
) -> anyhow::Result<Versioned<T>>
where T: 'static + Send + Sync,
{
    let key = format!("entity:{namespace}:{id}");
    let client = ctx.redis.pool.next();
    let hash: std::collections::HashMap<String, String> = client.hgetall(&key).await?;
    if !hash.is_empty() {
        let v: u64 = hash.get("v").and_then(|s| s.parse().ok()).unwrap_or(0);
        let d = hash.get("d").ok_or_else(|| anyhow::anyhow!("missing d"))?;
        let parsed: serde_json::Value = serde_json::from_str(d)?;
        let value = decode_fn(&parsed)?;
        return Ok(Versioned { version: v, value: Arc::new(value) });
    }
    // L2: PgSQL
    let row: Option<(serde_json::Value, i64)> = sqlx::query_as(sql)
        .bind(id).fetch_optional(&ctx.pg).await?;
    let (payload, version) = row.ok_or_else(|| anyhow::anyhow!("not found in pg: {namespace}:{id}"))?;
    let value = decode_fn(&payload)?;
    let v = version as u64;
    // L3: 回写 Redis（带 TTL）
    let ttl = ttl_for(namespace);
    let _ = client.hset::<(), _, _>(&key, &[("v", v.to_string()), ("enc", "json".to_string()), ("d", payload.to_string())]).await;
    let _ = client.expire::<(), _>(&key, ttl as i64).await;
    Ok(Versioned { version: v, value: Arc::new(value) })
}

fn ttl_for(ns: &str) -> u64 {
    match ns {
        "user" | "user_group" | "apikey" | "ipblock" => 300,
        "vendor" | "vendor_key" | "model_group" | "pricing" => 1800,
        "jwks" => 3600,
        _ => 300,
    }
}
```

- [ ] **Step 10：stream_consumer.rs（ADR-007 启动锚点 + XREADGROUP）**

```rust
use fred::interfaces::*;
use gw_core::ids::*;
use std::sync::Arc;
use tokio::task::JoinHandle;
use super::redis_client::RedisCtx;
use super::entity_store::EntityStore;

pub struct StreamConsumer {
    pub redis: RedisCtx,
    pub store: Arc<EntityStore>,
    pub instance_id: String,
}

impl StreamConsumer {
    pub async fn start(self: Arc<Self>) -> anyhow::Result<JoinHandle<()>> {
        let client = self.redis.pool.next();
        let _: Result<(), _> = client.xgroup_create("events:entity", &self.instance_id, "$", true).await;
        let _: Result<(), _> = client.xgroup_create("events:counter", &self.instance_id, "$", true).await;
        let me = self.clone();
        let h = tokio::spawn(async move {
            me.consume_loop().await;
        });
        Ok(h)
    }

    async fn consume_loop(&self) {
        // 简化骨架：循环 XREADGROUP，每条事件应用到 EntityStore.upsert_if_newer
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            // TODO: XREADGROUP 实现
        }
    }
}
```

- [ ] **Step 11：migrations/0001_initial.sql（建表 SQL，§8.2 schema）**

完整建表（users / user_groups / api_keys / vendors / vendor_keys / model_groups / pricings / jwks / ip_blocks），每张表必含公共字段 `version BIGINT NOT NULL DEFAULT 0`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT now()`, `deleted_at TIMESTAMPTZ NULL`。SQL 内容直接抄 §8.2，省略复述。

- [ ] **Step 12：编译通过**

Run: `cargo check -p gw-store`
Expected: 无错误。

- [ ] **Step 13：提交**

```bash
git add crates/gw-store
git commit -m "feat(gw-store): redis/pgsql clients + EntityStore + loader chain + encrypt"
```

---

### Task 2B.2：`gw-store` 单元 + 集成测试（testcontainers）

**Files:**
- Create: `crates/gw-store/tests/encrypt_roundtrip.rs`
- Create: `crates/gw-store/tests/deduction.rs`
- Create: `crates/gw-store/tests/entity_loader.rs`
- Create: `crates/gw-store/tests/counter_store.rs`

- [ ] **Step 1：encrypt_roundtrip.rs（纯单元，无容器）**

```rust
use gw_store::encrypt::{decrypt, encrypt_for_test, KekCache};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn aes_gcm_encrypt_decrypt_roundtrip() {
    let kek = [42u8; 32];
    let enc = encrypt_for_test("sk-test-secret", kek);
    let mut map = HashMap::new();
    map.insert("test".to_string(), kek);
    let cache = KekCache(Arc::new(map));
    let plain = decrypt(&enc, &cache).unwrap();
    assert_eq!(plain.expose_secret(), "sk-test-secret");
}

#[test]
fn missing_kek_kid_fails() {
    let kek = [1u8; 32];
    let enc = encrypt_for_test("x", kek);
    let cache = KekCache(Arc::new(HashMap::new()));
    assert!(decrypt(&enc, &cache).is_err());
}
```

- [ ] **Step 2：deduction.rs**

```rust
use gw_store::deduction::DeductionState;
use std::sync::atomic::{AtomicI64, Ordering};

#[test]
fn real_balance_is_base_minus_deducted_minus_frozen() {
    let s = DeductionState {
        base_micro: AtomicI64::new(1000),
        deducted_micro: AtomicI64::new(200),
        frozen_micro: AtomicI64::new(100),
    };
    assert_eq!(s.real(), 700);
}
```

- [ ] **Step 3：entity_loader.rs（testcontainers Redis + Postgres）**

```rust
use gw_store::redis_client::RedisCtx;
use gw_store::pg_client;
use gw_store::loader::{load_entity, LoaderCtx};
use gw_store::encrypt::KekCache;
use std::collections::HashMap;
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

#[tokio::test]
async fn cold_load_from_pgsql_writes_back_to_redis() {
    let redis_node = Redis::default().start().await.unwrap();
    let pg_node = Postgres::default().start().await.unwrap();
    let redis_port = redis_node.get_host_port_ipv4(6379).await.unwrap();
    let pg_port = pg_node.get_host_port_ipv4(5432).await.unwrap();

    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4).await.unwrap();
    let pg = pg_client::connect(&format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"), 4).await.unwrap();

    sqlx::query("CREATE TABLE t (id TEXT PK, payload JSONB, version BIGINT NOT NULL DEFAULT 0)").execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO t VALUES ('u1', '{\"name\":\"alice\"}', 5)").execute(&pg).await.unwrap();

    let ctx = LoaderCtx { redis: redis.clone(), pg, kek: KekCache(Arc::new(HashMap::new())) };
    let v = load_entity(&ctx, "user", "u1",
        "SELECT payload, version FROM t WHERE id = $1",
        |val| -> anyhow::Result<String> { Ok(val["name"].as_str().unwrap().to_string()) }
    ).await.unwrap();

    assert_eq!(v.version, 5);
    assert_eq!(&*v.value, "alice");

    // 再次 load 应命中 Redis
    let v2 = load_entity(&ctx, "user", "u1",
        "SELECT payload, version FROM t WHERE id = $1",
        |val| -> anyhow::Result<String> { Ok(val["name"].as_str().unwrap().to_string()) }
    ).await.unwrap();
    assert_eq!(v2.version, 5);
}
```

- [ ] **Step 4：counter_store.rs（testcontainers Redis）**

```rust
use gw_store::counter_store::CounterStore;
use gw_store::redis_client::RedisCtx;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

#[tokio::test]
async fn incrby_returns_new_value() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 2).await.unwrap();
    let cs = CounterStore { redis };
    let v1 = cs.incrby("user", "u1", "balance_micro", 1000).await.unwrap();
    assert_eq!(v1, 1000);
    let v2 = cs.incrby("user", "u1", "balance_micro", -300).await.unwrap();
    assert_eq!(v2, 700);
    let g = cs.get("user", "u1", "balance_micro").await.unwrap();
    assert_eq!(g, 700);
}
```

- [ ] **Step 5：运行测试**

Run: `cargo test -p gw-store --features integration-tests || cargo test -p gw-store`
Expected: 全部通过。测试依赖 Docker；如本地无 Docker，加 `#[ignore]` 注解供 CI 跑。

- [ ] **Step 6：提交**

```bash
git add crates/gw-store/tests
git commit -m "test(gw-store): encrypt + deduction + loader + counter integration tests"
```

---

## Phase 3：业务三 crate（**三路并行**）

> Phase 3 包含三条独立路径：A（gw-auth）、B（gw-routing）、C（gw-ledger）。三者都依赖 `gw-core` + `gw-store`，互不依赖。可分配给三个 agent 并行执行。

### Path A：`gw-auth`

### Task 3A.1：crate 骨架 + JWT / APIKey 鉴权

**Files:**
- Create: `crates/gw-auth/Cargo.toml`
- Create: `crates/gw-auth/src/lib.rs`
- Create: `crates/gw-auth/src/jwt.rs`
- Create: `crates/gw-auth/src/apikey.rs`
- Create: `crates/gw-auth/src/merge.rs`
- Create: `crates/gw-auth/src/ip_filter.rs`
- Create: `crates/gw-auth/src/signature.rs`
- Create: `crates/gw-auth/src/ratelimit.rs`
- Create: `crates/gw-auth/src/hard_cap.rs`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-auth"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-store = { path = "../gw-store" }
serde.workspace = true
thiserror.workspace = true
tokio.workspace = true
async-trait.workspace = true
futures.workspace = true
parking_lot.workspace = true
dashmap.workspace = true
jsonwebtoken.workspace = true
governor.workspace = true
hmac.workspace = true
sha2.workspace = true
subtle.workspace = true
time.workspace = true
tracing.workspace = true
metrics.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros"] }
```

- [ ] **Step 2：lib.rs**

```rust
pub mod jwt;
pub mod apikey;
pub mod merge;
pub mod ip_filter;
pub mod signature;
pub mod ratelimit;
pub mod hard_cap;

pub struct AuthCtx {
    pub user_id: gw_core::ids::UserId,
    pub api_key_id: Option<gw_core::ids::ApiKeyId>,
    pub client_ip: std::net::IpAddr,
}

pub use merge::merge_permissions;
pub use ratelimit::{RateLimitGuard, RateLimitTicket, StreamSlot};
```

- [ ] **Step 3：jwt.rs（JWKS 缓存命中 < 200µs）**

```rust
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{Kid, UserId};
use gw_store::EntityStore;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::borrow::Cow;
use std::sync::Arc;

#[derive(Deserialize)]
struct Claims { sub: String, exp: usize }

pub async fn verify_jwt(token: &str, store: &EntityStore) -> Result<UserId, ErrorIR<'static>> {
    let hdr = decode_header(token).map_err(|_| auth_err("invalid jwt header"))?;
    let kid = hdr.kid.ok_or_else(|| auth_err("missing kid"))?;
    let jwk = store.jwks.get(&Kid::from(kid.clone())).await.ok_or_else(|| auth_err("kid not found"))?;
    let key = DecodingKey::from_rsa_pem(jwk.value.pem.as_bytes()).map_err(|_| auth_err("bad pem"))?;
    let mut val = Validation::new(Algorithm::RS256);
    val.validate_exp = true;
    let data = decode::<Claims>(token, &key, &val).map_err(|_| auth_err("jwt verify failed"))?;
    Ok(UserId::from(data.claims.sub))
}

fn auth_err(_msg: &'static str) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth, status: 401,
        upstream_status: None, code: Cow::Borrowed(codes::GW_AUTH_INVALID_JWT),
        message: Cow::Borrowed("auth failed"), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None,
    }
}
```

- [ ] **Step 4：apikey.rs（hash 比对，bcrypt / argon2）**

```rust
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::ApiKeyId;
use gw_store::EntityStore;
use std::borrow::Cow;
use subtle::ConstantTimeEq;

pub async fn verify_apikey(key: &str, key_id: &ApiKeyId, store: &EntityStore) -> Result<(), ErrorIR<'static>> {
    let entry = store.apikey.get(key_id).await.ok_or_else(|| err("not found"))?;
    let provided = sha2_hash(key.as_bytes());
    if provided.ct_eq(&entry.value.hash).into() {
        Ok(())
    } else {
        Err(err("hash mismatch"))
    }
}

fn sha2_hash(b: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().to_vec()
}

fn err(_: &'static str) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth, status: 401,
        upstream_status: None, code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
        message: Cow::Borrowed("api key invalid"), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None,
    }
}
```

- [ ] **Step 5：ip_filter.rs（黑/白名单，CIDR 匹配）**

```rust
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_store::EntityStore;
use std::borrow::Cow;
use std::net::IpAddr;

pub async fn check_blocklist(ip: IpAddr, store: &EntityStore) -> Result<(), ErrorIR<'static>> {
    // 简化：扫描 ipblock cache 中所有条目（v1 数据量小）
    // 命中 → 403
    // 实际实现可用 Patricia Trie 索引
    for entry in store.ipblock.iter() {
        if cidr_contains(&entry.1.value.cidr, ip) {
            return Err(blocked());
        }
    }
    Ok(())
}

fn cidr_contains(_cidr: &str, _ip: IpAddr) -> bool { false /* impl ipnet */ }

fn blocked() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth, status: 403,
        upstream_status: None, code: Cow::Borrowed(codes::GW_AUTH_IP_BLOCKED),
        message: Cow::Borrowed("ip blocked"), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None,
    }
}
```

> 需在 Cargo.toml 加 `ipnet = "2.10"`。

- [ ] **Step 6：signature.rs（HMAC-SHA256，常量时间比对）**

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub struct SignatureInput<'a> {
    pub method: &'a str, pub path: &'a str,
    pub timestamp: &'a str, pub body: &'a [u8],
}

pub fn verify(secret: &[u8], received: &[u8], input: &SignatureInput<'_>) -> bool {
    let body_hash = {
        use sha2::Digest;
        let mut h = Sha256::new();
        h.update(input.body);
        hex::encode(h.finalize())
    };
    let signing = format!("{}\n{}\n{}\n{}", input.method, input.path, input.timestamp, body_hash);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    expected.ct_eq(received).into()
}
```

> 需在 Cargo.toml 加 `hex = "0.4"`。

- [ ] **Step 7：merge.rs（ADR-023 五级合并）**

```rust
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_store::EntityStore;
use std::collections::HashSet;
use std::net::IpAddr;

use crate::AuthCtx;

pub async fn merge_permissions(
    ctx: &AuthCtx,
    store: &EntityStore,
    global_rpm: u32,
) -> anyhow::Result<MergedPermissions> {
    let user = store.user.get(&ctx.user_id).await.ok_or_else(|| anyhow::anyhow!("user not found"))?;
    let main = match &user.value.parent_id {
        Some(p) => store.user.get(p).await.ok_or_else(|| anyhow::anyhow!("parent not found"))?,
        None => user.clone(),
    };
    let mut groups = Vec::new();
    for gid in &main.value.user_group_ids {
        if let Some(g) = store.user_group.get(gid).await { groups.push(g); }
    }
    let api_key = if let Some(akid) = &ctx.api_key_id {
        store.apikey.get(akid).await
    } else { None };

    let mut allowed_models: HashSet<ModelName> = user.value.allowed_models.iter().cloned().collect();
    intersect(&mut allowed_models, main.value.allowed_models.iter());
    for g in &groups { intersect(&mut allowed_models, g.value.allowed_models.iter()); }
    if let Some(ak) = &api_key { intersect(&mut allowed_models, ak.value.allowed_models.iter()); }

    let rpm = min_pos(&[user.value.rpm_limit, main.value.rpm_limit, groups.iter().map(|g| g.value.rpm_limit).min().unwrap_or(0),
        api_key.as_ref().map(|a| a.value.rpm_limit).unwrap_or(0), global_rpm]);
    let tpm = min_pos(&[user.value.tpm_limit, main.value.tpm_limit, groups.iter().map(|g| g.value.tpm_limit).min().unwrap_or(0),
        api_key.as_ref().map(|a| a.value.tpm_limit).unwrap_or(0)]);

    let markup = user.value.markup * main.value.markup
        * groups.iter().fold(1.0_f32, |a, g| a * g.value.markup)
        * api_key.as_ref().map(|a| a.value.markup).unwrap_or(1.0);

    let forced = groups.iter().max_by_key(|g| g.value.priority).and_then(|g| g.value.model_group_binding.clone());

    let effective = user.value.parent_id.clone().unwrap_or(ctx.user_id.clone());

    Ok(MergedPermissions {
        user_id: ctx.user_id.clone(), effective_user_id: effective, client_ip: ctx.client_ip,
        allowed_models, allowed_channels: HashSet::new(), allowed_groups: HashSet::new(),
        forced_model_group: forced,
        rpm_limit: rpm, tpm_limit: tpm,
        max_concurrent_streams: main.value.max_concurrent_streams,
        markup_product: markup,
    })
}

fn intersect<'a, I: IntoIterator<Item = &'a ModelName>>(set: &mut HashSet<ModelName>, other: I) {
    let o: HashSet<_> = other.into_iter().cloned().collect();
    if !o.is_empty() {
        set.retain(|m| o.contains(m));
    }
}

fn min_pos(vals: &[u32]) -> u32 {
    vals.iter().filter(|v| **v > 0).min().copied().unwrap_or(0)
}
```

- [ ] **Step 8：ratelimit.rs（governor + RAII Ticket）**

```rust
use dashmap::DashMap;
use governor::{Quota, RateLimiter, DefaultDirectRateLimiter};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct RateLimitGuard {
    pub per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_streams: DashMap<UserId, Arc<AtomicUsize>>,
    pub total_streams: AtomicUsize,
}

impl RateLimitGuard {
    pub fn new() -> Self { Self { per_user: DashMap::new(), per_user_streams: DashMap::new(), total_streams: AtomicUsize::new(0) } }

    pub fn check(&self, perm: &MergedPermissions) -> Result<RateLimitTicket, ErrorIR<'static>> {
        if perm.rpm_limit > 0 {
            let lim = self.per_user.entry(perm.effective_user_id.clone()).or_insert_with(|| {
                let q = Quota::per_minute(NonZeroU32::new(perm.rpm_limit).unwrap());
                Arc::new(RateLimiter::direct(q))
            }).clone();
            if lim.check().is_err() {
                return Err(rpm_err());
            }
        }
        Ok(RateLimitTicket { _placeholder: () })
    }

    pub fn check_stream_slot(&self, uid: &UserId, max: u32) -> Result<StreamSlot, ErrorIR<'static>> {
        let counter = self.per_user_streams.entry(uid.clone()).or_insert_with(|| Arc::new(AtomicUsize::new(0))).clone();
        let n = counter.fetch_add(1, Ordering::AcqRel);
        if max > 0 && n + 1 > max as usize {
            counter.fetch_sub(1, Ordering::AcqRel);
            return Err(concurrent_err());
        }
        self.total_streams.fetch_add(1, Ordering::Relaxed);
        Ok(StreamSlot { uid: uid.clone(), counter, total: &self.total_streams as *const _ })
    }
}

pub struct RateLimitTicket { _placeholder: () }

pub struct StreamSlot {
    pub uid: UserId,
    pub counter: Arc<AtomicUsize>,
    pub total: *const AtomicUsize,
}
unsafe impl Send for StreamSlot {}
unsafe impl Sync for StreamSlot {}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        unsafe { (*self.total).fetch_sub(1, Ordering::Relaxed); }
    }
}

fn rpm_err() -> ErrorIR<'static> {
    ErrorIR { category: ErrorCategory::ClientQuota, status: 429,
        upstream_status: None, code: Cow::Borrowed(codes::GW_QUOTA_RPM_EXCEEDED),
        message: Cow::Borrowed("rpm exceeded"), retryable: true,
        upstream_raw: None, request_id: "", retry_after_secs: Some(60) }
}

fn concurrent_err() -> ErrorIR<'static> {
    ErrorIR { category: ErrorCategory::ClientQuota, status: 429,
        upstream_status: None, code: Cow::Borrowed(codes::GW_QUOTA_CONCURRENT_EXCEEDED),
        message: Cow::Borrowed("concurrent streams exceeded"), retryable: true,
        upstream_raw: None, request_id: "", retry_after_secs: Some(5) }
}
```

- [ ] **Step 9：hard_cap.rs（ADR-024 单请求 hard cap）**

```rust
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use std::borrow::Cow;

pub struct HardCapCfg { pub max_input_tokens: u32, pub max_cost_micro: u64 }

pub fn check_input_tokens(est: u32, cfg: &HardCapCfg) -> Result<(), ErrorIR<'static>> {
    if est <= cfg.max_input_tokens { return Ok(()); }
    Err(ErrorIR { category: ErrorCategory::ClientBadRequest, status: 413,
        upstream_status: None, code: Cow::Borrowed(codes::GW_REQUEST_INPUT_TOO_LARGE),
        message: Cow::Borrowed("input too large"), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None })
}

pub fn check_cost(est_micro: u64, cfg: &HardCapCfg) -> Result<(), ErrorIR<'static>> {
    if est_micro <= cfg.max_cost_micro { return Ok(()); }
    Err(ErrorIR { category: ErrorCategory::ClientBadRequest, status: 413,
        upstream_status: None, code: Cow::Borrowed(codes::GW_REQUEST_COST_TOO_HIGH),
        message: Cow::Borrowed("cost too high"), retryable: false,
        upstream_raw: None, request_id: "", retry_after_secs: None })
}
```

- [ ] **Step 10：编译通过**

Run: `cargo check -p gw-auth`
Expected: 无错误。可能需要调整 governor / store API（EntityStore 上添加 `pub async fn get(&self, k: &K) -> Option<Arc<Versioned<V>>>` 与 `iter()` 等便捷方法到 gw-store）。

- [ ] **Step 11：提交**

```bash
git add crates/gw-auth crates/gw-store
git commit -m "feat(gw-auth): jwt/apikey/merge/ip/signature/ratelimit/hard_cap"
```

---

### Task 3A.2：`gw-auth` 单元测试

**Files:**
- Create: `crates/gw-auth/tests/merge.rs`
- Create: `crates/gw-auth/tests/signature.rs`
- Create: `crates/gw-auth/tests/ratelimit.rs`
- Create: `crates/gw-auth/tests/hard_cap.rs`

- [ ] **Step 1：merge.rs（构造 EntityStore + mock 数据测交集 / min）**

```rust
use gw_auth::merge_permissions;
use gw_auth::AuthCtx;
// 用 EntityStore::new()，直接通过 `entity.user.insert(...)` 注入测试数据
// 验证 AC-023-1: 用户 + 分组 + APIKey 三方交集
// 验证 AC-023-2: RPM 取 min
// 验证 AC-023-3: 子账户 effective_user_id = parent_id
// （完整代码因篇幅省略，模式与 `entity_loader.rs` 一致）
```

- [ ] **Step 2：signature.rs**

```rust
use gw_auth::signature::*;

#[test]
fn signature_match() {
    let secret = b"my-secret";
    let body = b"{\"x\":1}";
    let input = SignatureInput { method: "POST", path: "/v1/chat", timestamp: "1716000000", body };
    let body_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new(); h.update(body); hex::encode(h.finalize())
    };
    let signing = format!("POST\n/v1/chat\n1716000000\n{body_hash}");
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    assert!(verify(secret, &expected, &input));
}

#[test]
fn signature_mismatch_with_tampered_body() {
    let secret = b"s";
    let input1 = SignatureInput { method: "POST", path: "/x", timestamp: "1", body: b"a" };
    let input2 = SignatureInput { method: "POST", path: "/x", timestamp: "1", body: b"b" };
    // 用 input1 算签名，对 input2 验签
    use hmac::{Hmac, Mac}; use sha2::{Digest, Sha256};
    let body_hash = { let mut h = Sha256::new(); h.update(b"a"); hex::encode(h.finalize()) };
    let s = format!("POST\n/x\n1\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(s.as_bytes());
    let sig = mac.finalize().into_bytes();
    assert!(!verify(secret, &sig, &input2));
}
```

- [ ] **Step 3：ratelimit.rs**

```rust
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::UserId;
use gw_core::permissions::MergedPermissions;
use std::collections::HashSet;

fn perm(rpm: u32) -> MergedPermissions {
    MergedPermissions {
        user_id: UserId::from("u1"), effective_user_id: UserId::from("u1"),
        client_ip: "127.0.0.1".parse().unwrap(),
        allowed_models: HashSet::new(), allowed_channels: HashSet::new(), allowed_groups: HashSet::new(),
        forced_model_group: None, rpm_limit: rpm, tpm_limit: 0, max_concurrent_streams: 100, markup_product: 1.0,
    }
}

#[test]
fn rpm_within_limit_passes() {
    let g = RateLimitGuard::new();
    let p = perm(60);
    for _ in 0..5 { let _ = g.check(&p).unwrap(); }
}

#[test]
fn stream_slot_rai_decrements() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    {
        let _slot = g.check_stream_slot(&uid, 1).unwrap();
        // 第二个槽位应被拒
        assert!(g.check_stream_slot(&uid, 1).is_err());
    }
    // RAII drop 后槽位回到 0，重新允许
    let _slot = g.check_stream_slot(&uid, 1).unwrap();
}
```

- [ ] **Step 4：hard_cap.rs**

```rust
use gw_auth::hard_cap::*;

#[test]
fn input_tokens_under_cap_pass() {
    let cfg = HardCapCfg { max_input_tokens: 2_000_000, max_cost_micro: 100_000_000 };
    assert!(check_input_tokens(1000, &cfg).is_ok());
}

#[test]
fn input_tokens_over_cap_fails() {
    let cfg = HardCapCfg { max_input_tokens: 100, max_cost_micro: 1_000_000 };
    let err = check_input_tokens(101, &cfg).unwrap_err();
    assert_eq!(err.status, 413);
}
```

- [ ] **Step 5：运行测试**

Run: `cargo test -p gw-auth`
Expected: 全部通过。

- [ ] **Step 6：提交**

```bash
git add crates/gw-auth/tests
git commit -m "test(gw-auth): merge/signature/ratelimit/hard_cap unit tests"
```

---

### Path B：`gw-routing`

### Task 3B.1：crate 骨架 + Strategy trait + 8 实现 + BreakerStore + RetryExecutor

**Files:**
- Create: `crates/gw-routing/Cargo.toml`
- Create: `crates/gw-routing/src/lib.rs`
- Create: `crates/gw-routing/src/breaker.rs`
- Create: `crates/gw-routing/src/strategy/mod.rs`
- Create: `crates/gw-routing/src/strategy/fixed_order.rs`
- Create: `crates/gw-routing/src/strategy/round_robin.rs`
- Create: `crates/gw-routing/src/strategy/load_balance.rs`
- Create: `crates/gw-routing/src/strategy/weighted_rr.rs`
- Create: `crates/gw-routing/src/strategy/cost_first.rs`
- Create: `crates/gw-routing/src/strategy/latency_first.rs`
- Create: `crates/gw-routing/src/strategy/cache_affinity.rs`
- Create: `crates/gw-routing/src/strategy/sticky.rs`
- Create: `crates/gw-routing/src/retry.rs`
- Create: `crates/gw-routing/src/candidates.rs`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-routing"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-store = { path = "../gw-store" }
serde.workspace = true
tokio.workspace = true
parking_lot.workspace = true
dashmap.workspace = true
async-trait.workspace = true
tracing.workspace = true
metrics.workspace = true
rand.workspace = true
sha2.workspace = true
```

- [ ] **Step 2：lib.rs**

```rust
pub mod breaker;
pub mod strategy;
pub mod retry;
pub mod candidates;
pub use breaker::{BreakerState, BreakerStore};
pub use strategy::{Strategy, StrategyAny, PickCtx};
pub use retry::{RetryConfig, RetryMode, RetryExecutor};
```

- [ ] **Step 3：breaker.rs（ADR-008 30s 滑动窗口 + half-open）**

```rust
use dashmap::DashMap;
use gw_core::ids::KeyId;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct SlidingWindow {
    pub samples: std::collections::VecDeque<(Instant, bool)>, // bool=success
}
impl SlidingWindow {
    pub fn new() -> Self { Self { samples: std::collections::VecDeque::with_capacity(256) } }
    pub fn record(&mut self, ok: bool) {
        let now = Instant::now();
        self.samples.push_back((now, ok));
        let cutoff = now - Duration::from_secs(30);
        while self.samples.front().map(|(t, _)| *t < cutoff).unwrap_or(false) { self.samples.pop_front(); }
    }
    pub fn failure_rate(&self) -> (usize, f32) {
        let total = self.samples.len();
        if total == 0 { return (0, 0.0); }
        let fail = self.samples.iter().filter(|(_, ok)| !ok).count();
        (total, fail as f32 / total as f32)
    }
}

pub struct BreakerState {
    pub failure_window: Mutex<SlidingWindow>,
    pub circuit: AtomicU8,        // 0 closed, 1 open, 2 half-open
    pub open_until_ms: AtomicU64,
    pub in_flight: AtomicU32,
}

impl BreakerState {
    pub fn new() -> Self {
        Self { failure_window: Mutex::new(SlidingWindow::new()),
            circuit: AtomicU8::new(0), open_until_ms: AtomicU64::new(0), in_flight: AtomicU32::new(0) }
    }
    pub fn record(&self, ok: bool) {
        let mut w = self.failure_window.lock();
        w.record(ok);
        let (total, rate) = w.failure_rate();
        if total >= 20 && rate > 0.5 {
            self.circuit.store(1, Ordering::Release);
            let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            self.open_until_ms.store(now_ms + 60_000, Ordering::Release);
        }
    }
    pub fn is_available(&self) -> bool {
        let c = self.circuit.load(Ordering::Acquire);
        if c == 0 { return true; }
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        if now_ms >= self.open_until_ms.load(Ordering::Acquire) {
            self.circuit.compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire).ok();
            return true;
        }
        false
    }
}

#[derive(Default)]
pub struct BreakerStore { pub by_key: DashMap<KeyId, Arc<BreakerState>> }
impl BreakerStore {
    pub fn get_or_create(&self, k: &KeyId) -> Arc<BreakerState> {
        self.by_key.entry(k.clone()).or_insert_with(|| Arc::new(BreakerState::new())).clone()
    }
    pub fn in_flight(&self, k: &KeyId) -> u32 {
        self.get_or_create(k).in_flight.load(Ordering::Relaxed)
    }
}
```

- [ ] **Step 4：strategy/mod.rs + PickCtx + Strategy trait**

```rust
use gw_core::entity::VendorKey;
use gw_core::ids::*;
use std::sync::Arc;
use crate::breaker::BreakerStore;

pub mod fixed_order;
pub mod round_robin;
pub mod load_balance;
pub mod weighted_rr;
pub mod cost_first;
pub mod latency_first;
pub mod cache_affinity;
pub mod sticky;

pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
}

pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn pick<'a>(&self, candidates: &'a [Arc<VendorKey>], breakers: &BreakerStore, ctx: &PickCtx<'_>) -> Option<&'a Arc<VendorKey>>;
}

pub enum StrategyAny {
    FixedOrder(fixed_order::FixedOrder),
    RoundRobin(round_robin::RoundRobin),
    LoadBalance(load_balance::LoadBalance),
    WeightedRr(weighted_rr::WeightedRr),
    CostFirst(cost_first::CostFirst),
    LatencyFirst(latency_first::LatencyFirst),
    CacheAffinity(cache_affinity::CacheAffinity),
    Sticky(sticky::Sticky),
}

impl Strategy for StrategyAny {
    fn name(&self) -> &'static str {
        match self {
            StrategyAny::FixedOrder(s) => s.name(),
            StrategyAny::RoundRobin(s) => s.name(),
            StrategyAny::LoadBalance(s) => s.name(),
            StrategyAny::WeightedRr(s) => s.name(),
            StrategyAny::CostFirst(s) => s.name(),
            StrategyAny::LatencyFirst(s) => s.name(),
            StrategyAny::CacheAffinity(s) => s.name(),
            StrategyAny::Sticky(s) => s.name(),
        }
    }
    fn pick<'a>(&self, c: &'a [Arc<VendorKey>], b: &BreakerStore, ctx: &PickCtx<'_>) -> Option<&'a Arc<VendorKey>> {
        match self {
            StrategyAny::FixedOrder(s) => s.pick(c, b, ctx),
            StrategyAny::RoundRobin(s) => s.pick(c, b, ctx),
            StrategyAny::LoadBalance(s) => s.pick(c, b, ctx),
            StrategyAny::WeightedRr(s) => s.pick(c, b, ctx),
            StrategyAny::CostFirst(s) => s.pick(c, b, ctx),
            StrategyAny::LatencyFirst(s) => s.pick(c, b, ctx),
            StrategyAny::CacheAffinity(s) => s.pick(c, b, ctx),
            StrategyAny::Sticky(s) => s.pick(c, b, ctx),
        }
    }
}
```

- [ ] **Step 5：strategy/fixed_order.rs**

```rust
use super::*;
pub struct FixedOrder;
impl Strategy for FixedOrder {
    fn name(&self) -> &'static str { "fixed_order" }
    fn pick<'a>(&self, candidates: &'a [Arc<VendorKey>], b: &BreakerStore, _ctx: &PickCtx<'_>) -> Option<&'a Arc<VendorKey>> {
        let mut sorted: Vec<_> = candidates.iter().enumerate().collect();
        sorted.sort_by_key(|(_, k)| -k.priority);
        for (i, _) in &sorted { if b.get_or_create(&candidates[*i].key_id).is_available() { return Some(&candidates[*i]); } }
        None
    }
}
```

- [ ] **Step 6：strategy/round_robin.rs**

```rust
use super::*;
use dashmap::DashMap;
use gw_core::ids::ModelName;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RoundRobin { pub counters: DashMap<ModelName, AtomicUsize> }
impl RoundRobin { pub fn new() -> Self { Self { counters: DashMap::new() } } }
impl Strategy for RoundRobin {
    fn name(&self) -> &'static str { "round_robin" }
    fn pick<'a>(&self, c: &'a [Arc<VendorKey>], b: &BreakerStore, ctx: &PickCtx<'_>) -> Option<&'a Arc<VendorKey>> {
        if c.is_empty() { return None; }
        let counter = self.counters.entry(ctx.model.clone()).or_insert_with(|| AtomicUsize::new(0));
        for _ in 0..c.len() {
            let idx = counter.fetch_add(1, Ordering::Relaxed) % c.len();
            if b.get_or_create(&c[idx].key_id).is_available() { return Some(&c[idx]); }
        }
        None
    }
}
```

- [ ] **Step 7：其余 6 个 strategy 实现**

每个 strategy 文件包含 `pub struct X;` + `impl Strategy for X { fn name; fn pick }`。

- `load_balance.rs`：`score = α·in_flight + β·avg_latency + γ·error_rate`，取最低分。
- `weighted_rr.rs`：SWRR 平滑加权轮询，按 `vendor_key.weight` 计算。
- `cost_first.rs`：按 Key.markup 升序。
- `latency_first.rs`：维护 TTFT histogram，挑 P50 最低。
- `cache_affinity.rs`：对 messages hash + 采样参数，hash mod len。
- `sticky.rs`：基于 `conversation_id`，hash 到固定 Key（TTL 30min 内存 map）。

每个 strategy 至少占 30-60 行，骨架相似，给出完整代码模板即可。

- [ ] **Step 8：retry.rs（ADR-022 RetryExecutor + 退避）**

```rust
use gw_core::entity::VendorKey;
use gw_core::error::ErrorIR;
use gw_core::ids::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum RetryMode { SameKey, CrossKey, Mixed }

#[derive(Clone)]
pub struct RetryConfig {
    pub mode: RetryMode,
    pub same_key_max: u32,
    pub cross_key_max: u32,
    pub backoff_base_ms: u32,
    pub backoff_max_ms: u32,
    pub total_budget_ms_nonstream: u32,
    pub total_budget_ms_stream: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self { mode: RetryMode::Mixed, same_key_max: 2, cross_key_max: 3,
            backoff_base_ms: 500, backoff_max_ms: 10_000,
            total_budget_ms_nonstream: 60_000, total_budget_ms_stream: 600_000 }
    }
}

pub struct RetryExecutor {
    pub config: RetryConfig,
    pub visited: HashSet<KeyId>,
    pub attempts: u32,
    pub start: Instant,
    pub idempotent: bool,
    pub is_stream: bool,
}

impl RetryExecutor {
    pub fn new(config: RetryConfig, idempotent: bool, is_stream: bool) -> Self {
        Self { config, visited: HashSet::new(), attempts: 0, start: Instant::now(), idempotent, is_stream }
    }

    pub fn should_retry(&mut self, err: &ErrorIR<'_>, bytes_sent_to_client: u64) -> bool {
        if !err.retryable { return false; }
        if self.is_stream && bytes_sent_to_client > 0 { return false; }
        if !self.idempotent && err.category != gw_core::error::ErrorCategory::UpstreamTimeout { return false; }
        let budget = if self.is_stream { self.config.total_budget_ms_stream } else { self.config.total_budget_ms_nonstream };
        if self.start.elapsed() > Duration::from_millis(budget as u64) { return false; }
        if self.attempts >= self.config.cross_key_max { return false; }
        self.attempts += 1;
        true
    }

    pub fn backoff(&self) -> Duration {
        let n = self.attempts as u32;
        let ms = (self.config.backoff_base_ms.saturating_mul(1u32 << n.min(8))).min(self.config.backoff_max_ms);
        let jitter = (rand::random::<u32>() % (self.config.backoff_base_ms / 2 + 1)) as u64;
        Duration::from_millis(ms as u64 + jitter)
    }

    pub fn mark_visited(&mut self, k: &KeyId) { self.visited.insert(k.clone()); }
    pub fn was_visited(&self, k: &KeyId) -> bool { self.visited.contains(k) }
}
```

- [ ] **Step 9：candidates.rs（get_candidate_keys，ADR-021）**

```rust
use gw_core::entity::{KeyStatus, ModelGroup, VendorKey};
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_store::EntityStore;
use std::sync::Arc;
use crate::breaker::BreakerStore;

pub async fn get_candidate_keys(
    group: &ModelGroup,
    model: &ModelName,
    target_protocol: ProtocolKind,
    perm: &MergedPermissions,
    breakers: &BreakerStore,
    store: &EntityStore,
    breaker_in_flight_per_key: u32,
) -> Vec<Arc<VendorKey>> {
    let mut out = Vec::with_capacity(group.vendor_key_ids.len());
    for kid in &group.vendor_key_ids {
        let Some(v) = store.vendor_key.get(kid).await else { continue };
        let key = &v.value;
        if !matches!(key.status, KeyStatus::Active) { continue; }
        if !key.accessible_models.contains(model) { continue; }
        if !(key.accessible_endpoints.contains(&target_protocol) || key.protocol_conversion) { continue; }
        if !perm.allowed_channels.is_empty() && !perm.allowed_channels.contains(&key.key_id) { continue; }
        if breakers.in_flight(&key.key_id) >= breaker_in_flight_per_key { continue; }
        // Arc<Versioned<VendorKey>> → Arc<VendorKey>
        out.push(Arc::new(VendorKey {
            key_id: key.key_id.clone(), vendor_id: key.vendor_id.clone(),
            api_key: secrecy::SecretString::from(secrecy::ExposeSecret::expose_secret(&key.api_key).clone()),
            accessible_models: key.accessible_models.clone(),
            model_mapping: key.model_mapping.clone(),
            accessible_endpoints: key.accessible_endpoints.clone(),
            protocol_conversion: key.protocol_conversion,
            priority: key.priority, weight: key.weight,
            rpm_limit: key.rpm_limit, tpm_limit: key.tpm_limit,
            max_usage_count: key.max_usage_count, proxy: key.proxy.clone(),
            ip_allowlist: key.ip_allowlist.clone(), expires_at: key.expires_at,
            status: key.status.clone(), tags: key.tags.clone(),
        }));
    }
    out
}
```

- [ ] **Step 10：编译通过**

Run: `cargo check -p gw-routing`
Expected: 无错误。

- [ ] **Step 11：提交**

```bash
git add crates/gw-routing
git commit -m "feat(gw-routing): Strategy x8 + BreakerStore + RetryExecutor + candidate filter"
```

---

### Task 3B.2：`gw-routing` 单元测试

**Files:**
- Create: `crates/gw-routing/tests/breaker.rs`
- Create: `crates/gw-routing/tests/strategies.rs`
- Create: `crates/gw-routing/tests/retry.rs`

- [ ] **Step 1：breaker.rs**

```rust
use gw_routing::breaker::*;

#[test]
fn closed_until_failure_rate_exceeds() {
    let b = BreakerState::new();
    for _ in 0..15 { b.record(false); }
    assert!(b.is_available());  // sample < 20
    for _ in 0..10 { b.record(false); }  // 25 samples 100% fail
    assert!(!b.is_available());  // open
}

#[test]
fn open_to_half_open_after_60s() {
    use std::sync::atomic::Ordering;
    let b = BreakerState::new();
    for _ in 0..25 { b.record(false); }
    assert!(!b.is_available());
    // 强制 open_until 立即过期
    b.open_until_ms.store(0, Ordering::Release);
    assert!(b.is_available());
    assert_eq!(b.circuit.load(Ordering::Acquire), 2);  // half-open
}
```

- [ ] **Step 2：strategies.rs**

```rust
use gw_core::entity::*;
use gw_core::ids::*;
use gw_routing::breaker::BreakerStore;
use gw_routing::strategy::*;
use std::sync::Arc;

fn fake_key(id: &str, prio: i32) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id), vendor_id: VendorId::from("v1"),
        api_key: secrecy::SecretString::from("sk".to_string()),
        accessible_models: vec![], model_mapping: Default::default(),
        accessible_endpoints: vec![], protocol_conversion: false,
        priority: prio, weight: 1, rpm_limit: 0, tpm_limit: 0,
        max_usage_count: None, proxy: None, ip_allowlist: None,
        expires_at: None, status: KeyStatus::Active, tags: vec![],
    })
}

#[test]
fn fixed_order_picks_highest_priority() {
    use gw_routing::strategy::fixed_order::FixedOrder;
    let keys = vec![fake_key("a", 0), fake_key("b", 10), fake_key("c", 5)];
    let b = BreakerStore::default();
    let ctx = PickCtx { user_id: &UserId::from("u"), model: &ModelName::from("m"), conversation_id: None, message_hash: None };
    let picked = FixedOrder.pick(&keys, &b, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "b");
}

#[test]
fn round_robin_rotates() {
    use gw_routing::strategy::round_robin::RoundRobin;
    let keys = vec![fake_key("a", 0), fake_key("b", 0), fake_key("c", 0)];
    let b = BreakerStore::default();
    let rr = RoundRobin::new();
    let ctx = PickCtx { user_id: &UserId::from("u"), model: &ModelName::from("m"), conversation_id: None, message_hash: None };
    let p1 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let p2 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let p3 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let mut set = std::collections::HashSet::new();
    set.insert(p1); set.insert(p2); set.insert(p3);
    assert_eq!(set.len(), 3);
}
```

- [ ] **Step 3：retry.rs**

```rust
use gw_core::error::*;
use gw_routing::retry::*;
use std::borrow::Cow;

fn err(retryable: bool, cat: ErrorCategory) -> ErrorIR<'static> {
    ErrorIR { category: cat, status: 500, upstream_status: Some(500),
        code: Cow::Borrowed("x"), message: Cow::Borrowed(""), retryable,
        upstream_raw: None, request_id: "", retry_after_secs: None }
}

#[test]
fn non_retryable_skips() {
    let mut r = RetryExecutor::new(RetryConfig::default(), true, false);
    assert!(!r.should_retry(&err(false, ErrorCategory::ClientBadRequest), 0));
}

#[test]
fn stream_after_first_chunk_no_retry() {
    let mut r = RetryExecutor::new(RetryConfig::default(), true, true);
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 100));
}

#[test]
fn non_idempotent_only_retries_on_timeout() {
    let mut r = RetryExecutor::new(RetryConfig::default(), false, false);
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 0));
    let mut r2 = RetryExecutor::new(RetryConfig::default(), false, false);
    assert!(r2.should_retry(&err(true, ErrorCategory::UpstreamTimeout), 0));
}

#[test]
fn budget_exhaustion_stops() {
    let cfg = RetryConfig { total_budget_ms_nonstream: 0, ..Default::default() };
    let mut r = RetryExecutor::new(cfg, true, false);
    std::thread::sleep(std::time::Duration::from_millis(1));
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 0));
}
```

- [ ] **Step 4：运行测试**

Run: `cargo test -p gw-routing`
Expected: 全部通过。

- [ ] **Step 5：提交**

```bash
git add crates/gw-routing/tests
git commit -m "test(gw-routing): breaker + strategies + retry unit tests"
```

---

### Path C：`gw-ledger`

### Task 3C.1：crate 骨架 + BillingRecord + FreezeGuard + mpsc flusher

**Files:**
- Create: `crates/gw-ledger/Cargo.toml`
- Create: `crates/gw-ledger/src/lib.rs`
- Create: `crates/gw-ledger/src/billing.rs`
- Create: `crates/gw-ledger/src/freeze.rs`
- Create: `crates/gw-ledger/src/flusher.rs`
- Create: `crates/gw-ledger/src/log_sink.rs`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-ledger"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-store = { path = "../gw-store" }
serde.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["sync", "rt-multi-thread", "time", "macros"] }
async-trait.workspace = true
time.workspace = true
fred.workspace = true
tracing.workspace = true
metrics.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "test-util"] }
testcontainers.workspace = true
testcontainers-modules.workspace = true
```

- [ ] **Step 2：lib.rs**

```rust
pub mod billing;
pub mod freeze;
pub mod flusher;
pub mod log_sink;
pub use billing::BillingRecord;
pub use freeze::FreezeGuard;
pub use flusher::Ledger;
```

- [ ] **Step 3：billing.rs（ADR-026 simple_log schema）**

```rust
use gw_core::ids::*;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct BillingRecord {
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub status: BillingStatus,
    pub error_code: Option<String>,
    pub http_status: u16,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cost_micro: u64,
    pub freeze_amount_micro: u64,
    pub markup_product: f32,
    pub usage_source: UsageSource,
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum BillingStatus { Ok, Err, Cancelled }

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource { Upstream, Estimated, FallbackConservative }
```

- [ ] **Step 4：freeze.rs（FreezeGuard RAII，ADR-004）**

```rust
use gw_core::ids::UserId;
use gw_store::deduction::{DeductionState, DEDUCT};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct FreezeGuard {
    pub uid: UserId,
    pub freeze_amount: u64,
    pub settled: AtomicBool,
    pub state: Arc<DeductionState>,
}

impl FreezeGuard {
    pub fn freeze(uid: UserId, amount: u64) -> Self {
        let state = DEDUCT.entry(uid.clone()).or_insert_with(|| Arc::new(DeductionState {
            base_micro: std::sync::atomic::AtomicI64::new(0),
            deducted_micro: std::sync::atomic::AtomicI64::new(0),
            frozen_micro: std::sync::atomic::AtomicI64::new(0),
        })).clone();
        state.frozen_micro.fetch_add(amount as i64, Ordering::AcqRel);
        Self { uid, freeze_amount: amount, settled: AtomicBool::new(false), state }
    }

    pub fn settle(&self, actual_micro: u64) {
        if self.settled.swap(true, Ordering::AcqRel) { return; }
        self.state.frozen_micro.fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
        self.state.deducted_micro.fetch_add(actual_micro as i64, Ordering::AcqRel);
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if !self.settled.load(Ordering::Acquire) {
            self.state.frozen_micro.fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}
```

- [ ] **Step 5：flusher.rs（mpsc + 批量 PIPELINE flush）**

```rust
use crate::billing::BillingRecord;
use fred::interfaces::*;
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

pub struct Ledger { pub tx: mpsc::Sender<BillingRecord> }

impl Ledger {
    pub fn start(redis: RedisCtx, channel_capacity: usize, batch_size: usize, flush_interval: Duration) -> Self {
        let (tx, mut rx) = mpsc::channel::<BillingRecord>(channel_capacity);
        tokio::spawn(async move {
            let mut buf: Vec<BillingRecord> = Vec::with_capacity(batch_size);
            let mut ticker = interval(flush_interval);
            loop {
                tokio::select! {
                    Some(rec) = rx.recv() => {
                        buf.push(rec);
                        if buf.len() >= batch_size {
                            flush(&redis, &mut buf).await;
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() { flush(&redis, &mut buf).await; }
                    }
                    else => break,
                }
            }
        });
        Self { tx }
    }

    pub fn try_send(&self, rec: BillingRecord) -> Result<(), ()> {
        self.tx.try_send(rec).map_err(|_| {
            metrics::counter!("gateway_billing_channel_full_total").increment(1);
        })
    }
}

async fn flush(redis: &RedisCtx, buf: &mut Vec<BillingRecord>) {
    let client = redis.pool.next();
    let pipe = client.pipeline();
    for rec in buf.drain(..) {
        let key = format!("counter:user:{}:balance_micro", rec.user_id);
        let _ = pipe.incr_by::<(), _>(&key, -(rec.cost_micro as i64));
        let _ = pipe.xadd::<(), _, _, _>("billing:records", false, None, "*", serde_json::to_string(&rec).unwrap());
    }
    let _ = pipe.all::<()>().await;
}
```

- [ ] **Step 6：log_sink.rs（DetailedLog → 对象存储 stub）**

v1 提供 trait + stdout fallback；S3 / OpenDAL 装配在 `gw-gateway` 完成。

```rust
use async_trait::async_trait;

#[async_trait]
pub trait DetailedLogSink: Send + Sync {
    async fn write(&self, payload: serde_json::Value);
}

pub struct StdoutSink;
#[async_trait]
impl DetailedLogSink for StdoutSink {
    async fn write(&self, payload: serde_json::Value) {
        tracing::info!(target: "gw_detailed_log", "{}", payload);
    }
}
```

- [ ] **Step 7：编译 + 提交**

Run: `cargo check -p gw-ledger`
Expected: 无错误。

```bash
git add crates/gw-ledger
git commit -m "feat(gw-ledger): BillingRecord + FreezeGuard + mpsc flusher + log sink"
```

---

### Task 3C.2：`gw-ledger` 单元 + 集成测试

**Files:**
- Create: `crates/gw-ledger/tests/freeze.rs`
- Create: `crates/gw-ledger/tests/flusher.rs`

- [ ] **Step 1：freeze.rs**

```rust
use gw_core::ids::UserId;
use gw_ledger::FreezeGuard;
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

#[test]
fn freeze_and_settle_balanced() {
    let uid = UserId::from("u-freeze");
    let g = FreezeGuard::freeze(uid.clone(), 1000);
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 1000);
    g.settle(800);
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 800);
}

#[test]
fn freeze_drop_without_settle_rolls_back() {
    let uid = UserId::from("u-leak");
    {
        let _g = FreezeGuard::freeze(uid.clone(), 500);
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}
```

- [ ] **Step 2：flusher.rs（testcontainers Redis 集成测试）**

```rust
use fred::interfaces::*;
use gw_core::ids::*;
use gw_ledger::{billing::*, Ledger};
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

#[tokio::test]
async fn batch_flush_writes_billing_stream_and_decrs_balance() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4).await.unwrap();
    let ledger = Ledger::start(redis.clone(), 1024, 2, Duration::from_millis(100));
    let rec = BillingRecord {
        ts_ms: 0, request_id: "r1".into(), trace_id: "t".into(),
        user_id: UserId::from("u1"), child_id: None, api_key_id: None,
        client_protocol: "openai".into(), upstream_protocol: "openai".into(),
        model: ModelName::from("gpt-4"), vendor_id: VendorId::from("v1"),
        vendor_key_id: KeyId::from("k1"), status: BillingStatus::Ok,
        error_code: None, http_status: 200, input_tokens: 100, output_tokens: 50,
        cached_tokens: None, cost_micro: 1000, freeze_amount_micro: 1500,
        markup_product: 1.0, usage_source: UsageSource::Upstream,
        retry_count: 0, latency_ms: 100, ttft_ms: None, is_stream: false,
    };
    ledger.try_send(rec.clone()).unwrap();
    ledger.try_send(rec).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = redis.pool.next();
    let bal: i64 = client.get("counter:user:u1:balance_micro").await.unwrap();
    assert_eq!(bal, -2000);
    let len: i64 = client.xlen("billing:records").await.unwrap();
    assert_eq!(len, 2);
}
```

- [ ] **Step 3：运行测试**

Run: `cargo test -p gw-ledger`
Expected: 全部通过。

- [ ] **Step 4：提交**

```bash
git add crates/gw-ledger/tests
git commit -m "test(gw-ledger): freeze RAII + mpsc flusher integration tests"
```

---

## Phase 4：`gw-proxy` 编排层（串行）

> 依赖 Phase 1-3 全部产物。`gw-proxy` 实现 Pingora `ProxyHttp` trait，串联所有 filter；`ReqCtx` 持有跨 phase 状态。

### Task 4.1：crate 骨架 + ReqCtx + Pingora 集成

**Files:**
- Create: `crates/gw-proxy/Cargo.toml`
- Create: `crates/gw-proxy/src/lib.rs`
- Create: `crates/gw-proxy/src/ctx.rs`
- Create: `crates/gw-proxy/src/gateway.rs`
- Create: `crates/gw-proxy/src/timeouts.rs`
- Create: `crates/gw-proxy/src/mem.rs`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-proxy"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-codec = { path = "../gw-codec" }
gw-store = { path = "../gw-store" }
gw-auth = { path = "../gw-auth" }
gw-routing = { path = "../gw-routing" }
gw-ledger = { path = "../gw-ledger" }
async-trait.workspace = true
bytes.workspace = true
pingora.workspace = true
pingora-proxy.workspace = true
serde.workspace = true
serde_json.workspace = true
sonic-rs.workspace = true
tokio.workspace = true
tracing.workspace = true
metrics.workspace = true
time.workspace = true
secrecy.workspace = true
tikv-jemalloc-ctl.workspace = true
```

- [ ] **Step 2：lib.rs**

```rust
pub mod ctx;
pub mod gateway;
pub mod timeouts;
pub mod mem;
pub use gateway::LlmGateway;
```

- [ ] **Step 3：mem.rs（jemalloc 采样）**

```rust
use tikv_jemalloc_ctl::thread;

#[inline(always)]
pub fn sample_thread_alloc() -> u64 {
    thread::allocatedp::mib()
        .ok()
        .and_then(|mib| mib.read().ok())
        .map(|tp| unsafe { *tp.get() })
        .unwrap_or(0)
}
```

- [ ] **Step 4：ctx.rs（ReqCtx 完整定义，对齐 spec §4.3）**

```rust
use bytes::Bytes;
use gw_codec::AnyCodec;
use gw_core::debug::{DebugMode, TraceBuffer};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_core::versioned::Versioned;
use gw_ledger::FreezeGuard;
use gw_auth::ratelimit::StreamSlot;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

pub struct UsageAccumulator {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
}

#[derive(Default)]
pub struct ReqTiming {
    pub start: Option<Instant>,
    pub ttft: Option<Instant>,
    pub end: Option<Instant>,
}

pub struct ReqCtx {
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub client_codec: Option<AnyCodec>,
    pub upstream_codec: Option<AnyCodec>,
    pub user: Option<Arc<Versioned<UserEntry>>>,
    pub merged_perm: Option<MergedPermissions>,
    pub model: Option<ModelName>,
    pub is_stream: bool,
    pub idempotent: bool,
    pub cached_body: Option<Bytes>,
    pub picked_vendor_key: Option<Arc<VendorKey>>,
    pub picked_vendor: Option<Arc<Versioned<Vendor>>>,
    pub pricing: Option<Arc<Versioned<Pricing>>>,
    pub timing: ReqTiming,
    pub usage: UsageAccumulator,
    pub freeze_guard: Option<FreezeGuard>,
    pub stream_slot: Option<StreamSlot>,
    pub bytes_sent_to_client: AtomicU64,
    pub trace: TraceBuffer,
    pub debug: DebugMode,
}

impl ReqCtx {
    pub fn new(trace_id: TraceId, request_id: RequestId) -> Self {
        Self {
            trace_id, request_id, client_codec: None, upstream_codec: None,
            user: None, merged_perm: None, model: None, is_stream: false, idempotent: true,
            cached_body: None, picked_vendor_key: None, picked_vendor: None, pricing: None,
            timing: ReqTiming::default(),
            usage: UsageAccumulator { input_tokens: 0, output_tokens: 0, cached_tokens: None },
            freeze_guard: None, stream_slot: None,
            bytes_sent_to_client: AtomicU64::new(0),
            trace: TraceBuffer::new(),
            debug: DebugMode::DISABLED,
        }
    }

    #[inline(always)]
    pub fn phase_start(&mut self, name: &'static str) {
        let alloc = super::mem::sample_thread_alloc();
        self.trace.phase_start(name, alloc);
    }
    #[inline(always)]
    pub fn phase_end(&mut self) {
        let alloc = super::mem::sample_thread_alloc();
        self.trace.phase_end(alloc);
    }
}
```

- [ ] **Step 5：timeouts.rs**

```rust
use pingora::upstreams::peer::HttpPeer;
use std::time::Duration;

pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
    pub total: Duration,
    pub stream_total: Duration,
}

impl Timeouts {
    pub fn default_for_nonstream() -> Self {
        Self { connect: Duration::from_secs(5), read: Duration::from_secs(30),
            total: Duration::from_secs(60), stream_total: Duration::from_secs(600) }
    }
    pub fn apply_to_peer(&self, peer: &mut HttpPeer, is_stream: bool) {
        peer.options.connection_timeout = Some(self.connect);
        peer.options.read_timeout = Some(self.read);
        if !is_stream {
            peer.options.total_connection_timeout = Some(self.total);
        }
    }
}
```

- [ ] **Step 6：gateway.rs 骨架**

```rust
use async_trait::async_trait;
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session};
use std::sync::Arc;
use crate::ctx::ReqCtx;
use gw_store::EntityStore;
use gw_routing::{BreakerStore, RetryConfig};
use gw_ledger::Ledger;
use gw_auth::ratelimit::RateLimitGuard;

pub struct LlmGateway {
    pub store: Arc<EntityStore>,
    pub breakers: Arc<BreakerStore>,
    pub ratelimit: Arc<RateLimitGuard>,
    pub ledger: Arc<Ledger>,
    pub retry_cfg: RetryConfig,
    pub global_rpm: u32,
}

#[async_trait]
impl ProxyHttp for LlmGateway {
    type CTX = ReqCtx;
    fn new_ctx(&self) -> Self::CTX {
        use gw_core::ids::{TraceId, RequestId};
        let trace = TraceId::from(uuid_v4_string());
        let req = RequestId::from(trace.0.clone());
        ReqCtx::new(trace, req)
    }

    async fn upstream_peer(&self, _session: &mut Session, ctx: &mut Self::CTX)
        -> pingora::Result<Box<HttpPeer>>
    {
        ctx.phase_start("upstream_peer");
        // TODO: 选 Vendor.base_urls[active]，构造 HttpPeer
        let host = ctx.picked_vendor.as_ref()
            .and_then(|v| v.value.base_urls.first())
            .map(|u| u.host_str().unwrap_or("api.openai.com").to_string())
            .unwrap_or_else(|| "api.openai.com".into());
        let peer = HttpPeer::new(format!("{host}:443"), true, host);
        ctx.phase_end();
        Ok(Box::new(peer))
    }
}

fn uuid_v4_string() -> String {
    // 简化：用 ulid / rand 生成 32 hex
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
```

> Cargo.toml 添加 `rand.workspace = true`。

- [ ] **Step 7：编译通过**

Run: `cargo check -p gw-proxy`
Expected: 无错误。

- [ ] **Step 8：提交**

```bash
git add crates/gw-proxy
git commit -m "feat(gw-proxy): ReqCtx + LlmGateway skeleton + Timeouts + jemalloc mem"
```

---

### Task 4.2：完整 filter 链

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Create: `crates/gw-proxy/src/filters/mod.rs`
- Create: `crates/gw-proxy/src/filters/early.rs`
- Create: `crates/gw-proxy/src/filters/request.rs`
- Create: `crates/gw-proxy/src/filters/upstream.rs`
- Create: `crates/gw-proxy/src/filters/response.rs`
- Create: `crates/gw-proxy/src/filters/logging.rs`
- Create: `crates/gw-proxy/src/error_emit.rs`
- Create: `crates/gw-proxy/src/detailed_log.rs`

- [ ] **Step 1：filters/early.rs**

实现 `early_request_filter`：
1. 注入 `trace_id`（W3C `traceparent` 透传 or 生成）
2. IP 黑名单 check（`gw_auth::ip_filter::check_blocklist`）
3. 全局并发流式上限 check
4. 协议 detect（`AnyCodec::detect`）→ 写入 `ctx.client_codec`
5. Debug 触发条件（header / sampling / trace_suffix）

每步均 `ctx.phase_start(...)/phase_end()`。失败 → 调用 `error_emit::send_error` 写响应并返回 `Ok(true)` short-circuit。

- [ ] **Step 2：filters/request.rs**

实现 `request_filter`：
1. JWT / APIKey 鉴权（`gw_auth::jwt::verify_jwt` / `apikey::verify_apikey`）
2. 签名校验（如 API Key 启用）
3. 读 body → `ctx.cached_body`
4. `client_codec.parse_request` → IR
5. `merge_permissions` → `ctx.merged_perm`
6. 模型权限校验
7. `RateLimitGuard.check`
8. 流式时 `check_stream_slot` → `ctx.stream_slot`
9. `hard_cap::check_input_tokens` + `check_cost`
10. `freeze_estimate` → 构造 `FreezeGuard::freeze`，挂入 `ctx.freeze_guard`
11. `USER_CIRCUIT` check

任一失败 → `error_emit::send_error` + short-circuit。

- [ ] **Step 3：filters/upstream.rs（upstream_peer / upstream_request_filter）**

`upstream_peer`：
1. 懒加载 `model_group` + `pricing`
2. `get_candidate_keys`（已在 gw-routing）
3. `Strategy::pick`（按 ModelGroup.strategy 分派）
4. 解密 vendor_key.api_key（已在 cold-load 内完成）→ 取 `Arc<VendorKey>`
5. Vendor.base_urls 主备选 active（DashMap 缓存当前 active idx）
6. 构造 `HttpPeer` + 应用 Timeouts

`upstream_request_filter`：
1. `upstream_codec.render_request` 到 buf（同协议透传走 `same_protocol_passthrough`）
2. 按 `Vendor.auth_template` 注入 Authorization 头
3. 透传 traceparent

- [ ] **Step 4：filters/response.rs（response_body_filter，增量解析 SSE）**

```rust
// 维护 leftover buffer（unparsed 字节）；每次 callback 拼接 + parse_stream_chunk
// 跨协议：parsed → render_stream_chunk → 替换返回 body
// 累 usage：StreamEventIR::Usage 出现时更新 ctx.usage
// stream_total deadline 检查；超时发 StreamEventIR::Error + 关流
```

- [ ] **Step 5：filters/logging.rs**

实现 `logging`：
1. `FreezeGuard::settle(actual_micro)`，actual_micro 按 ctx.usage 计算
2. `BreakerState::record(success/fail)`
3. RateLimit 记录消耗（TPM）
4. 构造 `BillingRecord` → `ledger.try_send`
5. 构造 `DetailedLogRecord` → 异步写出
6. 若 `ctx.debug.enabled` → emit trace buffer

- [ ] **Step 6：error_emit.rs**

```rust
use bytes::BytesMut;
use gw_codec::{AnyCodec, ProtocolCodec};
use gw_core::error::ErrorIR;
use pingora::http::ResponseHeader;
use pingora_proxy::Session;

pub async fn send_error<C: ProtocolCodec>(session: &mut Session, codec: &C, err: &ErrorIR<'_>) {
    let mut buf = BytesMut::new();
    codec.render_error(err, &mut buf);
    let mut resp = ResponseHeader::build(err.status, None).unwrap();
    let _ = resp.insert_header("Content-Type", "application/json");
    let _ = resp.insert_header("X-Request-Id", err.request_id);
    if let Some(s) = err.retry_after_secs { let _ = resp.insert_header("Retry-After", s.to_string()); }
    let _ = session.write_response_header(Box::new(resp), false).await;
    let _ = session.write_response_body(Some(buf.freeze()), true).await;
}
```

- [ ] **Step 7：detailed_log.rs（DetailedLogRecord 构造 + Redactor 调用）**

- [ ] **Step 8：gateway.rs 串联所有 filter**

修改 `impl ProxyHttp` 加入 `early_request_filter` / `request_filter` / `request_body_filter` / `upstream_peer` / `upstream_request_filter` / `response_body_filter` / `logging`，各自调用对应 module 中的函数。

- [ ] **Step 9：编译通过**

Run: `cargo check -p gw-proxy`
Expected: 无错误。

- [ ] **Step 10：提交**

```bash
git add crates/gw-proxy
git commit -m "feat(gw-proxy): full filter chain (early/request/upstream/response/logging)"
```

---

### Task 4.3：`gw-proxy` 集成测试

**Files:**
- Create: `crates/gw-proxy/tests/end_to_end_openai.rs`
- Create: `crates/gw-proxy/tests/error_emit.rs`
- Create: `crates/gw-proxy/tests/freeze_lifecycle.rs`

- [ ] **Step 1：end_to_end_openai.rs（wiremock 模拟 OpenAI 上游）**

```rust
// 启动 wiremock 模拟 OpenAI /v1/chat/completions
// 启动 Pingora gateway listener
// 用 reqwest 发请求到 gateway，验证 200 + 响应体格式
// 验证 EntityStore 中 user_id="u1" 已被缓存（cold-load 走通）
// 验证 counter:user:u1:balance_micro 减小
```

实现要点：
- 在 testcontainers 启动 Redis + Postgres，插入 mock user / vendor_key / model_group / pricing
- 配置 vendor.base_url 指向 wiremock 地址
- 在 tokio test 内 spawn Pingora server 监听 random port
- 用 reqwest 发请求

- [ ] **Step 2：error_emit.rs**

模拟用户余额为 0 → 请求应返回 402 `GW_QUOTA_BALANCE_ZERO`，且响应体为 OpenAI 协议风格 error JSON。

- [ ] **Step 3：freeze_lifecycle.rs**

发起请求 → 中途模拟 panic / 客户端断开 → 验证 `gateway_freeze_leak_recovered_total` 增加，且 `DEDUCT.frozen_micro` 归零。

- [ ] **Step 4：运行测试**

Run: `cargo test -p gw-proxy --test end_to_end_openai`
Expected: 全部通过（含 testcontainers + wiremock）。

- [ ] **Step 5：提交**

```bash
git add crates/gw-proxy/tests
git commit -m "test(gw-proxy): end-to-end OpenAI + error emit + freeze lifecycle"
```

---

## Phase 5：`gw-gateway` bin（串行）

### Task 5.1：bin crate + main + config

**Files:**
- Create: `crates/gw-gateway/Cargo.toml`
- Create: `crates/gw-gateway/src/main.rs`
- Create: `crates/gw-gateway/src/config.rs`
- Create: `crates/gw-gateway/src/kms.rs`
- Create: `crates/gw-gateway/src/observability.rs`
- Create: `crates/gw-gateway/src/health.rs`
- Create: `config/default.toml`

- [ ] **Step 1：Cargo.toml**

```toml
[package]
name = "gw-gateway"
version = "0.1.0"
edition.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-proxy = { path = "../gw-proxy" }
gw-store = { path = "../gw-store" }
gw-auth = { path = "../gw-auth" }
gw-routing = { path = "../gw-routing" }
gw-ledger = { path = "../gw-ledger" }
gw-codec = { path = "../gw-codec" }
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true
metrics.workspace = true
metrics-exporter-prometheus.workspace = true
figment.workspace = true
pingora.workspace = true
pingora-proxy.workspace = true
tikv-jemallocator.workspace = true
```

- [ ] **Step 2：main.rs 装配（jemalloc 默认启用）**

```rust
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use gw_gateway::*;

fn main() -> anyhow::Result<()> {
    let cfg = config::load()?;
    observability::init(&cfg)?;

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        // 1. KMS 初始化
        let kek_cache = kms::init(&cfg).await?;
        // 2. Redis / PgSQL
        let redis = gw_store::redis_client::RedisCtx::connect(&cfg.redis.url, cfg.redis.pool_size).await?;
        let pg = gw_store::pg_client::connect(&cfg.pgsql.url, cfg.pgsql.max_connections).await?;
        // 3. EntityStore / Ledger / Breakers / RateLimit
        let store = std::sync::Arc::new(gw_store::EntityStore::new());
        let breakers = std::sync::Arc::new(gw_routing::BreakerStore::default());
        let ratelimit = std::sync::Arc::new(gw_auth::ratelimit::RateLimitGuard::new());
        let ledger = std::sync::Arc::new(gw_ledger::Ledger::start(
            redis.clone(), cfg.ledger.channel_capacity, 100, std::time::Duration::from_millis(200)));
        // 4. Stream consumer 启动
        // 5. Health checker / Key status scanner / RateLimit sync 后台 task 启动
        // 6. Pingora 启动 3 listener（business 8000 / metrics 9090 / health 8080）
        let mut server = pingora::server::Server::new(None).map_err(|e| anyhow::anyhow!("{e}"))?;
        server.bootstrap();
        let gw = gw_proxy::LlmGateway { store, breakers, ratelimit, ledger,
            retry_cfg: gw_routing::RetryConfig::default(), global_rpm: cfg.ratelimit.global_rpm };
        let mut business = pingora_proxy::http_proxy_service(&server.configuration, gw);
        business.add_tcp(&format!("0.0.0.0:{}", cfg.server.business_port));
        server.add_service(business);
        server.run_forever();
        anyhow::Ok(())
    })?;
    Ok(())
}

pub mod config;
pub mod kms;
pub mod observability;
pub mod health;
```

- [ ] **Step 3：config.rs（figment toml + env）**

```rust
use figment::{Figment, providers::{Format, Toml, Env}};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerCfg,
    pub redis: RedisCfg,
    pub pgsql: PgCfg,
    pub ratelimit: RateLimitCfg,
    pub ledger: LedgerCfg,
    pub kms: KmsCfg,
    pub debug: DebugCfg,
}

// 各子 cfg struct 定义...

pub fn load() -> anyhow::Result<Config> {
    let cfg = Figment::new()
        .merge(Toml::file("config/default.toml"))
        .merge(Toml::file("config/local.toml").nested())
        .merge(Env::prefixed("GW_").split("__"))
        .extract()?;
    Ok(cfg)
}
```

- [ ] **Step 4：kms.rs（环境变量 dev / KMS API prod）**

dev 模式从 `GW_KMS__KEK_HEX` 读 64 hex；生产模式预留 aws-sdk-kms hook。

- [ ] **Step 5：observability.rs（tracing JSON + prometheus exporter）**

- [ ] **Step 6：health.rs（/livez /readyz Pingora service 实现）**

- [ ] **Step 7：config/default.toml**

包含 `[debug] sampling_rate=0.0001` 等所有 ADR-027 配置项。

- [ ] **Step 8：编译 + 启动 smoke test**

Run: `cargo build --release -p gw-gateway`
Expected: 编译通过。

Run: `GW_REDIS__URL=redis://localhost GW_PGSQL__URL=... ./target/release/gw-gateway --version`
Expected: 启动失败回退到错误信息（无 Redis），但二进制可执行。

- [ ] **Step 9：提交**

```bash
git add crates/gw-gateway config/
git commit -m "feat(gw-gateway): bin entrypoint with jemalloc, config, kms, health, pingora server"
```

---

## Phase 6：端到端集成测试 + 性能基准（串行）

### Task 6.1：端到端集成测试套件

**Files:**
- Create: `tests/e2e/Cargo.toml`（独立测试 crate，或放 `crates/gw-gateway/tests/`）
- Create: `crates/gw-gateway/tests/e2e_full_flow.rs`
- Create: `crates/gw-gateway/tests/e2e_failure_modes.rs`
- Create: `crates/gw-gateway/tests/fixtures/seed.sql`

- [ ] **Step 1：e2e_full_flow.rs**

覆盖完整快乐路径：
1. 启动 Redis + Postgres（testcontainers）
2. 启动 wiremock 模拟 OpenAI 上游
3. 应用 migrations + seed.sql
4. 启动 gateway binary（用 `tokio::process` 或 Pingora in-process）
5. 客户端发请求验证：
   - 200 响应、响应体格式正确
   - billing:records stream 有 1 条
   - counter:user:u1:balance_micro 减小预期金额
   - DEDUCT.frozen 为 0（已结算）
6. 验证响应头：`X-Request-Id` / `X-Gateway-Version`

- [ ] **Step 2：e2e_failure_modes.rs**

覆盖失败矩阵中的多个场景：
1. **AC-2.6-2**（错误归一化）：客户端 OpenAI，上游 Anthropic 返回 invalid_request_error → 客户端收到 OpenAI 风格 400
2. **AC-022-2**（重试）：上游 5xx → 应触发跨 Key 重试 → billing.retry_count = 5
3. **AC-022-4**（幂等）：含 tool_calls 的请求 + 上游 5xx → 不重试，直接 502
4. **AC-024-1**（限流）：用户 RPM=100，连发 101 个请求 → 第 101 个 429
5. **AC-026-1**（日志）：每请求恰好 1 条 simple_log
6. **AC-027-3**（脱敏）：debug log 中 grep `api_key` 无命中

- [ ] **Step 3：seed.sql**

最小测试数据：1 个 user（balance=100M micro），1 个 vendor（base_url=wiremock），1 个 vendor_key（accessible_models=gpt-4），1 个 model_group（strategy=fixed_order），1 个 pricing。

- [ ] **Step 4：运行所有 e2e**

Run: `cargo test -p gw-gateway --test e2e_full_flow --test e2e_failure_modes`
Expected: 全部通过。

- [ ] **Step 5：提交**

```bash
git add crates/gw-gateway/tests
git commit -m "test(e2e): full flow + failure modes covering AC-2.6/022/024/026/027"
```

---

### Task 6.2：性能基准（criterion）

**Files:**
- Create: `crates/gw-codec/benches/passthrough.rs`
- Create: `crates/gw-codec/benches/cross_protocol.rs`
- Create: `crates/gw-auth/benches/merge_permissions.rs`
- Create: `crates/gw-routing/benches/strategy_pick.rs`

- [ ] **Step 1：passthrough.rs（对齐 AC-2.3-1）**

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gw_codec::{openai::OpenAi, passthrough::same_protocol_passthrough};
use gw_core::ir::Mutations;

fn bench_passthrough(c: &mut Criterion) {
    let body = include_bytes!("../tests/fixtures/openai_basic.json");
    let codec = OpenAi;
    c.bench_function("openai_same_proto_no_mutation_8kb", |b| {
        b.iter(|| {
            let _ = same_protocol_passthrough(&codec, black_box(body), &Mutations::NONE).unwrap();
        });
    });
}

criterion_group!(benches, bench_passthrough);
criterion_main!(benches);
```

预期结果：P50 ≤ 200µs / P99 ≤ 800µs（验证 AC-2.3-1）。

- [ ] **Step 2：cross_protocol.rs（对齐 AC-2.3-2）**

预期：P50 ≤ 800µs / P99 ≤ 2ms。

- [ ] **Step 3：merge_permissions.rs（对齐 AC-023-5）**

10 个 user_group 场景，P99 ≤ 5µs。

- [ ] **Step 4：strategy_pick.rs**

8 种策略各 bench 一遍，确认无单一策略 P99 > 100µs。

- [ ] **Step 5：运行 bench**

Run: `cargo bench --workspace`
Expected: criterion 输出 P50 / P95 / P99，均满足 spec §7 性能预算；不满足则记录差距并标 TODO。

- [ ] **Step 6：提交**

```bash
git add crates/*/benches
git commit -m "bench: criterion benchmarks for codec passthrough / merge / strategy.pick"
```

---

### Task 6.3：文档与 README 完善

**Files:**
- Modify: `README.md`
- Create: `docs/dev-setup.md`

- [ ] **Step 1：README.md 更新**

补充：
- 9-crate workspace 简介
- 快速本地开发（docker compose redis + postgres，cargo run）
- 测试矩阵（unit / integration / e2e / bench）

- [ ] **Step 2：dev-setup.md**

记录：
- testcontainers 依赖（需要 Docker daemon 运行）
- KMS dev 模式 `GW_KMS__KEK_HEX` 64 hex
- sqlx offline `cargo sqlx prepare --workspace` 准备 `.sqlx` 缓存

- [ ] **Step 3：提交**

```bash
git add README.md docs/dev-setup.md
git commit -m "docs: README + dev-setup with workspace overview and testing matrix"
```

---

## 完成判定

工作流执行完毕需满足以下条件，才算交付 v1：

1. `cargo check --workspace --all-targets` 无错误、无 warning。
2. `cargo clippy --workspace --all-targets -- -D warnings` 通过。
3. `cargo test --workspace`（含 testcontainers）通过。
4. `cargo bench --workspace` 输出符合 §7 性能预算。
5. `cargo build --release -p gw-gateway` 产出可执行二进制；`./gw-gateway` 能在 < 200ms 内完成 ready 探针响应（Redis / PgSQL 就绪后）。
6. 端到端测试覆盖 spec §5.3 所列错误响应映射至少 10 条（其余可标 TODO 留后续 PR）。
7. 提交历史按 Phase 顺序、每个 Task 都对应至少 1 个 commit；commit message 遵循 Conventional Commits。

---

## 并行执行指引（subagent-driven 模式）

| Phase | 并行度 | 建议 agent 分配 |
|---|---|---|
| 0 | 1 | main |
| 1 | 1 | agent-core |
| 2 | **2** | agent-codec（Path A）+ agent-store（Path B） |
| 3 | **3** | agent-auth + agent-routing + agent-ledger |
| 4 | 1 | agent-proxy（依赖前 6 个 crate） |
| 5 | 1 | agent-bin |
| 6 | 1 | agent-e2e |

并行启动方式：在 `subagent-driven-development` 流程中，Phase 2 / Phase 3 开始时一次性 dispatch 多个 subagent，每个 subagent 拿到一份本 plan + 指定 Task 范围；完成后回 main 进行 review + 合并。

跨 Phase 严格串行 —— 进入新 Phase 前必须确认上一阶段所有 crate `cargo check` 通过、单元测试通过。





