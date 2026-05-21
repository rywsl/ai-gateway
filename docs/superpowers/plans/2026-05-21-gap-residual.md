# Gap Residual Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 [2026-05-21-gap-residual-design.md](../specs/2026-05-21-gap-residual-design.md) 全部 6 节 / 34 ADR / 8 桶残项。

**Architecture:** 三层 crate 不变（支撑 / 业务 / 执行）。新增两个业务 crate `gw-capability` 与 `gw-tunnel-client`（后者 cargo feature gated），同时扩展 `gw-core::error` / `gw-auth` / `gw-codec` / `gw-store` / `gw-ledger` / `gw-gateway` / `gw-proxy` / `gw-adapter`。

**Tech Stack:** Rust 1.86 / Pingora 0.8 / fred (Redis) / sqlx (Postgres) / metrics + prometheus / regex (DFA-only) / brotli / flate2 / aes-gcm / jsonwebtoken / hmac+sha2 / dashmap+moka / TDD 全程。

**Parallelism waves:** 36 task 分 7 波；同一波内 task 在不相交文件集上跑，可并行 dispatch subagent。波间存在依赖。

---

## Parallelism map

| Wave | Tasks | 并行度 | 依赖 |
|---|---|---|---|
| 0 | T0-1, T0-2 | 顺序 | — |
| 1 | T1-A..G | 7 并行 | Wave 0 |
| 2 | T2-A..F | 6 并行 | Wave 0 |
| 3 | T3-A..H | 8 并行 | Wave 1+2 部分 |
| 4 | T4-A..E | 5 并行 | T1-F (capability skeleton) |
| 5 | T5-A..F | 6 并行 | Wave 3+4 |
| 6 | T6-A..F | 顺序 | All（T6-F 是收尾） |

**总 task 数：40**（2+7+6+8+5+6+6）。**最大并行度：8**（Wave 3）。

补回的 6 项原"follow-up"（spec ADR 要求实现）全部升格为正式 task：

| 原 follow-up 项 | 升格为 task | 所属 ADR |
|---|---|---|
| Tunnel hub WebSocket 真连接 + reconnect / heartbeat | **T3-G** | ADR-G5-3 |
| DoH client（safe_dns 真实解析） | **T3-H** | ADR-G5-5 |
| Capability resolver 内嵌 model_permissions runtime 熔断接 capability filter | **T5-E** | ADR-G2-8 |
| OpenAI Responses ⇄ Chat ⇄ Anthropic / Gemini 跨形矩阵转换 | **T5-F** | ADR-G4-3 |
| Stream policy AccumulateUpstreamStream 真实现 | **T6-D** | ADR-G4-1 |
| usage:detailed Redis stream sink 真实现 | **T6-E** | ADR-G6-4 |

> KEK 轮换"管理面入口" 明确归管理面 spec；T1-E 已实现 dual-read 状态机本身（spec § 1.7 实际要求）。

---

## Wave 0 — Foundations

### Task T0-1: ErrorClass + AccountBlockReason 类型

**Files:**
- Create: `crates/gw-core/src/error/mod.rs`（把现有 `error.rs` 改成模块目录）
- Create: `crates/gw-core/src/error/codes.rs`（搬迁 `pub mod codes` 出来）
- Create: `crates/gw-core/src/error/ir.rs`（搬迁 `ErrorIR / ErrorCategory`）
- Create: `crates/gw-core/src/error/class.rs`（**新** ErrorClass / AccountBlockReason / ClassifyCtx / ClassifierOutcome）
- Modify: `crates/gw-core/src/lib.rs`（重导出未变）
- Test: `crates/gw-core/tests/error_class.rs`

- [ ] **Step 1: 把现有 `error.rs` 拆成 mod 目录**

```bash
mkdir -p crates/gw-core/src/error
git mv crates/gw-core/src/error.rs crates/gw-core/src/error/_legacy.rs   # 临时占位防丢
```

把 `_legacy.rs` 的内容拆到 `ir.rs`（ErrorCategory + ErrorIR）和 `codes.rs`（pub mod codes 全部常量），然后删 `_legacy.rs`。`error/mod.rs` 内容：

```rust
mod class;
mod codes_;
mod ir;

pub use class::*;
pub use ir::{ErrorCategory, ErrorIR};
pub mod codes {
    pub use super::codes_::*;
}
```

注意：`pub mod codes_` 在 mod.rs 内 re-export 为 `codes`，外部使用 `gw_core::error::codes::*` 路径不变。

- [ ] **Step 2: 写失败测试**

`crates/gw-core/tests/error_class.rs`：

```rust
use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, ErrorClass, Retryability,
};

#[test]
fn error_class_default_is_internal() {
    let oc = ClassifierOutcome::default();
    assert_eq!(oc.class, ErrorClass::Internal);
    assert_eq!(oc.retryable, Retryability::No);
    assert!(oc.cooldown.is_none());
}

#[test]
fn error_class_account_blocked_carries_reason() {
    let class = ErrorClass::AccountBlocked(AccountBlockReason::Status401AccountDeactivated);
    match class {
        ErrorClass::AccountBlocked(r) => assert_eq!(r, AccountBlockReason::Status401AccountDeactivated),
        _ => panic!(),
    }
}

#[test]
fn rate_limit_kind_use_only_no_redefine() {
    // 只验证可以从 RL-Core 的 path 引到 ErrorClass。
    let _: ErrorClass = ErrorClass::RateLimitedUpstream(gw_core::ratelimit::RateLimitKind::Rpm);
}
```

- [ ] **Step 3: 运行测试确认失败**

```bash
cargo test -p gw-core --test error_class
```

Expected: compile error（`gw_core::error::ErrorClass` 不存在）。

- [ ] **Step 4: 实现 class.rs**

`crates/gw-core/src/error/class.rs`：

```rust
//! ErrorClass — 客户端 / 编排层错误语义枚举。
//! 与 K-A 的 CooldownReason 故意不强耦合：ErrorClass 面向编排 / 客户端，
//! CooldownReason 面向 KeyHealthStore 内部状态。
//! 一份 ClassifierOutcome 可同时给出两者，由 classifier 负责映射。

use crate::ratelimit::RateLimitKind;

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    AuthDenied,
    QuotaDenied,
    RateLimitedSelf,
    RateLimitedUpstream(RateLimitKind),
    AccountBlocked(AccountBlockReason),
    UpstreamTransient,
    ProtocolError,
    ThinkingSignatureMismatch,
    ClientAbort,
    Internal,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountBlockReason {
    Status401AccountDeactivated,
    Status401Unauthorized,
    Status402PaymentRequired,
    Status403AccountSuspended,
    Status400OrganizationDisabled,
    OAuthRefreshFailed,
    AdminForce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retryability {
    /// 不重试
    No,
    /// 同 key 可重试（瞬时）
    SameKey,
    /// 跨 key 可重试（账号问题 / RPM 满）
    CrossKey,
    /// 跨 vendor 可重试
    CrossVendor,
}

impl Default for Retryability {
    fn default() -> Self { Retryability::No }
}

#[derive(Debug, Default, Clone)]
pub struct ClassifierOutcome {
    pub class: ErrorClass,
    pub retryable: Retryability,
    pub cooldown: Option<CooldownDirective>,
    pub rate_limit_hint: Option<RateLimitHint>,
    pub oauth_signal: Option<OAuthSignal>,
    pub passthrough_body: Option<PassthroughBody>,
}

impl Default for ErrorClass {
    fn default() -> Self { ErrorClass::Internal }
}

#[derive(Debug, Clone)]
pub struct CooldownDirective {
    pub reason_code: u16, // 与 K-A CooldownReason u16 编码同步
    pub ttl_secs: u32,
    pub reset_consecutive: bool,
}

#[derive(Debug, Clone)]
pub struct RateLimitHint {
    pub kind: RateLimitKind,
    pub retry_after_secs: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct OAuthSignal {
    pub kind: OAuthSignalKind,
}

#[derive(Debug, Copy, Clone)]
pub enum OAuthSignalKind {
    RefreshFailed,
    AccountBlocked,
}

#[derive(Debug, Clone)]
pub struct PassthroughBody {
    pub upstream_status: u16,
    pub body_json: serde_json::Value,
    pub is_sse: bool,
}

pub struct ClassifyCtx<'a> {
    pub key_id: &'a str,
    pub vendor_id: &'a str,
    pub api_format: crate::protocol::ProtocolKind,
    pub protocol: crate::protocol::ProtocolKind,
}

pub trait HeadersAccessor {
    fn get(&self, name: &str) -> Option<&str>;
}

pub trait ErrorClassifier: Send + Sync {
    fn classify_local(&self, err: &super::ir::ErrorIR<'_>) -> ClassifierOutcome;
    fn classify_upstream(
        &self,
        ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome;
}
```

`gw-core/src/lib.rs` 加 `pub mod ratelimit;`（即使 RL-Core 还没落地也先占位）：

```rust
pub mod ratelimit {
    /// Owned by RL-Core (2026-05-19-ratelimit-rl-core.md). 此处仅占位，
    /// 待 RL-Core 落地后整体替换为 use rl_core::*。
    #[non_exhaustive]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum RateLimitKind {
        Rpm, Tpm, Daily, Monthly, Concurrent, Unknown,
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cargo test -p gw-core --test error_class
cargo build --workspace
```

Expected: 全过；`cargo build` 不动 callsite。

- [ ] **Step 6: 提交**

```bash
git add crates/gw-core
git commit -m "feat(gw-core): ErrorClass + ClassifierOutcome + ClassifyCtx (G-α.1)"
```

---

### Task T0-2: K-B AnalyzerOutcome 扩展（吸收 E4 enhancer）

**Files:**
- Modify: `crates/gw-adapter/src/analyzer.rs`
- Test: `crates/gw-adapter/tests/analyzer_outcome.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-adapter/tests/analyzer_outcome.rs`：

```rust
use gw_adapter::{
    DefaultErrorAnalyzer, ErrorOutcome, HeaderView, UpstreamErrorAnalyzer,
};

struct DummyHeaders;
impl HeaderView for DummyHeaders {
    fn get(&self, _: &str) -> Option<&str> { None }
}

#[test]
fn outcome_has_cooldown_enhanced_body_oauth_signal() {
    let oc = ErrorOutcome::default();
    assert!(oc.cooldown.is_none());
    assert!(oc.enhanced_body.is_none());
    assert!(oc.oauth_signal.is_none());
}

#[test]
fn default_analyzer_401_yields_oauth_signal_none_until_vendor() {
    let a = DefaultErrorAnalyzer;
    let oc = a.analyze(401, b"", &DummyHeaders);
    // Default 不知道是不是 oauth；vendor 子类才决定
    assert!(oc.oauth_signal.is_none());
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p gw-adapter --test analyzer_outcome
```

Expected: compile error，`ErrorOutcome.cooldown` / `enhanced_body` / `oauth_signal` 字段不存在。

- [ ] **Step 3: 扩 ErrorOutcome**

修改 `crates/gw-adapter/src/analyzer.rs`，在 `ErrorOutcome` 加：

```rust
use bytes::Bytes;
use gw_core::error::OAuthSignal;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ErrorOutcome {
    pub kind: Option<ErrorKind>,
    pub retry_after_secs: Option<u32>,

    /// K-A 的冷却命令；adapter 看了 status + body 后决定。
    /// 由 § 1 ClassifierOutcome 透传 K-A apply_cooldown。
    pub cooldown: Option<gw_core::error::CooldownDirective>,

    /// vendor 错误体 → 客户端协议错误体的"vendor 端增强"产物。
    /// kiro 等 vendor 在此处把帧头剥掉 / type 字段改写。
    /// 不做 vendor 改写时为 None（passthrough）。
    pub enhanced_body: Option<Bytes>,

    /// OAuth 标记：401 是 token 失效还是账号挂起？vendor 决定。
    pub oauth_signal: Option<OAuthSignal>,
}
```

`Eq, PartialEq` 与新加 `Bytes` / `serde_json::Value`（如果有的话）冲突——`Bytes: Eq`，`OAuthSignal` 加 derive。检查并修：在 `class.rs` 中给 `OAuthSignal` 加 `PartialEq, Eq`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSignal {
    pub kind: OAuthSignalKind,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum OAuthSignalKind {
    RefreshFailed,
    AccountBlocked,
}
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p gw-adapter --test analyzer_outcome
cargo build --workspace
```

Expected: 全过。

- [ ] **Step 5: 提交**

```bash
git add crates/gw-adapter crates/gw-core
git commit -m "feat(gw-adapter): extend AnalyzerOutcome with cooldown/enhanced_body/oauth_signal (G-α absorb E4)"
```

<!-- WAVE-1 -->

## Wave 1 — Auth hardening + capability skeleton + KEK rotate（7 并行）

> Wave 1 全部 task 互不依赖文件；可同时 dispatch 7 个 subagent。

### Task T1-A: JWT iss / aud / nbf 完整校验

**Files:**
- Modify: `crates/gw-auth/src/jwt.rs`
- Modify: `crates/gw-store/src/loader.rs`（加 `entity:auth_config` 装载，仅声明 stub，由 Wave 3 entity 扩展实数据）
- Modify: `crates/gw-core/src/entity.rs`（加 AuthConfig 实体）
- Test: `crates/gw-auth/tests/jwt_validation.rs`

- [ ] **Step 1: 加 AuthConfig entity stub**

`crates/gw-core/src/entity.rs` 末尾追加：

```rust
use smallvec::SmallVec;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct AuthConfig {
    pub expected_iss: SmallVec<[String; 2]>,
    pub expected_aud: SmallVec<[String; 2]>,
    pub leeway_secs: u32,
    pub require_nbf: bool,
    pub blacklist_fail_mode: BlacklistFailMode,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlacklistFailMode { FailClosed, FailOpen }

impl Default for BlacklistFailMode {
    fn default() -> Self { BlacklistFailMode::FailClosed }
}
```

- [ ] **Step 2: 写失败测试**

`crates/gw-auth/tests/jwt_validation.rs`：

```rust
use gw_auth::jwt::{JwtValidator, JwtValidatorConfig};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use smallvec::smallvec;

#[derive(Serialize)]
struct Claims {
    sub: String,
    iss: String,
    aud: String,
    exp: usize,
    nbf: usize,
}

fn rs256_pem_pair() -> (String, String) {
    // 用 openssl/rsa 不在 build；此处用 jsonwebtoken 自身 HS256 临时变体
    // 实测改回 HS256 + symmetric secret；上层接口不变
    unreachable!("替换为下面 HS256 版本")
}

#[test]
fn rejects_iss_mismatch() {
    let secret = b"secret";
    let claims = Claims {
        sub: "u1".into(),
        iss: "evil".into(),
        aud: "gw".into(),
        exp: now() + 60,
        nbf: now() - 1,
    };
    let token = encode(&Header::default(), &claims,
        &EncodingKey::from_secret(secret)).unwrap();

    let v = JwtValidator::new(JwtValidatorConfig {
        secret: secret.to_vec(),
        expected_iss: smallvec!["good".into()],
        expected_aud: smallvec!["gw".into()],
        leeway_secs: 30,
        require_nbf: true,
    });
    let err = v.verify::<Claims>(&token).unwrap_err();
    assert_eq!(err.reason(), "iss");
}

#[test]
fn rejects_aud_mismatch() { /* 类似，aud=evil */ }

#[test]
fn rejects_nbf_in_future() { /* nbf=now+120 */ }

#[test]
fn accepts_valid_with_leeway() { /* exp=now-10 leeway=30 */ }

#[test]
fn reason_does_not_contain_token() {
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: b"secret".to_vec(),
        expected_iss: smallvec!["a".into()],
        expected_aud: smallvec!["b".into()],
        leeway_secs: 0,
        require_nbf: false,
    });
    let err = v.verify::<serde_json::Value>("not.a.real.jwt").unwrap_err();
    let dbg = format!("{err:?}");
    assert!(!dbg.contains("not.a.real.jwt"));
}

fn now() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as usize
}
```

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p gw-auth --test jwt_validation
```

Expected: compile error，`JwtValidator` / `JwtValidatorConfig` / `verify` 不存在。

- [ ] **Step 4: 实现 JwtValidator**

把 `crates/gw-auth/src/jwt.rs` 重写为：

```rust
//! JWT validator with iss / aud / nbf / leeway support.
//!
//! 校验顺序：签名 → exp → nbf → iss → aud。任一失败即返回 JwtError，
//! reason 不带原始 token 文本（防止日志泄露）。

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use smallvec::SmallVec;
use std::collections::HashSet;

pub struct JwtValidator {
    cfg: JwtValidatorConfig,
}

#[derive(Clone)]
pub struct JwtValidatorConfig {
    pub secret: Vec<u8>,
    pub expected_iss: SmallVec<[String; 2]>,
    pub expected_aud: SmallVec<[String; 2]>,
    pub leeway_secs: u32,
    pub require_nbf: bool,
}

#[derive(Debug)]
pub struct JwtError {
    reason: &'static str,
}
impl JwtError {
    pub fn reason(&self) -> &'static str { self.reason }
}
impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "jwt invalid: {}", self.reason)
    }
}
impl std::error::Error for JwtError {}

impl JwtValidator {
    pub fn new(cfg: JwtValidatorConfig) -> Self { Self { cfg } }

    pub fn verify<C: DeserializeOwned>(&self, token: &str) -> Result<C, JwtError> {
        let header = decode_header(token).map_err(|_| JwtError { reason: "header" })?;
        let alg = header.alg;
        if !matches!(alg, Algorithm::HS256 | Algorithm::RS256) {
            return Err(JwtError { reason: "alg" });
        }
        let mut val = Validation::new(alg);
        val.leeway = self.cfg.leeway_secs as u64;
        val.validate_exp = true;
        val.validate_nbf = self.cfg.require_nbf;
        val.iss = Some(HashSet::from_iter(self.cfg.expected_iss.iter().cloned()));
        val.aud = Some(HashSet::from_iter(self.cfg.expected_aud.iter().cloned()));

        let key = match alg {
            Algorithm::HS256 => DecodingKey::from_secret(&self.cfg.secret),
            Algorithm::RS256 => DecodingKey::from_rsa_pem(&self.cfg.secret)
                .map_err(|_| JwtError { reason: "key" })?,
            _ => return Err(JwtError { reason: "alg" }),
        };

        match decode::<C>(token, &key, &val) {
            Ok(d) => Ok(d.claims),
            Err(e) => Err(JwtError {
                reason: classify_jwt_error(&e),
            }),
        }
    }
}

fn classify_jwt_error(e: &jsonwebtoken::errors::Error) -> &'static str {
    use jsonwebtoken::errors::ErrorKind::*;
    match e.kind() {
        InvalidSignature => "sig",
        ExpiredSignature => "exp",
        ImmatureSignature => "nbf",
        InvalidIssuer => "iss",
        InvalidAudience => "aud",
        InvalidToken | Json(_) | Base64(_) => "format",
        InvalidAlgorithm | InvalidAlgorithmName => "alg",
        InvalidKeyFormat | InvalidRsaKey(_) | InvalidEcdsaKey => "key",
        _ => "other",
    }
}
```

`gw-auth/src/lib.rs` 中 `pub mod jwt;` 加 `pub use jwt::{JwtValidator, JwtValidatorConfig, JwtError};`。

旧 `verify_jwt` 调用路径（`gw-proxy::filters::request`）后续 Wave 5 接通；本 task 仅扩 jwt 模块。

- [ ] **Step 5: 跑测试**

```bash
cargo test -p gw-auth --test jwt_validation
cargo build --workspace
```

Expected: 全过。

- [ ] **Step 6: 提交**

```bash
git add crates/gw-auth crates/gw-core/src/entity.rs
git commit -m "feat(gw-auth): JwtValidator with iss/aud/nbf/leeway (G-α.7)"
```

---

### Task T1-B: JWT 黑名单 (Redis Set + FailMode)

**Files:**
- Create: `crates/gw-auth/src/jwt_blacklist.rs`
- Modify: `crates/gw-auth/src/lib.rs`
- Test: `crates/gw-auth/tests/jwt_blacklist.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_auth::jwt_blacklist::{JwtBlacklist, BlacklistFailMode, RedisJwtBlacklist};
use std::sync::Arc;

mod fakes {
    use async_trait::async_trait;
    use gw_auth::jwt_blacklist::{BlacklistError, JwtBlacklist};
    pub struct AlwaysOk;
    pub struct AlwaysFail;
    pub struct Hit;
    #[async_trait]
    impl JwtBlacklist for AlwaysOk {
        async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> { Ok(false) }
        async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> { Ok(()) }
    }
    #[async_trait]
    impl JwtBlacklist for AlwaysFail {
        async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> {
            Err(BlacklistError::Backend("redis down".into()))
        }
        async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> {
            Err(BlacklistError::Backend("redis down".into()))
        }
    }
    #[async_trait]
    impl JwtBlacklist for Hit {
        async fn is_revoked(&self, _: &str) -> Result<bool, BlacklistError> { Ok(true) }
        async fn revoke(&self, _: &str, _: u64) -> Result<(), BlacklistError> { Ok(()) }
    }
}

#[tokio::test]
async fn fail_closed_treats_redis_down_as_revoked() {
    use gw_auth::jwt_blacklist::check_revoked;
    let bl: Arc<dyn JwtBlacklist> = Arc::new(fakes::AlwaysFail);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailClosed).await);
}

#[tokio::test]
async fn fail_open_treats_redis_down_as_clean() {
    use gw_auth::jwt_blacklist::check_revoked;
    let bl: Arc<dyn JwtBlacklist> = Arc::new(fakes::AlwaysFail);
    assert!(!check_revoked(&*bl, "jti1", BlacklistFailMode::FailOpen).await);
}

#[tokio::test]
async fn hit_means_revoked_regardless_of_mode() {
    use gw_auth::jwt_blacklist::check_revoked;
    let bl: Arc<dyn JwtBlacklist> = Arc::new(fakes::Hit);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailClosed).await);
    assert!(check_revoked(&*bl, "jti1", BlacklistFailMode::FailOpen).await);
}
```

- [ ] **Step 2: 跑测试**

```bash
cargo test -p gw-auth --test jwt_blacklist
```

Expected: compile error。

- [ ] **Step 3: 实现**

`crates/gw-auth/src/jwt_blacklist.rs`：

```rust
//! JWT blacklist 接口 + Redis 实现 + FailMode 决策。

use async_trait::async_trait;
use fred::interfaces::KeysInterface;

#[derive(Debug, thiserror::Error)]
pub enum BlacklistError {
    #[error("backend: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, Copy)]
pub enum BlacklistFailMode { FailClosed, FailOpen }

impl From<gw_core::entity::BlacklistFailMode> for BlacklistFailMode {
    fn from(v: gw_core::entity::BlacklistFailMode) -> Self {
        match v {
            gw_core::entity::BlacklistFailMode::FailClosed => Self::FailClosed,
            gw_core::entity::BlacklistFailMode::FailOpen   => Self::FailOpen,
        }
    }
}

#[async_trait]
pub trait JwtBlacklist: Send + Sync {
    async fn is_revoked(&self, jti: &str) -> Result<bool, BlacklistError>;
    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), BlacklistError>;
}

/// 高层入口：按 fail mode 折叠 backend error。
pub async fn check_revoked(
    bl: &dyn JwtBlacklist,
    jti: &str,
    mode: BlacklistFailMode,
) -> bool {
    match bl.is_revoked(jti).await {
        Ok(b) => b,
        Err(_) => matches!(mode, BlacklistFailMode::FailClosed),
    }
}

const KEY_PREFIX: &str = "auth:jwt_blacklist:";

pub struct RedisJwtBlacklist {
    pool: fred::clients::RedisPool,
}

impl RedisJwtBlacklist {
    pub fn new(pool: fred::clients::RedisPool) -> Self { Self { pool } }
}

#[async_trait]
impl JwtBlacklist for RedisJwtBlacklist {
    async fn is_revoked(&self, jti: &str) -> Result<bool, BlacklistError> {
        let client = self.pool.next();
        let key = format!("{KEY_PREFIX}{jti}");
        let v: Option<String> = client.get(key).await
            .map_err(|e| BlacklistError::Backend(e.to_string()))?;
        Ok(v.is_some())
    }
    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), BlacklistError> {
        let client = self.pool.next();
        let key = format!("{KEY_PREFIX}{jti}");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0);
        let ttl = until_unix_s.saturating_sub(now).max(1);
        client.set::<(), _, _>(
            key, "1", Some(fred::types::Expiration::EX(ttl as i64)),
            None, false,
        ).await.map_err(|e| BlacklistError::Backend(e.to_string()))?;
        Ok(())
    }
}
```

`gw-auth/src/lib.rs`：

```rust
pub mod jwt_blacklist;
pub use jwt_blacklist::{JwtBlacklist, BlacklistFailMode, RedisJwtBlacklist, check_revoked};
```

`gw-auth/Cargo.toml` 加 `async-trait`、`fred = { workspace = true }`、`thiserror = { workspace = true }`、`tokio = { workspace = true, features = ["macros", "rt"] }`（dev only）。

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-auth --test jwt_blacklist
git add crates/gw-auth
git commit -m "feat(gw-auth): JwtBlacklist trait + Redis impl + FailMode (G-α.7)"
```

---

### Task T1-C: HMAC 签名强制（required + body-after 时序）

**Files:**
- Modify: `crates/gw-auth/src/signature.rs`
- Modify: `crates/gw-core/src/entity.rs`（ApiKeyEntry 加 `signature_required` / `signature_algo`）
- Test: `crates/gw-auth/tests/signature_required.rs`

- [ ] **Step 1: ApiKeyEntry 加字段**

在 `crates/gw-core/src/entity.rs` 的 `ApiKeyEntry` 上加：

```rust
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgo {
    #[default]
    HmacSha256,
    HmacSha512,
}

// ApiKeyEntry 内：
pub signature_required: bool,
pub signature_algo: SignatureAlgo,
```

（如果 ApiKeyEntry 当前没有 `Default`，加 `#[serde(default)]` 在两个新字段上避免反序列化破坏。）

- [ ] **Step 2: 写失败测试**

```rust
use gw_auth::signature::{verify_required, SignatureInput, SignatureCheckResult};
use gw_core::entity::SignatureAlgo;

#[test]
fn required_false_passes_without_header() {
    let r = verify_required(
        false, SignatureAlgo::HmacSha256,
        b"secret", None, None,
        &SignatureInput { method: "POST", path: "/v1", timestamp: "0", body: b"{}" },
    );
    assert_eq!(r, SignatureCheckResult::NotRequired);
}

#[test]
fn required_true_missing_header_fails() {
    let r = verify_required(
        true, SignatureAlgo::HmacSha256,
        b"secret", None, None,
        &SignatureInput { method: "POST", path: "/v1", timestamp: "0", body: b"{}" },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn required_true_bad_sig_fails() {
    let r = verify_required(
        true, SignatureAlgo::HmacSha256,
        b"secret", Some(b"deadbeef"), Some("9999999999"),
        &SignatureInput { method: "POST", path: "/v1", timestamp: "9999999999", body: b"{}" },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn required_true_stale_timestamp_fails() {
    let r = verify_required(
        true, SignatureAlgo::HmacSha256,
        b"secret", Some(b"x"), Some("1"),
        &SignatureInput { method: "POST", path: "/v1", timestamp: "1", body: b"{}" },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p gw-auth --test signature_required
```

Expected: compile error。

- [ ] **Step 4: 实现**

在 `crates/gw-auth/src/signature.rs` 末尾加：

```rust
use gw_core::entity::SignatureAlgo;
use sha2::Sha512;

const REPLAY_WINDOW_SECS: i64 = 300;

#[derive(Debug, PartialEq, Eq)]
pub enum SignatureCheckResult { Valid, Invalid, NotRequired }

pub fn verify_required(
    required: bool,
    algo: SignatureAlgo,
    secret: &[u8],
    received_b64: Option<&[u8]>,
    timestamp_str: Option<&str>,
    input: &SignatureInput<'_>,
) -> SignatureCheckResult {
    if !required { return SignatureCheckResult::NotRequired; }
    let received = match received_b64 { Some(s) => s, None => return SignatureCheckResult::Invalid };
    let ts = match timestamp_str.and_then(|s| s.parse::<i64>().ok()) {
        Some(t) => t, None => return SignatureCheckResult::Invalid,
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64).unwrap_or(0);
    if (now - ts).abs() > REPLAY_WINDOW_SECS {
        return SignatureCheckResult::Invalid;
    }
    let ok = match algo {
        SignatureAlgo::HmacSha256 => verify(secret, received, input),
        SignatureAlgo::HmacSha512 => verify_with::<Sha512>(secret, received, input),
    };
    if ok { SignatureCheckResult::Valid } else { SignatureCheckResult::Invalid }
}

fn verify_with<D>(secret: &[u8], received: &[u8], input: &SignatureInput<'_>) -> bool
where D: digest::Digest + digest::core_api::BlockSizeUser + Clone, // 取决于 sha2 版本
{
    use hmac::{Hmac, Mac};
    let body_hash = {
        use sha2::Digest;
        let mut h = D::new();
        h.update(input.body);
        hex::encode(h.finalize())
    };
    let signing = format!("{}\n{}\n{}\n{}",
        input.method, input.path, input.timestamp, body_hash);
    let mut mac = match Hmac::<D>::new_from_slice(secret) { Ok(m) => m, Err(_) => return false };
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    use subtle::ConstantTimeEq;
    expected.ct_eq(received).into()
}
```

如果 `verify_with::<Sha512>` 的 trait bound 不匹配（hmac/digest 版本特性），改写为只支持 SHA256（algo SHA512 留 trait 槽，未来扩）；测试相应只跑 SHA256 路径。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-auth --test signature_required
git add crates/gw-auth crates/gw-core/src/entity.rs
git commit -m "feat(gw-auth): signature_required + replay window (G-α.8)"
```

---

### Task T1-D: management_token 路径分流 + entity stub

**Files:**
- Create: `crates/gw-auth/src/mgmt.rs`
- Modify: `crates/gw-auth/src/lib.rs`
- Modify: `crates/gw-core/src/entity.rs`
- Test: `crates/gw-auth/tests/mgmt_path.rs`

- [ ] **Step 1: entity stub**

`crates/gw-core/src/entity.rs` 加：

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagementToken {
    pub token_id: String,
    pub secret_hash: Vec<u8>,
    pub scopes: smallvec::SmallVec<[String; 4]>, // e.g. ["mgmt:freeze:write", "mgmt:health:read"]
    pub expires_at_unix_s: Option<u64>,
}
```

- [ ] **Step 2: 写失败测试**

`crates/gw-auth/tests/mgmt_path.rs`：

```rust
use gw_auth::mgmt::{is_management_path, parse_mgmt_authorization, MgmtAuth};

#[test]
fn admin_path_routed() {
    assert!(is_management_path("/admin/freeze"));
    assert!(is_management_path("/admin/"));
    assert!(!is_management_path("/admin"));   // 严格要求斜杠避免 /administer
    assert!(!is_management_path("/v1/messages"));
}

#[test]
fn parses_mgmt_header() {
    let p = parse_mgmt_authorization("Mgmt abc.def.ghi").unwrap();
    assert_eq!(p, MgmtAuth { token: "abc.def.ghi".into() });
}

#[test]
fn rejects_business_authorization() {
    assert!(parse_mgmt_authorization("Bearer abc").is_none());
    assert!(parse_mgmt_authorization("").is_none());
}
```

- [ ] **Step 3: 实现 mgmt.rs**

```rust
//! Management 通道：/admin/* 路径独立鉴权。

#[derive(Debug, PartialEq, Eq)]
pub struct MgmtAuth { pub token: String }

pub fn is_management_path(path: &str) -> bool {
    path.starts_with("/admin/")
}

pub fn parse_mgmt_authorization(header: &str) -> Option<MgmtAuth> {
    let h = header.trim();
    let rest = h.strip_prefix("Mgmt ")?;
    if rest.is_empty() { return None; }
    Some(MgmtAuth { token: rest.to_string() })
}
```

`gw-auth/src/lib.rs` 加 `pub mod mgmt;`。

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-auth --test mgmt_path
git add crates/gw-auth crates/gw-core/src/entity.rs
git commit -m "feat(gw-auth): management_token path + parse stub (G-α.9)"
```

---

### Task T1-E: KEK rotate 状态机（dual-read）

**Files:**
- Modify: `crates/gw-store/src/encrypt.rs`
- Modify: `crates/gw-gateway/src/kms.rs`
- Test: `crates/gw-store/tests/encrypt_dual_read.rs`

- [ ] **Step 1: 扩 KekCache 支持双 KEK**

```rust
// crates/gw-store/src/encrypt.rs

#[derive(Clone)]
pub struct KekCache {
    /// 多版本 KEK，按 kid 索引。read 时 try 全部，write 时按 active_kid。
    inner: Arc<KekCacheInner>,
}

struct KekCacheInner {
    keks: HashMap<String, [u8; 32]>,
    active_kid: arc_swap::ArcSwap<String>,
    state: arc_swap::ArcSwap<KekState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KekState {
    Healthy,
    DualRead { reencrypting: bool },
}

impl KekCache {
    pub fn new(map: HashMap<String, [u8; 32]>) -> Self { Self::with_active(map, None) }

    pub fn with_active(map: HashMap<String, [u8; 32]>, active: Option<&str>) -> Self {
        let active = active
            .map(String::from)
            .or_else(|| map.keys().next().cloned())
            .unwrap_or_default();
        Self {
            inner: Arc::new(KekCacheInner {
                keks: map,
                active_kid: arc_swap::ArcSwap::from_pointee(active),
                state: arc_swap::ArcSwap::from_pointee(KekState::Healthy),
            }),
        }
    }
    pub fn empty() -> Self { Self::new(HashMap::new()) }
    pub(crate) fn get(&self, kid: &str) -> Option<&[u8; 32]> { self.inner.keks.get(kid) }
    pub fn active_kid(&self) -> String { self.inner.active_kid.load_full().as_ref().clone() }
    pub fn state(&self) -> KekState { self.inner.state.load_full().as_ref().clone() }

    /// 进入 dual-read：新 kek 加入 keks 集合，active_kid 切到新 kek。
    /// 不可在 DualRead 状态再次进入（最多 2 KEK 并存）。
    pub fn enter_dual_read(&mut self, new_kid: String, new_kek: [u8; 32]) -> anyhow::Result<()> {
        if matches!(self.state(), KekState::DualRead { .. }) {
            anyhow::bail!("already in dual-read");
        }
        // 新加入需通过 Arc 替换；KekCacheInner 字段 keks 是 immutable HashMap.
        // 重建：
        let mut new_map = self.inner.keks.clone();
        new_map.insert(new_kid.clone(), new_kek);
        let new_inner = KekCacheInner {
            keks: new_map,
            active_kid: arc_swap::ArcSwap::from_pointee(new_kid),
            state: arc_swap::ArcSwap::from_pointee(KekState::DualRead { reencrypting: false }),
        };
        self.inner = Arc::new(new_inner);
        Ok(())
    }

    pub fn mark_reencrypting(&self) {
        self.inner.state.store(Arc::new(KekState::DualRead { reencrypting: true }));
    }

    pub fn drop_old_kek(&mut self, old_kid: &str) -> anyhow::Result<()> {
        if !matches!(self.state(), KekState::DualRead { .. }) {
            anyhow::bail!("not in dual-read");
        }
        let mut new_map = self.inner.keks.clone();
        new_map.remove(old_kid);
        if new_map.is_empty() { anyhow::bail!("cannot drop the only kek"); }
        let active = self.active_kid();
        if !new_map.contains_key(&active) {
            anyhow::bail!("active_kid not in remaining set");
        }
        let new_inner = KekCacheInner {
            keks: new_map,
            active_kid: arc_swap::ArcSwap::from_pointee(active),
            state: arc_swap::ArcSwap::from_pointee(KekState::Healthy),
        };
        self.inner = Arc::new(new_inner);
        Ok(())
    }
}
```

- [ ] **Step 2: 写失败测试**

```rust
use gw_store::encrypt::{decrypt, encrypt_for_test_only, KekCache, KekState};
use std::collections::HashMap;

#[test]
fn dual_read_can_decrypt_old_and_new() {
    let kek_v1 = [1u8; 32];
    let kek_v2 = [2u8; 32];
    let mut map = HashMap::new();
    map.insert("v1".into(), kek_v1);
    let mut cache = KekCache::with_active(map, Some("v1"));
    let enc_v1 = encrypt_for_test_only("plain", kek_v1);

    cache.enter_dual_read("v2".into(), kek_v2).unwrap();
    assert_eq!(cache.active_kid(), "v2");
    assert!(matches!(cache.state(), KekState::DualRead { .. }));

    let mut enc_with_v1 = enc_v1.clone();
    enc_with_v1.kek_kid = "v1".into();
    let _ = decrypt(&enc_with_v1, &cache).unwrap();
}

#[test]
fn cannot_drop_last_kek() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let mut c = KekCache::with_active(map, Some("v1"));
    let _ = c.enter_dual_read("v2".into(), [2u8; 32]);
    let err = c.drop_old_kek("v1");
    assert!(err.is_ok());     // 丢 v1, 留 v2
    let err2 = c.drop_old_kek("v2");
    assert!(err2.is_err());   // 不能丢最后一把
}

#[test]
fn cannot_enter_dual_read_twice() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let mut c = KekCache::with_active(map, Some("v1"));
    c.enter_dual_read("v2".into(), [2u8; 32]).unwrap();
    let err = c.enter_dual_read("v3".into(), [3u8; 32]);
    assert!(err.is_err());
}
```

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-store --test encrypt_dual_read
cargo build --workspace
git add crates/gw-store crates/gw-gateway/src/kms.rs
git commit -m "feat(gw-store): KekCache dual-read state machine (G-α.6 / SE3)"
```

注：`gw-gateway/src/kms.rs` 此 task **不**自动进入 dual-read，仅保持 init 路径用 `KekCache::with_active(map, Some(&kid))`。轮换触发器在管理面 spec，本 plan 不实施。

---

### Task T1-F: gw-capability crate 骨架

**Files:**
- Create: `crates/gw-capability/Cargo.toml`
- Create: `crates/gw-capability/src/lib.rs`
- Create: `crates/gw-capability/src/types.rs`
- Modify: `Cargo.toml`（workspace members）
- Modify: `scripts/check-invariants.sh`
- Test: `crates/gw-capability/tests/types.rs`

- [ ] **Step 1: 注册 workspace member**

`Cargo.toml` 顶层 `members = [...]` 内追加 `"crates/gw-capability"`。

`crates/gw-capability/Cargo.toml`：

```toml
[package]
name = "gw-capability"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"

[dependencies]
gw-core  = { path = "../gw-core" }
gw-store = { path = "../gw-store" }
serde = { workspace = true }
smallvec = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

`crates/gw-capability/tests/types.rs`：

```rust
use gw_capability::types::{
    Capability, CapabilityOrigin, MatchMode, RequiredCapabilities,
};

#[test]
fn required_capabilities_default_compatible() {
    let r = RequiredCapabilities::default();
    assert!(r.set.is_empty());
    assert_eq!(r.mode, MatchMode::Compatible);
}

#[test]
fn required_capabilities_provenance_logs_origin() {
    let mut r = RequiredCapabilities::default();
    r.add(Capability::Vision, CapabilityOrigin::Model);
    r.add(Capability::Thinking, CapabilityOrigin::HeaderRequire);
    assert_eq!(r.set.len(), 2);
    assert_eq!(r.provenance.len(), 2);
}

#[test]
fn add_dedups_capability_but_appends_origin() {
    let mut r = RequiredCapabilities::default();
    r.add(Capability::Vision, CapabilityOrigin::Model);
    r.add(Capability::Vision, CapabilityOrigin::HeaderRequire);
    assert_eq!(r.set.len(), 1);
    assert_eq!(r.provenance.len(), 2);
}
```

- [ ] **Step 3: 实现 types.rs**

```rust
use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Vision, Audio, Video, Thinking, ToolUse, StreamingOnly,
    OpenAiResponsesShape, VertexAiServiceAccount,
    Tagged(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode { Exclusive, Compatible }
impl Default for MatchMode { fn default() -> Self { MatchMode::Compatible } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityOrigin {
    Model, KeyForced, HeaderRequire, AdapterDetected, RetryUpgrade,
}

#[derive(Debug, Clone, Default)]
pub struct RequiredCapabilities {
    pub set: SmallVec<[Capability; 6]>,
    pub mode: MatchMode,
    pub provenance: SmallVec<[(Capability, CapabilityOrigin); 8]>,
}

impl RequiredCapabilities {
    pub fn add(&mut self, c: Capability, origin: CapabilityOrigin) {
        if !self.set.contains(&c) { self.set.push(c); }
        self.provenance.push((c, origin));
    }
}
```

`crates/gw-capability/src/lib.rs`：

```rust
pub mod types;
pub use types::*;
```

- [ ] **Step 4: 加 invariant lint**

`scripts/check-invariants.sh` 在 `SUPPORTING` 之外、`crate_layering` 段下追加：

```bash
echo "[crate_layering] gw-capability is business; must not depend on routing/proxy/adapter/tunnel-client"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  cap_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-capability") | .dependencies[] | select(.kind == null) | .name')"
  for forb in gw-routing gw-proxy gw-adapter gw-tunnel-client gw-gateway; do
    if echo "$cap_deps" | grep -qx "$forb"; then
      bad "gw-capability depends on $forb (business-layer rule)"
    fi
  done
  [ "$fail" -eq 0 ] && ok "gw-capability layering ok"
fi
```

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test types
bash scripts/check-invariants.sh
git add Cargo.toml crates/gw-capability scripts/check-invariants.sh
git commit -m "feat(gw-capability): new crate skeleton + RequiredCapabilities (G-β.2)"
```

---

### Task T1-G: ResilienceState（gw-proxy）

**Files:**
- Create: `crates/gw-proxy/src/resilience.rs`
- Modify: `crates/gw-proxy/src/lib.rs`
- Test: `crates/gw-proxy/tests/resilience.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_proxy::resilience::{ResilienceState, RedisHealthState};

#[test]
fn default_is_healthy() {
    let r = ResilienceState::default();
    assert_eq!(r.redis_state(), RedisHealthState::Healthy);
}

#[test]
fn record_redis_error_degrades() {
    let r = ResilienceState::default();
    r.record_redis_error("get", "timeout");
    assert_eq!(r.redis_state(), RedisHealthState::Degraded);
    assert!(r.last_redis_error_ms() > 0);
}

#[test]
fn record_redis_ok_recovers() {
    let r = ResilienceState::default();
    r.record_redis_error("get", "timeout");
    r.record_redis_ok();
    assert_eq!(r.redis_state(), RedisHealthState::Healthy);
}

#[test]
fn fallback_counters_increment() {
    let r = ResilienceState::default();
    r.bump_freeze_fallback();
    r.bump_cooldown_fallback();
    assert_eq!(r.freeze_fallback_used(), 1);
    assert_eq!(r.cooldown_fallback_used(), 1);
}
```

- [ ] **Step 2: 实现**

`crates/gw-proxy/src/resilience.rs`：

```rust
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisHealthState { Healthy = 0, Degraded = 1, Unavailable = 2 }

impl From<u8> for RedisHealthState {
    fn from(v: u8) -> Self {
        match v { 0 => Self::Healthy, 1 => Self::Degraded, _ => Self::Unavailable }
    }
}

#[derive(Default)]
pub struct ResilienceState {
    redis_state: AtomicU8,
    last_redis_error_ms: AtomicU64,
    freeze_fallback_used: AtomicU64,
    cooldown_fallback_used: AtomicU64,
}

impl ResilienceState {
    pub fn redis_state(&self) -> RedisHealthState {
        self.redis_state.load(Ordering::Acquire).into()
    }
    pub fn last_redis_error_ms(&self) -> u64 {
        self.last_redis_error_ms.load(Ordering::Acquire)
    }
    pub fn record_redis_error(&self, _op: &str, _reason: &str) {
        self.redis_state.store(RedisHealthState::Degraded as u8, Ordering::Release);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64).unwrap_or(0);
        self.last_redis_error_ms.store(ms, Ordering::Release);
        metrics::counter!("gw_resilience_redis_error_total").increment(1);
    }
    pub fn record_redis_ok(&self) {
        self.redis_state.store(RedisHealthState::Healthy as u8, Ordering::Release);
    }
    pub fn bump_freeze_fallback(&self) {
        self.freeze_fallback_used.fetch_add(1, Ordering::AcqRel);
        metrics::counter!("gw_resilience_freeze_fallback_total").increment(1);
    }
    pub fn bump_cooldown_fallback(&self) {
        self.cooldown_fallback_used.fetch_add(1, Ordering::AcqRel);
        metrics::counter!("gw_resilience_cooldown_fallback_total").increment(1);
    }
    pub fn freeze_fallback_used(&self) -> u64 {
        self.freeze_fallback_used.load(Ordering::Acquire)
    }
    pub fn cooldown_fallback_used(&self) -> u64 {
        self.cooldown_fallback_used.load(Ordering::Acquire)
    }
}
```

`gw-proxy/src/lib.rs` 加 `pub mod resilience;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test resilience
git add crates/gw-proxy
git commit -m "feat(gw-proxy): ResilienceState atomic counters (G-α.4)"
```

<!-- WAVE-2 -->

## Wave 2 — Codec residuals + classifier default + gw-tunnel-client skeleton + metrics catalog（6 并行）

### Task T2-A: DefaultClassifier 实现

**Files:**
- Create: `crates/gw-core/src/error/classify_default.rs`
- Modify: `crates/gw-core/src/error/mod.rs`（mod 注册）
- Test: `crates/gw-core/tests/classifier_default.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_core::error::{
    AccountBlockReason, ClassifyCtx, DefaultClassifier, ErrorClass, ErrorClassifier,
    HeadersAccessor, OAuthSignalKind, Retryability,
};
use gw_core::protocol::ProtocolKind;

struct H { rh: Option<&'static str> }
impl HeadersAccessor for H {
    fn get(&self, name: &str) -> Option<&str> {
        if name.eq_ignore_ascii_case("retry-after") { self.rh } else { None }
    }
}

fn ctx() -> ClassifyCtx<'static> {
    ClassifyCtx { key_id: "k1", vendor_id: "v1",
        api_format: ProtocolKind::OpenAi, protocol: ProtocolKind::OpenAi }
}

#[test]
fn upstream_429_with_retry_after_yields_rl_hint() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 429, &H { rh: Some("30") }, None);
    assert!(matches!(oc.class, ErrorClass::RateLimitedUpstream(_)));
    assert_eq!(oc.retryable, Retryability::CrossKey);
    let hint = oc.rate_limit_hint.unwrap();
    assert_eq!(hint.retry_after_secs, Some(30));
    let cd = oc.cooldown.unwrap();
    assert_eq!(cd.ttl_secs, 30);
}

#[test]
fn upstream_401_yields_account_blocked_no_retry_same_key() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 401, &H { rh: None }, None);
    match oc.class {
        ErrorClass::AccountBlocked(r) => {
            assert!(matches!(r, AccountBlockReason::Status401Unauthorized
                | AccountBlockReason::Status401AccountDeactivated));
        }
        _ => panic!()
    }
    assert_eq!(oc.retryable, Retryability::CrossKey);
    assert!(matches!(oc.oauth_signal.unwrap().kind, OAuthSignalKind::AccountBlocked));
}

#[test]
fn upstream_5xx_transient() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 503, &H { rh: None }, None);
    assert_eq!(oc.class, ErrorClass::UpstreamTransient);
    assert_eq!(oc.retryable, Retryability::SameKey);
}
```

- [ ] **Step 2: 实现**

`crates/gw-core/src/error/classify_default.rs`：

```rust
use super::class::*;
use super::ir::ErrorIR;
use crate::ratelimit::RateLimitKind;

pub struct DefaultClassifier;

impl ErrorClassifier for DefaultClassifier {
    fn classify_local(&self, err: &ErrorIR<'_>) -> ClassifierOutcome {
        let class = match err.category {
            crate::error::ErrorCategory::ClientAuth        => ErrorClass::AuthDenied,
            crate::error::ErrorCategory::ClientQuota       => ErrorClass::QuotaDenied,
            crate::error::ErrorCategory::ClientBadRequest  => ErrorClass::ProtocolError,
            crate::error::ErrorCategory::UpstreamTimeout
            | crate::error::ErrorCategory::UpstreamError   => ErrorClass::UpstreamTransient,
            crate::error::ErrorCategory::UpstreamOverloaded
                => ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown),
            crate::error::ErrorCategory::Cancelled         => ErrorClass::ClientAbort,
            crate::error::ErrorCategory::GatewayInternal   => ErrorClass::Internal,
        };
        ClassifierOutcome { class, retryable: Retryability::No, ..Default::default() }
    }

    fn classify_upstream(
        &self,
        _ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        _vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome {
        let retry_after = headers.get("retry-after").and_then(|s| s.parse::<u32>().ok());
        match http_status {
            401 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 2,
                    ttl_secs: retry_after.unwrap_or(3600),
                    reset_consecutive: false,
                }),
                oauth_signal: Some(OAuthSignal { kind: OAuthSignalKind::AccountBlocked }),
                ..Default::default()
            },
            402 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status402PaymentRequired),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective { reason_code: 3, ttl_secs: 3600, reset_consecutive: false }),
                ..Default::default()
            },
            403 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status403AccountSuspended),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective { reason_code: 4, ttl_secs: 1800, reset_consecutive: false }),
                ..Default::default()
            },
            429 => ClassifierOutcome {
                class: ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 7,
                    ttl_secs: retry_after.unwrap_or(60),
                    reset_consecutive: false,
                }),
                rate_limit_hint: Some(RateLimitHint {
                    kind: RateLimitKind::Unknown, retry_after_secs: retry_after,
                }),
                ..Default::default()
            },
            408 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::SameKey,
                cooldown: Some(CooldownDirective { reason_code: 10, ttl_secs: 30, reset_consecutive: false }),
                ..Default::default()
            },
            529 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective { reason_code: 8, ttl_secs: 30, reset_consecutive: false }),
                ..Default::default()
            },
            500..=599 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::SameKey,
                cooldown: Some(CooldownDirective { reason_code: 9, ttl_secs: 30, reset_consecutive: false }),
                ..Default::default()
            },
            _ => ClassifierOutcome {
                class: ErrorClass::ProtocolError,
                retryable: Retryability::No,
                ..Default::default()
            },
        }
    }
}
```

`gw-core/src/error/mod.rs` 加 `mod classify_default; pub use classify_default::DefaultClassifier;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-core --test classifier_default
git add crates/gw-core
git commit -m "feat(gw-core): DefaultClassifier HTTP 码硬编码兜底 (G-α.1)"
```

---

### Task T2-B: 内容编码处理（gzip / br 解压 + 协商）

**Files:**
- Create: `crates/gw-codec/src/compression.rs`
- Modify: `crates/gw-codec/src/lib.rs`
- Modify: `crates/gw-codec/Cargo.toml`（加 `flate2` `brotli`）
- Modify: `Cargo.toml`（workspace dep `brotli`）
- Test: `crates/gw-codec/tests/compression.rs`

- [ ] **Step 1: workspace dep**

`Cargo.toml` `[workspace.dependencies]` 内追加：

```toml
flate2 = "1"
brotli = "7"
```

`crates/gw-codec/Cargo.toml` `[dependencies]` 加：

```toml
flate2 = { workspace = true }
brotli = { workspace = true }
bytes  = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

`crates/gw-codec/tests/compression.rs`：

```rust
use bytes::Bytes;
use gw_codec::compression::{decode, encode, Encoding};

#[test]
fn gzip_roundtrip() {
    let data = b"hello compress world".repeat(64);
    let enc = encode(Encoding::Gzip, &data).unwrap();
    let dec = decode(Encoding::Gzip, &enc).unwrap();
    assert_eq!(&dec[..], &data[..]);
}

#[test]
fn brotli_roundtrip() {
    let data = b"hello compress world".repeat(64);
    let enc = encode(Encoding::Br, &data).unwrap();
    let dec = decode(Encoding::Br, &enc).unwrap();
    assert_eq!(&dec[..], &data[..]);
}

#[test]
fn identity_passthrough() {
    let data = Bytes::from_static(b"raw");
    let enc = encode(Encoding::Identity, &data).unwrap();
    assert_eq!(&enc[..], b"raw");
}

#[test]
fn negotiate_prefers_br_over_gzip_when_both_listed() {
    use gw_codec::compression::negotiate_outbound;
    assert_eq!(negotiate_outbound("gzip, br"), Encoding::Br);
    assert_eq!(negotiate_outbound("gzip"),     Encoding::Gzip);
    assert_eq!(negotiate_outbound("identity"), Encoding::Identity);
    assert_eq!(negotiate_outbound("zstd"),     Encoding::Identity); // v1 不接受
}

#[test]
fn parse_content_encoding() {
    use gw_codec::compression::parse_content_encoding;
    assert_eq!(parse_content_encoding(Some("gzip")), Encoding::Gzip);
    assert_eq!(parse_content_encoding(Some("br")),   Encoding::Br);
    assert_eq!(parse_content_encoding(Some("identity")), Encoding::Identity);
    assert_eq!(parse_content_encoding(None),         Encoding::Identity);
}
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p gw-codec --test compression
```

Expected: compile error。

- [ ] **Step 4: 实现**

`crates/gw-codec/src/compression.rs`：

```rust
//! 入站解压 + 出站协商 + 出向压缩。
//! v1 支持 gzip / br；zstd 留 trait 槽，遇到时降级到 identity。

use bytes::Bytes;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding { Identity, Gzip, Br }

#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("brotli: {0}")]
    Brotli(String),
}

pub fn decode(enc: Encoding, data: &[u8]) -> Result<Bytes, CompressionError> {
    match enc {
        Encoding::Identity => Ok(Bytes::copy_from_slice(data)),
        Encoding::Gzip => {
            let mut d = flate2::read::GzDecoder::new(data);
            let mut out = Vec::with_capacity(data.len() * 4);
            d.read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        Encoding::Br => {
            let mut out = Vec::with_capacity(data.len() * 4);
            brotli::Decompressor::new(data, 4096).read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
    }
}

pub fn encode(enc: Encoding, data: &[u8]) -> Result<Bytes, CompressionError> {
    match enc {
        Encoding::Identity => Ok(Bytes::copy_from_slice(data)),
        Encoding::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data)?;
            Ok(Bytes::from(e.finish()?))
        }
        Encoding::Br => {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams::default();
            brotli::BrotliCompress(&mut std::io::Cursor::new(data), &mut out, &params)
                .map_err(|e| CompressionError::Brotli(format!("{e:?}")))?;
            Ok(Bytes::from(out))
        }
    }
}

pub fn parse_content_encoding(header: Option<&str>) -> Encoding {
    match header.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("gzip") => Encoding::Gzip,
        Some(s) if s.eq_ignore_ascii_case("br")   => Encoding::Br,
        _ => Encoding::Identity,
    }
}

pub fn negotiate_outbound(accept: &str) -> Encoding {
    let lower = accept.to_ascii_lowercase();
    if lower.split(',').any(|p| p.trim().starts_with("br")) {
        Encoding::Br
    } else if lower.split(',').any(|p| p.trim().starts_with("gzip")) {
        Encoding::Gzip
    } else {
        Encoding::Identity
    }
}
```

`gw-codec/src/lib.rs` 加 `pub mod compression;`。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test compression
git add Cargo.toml crates/gw-codec
git commit -m "feat(gw-codec): gzip/br compression negotiation + decode/encode (G-γ.2)"
```

> SSE 强制 identity 的运行期判定 + filter 集成在 Wave 6 Task T6-C 完成。

---

### Task T2-C: stream_policy 决策表（仅接口 + 测试，不接 filter）

**Files:**
- Create: `crates/gw-codec/src/stream_policy.rs`
- Modify: `crates/gw-codec/src/lib.rs`
- Test: `crates/gw-codec/tests/stream_policy.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_codec::stream_policy::{decide, UpstreamStreamCapability, UpstreamStreamPolicy};

#[test]
fn truth_table() {
    let both = UpstreamStreamCapability { supports_sync: true, supports_stream: true };
    let only_stream = UpstreamStreamCapability { supports_sync: false, supports_stream: true };
    let only_sync = UpstreamStreamCapability { supports_sync: true, supports_stream: false };

    assert_eq!(decide(false, both), UpstreamStreamPolicy::PassThroughSync);
    assert_eq!(decide(true,  both), UpstreamStreamPolicy::PassThroughStream);
    assert_eq!(decide(false, only_stream), UpstreamStreamPolicy::AccumulateUpstreamStream);
    assert_eq!(decide(true,  only_sync),   UpstreamStreamPolicy::SimulateStreamFromSync);
}
```

- [ ] **Step 2: 实现**

```rust
// crates/gw-codec/src/stream_policy.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamStreamPolicy {
    PassThroughSync,
    AccumulateUpstreamStream,
    PassThroughStream,
    SimulateStreamFromSync,
}

#[derive(Debug, Clone, Copy)]
pub struct UpstreamStreamCapability {
    pub supports_sync: bool,
    pub supports_stream: bool,
}

pub fn decide(client_wants_stream: bool, caps: UpstreamStreamCapability) -> UpstreamStreamPolicy {
    match (client_wants_stream, caps.supports_sync, caps.supports_stream) {
        (false, true, _) => UpstreamStreamPolicy::PassThroughSync,
        (false, false, true) => UpstreamStreamPolicy::AccumulateUpstreamStream,
        (true, _, true) => UpstreamStreamPolicy::PassThroughStream,
        (true, true, false) => UpstreamStreamPolicy::SimulateStreamFromSync,
        // 无能力（!sync && !stream）→ 误配，按 sync 走给上层报错
        (_, false, false) => UpstreamStreamPolicy::PassThroughSync,
    }
}
```

`gw-codec/src/lib.rs` 加 `pub mod stream_policy;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test stream_policy
git add crates/gw-codec
git commit -m "feat(gw-codec): UpstreamStreamPolicy decision table (G-γ.1 interface)"
```

---

### Task T2-D: model_permissions 正则 + ReDoS 防护

**Files:**
- Create: `crates/gw-auth/src/model_permissions.rs`
- Modify: `crates/gw-auth/src/lib.rs`
- Modify: `crates/gw-auth/Cargo.toml`（加 `regex`）
- Modify: `Cargo.toml`（workspace dep `regex` `+ regex-automata`）
- Test: `crates/gw-auth/tests/model_permissions.rs`

- [ ] **Step 1: 加依赖**

`Cargo.toml`：

```toml
regex = "1"
```

`crates/gw-auth/Cargo.toml`：

```toml
regex = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

```rust
use gw_auth::model_permissions::{
    check_model_against_permissions, compile_pattern, ModelPermission, PermissionEffect,
};

#[test]
fn allow_then_deny_specific() {
    let p = vec![
        ModelPermission::new("^claude-.*$", PermissionEffect::Allow).unwrap(),
        ModelPermission::new("^claude-3-haiku.*$", PermissionEffect::Deny).unwrap(),
    ];
    assert!(check_model_against_permissions("claude-3-opus", &p));
    assert!(!check_model_against_permissions("claude-3-haiku-20240307", &p));
}

#[test]
fn no_match_default_deny() {
    let p = vec![
        ModelPermission::new("^gpt-4.*$", PermissionEffect::Allow).unwrap(),
    ];
    assert!(!check_model_against_permissions("claude-3-opus", &p));
}

#[test]
fn compile_rejects_overlong_pattern() {
    // 用一个超大复杂度 pattern：（regex crate 自带 size_limit 会拒；这里再加业务上限）
    let big = "(a|b|c|d|e|f){50}".to_string() + &"x".repeat(2000);
    assert!(compile_pattern(&big).is_err());
}
```

- [ ] **Step 3: 实现**

```rust
// crates/gw-auth/src/model_permissions.rs

use regex::{Regex, RegexBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionEffect { Allow, Deny }

#[derive(Debug, Clone)]
pub struct ModelPermission {
    pub regex: Regex,
    pub raw_pattern: String,
    pub effect: PermissionEffect,
}

impl ModelPermission {
    pub fn new(pattern: &str, effect: PermissionEffect) -> Result<Self, RegexError> {
        let regex = compile_pattern(pattern)?;
        Ok(Self { regex, raw_pattern: pattern.to_string(), effect })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegexError {
    #[error("pattern too large")]
    TooLarge,
    #[error("regex compile: {0}")]
    Compile(String),
}

const MAX_PATTERN_LEN: usize = 1024;
/// 8 MB DFA size limit — 默认 1MB 太苛刻，但仍远低于无界
const DFA_SIZE_LIMIT: usize = 8 * 1024 * 1024;
/// regex crate size_limit (源码 NFA size)
const NFA_SIZE_LIMIT: usize = 256 * 1024;

pub fn compile_pattern(pattern: &str) -> Result<Regex, RegexError> {
    if pattern.len() > MAX_PATTERN_LEN { return Err(RegexError::TooLarge); }
    RegexBuilder::new(pattern)
        .size_limit(NFA_SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Allow-then-Deny semantics: 走遍 perms，命中最后一条 effect 决定。
/// 默认 Deny。
pub fn check_model_against_permissions(model: &str, perms: &[ModelPermission]) -> bool {
    let mut allow = false;
    for p in perms {
        if p.regex.is_match(model) {
            allow = matches!(p.effect, PermissionEffect::Allow);
        }
    }
    allow
}
```

`gw-auth/src/lib.rs` 加 `pub mod model_permissions;`。

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-auth --test model_permissions
git add crates/gw-auth Cargo.toml
git commit -m "feat(gw-auth): regex-based model_permissions with size limits (G-β.7 / AU7)"
```

> 100ms 运行时熔断 + 单 ApiKey 自动 disable 在 Wave 4 接入 capability 时落地。本 task 仅编译期 reject。

---

### Task T2-E: gw-tunnel-client crate skeleton + frame protocol

**Files:**
- Create: `crates/gw-tunnel-client/Cargo.toml`
- Create: `crates/gw-tunnel-client/src/lib.rs`
- Create: `crates/gw-tunnel-client/src/frame.rs`
- Modify: `Cargo.toml`（workspace member）
- Modify: `scripts/check-invariants.sh`（lint）
- Test: `crates/gw-tunnel-client/tests/frame.rs`

- [ ] **Step 1: workspace member + cargo manifest**

`Cargo.toml` `members = [...]` 加 `"crates/gw-tunnel-client"`。

`crates/gw-tunnel-client/Cargo.toml`：

```toml
[package]
name = "gw-tunnel-client"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"

[features]
default = []

[dependencies]
gw-core  = { path = "../gw-core" }
gw-store = { path = "../gw-store" }
bytes    = { workspace = true }
tokio    = { workspace = true, features = ["sync", "net", "macros", "rt", "time"] }
thiserror = { workspace = true }
serde    = { workspace = true }
url      = { workspace = true }
secrecy  = { workspace = true }
arc-swap = { workspace = true }
parking_lot = { workspace = true }
rand     = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

`crates/gw-tunnel-client/tests/frame.rs`：

```rust
use bytes::{Bytes, BytesMut};
use gw_tunnel_client::frame::{decode_frame, encode_frame, Frame, MsgType};

#[test]
fn open_data_close_roundtrip() {
    for (mt, payload) in [
        (MsgType::Open,  Bytes::from_static(b"target=https://api.x")),
        (MsgType::Data,  Bytes::from_static(b"hello body")),
        (MsgType::Close, Bytes::new()),
        (MsgType::Ping,  Bytes::from_static(b"pingpayload")),
    ] {
        let f = Frame { msg_type: mt, flags: 0, stream_id: 12345, payload: payload.clone() };
        let mut buf = BytesMut::new();
        encode_frame(&f, &mut buf);
        let (decoded, consumed) = decode_frame(&buf).expect("decode").expect("frame");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.msg_type, mt);
        assert_eq!(decoded.stream_id, 12345);
        assert_eq!(&decoded.payload[..], &payload[..]);
    }
}

#[test]
fn truncated_returns_need_more() {
    let buf = vec![0xAEu8, 0x21]; // magic only
    let r = decode_frame(&buf).expect("decode err shouldn't fire");
    assert!(r.is_none());
}

#[test]
fn bad_magic_errors() {
    let buf = vec![0xFFu8; 32];
    let r = decode_frame(&buf);
    assert!(r.is_err());
}

#[test]
fn unknown_msg_type_errors() {
    let mut buf = BytesMut::new();
    encode_frame(&Frame {
        msg_type: MsgType::Data, flags: 0, stream_id: 1, payload: Bytes::from_static(b"a")
    }, &mut buf);
    buf[3] = 0xEE; // 替换 msg_type byte
    let r = decode_frame(&buf);
    assert!(r.is_err());
}
```

- [ ] **Step 3: 实现 frame.rs**

`crates/gw-tunnel-client/src/frame.rs`：

```rust
//! Tunnel frame codec — 与 aether-hub/src/protocol.rs 同形态。
//! 帧格式：magic(2) | version(1) | msg_type(1) | flags(2) | stream_id(4) | length(4) | payload(length)

use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const MAGIC: [u8; 2] = [0xAE, 0x21];
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Open  = 0x01,
    Data  = 0x02,
    Close = 0x03,
    Ping  = 0x04,
    Pong  = 0x05,
    Error = 0x06,
    ControlAuth     = 0x10,
    ControlNodeInfo = 0x11,
}

impl MsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Open), 0x02 => Some(Self::Data),
            0x03 => Some(Self::Close), 0x04 => Some(Self::Ping),
            0x05 => Some(Self::Pong), 0x06 => Some(Self::Error),
            0x10 => Some(Self::ControlAuth),
            0x11 => Some(Self::ControlNodeInfo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub msg_type: MsgType,
    pub flags: u16,
    pub stream_id: u32,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("bad magic")] BadMagic,
    #[error("unsupported version")] BadVersion,
    #[error("unknown msg type")] UnknownMsgType,
    #[error("payload exceeds {0} bytes")] PayloadTooLarge(usize),
}

pub const MAX_PAYLOAD_LEN: usize = 4 * 1024 * 1024; // 4MB

pub fn encode_frame(f: &Frame, buf: &mut BytesMut) {
    buf.reserve(HEADER_LEN + f.payload.len());
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(f.msg_type as u8);
    buf.put_u16(f.flags);
    buf.put_u32(f.stream_id);
    buf.put_u32(f.payload.len() as u32);
    buf.put_slice(&f.payload);
}

/// 解码：返回 (Frame, consumed bytes) 或 None（数据不足，等待更多字节）；
/// 协议错误 → Err。
pub fn decode_frame(input: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
    if input.len() < HEADER_LEN { return Ok(None); }
    if &input[0..2] != MAGIC { return Err(FrameError::BadMagic); }
    if input[2] != VERSION { return Err(FrameError::BadVersion); }
    let mt = MsgType::from_u8(input[3]).ok_or(FrameError::UnknownMsgType)?;
    let flags = u16::from_be_bytes([input[4], input[5]]);
    let stream_id = u32::from_be_bytes([input[6], input[7], input[8], input[9]]);
    let length = u32::from_be_bytes([input[10], input[11], input[12], input[13]]) as usize;
    if length > MAX_PAYLOAD_LEN { return Err(FrameError::PayloadTooLarge(MAX_PAYLOAD_LEN)); }
    let total = HEADER_LEN + length;
    if input.len() < total { return Ok(None); }
    let payload = Bytes::copy_from_slice(&input[HEADER_LEN..total]);
    Ok(Some((Frame { msg_type: mt, flags, stream_id, payload }, total)))
}
```

`crates/gw-tunnel-client/src/lib.rs`：

```rust
pub mod frame;
```

- [ ] **Step 4: 加 invariant lint**

`scripts/check-invariants.sh` 追加：

```bash
echo "[crate_layering] gw-tunnel-client business; must not depend on execution"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  tc_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-tunnel-client") | .dependencies[] | select(.kind == null) | .name')"
  for forb in gw-proxy gw-gateway gw-task-runtime gw-routing; do
    if echo "$tc_deps" | grep -qx "$forb"; then
      bad "gw-tunnel-client depends on $forb"
    fi
  done
fi
```

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-tunnel-client --test frame
bash scripts/check-invariants.sh
git add Cargo.toml crates/gw-tunnel-client scripts/check-invariants.sh
git commit -m "feat(gw-tunnel-client): new crate skeleton + frame codec (G-ε.1)"
```

---

### Task T2-F: Metrics catalog（gw-gateway 集中声明）

**Files:**
- Create: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `crates/gw-gateway/src/observability.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `crates/gw-gateway/tests/metrics_catalog.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_gateway::metrics_catalog::{describe_all, METRICS};

#[test]
fn catalog_includes_required_families() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gw_codec_convert_total",
        "gw_codec_convert_duration_seconds",
        "gw_freeze_acquire_total",
        "gw_freeze_release_total",
        "gw_auth_failure_total",
        "gw_target_filter_denied_total",
        "gw_redos_timeout_total",
        "gw_kms_state",
        "gw_oauth_refresh_total",
        "gw_capability_resolve_total",
    ] {
        assert!(names.contains(&required), "missing: {required}");
    }
}

#[test]
fn describe_all_runs_clean() {
    // Just verifies no panics & helps lint cardinality.
    describe_all();
}
```

- [ ] **Step 2: 实现 catalog**

```rust
// crates/gw-gateway/src/metrics_catalog.rs

use metrics::Unit;

pub struct MetricSpec {
    pub name: &'static str,
    pub kind: MetricKind,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum MetricKind { Counter, Gauge, Histogram }

pub static METRICS: &[MetricSpec] = &[
    MetricSpec { name: "gw_codec_convert_total", kind: MetricKind::Counter,
        help: "codec conversion outcomes; labels=direction,source,target,status" },
    MetricSpec { name: "gw_codec_convert_duration_seconds", kind: MetricKind::Histogram,
        help: "codec conversion latency seconds" },
    MetricSpec { name: "gw_codec_passthrough_bytes_total", kind: MetricKind::Counter,
        help: "raw bytes passed through codec" },
    MetricSpec { name: "gw_freeze_acquire_total", kind: MetricKind::Counter,
        help: "freeze acquire outcomes; labels=outcome" },
    MetricSpec { name: "gw_freeze_release_total", kind: MetricKind::Counter,
        help: "freeze release outcomes; labels=outcome" },
    MetricSpec { name: "gw_freeze_estimate_micro", kind: MetricKind::Histogram,
        help: "freeze estimate in micro-cents" },
    MetricSpec { name: "gw_auth_failure_total", kind: MetricKind::Counter,
        help: "auth failures; labels=reason" },
    MetricSpec { name: "gw_target_filter_denied_total", kind: MetricKind::Counter,
        help: "outbound targets denied by filter; labels=host" },
    MetricSpec { name: "gw_redos_timeout_total", kind: MetricKind::Counter,
        help: "regex match exceeded budget; labels=api_key_id" },
    MetricSpec { name: "gw_kms_state", kind: MetricKind::Gauge,
        help: "KMS state; labels=state (healthy/dual_read/reencrypting)" },
    MetricSpec { name: "gw_kms_reencrypt_progress", kind: MetricKind::Gauge,
        help: "KEK rotate progress 0..1" },
    MetricSpec { name: "gw_oauth_refresh_total", kind: MetricKind::Counter,
        help: "oauth refresh outcomes; labels=vendor,outcome" },
    MetricSpec { name: "gw_oauth_token_age_seconds", kind: MetricKind::Histogram,
        help: "oauth token age at use; labels=vendor" },
    MetricSpec { name: "gw_capability_resolve_total", kind: MetricKind::Counter,
        help: "capability resolve outcomes; labels=match" },
    MetricSpec { name: "gw_capability_unknown_total", kind: MetricKind::Counter,
        help: "capability unknown for vendor; labels=vendor" },
    MetricSpec { name: "gw_resilience_redis_error_total", kind: MetricKind::Counter,
        help: "redis ops failed" },
    MetricSpec { name: "gw_resilience_freeze_fallback_total", kind: MetricKind::Counter,
        help: "freeze fallback used" },
    MetricSpec { name: "gw_resilience_cooldown_fallback_total", kind: MetricKind::Counter,
        help: "cooldown fallback used" },
    MetricSpec { name: "gw_detailed_log_oversized_total", kind: MetricKind::Counter,
        help: "detailed log dropped due to size" },
];

pub fn describe_all() {
    for m in METRICS {
        match m.kind {
            MetricKind::Counter   => metrics::describe_counter!(m.name, Unit::Count, m.help),
            MetricKind::Gauge     => metrics::describe_gauge!(m.name, m.help),
            MetricKind::Histogram => metrics::describe_histogram!(m.name, m.help),
        }
    }
}
```

`gw-gateway/src/observability.rs`：在 `init_metrics` 末尾、`PrometheusBuilder` install 之后，追加 `crate::metrics_catalog::describe_all();`。

`gw-gateway/src/lib.rs` 加 `pub mod metrics_catalog;`（仅 internal 暴露给 tests）。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-gateway --test metrics_catalog
git add crates/gw-gateway
git commit -m "feat(gw-gateway): metrics catalog (G-δ.1 baseline)"
```

<!-- WAVE-3 -->

## Wave 3 — Handler + listeners + tunnel transport + log v2 + codec residuals 收尾（6 并行）

### Task T3-A: ErrorHandler + 6 listeners

**Files:**
- Create: `crates/gw-proxy/src/error/mod.rs`
- Create: `crates/gw-proxy/src/error/handler.rs`
- Create: `crates/gw-proxy/src/error/listeners/{mod,key_health,rate_limit,oauth,cache_invalidate,metrics,ledger}.rs`
- Modify: `crates/gw-proxy/src/lib.rs`
- Test: `crates/gw-proxy/tests/error_handler.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-proxy/tests/error_handler.rs`：

```rust
use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, CooldownDirective, ErrorClass,
    OAuthSignal, OAuthSignalKind, RateLimitHint, Retryability,
};
use gw_core::ratelimit::RateLimitKind;
use gw_proxy::error::handler::{ErrorHandler, ErrorListener, HandlerCtx};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct CountingListener { name: &'static str, calls: AtomicUsize, order_log: Arc<parking_lot::Mutex<Vec<&'static str>>> }
impl ErrorListener for CountingListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, _oc: &ClassifierOutcome) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.order_log.lock().push(self.name);
    }
}

#[test]
fn fanout_invokes_listeners_in_registered_order() {
    let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let h = ErrorHandler::new(vec![
        Arc::new(CountingListener { name: "key_health", order_log: log.clone(), ..Default::default() }) as _,
        Arc::new(CountingListener { name: "rate_limit", order_log: log.clone(), ..Default::default() }) as _,
        Arc::new(CountingListener { name: "oauth",      order_log: log.clone(), ..Default::default() }) as _,
        Arc::new(CountingListener { name: "cache",      order_log: log.clone(), ..Default::default() }) as _,
        Arc::new(CountingListener { name: "metrics",    order_log: log.clone(), ..Default::default() }) as _,
        Arc::new(CountingListener { name: "ledger",     order_log: log.clone(), ..Default::default() }) as _,
    ]);
    let oc = ClassifierOutcome {
        class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective { reason_code: 2, ttl_secs: 3600, reset_consecutive: false }),
        rate_limit_hint: Some(RateLimitHint { kind: RateLimitKind::Unknown, retry_after_secs: None }),
        oauth_signal: Some(OAuthSignal { kind: OAuthSignalKind::AccountBlocked }),
        passthrough_body: None,
    };
    let ctx = HandlerCtx::stub();
    h.fanout(&ctx, &oc);
    assert_eq!(*log.lock(), vec!["key_health","rate_limit","oauth","cache","metrics","ledger"]);
}
```

- [ ] **Step 2: 实现 handler 骨架**

`crates/gw-proxy/src/error/handler.rs`：

```rust
use gw_core::error::ClassifierOutcome;
use std::sync::Arc;

pub trait ErrorListener: Send + Sync {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, outcome: &ClassifierOutcome);
}

pub struct ErrorHandler {
    listeners: Vec<Arc<dyn ErrorListener>>,
}

impl ErrorHandler {
    pub fn new(listeners: Vec<Arc<dyn ErrorListener>>) -> Self { Self { listeners } }

    pub fn fanout(&self, ctx: &HandlerCtx<'_>, outcome: &ClassifierOutcome) {
        for l in &self.listeners { l.on_outcome(ctx, outcome); }
    }
}

pub struct HandlerCtx<'a> {
    /// 占位字段；接通真实 ctx 在 Wave 6 完成。
    pub request_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub vendor_key_id: Option<&'a str>,
    pub api_format: Option<gw_core::protocol::ProtocolKind>,
}
impl HandlerCtx<'_> {
    pub fn stub() -> Self {
        Self { request_id: "", api_key_id: None, vendor_key_id: None, api_format: None }
    }
}
```

`crates/gw-proxy/src/error/mod.rs`：

```rust
pub mod handler;
pub mod listeners;
```

每个 listener 文件实现一个 stub（fanout 顺序注册时占位），各自 publish 一条 metric 即可，**真实副作用接线**在 Wave 6 完成：

```rust
// listeners/key_health.rs
use super::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

pub struct KeyHealthListener {
    // store: Arc<gw_health::KeyHealthStore>, // Wave 6 接通
}

impl ErrorListener for KeyHealthListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if let Some(_cd) = &oc.cooldown {
            metrics::counter!("gw_resilience_cooldown_fallback_total").increment(0);
            // Wave 6: store.apply_cooldown(...);
        }
    }
}
```

类似为 RateLimitFeedback / OAuth / CacheInvalidate / Metrics / Ledger 各写一个空实现。

`gw-proxy/src/lib.rs` 加 `pub mod error;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test error_handler
git add crates/gw-proxy
git commit -m "feat(gw-proxy): ErrorHandler + 6 listener stubs in fixed order (G-α.2)"
```

---

### Task T3-B: OpenAI Responses wire types

**Files:**
- Create: `crates/gw-codec/src/openai/responses/{mod,wire_request,wire_response,wire_stream_chunk}.rs`
- Modify: `crates/gw-codec/src/openai/mod.rs`
- Modify: `crates/gw-codec/src/wire/any.rs`（WireAnyRequest 加 OpenAiResponses）
- Test: `crates/gw-codec/tests/openai_responses_wire.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_codec::openai::responses::{OpenAiResponsesRequest, OpenAiResponsesResponse};

#[test]
fn deserialize_minimal_request() {
    let s = r#"{"model":"gpt-5","input":[{"type":"input_text","text":"hi"}],"stream":true}"#;
    let r: OpenAiResponsesRequest = serde_json::from_str(s).unwrap();
    assert_eq!(r.model, "gpt-5");
    assert!(r.stream);
}

#[test]
fn deserialize_response_object() {
    let s = r#"{"id":"resp_1","object":"response","model":"gpt-5","output":[]}"#;
    let r: OpenAiResponsesResponse = serde_json::from_str(s).unwrap();
    assert_eq!(r.id, "resp_1");
    assert_eq!(r.model, "gpt-5");
}
```

- [ ] **Step 2: 实现 wire types（最小字段集，足够转发 + capability 决策）**

```rust
// crates/gw-codec/src/openai/responses/wire_request.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: Vec<Value>,                  // ContentPart 列表（v1 用 Value 占位）
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// 透传剩余字段，避免上游加新参数时无法转发。
    #[serde(flatten)]
    pub extras: serde_json::Map<String, Value>,
}
```

```rust
// crates/gw-codec/src/openai/responses/wire_response.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesResponse {
    pub id: String,
    pub object: String,                     // "response"
    pub model: String,
    pub output: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(flatten)]
    pub extras: serde_json::Map<String, Value>,
}
```

```rust
// crates/gw-codec/src/openai/responses/wire_stream_chunk.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesStreamChunk {
    pub r#type: String,                     // e.g. "response.output_text.delta"
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}
```

`mod.rs`：

```rust
pub mod wire_request;
pub mod wire_response;
pub mod wire_stream_chunk;
pub use wire_request::OpenAiResponsesRequest;
pub use wire_response::OpenAiResponsesResponse;
pub use wire_stream_chunk::OpenAiResponsesStreamChunk;
```

`crates/gw-codec/src/openai/mod.rs` 加 `pub mod responses;`。

`crates/gw-codec/src/wire/any.rs` 的 `WireAnyRequest` enum 加变体（看现有形态选 newtype 或 named field）：

```rust
pub enum WireAnyRequest {
    OpenAiChat(/* existing */),
    OpenAiResponses(crate::openai::responses::OpenAiResponsesRequest),
    Anthropic(/* existing */),
    Gemini(/* existing */),
}
```

如果 `WireAnyRequest` 还是空骨架（S1 占位），加变体不破坏现有调用方。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test openai_responses_wire
git add crates/gw-codec
git commit -m "feat(gw-codec): OpenAI Responses wire types stub (G-γ.3)"
```

> 真实跨协议矩阵转换在 codec S1 矩阵宏完成后接入；本 task 仅 wire types。

---

### Task T3-C: Gemini Files API（gateway 端中转）

**Files:**
- Create: `crates/gw-codec/src/gemini/files.rs`
- Create: `crates/gw-store/src/gemini_file_mapping.rs`
- Modify: `crates/gw-store/src/entity_store.rs`（加 `gemini_file_mapping` cache）
- Modify: `crates/gw-store/src/lib.rs`
- Modify: `crates/gw-store/migrations/0002_gemini_file_mapping.sql`（NEW）
- Test: `crates/gw-codec/tests/gemini_files.rs`

- [ ] **Step 1: 写 SQL migration**

`crates/gw-store/migrations/0002_gemini_file_mapping.sql`：

```sql
create table if not exists gemini_file_mapping (
    sha256_hex text not null,
    user_id text not null,
    file_uri text not null,
    mime_type text not null,
    size_bytes bigint not null,
    expire_unix_s bigint not null,
    created_at timestamptz not null default now(),
    primary key (sha256_hex, user_id)
);

create index if not exists idx_gemini_file_mapping_expire on gemini_file_mapping (expire_unix_s);
```

- [ ] **Step 2: 写失败测试**

```rust
use gw_codec::gemini::files::{GeminiFileRef, GeminiFilesGateway, UploadCtx, FakeFilesGateway};

#[tokio::test]
async fn dedup_same_sha_only_once() {
    let g = FakeFilesGateway::default();
    let ctx = UploadCtx { user_id: "u1".into() };
    let bytes = b"image-bytes";
    let r1 = g.ensure_uploaded(&ctx, bytes, "image/png").await.unwrap();
    let r2 = g.ensure_uploaded(&ctx, bytes, "image/png").await.unwrap();
    assert_eq!(r1.file_uri, r2.file_uri);
    assert_eq!(g.upload_count(), 1);
}

#[tokio::test]
async fn different_user_uploads_independently() {
    let g = FakeFilesGateway::default();
    let bytes = b"image-bytes";
    g.ensure_uploaded(&UploadCtx { user_id: "u1".into() }, bytes, "image/png").await.unwrap();
    g.ensure_uploaded(&UploadCtx { user_id: "u2".into() }, bytes, "image/png").await.unwrap();
    assert_eq!(g.upload_count(), 2);
}
```

- [ ] **Step 3: 实现接口 + Fake**

```rust
// crates/gw-codec/src/gemini/files.rs
use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct GeminiFileRef {
    pub file_uri: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub expire_unix_s: u64,
}

pub struct UploadCtx { pub user_id: String }

#[async_trait]
pub trait GeminiFilesGateway: Send + Sync {
    async fn ensure_uploaded(
        &self, ctx: &UploadCtx, bytes: &[u8], mime: &str,
    ) -> anyhow::Result<GeminiFileRef>;
}

#[derive(Default)]
pub struct FakeFilesGateway {
    map: Mutex<HashMap<(String, String), GeminiFileRef>>, // (user, sha256) -> ref
    uploads: AtomicU64,
}

impl FakeFilesGateway {
    pub fn upload_count(&self) -> u64 { self.uploads.load(Ordering::Acquire) }
}

#[async_trait]
impl GeminiFilesGateway for FakeFilesGateway {
    async fn ensure_uploaded(
        &self, ctx: &UploadCtx, bytes: &[u8], mime: &str,
    ) -> anyhow::Result<GeminiFileRef> {
        use sha2::{Digest, Sha256};
        let sha = hex::encode(Sha256::digest(bytes));
        let key = (ctx.user_id.clone(), sha);
        if let Some(r) = self.map.lock().get(&key) { return Ok(r.clone()); }
        self.uploads.fetch_add(1, Ordering::AcqRel);
        let r = GeminiFileRef {
            file_uri: format!("files/{}", &key.1[..8]),
            mime_type: mime.into(),
            size_bytes: bytes.len() as u64,
            expire_unix_s: now() + 48 * 3600,
        };
        self.map.lock().insert(key, r.clone());
        Ok(r)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0)
}
```

> 真正访问 Gemini upload API 的 impl 由 adapter A-T2 (gemini_cli) 提供注入；本 task 仅约定 trait + Fake + entity 表。

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test gemini_files
git add crates/gw-codec crates/gw-store/migrations
git commit -m "feat(gw-codec): GeminiFilesGateway trait + dedup logic (G-γ.4)"
```

> **Wave 3 deferral note**: the entity-store loader for `gemini_file_mapping` is intentionally NOT added in T3-C. Production wiring of upload-and-cache lands in T6-E alongside the detailed-log stream sink, when adapter A-T2 (gemini_cli) is the natural injection point. Migration `0003_model_mapping.sql` and `0002_gemini_file_mapping.sql` ship pre-staged for the operator.

---

### Task T3-D: gw-tunnel-client pool + resolver + safe_dns + target_filter

**Files:**
- Create: `crates/gw-tunnel-client/src/pool.rs`
- Create: `crates/gw-tunnel-client/src/resolver.rs`
- Create: `crates/gw-tunnel-client/src/safe_dns.rs`
- Create: `crates/gw-tunnel-client/src/target_filter.rs`
- Modify: `crates/gw-tunnel-client/src/lib.rs`
- Test: `crates/gw-tunnel-client/tests/{pool,resolver,safe_dns,target_filter}.rs`

> 这一 task 内部 4 子模块互不依赖；可并行写。dispatch 单 subagent 处理整 task 也可。

- [ ] **Step 1: target_filter 测试**

```rust
// crates/gw-tunnel-client/tests/target_filter.rs
use gw_tunnel_client::target_filter::{set_whitelist, assert_target_allowed, ResetGuard};
use url::Url;

#[test]
fn whitelisted_443_passes() {
    let _g = ResetGuard::new();
    set_whitelist(vec![("api.anthropic.com".into(), 443)]);
    assert!(assert_target_allowed(&Url::parse("https://api.anthropic.com/v1").unwrap()).is_ok());
}

#[test]
fn non_whitelisted_denied() {
    let _g = ResetGuard::new();
    set_whitelist(vec![("api.anthropic.com".into(), 443)]);
    assert!(assert_target_allowed(&Url::parse("https://evil.example.com/").unwrap()).is_err());
}

#[test]
fn http_80_denied_unless_in_whitelist() {
    let _g = ResetGuard::new();
    set_whitelist(vec![]);
    assert!(assert_target_allowed(&Url::parse("http://api.anthropic.com/").unwrap()).is_err());
}
```

- [ ] **Step 2: target_filter 实现**

```rust
// crates/gw-tunnel-client/src/target_filter.rs
use arc_swap::ArcSwap;
use std::sync::Arc;
use url::Url;

static WHITELIST: once_cell::sync::Lazy<ArcSwap<Vec<(String, u16)>>> =
    once_cell::sync::Lazy::new(|| ArcSwap::from_pointee(Vec::new()));

pub fn set_whitelist(list: Vec<(String, u16)>) {
    WHITELIST.store(Arc::new(list));
}

#[derive(Debug, thiserror::Error)]
#[error("target {0} not in whitelist")]
pub struct TargetDenied(pub String);

pub fn assert_target_allowed(url: &Url) -> Result<(), TargetDenied> {
    let host = url.host_str().unwrap_or("");
    let port = url.port_or_known_default().unwrap_or(0);
    let wl = WHITELIST.load_full();
    if wl.iter().any(|(h, p)| h == host && *p == port) {
        Ok(())
    } else {
        metrics::counter!("gw_target_filter_denied_total", "host" => host.to_string()).increment(1);
        Err(TargetDenied(format!("{host}:{port}")))
    }
}

/// Test guard that snapshots and restores the whitelist.
pub struct ResetGuard(Arc<Vec<(String, u16)>>);
impl ResetGuard { pub fn new() -> Self { Self(WHITELIST.load_full()) } }
impl Drop for ResetGuard { fn drop(&mut self) { WHITELIST.store(self.0.clone()); } }
```

加 `Cargo.toml` workspace dep `once_cell = { workspace = true }`。

- [ ] **Step 3: safe_dns 测试**

```rust
// crates/gw-tunnel-client/tests/safe_dns.rs
use gw_tunnel_client::safe_dns::{is_ip_blocked, BlockedIpError};
use std::net::IpAddr;

#[test]
fn private_ranges_blocked() {
    for ip in ["10.0.0.1","127.0.0.1","169.254.169.254","192.168.1.1","::1","fc00::1"] {
        assert!(is_ip_blocked(ip.parse().unwrap()));
    }
}

#[test]
fn public_ips_allowed() {
    for ip in ["1.1.1.1","8.8.8.8","2606:4700:4700::1111"] {
        assert!(!is_ip_blocked(ip.parse().unwrap()));
    }
}
```

- [ ] **Step 4: safe_dns 实现**

```rust
// crates/gw-tunnel-client/src/safe_dns.rs
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, thiserror::Error)]
#[error("ip {0} is blocked")]
pub struct BlockedIpError(pub IpAddr);

pub fn is_ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_blocked(v4),
        IpAddr::V6(v6) => is_v6_blocked(v6),
    }
}

fn is_v4_blocked(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
        || ip.is_broadcast() || ip.is_documentation() || ip.is_unspecified()
        || ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 0x40 // 100.64/10 CGNAT
}
fn is_v6_blocked(ip: Ipv6Addr) -> bool {
    let s = ip.segments()[0];
    ip.is_loopback() || ip.is_unspecified()
        || (s & 0xFE00) == 0xFC00   // fc00::/7 unique-local
        || (s & 0xFFC0) == 0xFE80   // fe80::/10 link-local
}
```

> DoH client 层 v1 用 `hickory-dns` 直接接入会让 task 变重；本 task 仅落 IP 黑名单 + 白名单 host 解析的工具函数；DoH client 推迟到独立后续 plan。

- [ ] **Step 5: resolver 测试 + 实现**

```rust
// crates/gw-tunnel-client/tests/resolver.rs
use gw_tunnel_client::resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};

fn nodes() -> Vec<ProxyNodeView> {
    vec![
        ProxyNodeView { id: "n1".into(), enabled: true, latency_ms: 30,
            allowed_provider_ids: vec!["openai".into()], allowed_vendor_key_ids: vec![] },
        ProxyNodeView { id: "n2".into(), enabled: true, latency_ms: 10,
            allowed_provider_ids: vec!["anthropic".into()], allowed_vendor_key_ids: vec![] },
        ProxyNodeView { id: "n3".into(), enabled: false, latency_ms: 5,
            allowed_provider_ids: vec!["openai".into()], allowed_vendor_key_ids: vec![] },
    ]
}

#[test]
fn picks_lowest_latency_among_allowed() {
    let r = resolve_tunnel(&TunnelResolveCtx {
        provider_id: "openai", vendor_key_id: "vk1",
        preference: TunnelPreference::default(),
    }, &nodes());
    assert_eq!(r, TunnelResolution::Use("n1".into()));   // n3 disabled, n2 wrong provider
}

#[test]
fn returns_direct_when_no_match() {
    let r = resolve_tunnel(&TunnelResolveCtx {
        provider_id: "google", vendor_key_id: "vk1",
        preference: TunnelPreference::default(),
    }, &nodes());
    assert_eq!(r, TunnelResolution::Direct);
}
```

```rust
// crates/gw-tunnel-client/src/resolver.rs

#[derive(Debug, Clone)]
pub struct ProxyNodeView {
    pub id: String,
    pub enabled: bool,
    pub latency_ms: u32,
    pub allowed_provider_ids: Vec<String>,
    pub allowed_vendor_key_ids: Vec<String>,
}

pub struct TunnelResolveCtx<'a> {
    pub provider_id: &'a str,
    pub vendor_key_id: &'a str,
    pub preference: TunnelPreference,
}

#[derive(Debug, Clone, Default)]
pub struct TunnelPreference {
    pub prefer_low_latency: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelResolution { Use(String), Direct }

pub fn resolve_tunnel(ctx: &TunnelResolveCtx<'_>, nodes: &[ProxyNodeView]) -> TunnelResolution {
    let mut filtered: Vec<&ProxyNodeView> = nodes.iter()
        .filter(|n| n.enabled)
        .filter(|n| n.allowed_provider_ids.iter().any(|p| p == ctx.provider_id))
        .filter(|n| n.allowed_vendor_key_ids.is_empty()
            || n.allowed_vendor_key_ids.iter().any(|k| k == ctx.vendor_key_id))
        .collect();
    if filtered.is_empty() { return TunnelResolution::Direct; }
    filtered.sort_by_key(|n| n.latency_ms);
    TunnelResolution::Use(filtered[0].id.clone())
}
```

- [ ] **Step 6: pool 占位**

```rust
// crates/gw-tunnel-client/src/pool.rs
//! ConnectionPool — placeholder for tunnel multiplexed connections.
//! 实际重连 / heartbeat 在独立后续 plan（依赖 hub 端实测对接）。

use std::time::Duration;

pub struct PoolConfig {
    pub heartbeat_interval: Duration,
    pub max_streams_per_conn: u32,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub backoff_jitter: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            max_streams_per_conn: 256,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            backoff_jitter: 0.2,
        }
    }
}

pub fn jittered_backoff(attempt: u32, cfg: &PoolConfig) -> Duration {
    let base_ms = cfg.backoff_min.as_millis() as f64
        * 2_f64.powi(attempt as i32);
    let capped = base_ms.min(cfg.backoff_max.as_millis() as f64);
    let jitter = (rand::random::<f32>() - 0.5) * 2.0 * cfg.backoff_jitter;
    let final_ms = capped * (1.0 + jitter as f64);
    Duration::from_millis(final_ms.max(0.0) as u64)
}
```

`crates/gw-tunnel-client/src/lib.rs`：

```rust
pub mod frame;
pub mod pool;
pub mod resolver;
pub mod safe_dns;
pub mod target_filter;
```

- [ ] **Step 7: 跑全部测试 + 提交**

```bash
cargo test -p gw-tunnel-client
git add Cargo.toml crates/gw-tunnel-client
git commit -m "feat(gw-tunnel-client): pool config + resolver + safe_dns + target_filter (G-ε.2/3/4/5)"
```

---

### Task T3-E: LogVerbosity + 写入时脱敏（detailed_log v2）

**Files:**
- Modify: `crates/gw-proxy/src/detailed_log.rs`
- Create: `crates/gw-ledger/src/detailed_stream.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Modify: `crates/gw-ledger/src/log_sink.rs`（保留 trait，增加 verbosity-aware sink）
- Test: `crates/gw-ledger/tests/redaction.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_ledger::detailed_stream::{redact_headers, redact_body, REDACT_HEADER_DENYLIST};
use serde_json::json;

#[test]
fn redact_strips_authorization_and_apikey_and_cookie() {
    let h = vec![
        ("authorization".into(), "Bearer secret".into()),
        ("x-api-key".into(),    "sk-abc".into()),
        ("cookie".into(),       "sid=xx".into()),
        ("user-agent".into(),   "claude-cli/1.0".into()),
        ("accept-encoding".into(), "gzip, br".into()),
    ];
    let red = redact_headers(&h);
    let names: Vec<_> = red.iter().map(|(k,_)|k.as_str()).collect();
    for d in REDACT_HEADER_DENYLIST { assert!(!names.contains(d)); }
    assert!(names.contains(&"user-agent"));
}

#[test]
fn redact_body_replaces_message_content() {
    let body = json!({
        "model": "claude-3",
        "messages": [
            { "role":"user", "content": "私密内容很长很长很长" },
            { "role":"assistant", "tool_calls": [ { "name":"foo", "args": "x" } ] }
        ]
    });
    let red = redact_body(body.clone());
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("私密内容"));
    assert!(s.contains("foo"));   // tool_calls.name 保留
    assert!(s.contains("redacted"));  // 标记
}

#[test]
fn redact_body_replaces_inline_media() {
    let body = json!({
        "messages": [{
            "role":"user",
            "content":[ { "type":"image", "source": { "type":"base64", "data":"AAAA", "media_type":"image/png" } } ]
        }]
    });
    let red = redact_body(body);
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("AAAA"));
    assert!(s.contains("media:type=image/png"));
}
```

- [ ] **Step 2: 实现**

`crates/gw-ledger/src/detailed_stream.rs`：

```rust
//! Detailed log writer：严格区分 verbosity 三档 + 写入时脱敏。

use serde_json::Value;

pub const REDACT_HEADER_DENYLIST: &[&str] = &["authorization", "x-api-key", "cookie"];
pub const REDACT_HEADER_ALLOWLIST: &[&str] = &[
    "user-agent", "accept-encoding", "content-type", "content-length",
    "x-request-id", "x-stainless-os", "x-stainless-package-version",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogVerbosity { Base, Headers, Full }

pub fn redact_headers(input: &[(String, String)]) -> Vec<(String, String)> {
    input.iter()
        .filter(|(k, _)| !REDACT_HEADER_DENYLIST.contains(&k.as_str()))
        .filter(|(k, _)| REDACT_HEADER_ALLOWLIST.contains(&k.as_str())
                      || k.starts_with("x-stainless-"))
        .cloned()
        .collect()
}

pub fn redact_body(body: Value) -> Value {
    walk(body)
}

fn walk(v: Value) -> Value {
    match v {
        Value::Object(mut m) => {
            // messages 数组里每条 message.content 视为敏感
            if let Some(msgs) = m.get_mut("messages") {
                if let Value::Array(arr) = msgs {
                    for msg in arr.iter_mut() {
                        if let Value::Object(o) = msg {
                            if let Some(c) = o.get_mut("content") { *c = redact_message_content(c.take()); }
                        }
                    }
                }
            }
            // input 字段（OpenAI Responses）同理
            if let Some(input) = m.get_mut("input") {
                if let Value::Array(arr) = input {
                    for it in arr.iter_mut() { *it = redact_message_content(it.take()); }
                }
            }
            for (_k, vv) in m.iter_mut() { *vv = walk(vv.take()); }
            Value::Object(m)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(walk).collect()),
        other => other,
    }
}

fn redact_message_content(c: Value) -> Value {
    match c {
        Value::String(s) => Value::String(format!("<redacted-text-{}-chars>", s.chars().count())),
        Value::Array(arr) => Value::Array(arr.into_iter().map(redact_part).collect()),
        Value::Object(_) => redact_part(c),
        other => other,
    }
}

fn redact_part(p: Value) -> Value {
    if let Value::Object(mut m) = p {
        let typ = m.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        match typ.as_str() {
            "text" | "input_text" => {
                if let Some(t) = m.get("text").and_then(|v| v.as_str()) {
                    let n = t.chars().count();
                    m.insert("text".into(), Value::String(format!("<redacted-text-{n}-chars>")));
                }
            }
            "image" | "input_image" | "audio" | "input_audio" | "video" => {
                let media_type = m.get("source")
                    .and_then(|s| s.get("media_type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/octet-stream");
                let size = m.get("source").and_then(|s| s.get("data"))
                    .and_then(|v| v.as_str()).map(|s| s.len()).unwrap_or(0);
                m.insert("source".into(),
                    Value::String(format!("<media:type={media_type};size={size}>")));
            }
            _ => {}
        }
        return Value::Object(m);
    }
    p
}

pub const MAX_DETAILED_LOG_BYTES: usize = 64 * 1024;
```

`gw-ledger/src/lib.rs` 加 `pub mod detailed_stream;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-ledger --test redaction
git add crates/gw-ledger crates/gw-proxy/src/detailed_log.rs
git commit -m "feat(gw-ledger): LogVerbosity + write-time redaction (G-δ.4)"
```

> 实际写到 `usage:detailed` Redis stream + verbosity 加载在 Wave 6 接通。

---

### Task T3-F: Thinking signature 整流 listener

**Files:**
- Create: `crates/gw-proxy/src/error/listeners/thinking.rs`
- Modify: `crates/gw-proxy/src/error/listeners/mod.rs`
- Test: `crates/gw-proxy/tests/thinking_signature.rs`

> 此 listener 不改变副作用，只在 `outcome.class == ThinkingSignatureMismatch` 时把 `retryable` 决定写回到 ctx 上的 atomic flag，供后续 R-F 决策。R-F 真实接入在 Wave 6 完成。

- [ ] **Step 1: 写失败测试**

```rust
use gw_core::error::{ClassifierOutcome, ErrorClass, Retryability};
use gw_proxy::error::handler::{ErrorListener, HandlerCtx};
use gw_proxy::error::listeners::thinking::{ThinkingSignatureListener, FlowState};
use std::sync::Arc;

#[test]
fn pre_first_chunk_marks_retryable_via_failover() {
    let state = Arc::new(FlowState::new());
    state.mark_pre_first_chunk();
    let l = ThinkingSignatureListener::new(state.clone());
    let oc = ClassifierOutcome {
        class: ErrorClass::ThinkingSignatureMismatch,
        retryable: Retryability::No, // classifier 默认 no
        ..Default::default()
    };
    l.on_outcome(&HandlerCtx::stub(), &oc);
    assert!(state.should_failover());
}

#[test]
fn after_first_chunk_does_not_failover() {
    let state = Arc::new(FlowState::new());
    state.mark_post_first_chunk();
    let l = ThinkingSignatureListener::new(state.clone());
    let oc = ClassifierOutcome {
        class: ErrorClass::ThinkingSignatureMismatch,
        retryable: Retryability::No,
        ..Default::default()
    };
    l.on_outcome(&HandlerCtx::stub(), &oc);
    assert!(!state.should_failover());
}
```

- [ ] **Step 2: 实现**

```rust
// crates/gw-proxy/src/error/listeners/thinking.rs

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::{ClassifierOutcome, ErrorClass};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// 简化版流状态：0=pre-first-chunk, 1=post-first-chunk, 2=failover-requested
pub struct FlowState { st: AtomicU8 }

impl FlowState {
    pub fn new() -> Self { Self { st: AtomicU8::new(0) } }
    pub fn mark_pre_first_chunk(&self)  { self.st.store(0, Ordering::Release); }
    pub fn mark_post_first_chunk(&self) { self.st.store(1, Ordering::Release); }
    pub fn should_failover(&self) -> bool { self.st.load(Ordering::Acquire) == 2 }
    fn request_failover(&self) {
        let _ = self.st.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

pub struct ThinkingSignatureListener {
    flow: Arc<FlowState>,
}
impl ThinkingSignatureListener {
    pub fn new(flow: Arc<FlowState>) -> Self { Self { flow } }
}

impl ErrorListener for ThinkingSignatureListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if matches!(oc.class, ErrorClass::ThinkingSignatureMismatch) {
            self.flow.request_failover();
        }
    }
}
```

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test thinking_signature
git add crates/gw-proxy
git commit -m "feat(gw-proxy): ThinkingSignatureListener + FlowState gate (G-α.5)"
```

---

### Task T3-G: Tunnel hub WebSocket 长连接 + heartbeat + reconnect

**Files:**
- Modify: `crates/gw-tunnel-client/Cargo.toml`（加 `tokio-tungstenite` `futures-util`）
- Modify: `Cargo.toml`（workspace dep `tokio-tungstenite`、`futures-util`）
- Create: `crates/gw-tunnel-client/src/transport.rs`
- Modify: `crates/gw-tunnel-client/src/pool.rs`（接 transport）
- Modify: `crates/gw-tunnel-client/src/lib.rs`
- Test: `crates/gw-tunnel-client/tests/transport.rs`

> 实现 spec ADR-G5-3：跑通"网关 → hub WebSocket 长连，按 frame protocol 发 OPEN/DATA/CLOSE，PING/PONG 心跳 30s，3 次未收 PONG 重连，指数退避 jitter ±20%"。复用 T3-D 已实现的 `pool::jittered_backoff` 与 T2-E 已实现的 frame 编解码。

- [ ] **Step 1: 加依赖**

`Cargo.toml` `[workspace.dependencies]`：

```toml
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
futures-util = "0.3"
```

`crates/gw-tunnel-client/Cargo.toml` `[dependencies]`：

```toml
tokio-tungstenite = { workspace = true }
futures-util = { workspace = true }
```

- [ ] **Step 2: 写失败测试（用本地 axum-ws server 收 PING 回 PONG）**

`crates/gw-tunnel-client/tests/transport.rs`：

```rust
use bytes::Bytes;
use gw_tunnel_client::frame::{Frame, MsgType};
use gw_tunnel_client::transport::{TransportConfig, WsTransport};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};

async fn echo_pong_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Binary(bin) = m {
                let (f, _) = gw_tunnel_client::frame::decode_frame(&bin)
                    .unwrap().unwrap();
                if f.msg_type == MsgType::Ping {
                    let mut buf = bytes::BytesMut::new();
                    gw_tunnel_client::frame::encode_frame(&Frame {
                        msg_type: MsgType::Pong, flags: 0,
                        stream_id: f.stream_id, payload: f.payload,
                    }, &mut buf);
                    ws.send(Message::Binary(buf.to_vec())).await.unwrap();
                }
            }
        }
    });
    (port, h)
}

#[tokio::test]
async fn ping_round_trip_keeps_alive() {
    let (port, _h) = echo_pong_server().await;
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        heartbeat_interval: Duration::from_millis(50),
        missed_pong_threshold: 3,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(t.is_alive());
    assert!(t.last_pong_age().as_millis() < 200);
}

#[tokio::test]
async fn missed_pongs_mark_dead() {
    // server that ignores PING
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(_) = ws.next().await {}
    });
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        heartbeat_interval: Duration::from_millis(20),
        missed_pong_threshold: 3,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!t.is_alive());
}
```

- [ ] **Step 3: 实现 transport.rs**

```rust
// crates/gw-tunnel-client/src/transport.rs
use crate::frame::{decode_frame, encode_frame, Frame, MsgType};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub url: Url,
    pub heartbeat_interval: Duration,
    pub missed_pong_threshold: u32,
    pub max_streams_per_conn: u32,
}
impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:0".parse().unwrap(),
            heartbeat_interval: Duration::from_secs(30),
            missed_pong_threshold: 3,
            max_streams_per_conn: 256,
        }
    }
}

pub struct WsTransport {
    alive: Arc<AtomicBool>,
    last_pong_ms: Arc<AtomicU64>,
    next_stream_id: Arc<AtomicU32>,
    out_tx: mpsc::Sender<Frame>,
}

impl WsTransport {
    pub async fn connect(cfg: TransportConfig) -> anyhow::Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(cfg.url.as_str()).await?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let last_pong_ms = Arc::new(AtomicU64::new(now_ms()));
        let missed = Arc::new(AtomicU32::new(0));

        // sender task
        {
            let alive = alive.clone();
            tokio::spawn(async move {
                while let Some(frame) = out_rx.recv().await {
                    let mut buf = BytesMut::new();
                    encode_frame(&frame, &mut buf);
                    if sink.send(Message::Binary(buf.to_vec())).await.is_err() {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                }
            });
        }

        // reader task
        {
            let alive = alive.clone();
            let last_pong_ms = last_pong_ms.clone();
            let missed = missed.clone();
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    let m = match msg {
                        Ok(m) => m,
                        Err(_) => { alive.store(false, Ordering::Release); break; }
                    };
                    if let Message::Binary(bin) = m {
                        if let Ok(Some((f, _))) = decode_frame(&bin) {
                            if f.msg_type == MsgType::Pong {
                                last_pong_ms.store(now_ms(), Ordering::Release);
                                missed.store(0, Ordering::Release);
                            }
                            // OPEN/DATA/CLOSE 路由到 stream demux 由后续 plan 完成；
                            // 本 task 只做 alive 维持。
                        }
                    }
                }
                alive.store(false, Ordering::Release);
            });
        }

        // heartbeat task
        {
            let out_tx = out_tx.clone();
            let alive = alive.clone();
            let missed = missed.clone();
            let interval = cfg.heartbeat_interval;
            let threshold = cfg.missed_pong_threshold;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                let mut seq = 0u32;
                loop {
                    tick.tick().await;
                    if !alive.load(Ordering::Acquire) { break; }
                    seq = seq.wrapping_add(1);
                    let f = Frame {
                        msg_type: MsgType::Ping, flags: 0, stream_id: seq,
                        payload: Bytes::from_static(b"hb"),
                    };
                    if out_tx.send(f).await.is_err() {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                    let m = missed.fetch_add(1, Ordering::AcqRel) + 1;
                    if m >= threshold {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                }
            });
        }

        Ok(Self {
            alive,
            last_pong_ms,
            next_stream_id: Arc::new(AtomicU32::new(1)),
            out_tx,
        })
    }

    pub fn is_alive(&self) -> bool { self.alive.load(Ordering::Acquire) }
    pub fn last_pong_age(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.last_pong_ms.load(Ordering::Acquire)))
    }

    pub async fn send_open(&self, target: &str) -> anyhow::Result<u32> {
        let id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
        let f = Frame {
            msg_type: MsgType::Open, flags: 0, stream_id: id,
            payload: Bytes::copy_from_slice(target.as_bytes()),
        };
        self.out_tx.send(f).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(id)
    }

    pub async fn send_data(&self, stream_id: u32, payload: Bytes) -> anyhow::Result<()> {
        self.out_tx.send(Frame {
            msg_type: MsgType::Data, flags: 0, stream_id, payload,
        }).await.map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}
```

- [ ] **Step 4: 在 pool.rs 接 reconnect**

```rust
// crates/gw-tunnel-client/src/pool.rs (扩展)
use crate::transport::{TransportConfig, WsTransport};
use std::sync::Arc;

pub struct ReconnectingTransport {
    cfg: TransportConfig,
    pool_cfg: PoolConfig,
    inner: arc_swap::ArcSwapOption<WsTransport>,
}

impl ReconnectingTransport {
    pub async fn start(cfg: TransportConfig, pool_cfg: PoolConfig) -> Arc<Self> {
        let me = Arc::new(Self {
            cfg, pool_cfg,
            inner: arc_swap::ArcSwapOption::new(None),
        });
        let me_for_loop = me.clone();
        tokio::spawn(async move {
            let mut attempt = 0u32;
            loop {
                match WsTransport::connect(me_for_loop.cfg.clone()).await {
                    Ok(t) => {
                        me_for_loop.inner.store(Some(Arc::new(t)));
                        attempt = 0;
                        // 等到 dead 再重连
                        loop {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            let alive = me_for_loop.inner.load_full()
                                .map(|t| t.is_alive()).unwrap_or(false);
                            if !alive { break; }
                        }
                        me_for_loop.inner.store(None);
                    }
                    Err(_) => {
                        attempt = attempt.saturating_add(1);
                        let backoff = jittered_backoff(attempt, &me_for_loop.pool_cfg);
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        });
        me
    }

    pub fn current(&self) -> Option<Arc<WsTransport>> { self.inner.load_full() }
}
```

`crates/gw-tunnel-client/src/lib.rs` 加 `pub mod transport;`。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-tunnel-client --test transport
git add Cargo.toml crates/gw-tunnel-client
git commit -m "feat(gw-tunnel-client): WebSocket transport + heartbeat + reconnect (ADR-G5-3 full impl)"
```

---

### Task T3-H: safe_dns DoH 真实解析

**Files:**
- Modify: `Cargo.toml`（workspace dep `hickory-resolver`）
- Modify: `crates/gw-tunnel-client/Cargo.toml`
- Modify: `crates/gw-tunnel-client/src/safe_dns.rs`
- Test: `crates/gw-tunnel-client/tests/safe_dns_doh.rs`

> 实现 spec ADR-G5-5：DoH 上游 + LRU + IP 黑名单（黑名单部分 T3-D 已实现，本 task 加 DoH 解析路径与缓存）。

- [ ] **Step 1: 加依赖**

`Cargo.toml`：

```toml
hickory-resolver = { version = "0.24", features = ["dns-over-https-rustls", "tokio-runtime"] }
```

`crates/gw-tunnel-client/Cargo.toml`：

```toml
hickory-resolver = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

```rust
// crates/gw-tunnel-client/tests/safe_dns_doh.rs
use gw_tunnel_client::safe_dns::{is_ip_blocked, SafeDnsResolver};
use std::time::Duration;

#[tokio::test]
async fn resolves_public_host_via_doh() {
    let r = SafeDnsResolver::cloudflare_default();
    // 此测试需要外网；用 ignore + cargo test -- --ignored 跑
    let ips = r.resolve("one.one.one.one").await.unwrap();
    assert!(!ips.is_empty());
    for ip in ips { assert!(!is_ip_blocked(ip)); }
}

#[tokio::test]
#[ignore]
async fn cache_hit_under_ttl() {
    let r = SafeDnsResolver::cloudflare_default();
    let _ = r.resolve("one.one.one.one").await.unwrap();
    let t0 = std::time::Instant::now();
    let _ = r.resolve("one.one.one.one").await.unwrap();
    assert!(t0.elapsed() < Duration::from_millis(5)); // hit cache
}

#[test]
fn override_redirects_to_specified_ip() {
    use gw_tunnel_client::safe_dns::{set_override, OverrideGuard};
    use std::net::IpAddr;
    let _g = OverrideGuard::new();
    set_override("api.x.test", vec!["1.2.3.4".parse::<IpAddr>().unwrap()]);
    let r = SafeDnsResolver::cloudflare_default();
    let ips = futures::executor::block_on(r.resolve("api.x.test")).unwrap();
    assert_eq!(ips, vec!["1.2.3.4".parse::<IpAddr>().unwrap()]);
}
```

- [ ] **Step 3: 实现**

在 `crates/gw-tunnel-client/src/safe_dns.rs` 末尾追加：

```rust
use arc_swap::ArcSwap;
use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

static OVERRIDE: once_cell::sync::Lazy<ArcSwap<HashMap<String, Vec<IpAddr>>>> =
    once_cell::sync::Lazy::new(|| ArcSwap::from_pointee(HashMap::new()));

pub fn set_override(host: &str, ips: Vec<IpAddr>) {
    let mut m = (**OVERRIDE.load()).clone();
    m.insert(host.to_string(), ips);
    OVERRIDE.store(Arc::new(m));
}

pub struct OverrideGuard(Arc<HashMap<String, Vec<IpAddr>>>);
impl OverrideGuard { pub fn new() -> Self { Self(OVERRIDE.load_full()) } }
impl Drop for OverrideGuard { fn drop(&mut self) { OVERRIDE.store(self.0.clone()); } }

pub struct SafeDnsResolver {
    inner: TokioAsyncResolver,
    cache: parking_lot::Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>,
    cache_ttl: Duration,
    cache_cap: usize,
}

impl SafeDnsResolver {
    pub fn cloudflare_default() -> Self {
        let opts = ResolverOpts::default();
        let inner = TokioAsyncResolver::tokio(ResolverConfig::cloudflare_https(), opts);
        Self {
            inner,
            cache: parking_lot::Mutex::new(HashMap::new()),
            cache_ttl: Duration::from_secs(600),
            cache_cap: 4096,
        }
    }

    pub async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, BlockedIpError> {
        if let Some(ips) = OVERRIDE.load_full().get(host) {
            for ip in ips { if is_ip_blocked(*ip) { return Err(BlockedIpError(*ip)); } }
            return Ok(ips.clone());
        }
        if let Some((t, v)) = self.cache.lock().get(host) {
            if t.elapsed() < self.cache_ttl { return Ok(v.clone()); }
        }
        let lookup = self.inner.lookup_ip(host).await
            .map_err(|_| BlockedIpError("0.0.0.0".parse().unwrap()))?;
        let mut out: Vec<IpAddr> = lookup.iter().collect();
        for ip in &out { if is_ip_blocked(*ip) { return Err(BlockedIpError(*ip)); } }
        out.dedup();
        let mut c = self.cache.lock();
        if c.len() >= self.cache_cap { c.clear(); }
        c.insert(host.to_string(), (Instant::now(), out.clone()));
        Ok(out)
    }
}
```

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-tunnel-client --test safe_dns_doh -- --ignored
git add Cargo.toml crates/gw-tunnel-client
git commit -m "feat(gw-tunnel-client): DoH resolver + LRU cache + host override (ADR-G5-5 full impl)"
```



<!-- WAVE-4 -->

## Wave 4 — Capability resolver / mapper / adapter hook / events:entity invalidate / health endpoint（5 并行）

### Task T4-A: ModelMapper + entity:model_mapping

**Files:**
- Modify: `crates/gw-core/src/entity.rs`（加 ModelMapping）
- Create: `crates/gw-store/src/model_mapping_loader.rs`
- Modify: `crates/gw-store/src/entity_store.rs`（加 cache + 公开 `model_mapping_or_load`）
- Create: `crates/gw-capability/src/mapper.rs`
- Modify: `crates/gw-capability/src/lib.rs`
- Modify: `crates/gw-store/migrations/0003_model_mapping.sql`（NEW）
- Test: `crates/gw-capability/tests/mapper.rs`

- [ ] **Step 1: entity + migration**

`crates/gw-store/migrations/0003_model_mapping.sql`：

```sql
create table if not exists model_mapping (
    user_alias text not null,
    protocol text not null,           -- 'openai' | 'anthropic' | 'gemini'
    default_upstream_model text not null,
    vendor_key_overrides jsonb not null default '{}'::jsonb,
    availability text not null default 'enabled',
    pricing_strategy_ref text,
    updated_at timestamptz not null default now(),
    primary key (user_alias, protocol)
);
```

`crates/gw-core/src/entity.rs` 加：

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelMapping {
    pub user_alias: String,
    pub protocol: crate::protocol::ProtocolKind,
    pub default_upstream_model: String,
    pub vendor_key_overrides: std::collections::BTreeMap<String, String>,
    pub availability: ModelAvailability,
    pub pricing_strategy_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability { Enabled, Disabled, ShadowOnly }
```

- [ ] **Step 2: 写失败测试**

`crates/gw-capability/tests/mapper.rs`：

```rust
use gw_capability::mapper::{ModelMapper, FakeMapperBackend, MappingSource};
use gw_core::entity::{ModelMapping, ModelAvailability};
use gw_core::protocol::ProtocolKind;

#[tokio::test]
async fn entity_row_returns_default_upstream() {
    let mut be = FakeMapperBackend::default();
    be.insert(ModelMapping {
        user_alias: "claude-3-opus".into(),
        protocol: ProtocolKind::Anthropic,
        default_upstream_model: "claude-3-opus-20240229".into(),
        vendor_key_overrides: Default::default(),
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(std::sync::Arc::new(be));
    let mapped = m.map_for_routing("claude-3-opus", ProtocolKind::Anthropic).await.unwrap();
    assert_eq!(mapped.upstream_model, "claude-3-opus-20240229");
    assert_eq!(mapped.source, MappingSource::EntityRow);
}

#[tokio::test]
async fn identity_when_no_mapping() {
    let be = FakeMapperBackend::default();
    let m = ModelMapper::new(std::sync::Arc::new(be));
    let mapped = m.map_for_routing("unknown-model", ProtocolKind::OpenAi).await.unwrap();
    assert_eq!(mapped.upstream_model, "unknown-model");
    assert_eq!(mapped.source, MappingSource::Identity);
}

#[tokio::test]
async fn per_key_when_vendor_overrides_present() {
    let mut be = FakeMapperBackend::default();
    let mut ovr = std::collections::BTreeMap::new();
    ovr.insert("vk-1".into(), "gpt-5-2026".into());
    be.insert(ModelMapping {
        user_alias: "gpt-5".into(), protocol: ProtocolKind::OpenAi,
        default_upstream_model: "gpt-5".into(),
        vendor_key_overrides: ovr,
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(std::sync::Arc::new(be));
    let mapped = m.map_for_routing("gpt-5", ProtocolKind::OpenAi).await.unwrap();
    assert_eq!(mapped.source, MappingSource::PerKey);
}

#[tokio::test]
async fn disabled_returns_error() {
    let mut be = FakeMapperBackend::default();
    be.insert(ModelMapping {
        user_alias: "deprecated".into(), protocol: ProtocolKind::OpenAi,
        default_upstream_model: "x".into(), vendor_key_overrides: Default::default(),
        availability: ModelAvailability::Disabled, pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(std::sync::Arc::new(be));
    let err = m.map_for_routing("deprecated", ProtocolKind::OpenAi).await.unwrap_err();
    assert!(format!("{err:?}").contains("disabled"));
}
```

- [ ] **Step 3: 实现 mapper.rs + Fake backend**

```rust
// crates/gw-capability/src/mapper.rs
use async_trait::async_trait;
use gw_core::entity::{ModelAvailability, ModelMapping};
use gw_core::protocol::ProtocolKind;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingSource { EntityRow, Identity, PerKey }

#[derive(Debug, Clone)]
pub struct MappedModel {
    pub user_alias: String,
    pub upstream_model: String,
    pub source: MappingSource,
}

#[derive(Debug, thiserror::Error)]
pub enum MapperError {
    #[error("model disabled: {0}")]
    Disabled(String),
    #[error("backend: {0}")]
    Backend(String),
}

#[async_trait]
pub trait MapperBackend: Send + Sync {
    async fn get(&self, user_alias: &str, protocol: ProtocolKind) -> Option<ModelMapping>;
}

pub struct ModelMapper {
    backend: Arc<dyn MapperBackend>,
}

impl ModelMapper {
    pub fn new(backend: Arc<dyn MapperBackend>) -> Self { Self { backend } }

    pub async fn map_for_routing(
        &self, user_alias: &str, protocol: ProtocolKind,
    ) -> Result<MappedModel, MapperError> {
        let mm = self.backend.get(user_alias, protocol).await;
        let Some(mm) = mm else {
            return Ok(MappedModel {
                user_alias: user_alias.to_string(),
                upstream_model: user_alias.to_string(),
                source: MappingSource::Identity,
            });
        };
        match mm.availability {
            ModelAvailability::Disabled => return Err(MapperError::Disabled(user_alias.into())),
            ModelAvailability::ShadowOnly => {} // routing 路径处理：本接口对调用者透明
            ModelAvailability::Enabled => {}
        }
        let source = if mm.vendor_key_overrides.is_empty() {
            MappingSource::EntityRow
        } else {
            MappingSource::PerKey
        };
        Ok(MappedModel {
            user_alias: mm.user_alias.clone(),
            upstream_model: mm.default_upstream_model.clone(),
            source,
        })
    }

    pub async fn map_for_dispatch(
        &self, user_alias: &str, protocol: ProtocolKind, vendor_key_id: &str,
    ) -> String {
        if let Some(mm) = self.backend.get(user_alias, protocol).await {
            if let Some(o) = mm.vendor_key_overrides.get(vendor_key_id) {
                return o.clone();
            }
            return mm.default_upstream_model;
        }
        user_alias.to_string()
    }
}

#[derive(Default)]
pub struct FakeMapperBackend {
    map: parking_lot::Mutex<HashMap<(String, ProtocolKind), ModelMapping>>,
}

impl FakeMapperBackend {
    pub fn insert(&mut self, m: ModelMapping) {
        self.map.lock().insert((m.user_alias.clone(), m.protocol), m);
    }
}

#[async_trait]
impl MapperBackend for FakeMapperBackend {
    async fn get(&self, user_alias: &str, protocol: ProtocolKind) -> Option<ModelMapping> {
        self.map.lock().get(&(user_alias.to_string(), protocol)).cloned()
    }
}
```

`gw-capability/src/lib.rs` 加 `pub mod mapper;`。

`gw-capability/Cargo.toml` 加 `async-trait = { workspace = true }`、`parking_lot = { workspace = true }`、`gw-core = ...`（已有）。

`crates/gw-store/src/model_mapping_loader.rs`：v1 仅 sqlx 查询 stub（如未来真接 PgSQL 再扩；本 task 编译期不依赖）。

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test mapper
git add crates/gw-core crates/gw-store crates/gw-capability
git commit -m "feat(gw-capability): ModelMapper + FakeBackend + ModelMapping entity (G-β.4)"
```

---

### Task T4-B: CapabilityResolver + RetryUpgradeHint

**Files:**
- Create: `crates/gw-capability/src/resolver.rs`
- Modify: `crates/gw-capability/src/lib.rs`
- Modify: `crates/gw-core/src/entity.rs`（ApiKeyEntry 加 `force_capabilities` + `match_mode_exclusive`，可选 `match_mode`；HeaderRequire / Retry 的输入由 caller 给）
- Test: `crates/gw-capability/tests/resolver.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_capability::resolver::{
    CapabilityResolver, FakeResolveBackend, ResolveRequest, RetryUpgradeHint,
};
use gw_capability::types::{Capability, CapabilityOrigin, MatchMode};
use gw_core::error::ErrorClass;
use gw_core::protocol::ProtocolKind;
use smallvec::smallvec;

#[tokio::test]
async fn model_caps_merge_with_header_require() {
    let mut be = FakeResolveBackend::default();
    be.set_model_caps("claude-3-opus", vec![Capability::Vision]);
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let req = ResolveRequest {
        api_key_id: "ak1", model_alias: "claude-3-opus",
        header_require: Some(&["thinking"]),
        protocol: ProtocolKind::Anthropic,
        retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Vision));
    assert!(got.set.contains(&Capability::Thinking));
    assert_eq!(got.mode, MatchMode::Compatible);
}

#[tokio::test]
async fn key_forced_caps_take_origin() {
    let mut be = FakeResolveBackend::default();
    be.set_key_forced("ak1", vec![Capability::Audio]);
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let req = ResolveRequest {
        api_key_id: "ak1", model_alias: "any", header_require: None,
        protocol: ProtocolKind::OpenAi, retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Audio));
    assert!(got.provenance.iter().any(|(_, o)| *o == CapabilityOrigin::KeyForced));
}

#[tokio::test]
async fn retry_upgrade_adds_thinking() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let hint = RetryUpgradeHint {
        last_failure_class: ErrorClass::ProtocolError,
        upgrade_path: smallvec![Capability::Thinking],
    };
    let req = ResolveRequest {
        api_key_id: "ak1", model_alias: "any", header_require: None,
        protocol: ProtocolKind::Anthropic, retry_upgrade: Some(&hint),
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Thinking));
    assert!(got.provenance.iter().any(|(_, o)| *o == CapabilityOrigin::RetryUpgrade));
}

#[tokio::test]
async fn exclusive_mode_inherits_from_key() {
    let mut be = FakeResolveBackend::default();
    be.set_match_mode_exclusive("ak1", true);
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let req = ResolveRequest {
        api_key_id: "ak1", model_alias: "x", header_require: Some(&["thinking"]),
        protocol: ProtocolKind::OpenAi, retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert_eq!(got.mode, MatchMode::Exclusive);
}
```

- [ ] **Step 2: 实现**

`crates/gw-capability/src/resolver.rs`：

```rust
use crate::types::*;
use async_trait::async_trait;
use gw_core::error::ErrorClass;
use gw_core::protocol::ProtocolKind;
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

pub struct CapabilityResolver {
    backend: Arc<dyn ResolveBackend>,
}

pub struct ResolveRequest<'a> {
    pub api_key_id: &'a str,
    pub model_alias: &'a str,
    pub header_require: Option<&'a [&'a str]>,
    pub protocol: ProtocolKind,
    pub retry_upgrade: Option<&'a RetryUpgradeHint>,
}

#[derive(Debug, Clone)]
pub struct RetryUpgradeHint {
    pub last_failure_class: ErrorClass,
    pub upgrade_path: SmallVec<[Capability; 2]>,
}

#[async_trait]
pub trait ResolveBackend: Send + Sync {
    async fn model_caps(&self, model_alias: &str, protocol: ProtocolKind) -> Vec<Capability>;
    async fn key_forced_caps(&self, api_key_id: &str) -> Vec<Capability>;
    async fn match_mode_exclusive(&self, api_key_id: &str) -> bool;
}

impl CapabilityResolver {
    pub fn new(backend: Arc<dyn ResolveBackend>) -> Self { Self { backend } }

    pub async fn resolve(&self, req: &ResolveRequest<'_>) -> anyhow::Result<RequiredCapabilities> {
        let mut r = RequiredCapabilities::default();
        for c in self.backend.key_forced_caps(req.api_key_id).await {
            r.add(c, CapabilityOrigin::KeyForced);
        }
        for c in self.backend.model_caps(req.model_alias, req.protocol).await {
            r.add(c, CapabilityOrigin::Model);
        }
        if let Some(hs) = req.header_require {
            for s in hs { r.add(parse_capability(s), CapabilityOrigin::HeaderRequire); }
        }
        if let Some(hint) = req.retry_upgrade {
            for c in &hint.upgrade_path { r.add(*c, CapabilityOrigin::RetryUpgrade); }
        }
        r.mode = if self.backend.match_mode_exclusive(req.api_key_id).await {
            MatchMode::Exclusive
        } else { MatchMode::Compatible };
        metrics::counter!("gw_capability_resolve_total", "match" => "hit").increment(1);
        Ok(r)
    }
}

fn parse_capability(s: &str) -> Capability {
    match s.to_ascii_lowercase().as_str() {
        "vision" => Capability::Vision,
        "audio"  => Capability::Audio,
        "video"  => Capability::Video,
        "thinking"  => Capability::Thinking,
        "tool_use"  => Capability::ToolUse,
        "streaming_only" => Capability::StreamingOnly,
        "openai_responses_shape" => Capability::OpenAiResponsesShape,
        "vertex_ai_service_account" => Capability::VertexAiServiceAccount,
        other => Capability::Tagged(Box::leak(other.to_string().into_boxed_str())),
        // 上面 Box::leak 仅用于 v1 简化 'static 要求；运维侧 header 值受 entity 校验，
        // 真正的安全约束在 hot-path: api_key 上不允许的 cap 由 ApiKey entity 限定集合。
    }
}

#[derive(Default)]
pub struct FakeResolveBackend {
    model_caps: Mutex<HashMap<(String, ProtocolKind), Vec<Capability>>>,
    key_forced: Mutex<HashMap<String, Vec<Capability>>>,
    exclusive_keys: Mutex<HashMap<String, bool>>,
}
impl FakeResolveBackend {
    pub fn set_model_caps(&mut self, model: &str, caps: Vec<Capability>) {
        self.model_caps.lock().insert((model.into(), ProtocolKind::Anthropic), caps.clone());
        self.model_caps.lock().insert((model.into(), ProtocolKind::OpenAi),    caps.clone());
        self.model_caps.lock().insert((model.into(), ProtocolKind::Gemini),    caps);
    }
    pub fn set_key_forced(&mut self, ak: &str, caps: Vec<Capability>) {
        self.key_forced.lock().insert(ak.into(), caps);
    }
    pub fn set_match_mode_exclusive(&mut self, ak: &str, v: bool) {
        self.exclusive_keys.lock().insert(ak.into(), v);
    }
}

#[async_trait]
impl ResolveBackend for FakeResolveBackend {
    async fn model_caps(&self, m: &str, p: ProtocolKind) -> Vec<Capability> {
        self.model_caps.lock().get(&(m.to_string(), p)).cloned().unwrap_or_default()
    }
    async fn key_forced_caps(&self, ak: &str) -> Vec<Capability> {
        self.key_forced.lock().get(ak).cloned().unwrap_or_default()
    }
    async fn match_mode_exclusive(&self, ak: &str) -> bool {
        *self.exclusive_keys.lock().get(ak).unwrap_or(&false)
    }
}
```

`crates/gw-capability/src/lib.rs` 加 `pub mod resolver;`。

`crates/gw-capability/Cargo.toml` 加 `metrics = { workspace = true }`、`anyhow = { workspace = true }`、`async-trait = { workspace = true }`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test resolver
git add crates/gw-capability
git commit -m "feat(gw-capability): CapabilityResolver + RetryUpgradeHint (G-β.3 / G-β.8)"
```

---

### Task T4-C: AdapterCapabilityHook 接口

**Files:**
- Create: `crates/gw-capability/src/adapter_hook.rs`
- Modify: `crates/gw-capability/src/lib.rs`
- Test: `crates/gw-capability/tests/adapter_hook.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_capability::adapter_hook::{
    AdapterCapabilityHook, CapabilityDetectResult, DetectCtx, EntityVendorKeyView,
};
use gw_capability::types::{Capability, RequiredCapabilities, CapabilityOrigin};

struct StaticHook;
impl AdapterCapabilityHook for StaticHook {
    fn detect(&self, ctx: &DetectCtx<'_>) -> CapabilityDetectResult {
        for cap in ctx.required.set.iter() {
            if !ctx.vendor_key.detected_capabilities.contains(cap) {
                return CapabilityDetectResult::Missing(smallvec::smallvec![*cap]);
            }
        }
        CapabilityDetectResult::Match
    }
}

#[test]
fn match_when_all_caps_present() {
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    let vk = EntityVendorKeyView {
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    let r = StaticHook.detect(&DetectCtx { required: &req, vendor_key: &vk });
    assert_eq!(r, CapabilityDetectResult::Match);
}

#[test]
fn missing_when_cap_absent() {
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Audio, CapabilityOrigin::HeaderRequire);
    let vk = EntityVendorKeyView { detected_capabilities: vec![Capability::Vision] };
    let r = StaticHook.detect(&DetectCtx { required: &req, vendor_key: &vk });
    match r {
        CapabilityDetectResult::Missing(v) => assert_eq!(v[..], [Capability::Audio]),
        _ => panic!()
    }
}
```

- [ ] **Step 2: 实现**

```rust
// crates/gw-capability/src/adapter_hook.rs
use crate::types::{Capability, RequiredCapabilities};
use smallvec::SmallVec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityDetectResult {
    Match,
    Missing(SmallVec<[Capability; 2]>),
    Unknown,
}

pub struct DetectCtx<'a> {
    pub required: &'a RequiredCapabilities,
    pub vendor_key: &'a EntityVendorKeyView,
}

pub struct EntityVendorKeyView {
    pub detected_capabilities: Vec<Capability>,
}

pub trait AdapterCapabilityHook: Send + Sync {
    fn detect(&self, ctx: &DetectCtx<'_>) -> CapabilityDetectResult;
}
```

`crates/gw-capability/src/lib.rs` 加 `pub mod adapter_hook;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test adapter_hook
git add crates/gw-capability
git commit -m "feat(gw-capability): AdapterCapabilityHook trait + DetectResult (G-β.5)"
```

---

### Task T4-D: events:entity invalidate 扩展（订阅 model_mapping）

**Files:**
- Modify: `crates/gw-store/src/stream_consumer.rs`
- Modify: `crates/gw-store/src/entity_store.rs`（加 `model_mapping` cache）
- Test: `crates/gw-store/tests/events_entity_model_mapping.rs`

- [ ] **Step 1: 加 model_mapping cache**

`crates/gw-store/src/entity_store.rs` 在 `EntityStore` 结构上加：

```rust
pub model_mapping: Cache<(String, gw_core::protocol::ProtocolKind),
    Arc<gw_core::versioned::Versioned<gw_core::entity::ModelMapping>>>,
```

且在构造函数中按 1800s TTL 初始化。

- [ ] **Step 2: 扩 stream_consumer.apply**

```rust
// 在 apply 内 match ns:
"model_mapping" => {
    // id 形态 "user_alias|protocol"，protocol 是 snake_case
    if let Some((alias, proto)) = id.split_once('|') {
        let p = match proto {
            "openai"    => Some(gw_core::protocol::ProtocolKind::OpenAi),
            "anthropic" => Some(gw_core::protocol::ProtocolKind::Anthropic),
            "gemini"    => Some(gw_core::protocol::ProtocolKind::Gemini),
            _ => None,
        };
        if let Some(p) = p {
            self.store.model_mapping.invalidate(&(alias.to_string(), p)).await;
        }
    }
}
```

- [ ] **Step 3: 写失败测试**

```rust
// crates/gw-store/tests/events_entity_model_mapping.rs
// 这是 unit 测试 apply 路径——不需要真 Redis。把 stream_consumer 暴露一个
// pub(crate) fn apply_inline(&self, ns: &str, id: &str) 接口供测试调用。
//
// 否则用 testcontainers 真起 Redis （`-- --ignored`）。
// v1 走 inline path 即可。
```

- [ ] **Step 4: 暴露 apply_inline**

```rust
// stream_consumer.rs
impl StreamConsumer {
    #[doc(hidden)]
    pub async fn apply_inline_for_test(&self, ns: &str, id: &str) {
        let mut h = std::collections::HashMap::new();
        h.insert("ns".to_string(), ns.to_string());
        h.insert("id".to_string(), id.to_string());
        self.apply(super::stream_consumer::ENTITY_STREAM, &h).await;
    }
}
```

> 注意：`apply` 当前是 `pub(self) async fn apply(&self, stream: &str, fields: &HashMap<String,String>)`。把 stream 常量 `ENTITY_STREAM` 改成 `pub(crate)`，并加 `cfg(test)` 或 `doc(hidden)` 出口。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-store --test events_entity_model_mapping
git add crates/gw-store
git commit -m "feat(gw-store): events:entity invalidate model_mapping (G-β.6 / H2)"
```

---

### Task T4-E: health endpoint 扩展（snapshot + circuits + resilience）

**Files:**
- Modify: `crates/gw-gateway/src/health.rs`
- Test: `crates/gw-gateway/tests/health_endpoint.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test(flavor = "current_thread")]
async fn admin_required_for_circuits() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let h = tokio::spawn(gw_gateway::health::run_health_server(port));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"GET /healthz/circuits HTTP/1.1\r\n\r\n").await.unwrap();
    let mut buf = vec![0u8; 1024];
    let n = s.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(response.starts_with("HTTP/1.1 401"));
    h.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn livez_still_works() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let h = tokio::spawn(gw_gateway::health::run_health_server(port));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut s, b"GET /livez HTTP/1.1\r\n\r\n").await.unwrap();
    let mut buf = vec![0u8; 1024];
    let n = tokio::io::AsyncReadExt::read(&mut s, &mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    h.abort();
}
```

- [ ] **Step 2: 加 /healthz/* 路由**

`crates/gw-gateway/src/health.rs` 的 `handle_connection` 函数 match path 处追加：

```rust
"/healthz/circuits" | "/healthz/key" | "/healthz/resilience" => {
    // v1: 仅 admin token 可访问；本 plan 仅占位返回 401（mgmt token 真校验由 Wave 5 接入）。
    let _ = write_response(&mut stream, 401, "Unauthorized", b"mgmt token required").await;
}
```

`run_health_server` 的注释中加：

```rust
// /healthz/circuits, /healthz/key/<id>, /healthz/resilience: gated behind
// management-token authorization (parsed by gw-auth::mgmt). Bound to port 8080
// only; not exposed on metrics port 9090.
```

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-gateway --test health_endpoint
git add crates/gw-gateway
git commit -m "feat(gw-gateway): health endpoint /circuits + /resilience stubs (G-δ.3)"
```

<!-- WAVE-5 -->

## Wave 5 — Listener wire-through + filter hooks + ReDoS runtime + capability metric labels（4 并行）

### Task T5-A: 把 KeyHealth / RateLimit / OAuth / CacheInvalidate listeners 接到真依赖

**Files:**
- Modify: `crates/gw-proxy/src/error/listeners/{key_health,rate_limit,oauth,cache_invalidate,metrics,ledger}.rs`
- Modify: `crates/gw-proxy/src/error/handler.rs`（HandlerCtx 加真字段）
- Test: `crates/gw-proxy/tests/listeners_wire.rs`

- [ ] **Step 1: 把 HandlerCtx 由 stub 改为真 ctx**

```rust
// handler.rs
use gw_health::KeyHealthStore;
use gw_store::EntityStore;
use crate::resilience::ResilienceState;
use std::sync::Arc;

pub struct HandlerCtx<'a> {
    pub request_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub vendor_key_id: Option<&'a str>,
    pub api_format: Option<gw_core::protocol::ProtocolKind>,
    pub key_health: Option<&'a KeyHealthStore>,
    pub entity_store: Option<&'a Arc<EntityStore>>,
    pub resilience: Option<&'a ResilienceState>,
}
impl HandlerCtx<'_> {
    pub fn stub() -> Self {
        Self {
            request_id: "", api_key_id: None, vendor_key_id: None,
            api_format: None, key_health: None,
            entity_store: None, resilience: None,
        }
    }
}
```

`crates/gw-proxy/Cargo.toml` 加 `gw-health = { path = "../gw-health" }`、`gw-store = { path = "../gw-store" }`（如尚未有）。

- [ ] **Step 2: 写联通测试**

```rust
// crates/gw-proxy/tests/listeners_wire.rs
use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, CooldownDirective, ErrorClass, OAuthSignal,
    OAuthSignalKind, RateLimitHint, Retryability,
};
use gw_core::ratelimit::RateLimitKind;
use gw_health::KeyHealthStore;
use gw_proxy::error::handler::{ErrorHandler, HandlerCtx};
use gw_proxy::error::listeners::{
    cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
    metrics::MetricsListener, oauth::OAuthListener, rate_limit::RateLimitFeedbackListener,
};
use std::sync::Arc;

#[tokio::test]
async fn cooldown_directive_writes_to_key_health() {
    let store = Arc::new(KeyHealthStore::new());
    let h = ErrorHandler::new(vec![
        Arc::new(KeyHealthListener::new(store.clone())) as _,
        Arc::new(RateLimitFeedbackListener::default()) as _,
        Arc::new(OAuthListener::default()) as _,
        Arc::new(CacheInvalidateListener::default()) as _,
        Arc::new(MetricsListener::default()) as _,
    ]);
    let oc = ClassifierOutcome {
        class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective {
            reason_code: 2, ttl_secs: 60, reset_consecutive: false,
        }),
        oauth_signal: Some(OAuthSignal { kind: OAuthSignalKind::AccountBlocked }),
        rate_limit_hint: None, passthrough_body: None,
    };
    let ctx = HandlerCtx {
        request_id: "r1", api_key_id: Some("ak1"), vendor_key_id: Some("vk1"),
        api_format: Some(gw_core::protocol::ProtocolKind::OpenAi),
        key_health: Some(&store), entity_store: None, resilience: None,
    };
    h.fanout(&ctx, &oc);
    let snap = store.peek_for_test("vk1", gw_core::protocol::ProtocolKind::OpenAi);
    assert!(snap.in_cooldown);
}
```

> 注：`peek_for_test` / `KeyHealthStore::new()` 接口取决于 K-A 实际签名。如 K-A 未落地 `peek_for_test`，先在 K-A spec follow-up 加上；如本 task 与 K-A 进度冲突，则该测试改为只断言 `record_cooldown_calls()` mock 计数。

- [ ] **Step 3: 接通各 listener**

```rust
// listeners/key_health.rs
use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;
use gw_health::KeyHealthStore;
use std::sync::Arc;

pub struct KeyHealthListener { store: Arc<KeyHealthStore> }
impl KeyHealthListener { pub fn new(store: Arc<KeyHealthStore>) -> Self { Self { store } } }

impl ErrorListener for KeyHealthListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if let (Some(cd), Some(vk), Some(fmt)) = (&oc.cooldown, ctx.vendor_key_id, ctx.api_format) {
            self.store.apply_cooldown_for_test(vk, fmt, cd);
        }
    }
}
```

```rust
// listeners/rate_limit.rs
#[derive(Default)] pub struct RateLimitFeedbackListener;
impl crate::error::handler::ErrorListener for RateLimitFeedbackListener {
    fn on_outcome(&self, _ctx: &crate::error::handler::HandlerCtx<'_>,
                  oc: &gw_core::error::ClassifierOutcome) {
        if let Some(hint) = &oc.rate_limit_hint {
            metrics::counter!("gw_resilience_redis_error_total",
                "kind" => format!("{:?}", hint.kind)).increment(0);
            // RL-Adaptive observe 在 RL-Adaptive 落地时接通；本 task 仅打 metric。
        }
    }
}
```

```rust
// listeners/oauth.rs
#[derive(Default)] pub struct OAuthListener;
impl crate::error::handler::ErrorListener for OAuthListener {
    fn on_outcome(&self, ctx: &crate::error::handler::HandlerCtx<'_>,
                  oc: &gw_core::error::ClassifierOutcome) {
        if let Some(sig) = &oc.oauth_signal {
            let vendor = ctx.vendor_key_id.unwrap_or("unknown");
            let outcome = match sig.kind {
                gw_core::error::OAuthSignalKind::RefreshFailed => "refresh_failed",
                gw_core::error::OAuthSignalKind::AccountBlocked => "blocked",
            };
            metrics::counter!("gw_oauth_refresh_total",
                "vendor" => vendor.to_string(),
                "outcome" => outcome.to_string()).increment(1);
            // 真实 K-C oauth store mark_blocked 接通由 K-C 落地后接入。
        }
    }
}
```

```rust
// listeners/cache_invalidate.rs
#[derive(Default)] pub struct CacheInvalidateListener;
impl crate::error::handler::ErrorListener for CacheInvalidateListener {
    fn on_outcome(&self, ctx: &crate::error::handler::HandlerCtx<'_>,
                  oc: &gw_core::error::ClassifierOutcome) {
        let trigger = matches!(oc.class,
            gw_core::error::ErrorClass::AuthDenied
            | gw_core::error::ErrorClass::AccountBlocked(_));
        if trigger {
            if let (Some(es), Some(ak)) = (ctx.entity_store, ctx.api_key_id) {
                let id = gw_core::ids::ApiKeyId::from(ak.to_string());
                let _ = futures::executor::block_on(es.apikey.invalidate(&id));
            }
        }
    }
}
```

`crates/gw-proxy/Cargo.toml` 加 `futures = { workspace = true }`（已有）。

```rust
// listeners/metrics.rs
#[derive(Default)] pub struct MetricsListener;
impl crate::error::handler::ErrorListener for MetricsListener {
    fn on_outcome(&self, _ctx: &crate::error::handler::HandlerCtx<'_>,
                  oc: &gw_core::error::ClassifierOutcome) {
        let class = format!("{:?}", oc.class);
        metrics::counter!("gw_auth_failure_total", "reason" => "n/a").increment(0);
        metrics::counter!("gw_codec_passthrough_bytes_total").increment(0);
        metrics::counter!("gw_classifier_outcome_total", "class" => class).increment(1);
    }
}
```

```rust
// listeners/ledger.rs
#[derive(Default)] pub struct LedgerListener;
impl crate::error::handler::ErrorListener for LedgerListener {
    fn on_outcome(&self, _ctx: &crate::error::handler::HandlerCtx<'_>,
                  _oc: &gw_core::error::ClassifierOutcome) {
        // detailed_log 真接 sink 在 Wave 6 完成；本 task 占位
    }
}
```

- [ ] **Step 4: KeyHealthStore 加 test helper（若不存在）**

如 K-A 已落地 `apply_cooldown(KeyId, ApiFormat, CooldownDirective)`，listener 直接调即可。如 listener 用的字段命名不同，调整 listener。

为保险，在 `gw-health` 内加 `apply_cooldown_for_test` / `peek_for_test`（如已存在等价 API 直接 re-export）。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test listeners_wire
cargo build --workspace
git add crates/gw-proxy crates/gw-health
git commit -m "feat(gw-proxy): wire all 6 listeners to real KeyHealth/Entity/metrics (G-α.2 wire)"
```

---

### Task T5-B: ReDoS 100ms 运行时熔断 + 自动 disable

**Files:**
- Modify: `crates/gw-auth/src/model_permissions.rs`
- Test: `crates/gw-auth/tests/redos_runtime.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_auth::model_permissions::{
    check_with_budget, ModelPermission, PermissionEffect, RedosTracker, BUDGET_MS,
};
use std::time::Duration;

#[test]
fn match_within_budget_returns_normal() {
    let tracker = RedosTracker::new();
    let p = vec![ModelPermission::new("^claude.*$", PermissionEffect::Allow).unwrap()];
    let r = check_with_budget("claude-3-opus", &p, "ak1", &tracker);
    assert_eq!(r, Some(true));
    assert_eq!(tracker.timeout_count("ak1"), 0);
}

#[test]
fn timeout_counts_and_after_threshold_disables() {
    let tracker = RedosTracker::new();
    let p = vec![]; // 没有 perms 也走计时器路径
    // 模拟 timeout：用 fake_now 直接调 record_timeout
    for _ in 0..10 { tracker.record_timeout("ak1"); }
    assert!(tracker.is_disabled("ak1"));
    let r = check_with_budget("anything", &p, "ak1", &tracker);
    assert_eq!(r, None); // disabled → caller 应当 reject 请求
}
```

- [ ] **Step 2: 实现**

在 `model_permissions.rs` 末尾追加：

```rust
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const BUDGET_MS: u128 = 100;
const TIMEOUT_THRESHOLD: u32 = 10;

#[derive(Default)]
pub struct RedosTracker {
    inner: Mutex<HashMap<String, TrackerEntry>>,
}

#[derive(Default)]
struct TrackerEntry {
    timeout_count: u32,
    disabled: bool,
}

impl RedosTracker {
    pub fn new() -> Self { Self::default() }
    pub fn timeout_count(&self, ak: &str) -> u32 {
        self.inner.lock().get(ak).map(|e| e.timeout_count).unwrap_or(0)
    }
    pub fn is_disabled(&self, ak: &str) -> bool {
        self.inner.lock().get(ak).map(|e| e.disabled).unwrap_or(false)
    }
    pub fn record_timeout(&self, ak: &str) {
        let mut m = self.inner.lock();
        let e = m.entry(ak.to_string()).or_default();
        e.timeout_count += 1;
        if e.timeout_count >= TIMEOUT_THRESHOLD { e.disabled = true; }
        metrics::counter!("gw_redos_timeout_total", "api_key_id" => ak.to_string()).increment(1);
    }
}

/// 调用 regex match 时如果超过 BUDGET_MS，按 timeout 计；
/// 注意：regex crate 自身没有原子超时，通过 elapsed 判定 + 手动短路。
/// v1 用"启动定时器→匹配→检查耗时"的事后判定；超过即记录但本次仍取真实结果。
/// 后续可换 `regex_automata::dfa` 直接增量 step。
///
/// 返回：Some(matched)；disabled key 返回 None。
pub fn check_with_budget(
    model: &str,
    perms: &[ModelPermission],
    api_key_id: &str,
    tracker: &RedosTracker,
) -> Option<bool> {
    if tracker.is_disabled(api_key_id) { return None; }
    let start = Instant::now();
    let result = check_model_against_permissions(model, perms);
    let elapsed = start.elapsed();
    if elapsed.as_millis() > BUDGET_MS {
        tracker.record_timeout(api_key_id);
    }
    Some(result)
}
```

`crates/gw-auth/Cargo.toml` 加 `parking_lot = { workspace = true }`、`metrics = { workspace = true }`（如缺）。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-auth --test redos_runtime
git add crates/gw-auth
git commit -m "feat(gw-auth): RedosTracker 100ms budget + auto-disable (G-β.7)"
```

---

### Task T5-C: ErrorPassthrough（codec 端，按客户端协议渲染上游错误体）

**Files:**
- Create: `crates/gw-codec/src/error_passthrough.rs`
- Modify: `crates/gw-codec/src/lib.rs`
- Test: `crates/gw-codec/tests/error_passthrough.rs`

- [ ] **Step 1: 写失败测试**

```rust
use gw_codec::error_passthrough::render_error_to_client;
use gw_core::error::{ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::json;

#[test]
fn anthropic_error_renders_to_openai_with_type() {
    let body = PassthroughBody {
        upstream_status: 503,
        body_json: json!({"type":"overloaded_error","message":"too busy"}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::UpstreamTransient);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["type"], "service_unavailable");
}

#[test]
fn sse_error_emits_event_error_frame() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"error":"oops"}),
        is_sse: true,
    };
    let out = render_error_to_client(ProtocolKind::Anthropic, &body, ErrorClass::UpstreamTransient);
    let s = std::str::from_utf8(&out).unwrap();
    assert!(s.starts_with("event: error\n"));
    assert!(s.contains("data: "));
}
```

- [ ] **Step 2: 实现**

```rust
// crates/gw-codec/src/error_passthrough.rs
use bytes::Bytes;
use gw_core::error::{ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::{json, Value};

pub fn render_error_to_client(
    client: ProtocolKind, body: &PassthroughBody, fallback: ErrorClass,
) -> Bytes {
    let v = json_for_protocol(client, body, fallback);
    if body.is_sse {
        let s = format!("event: error\ndata: {}\n\n", serde_json::to_string(&v).unwrap());
        Bytes::from(s.into_bytes())
    } else {
        Bytes::from(serde_json::to_vec(&v).unwrap())
    }
}

fn json_for_protocol(p: ProtocolKind, b: &PassthroughBody, fb: ErrorClass) -> Value {
    let typ = match (p, b.upstream_status, fb) {
        (ProtocolKind::OpenAi, 5, _) | (ProtocolKind::OpenAi, 503, _)
            => "service_unavailable",
        (ProtocolKind::OpenAi, 401, _) => "invalid_request_error",
        (ProtocolKind::OpenAi, 429, _) => "rate_limit_exceeded",
        (ProtocolKind::OpenAi, _, _) => "api_error",
        (ProtocolKind::Anthropic, 401, _) => "authentication_error",
        (ProtocolKind::Anthropic, 429, _) => "rate_limit_error",
        (ProtocolKind::Anthropic, _, _) => "api_error",
        (ProtocolKind::Gemini, _, _) => "INTERNAL",
    };
    let msg = b.body_json.get("message")
        .or_else(|| b.body_json.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("upstream error");
    match p {
        ProtocolKind::OpenAi => json!({
            "error": { "type": typ, "message": msg, "code": null }
        }),
        ProtocolKind::Anthropic => json!({
            "type": "error",
            "error": { "type": typ, "message": msg }
        }),
        ProtocolKind::Gemini => json!({
            "error": { "code": b.upstream_status, "status": typ, "message": msg }
        }),
    }
}
```

`gw-codec/src/lib.rs` 加 `pub mod error_passthrough;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test error_passthrough
git add crates/gw-codec
git commit -m "feat(gw-codec): render_error_to_client (G-α.3 / E3)"
```

---

### Task T5-D: ProtocolKind 扩 OpenAiResponses 形态 + capability 自动加 OpenAiResponsesShape

**Files:**
- Modify: `crates/gw-core/src/protocol.rs`（如 enum 已有 OpenAi，加 ApiShape 子枚举）
- Modify: `crates/gw-capability/src/resolver.rs`（resolve 时若 protocol shape == Responses 则强制加 cap）
- Test: `crates/gw-capability/tests/responses_capability.rs`

- [ ] **Step 1: 加 ApiShape**

```rust
// crates/gw-core/src/protocol.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiShape {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
}
```

> 不破坏既有 `ProtocolKind`：`ApiShape` 是更细粒度，调用方按需用。

- [ ] **Step 2: 写失败测试**

```rust
use gw_capability::resolver::{
    CapabilityResolver, FakeResolveBackend,
};
use gw_capability::types::Capability;
use gw_core::protocol::{ApiShape, ProtocolKind};

#[tokio::test]
async fn responses_shape_auto_adds_cap() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let got = r.resolve_with_shape(
        "ak1", "gpt-5", None, ProtocolKind::OpenAi, ApiShape::OpenAiResponses, None,
    ).await.unwrap();
    assert!(got.set.contains(&Capability::OpenAiResponsesShape));
}

#[tokio::test]
async fn chat_shape_does_not_add_responses_cap() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(std::sync::Arc::new(be));
    let got = r.resolve_with_shape(
        "ak1", "gpt-5", None, ProtocolKind::OpenAi, ApiShape::OpenAiChat, None,
    ).await.unwrap();
    assert!(!got.set.contains(&Capability::OpenAiResponsesShape));
}
```

- [ ] **Step 3: 实现 resolve_with_shape**

`crates/gw-capability/src/resolver.rs` 内为 `CapabilityResolver` 加方法：

```rust
use gw_core::protocol::ApiShape;
use crate::types::CapabilityOrigin;

impl CapabilityResolver {
    pub async fn resolve_with_shape(
        &self, ak: &str, model: &str, header_require: Option<&[&str]>,
        protocol: ProtocolKind, shape: ApiShape, retry: Option<&RetryUpgradeHint>,
    ) -> anyhow::Result<RequiredCapabilities> {
        let req = ResolveRequest {
            api_key_id: ak, model_alias: model, header_require,
            protocol, retry_upgrade: retry,
        };
        let mut r = self.resolve(&req).await?;
        if shape == ApiShape::OpenAiResponses {
            r.add(Capability::OpenAiResponsesShape, CapabilityOrigin::HeaderRequire);
        }
        Ok(r)
    }
}
```

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test responses_capability
git add crates/gw-core crates/gw-capability
git commit -m "feat(gw-capability): auto-add OpenAiResponsesShape cap when shape detected (G-γ.3 ⇄ G-β.3)"
```

---

### Task T5-E: capability filter chain 入口 + ReDoS runtime 接通

**Files:**
- Create: `crates/gw-capability/src/filter.rs`
- Modify: `crates/gw-capability/src/lib.rs`
- Modify: `crates/gw-auth/src/model_permissions.rs`（暴露 `RedosTracker` 全局实例）
- Test: `crates/gw-capability/tests/filter.rs`

> 把 T2-D + T5-B 的 ReDoS 编译期 + runtime 熔断**接到** capability resolver 流水线。CapabilityResolver 在 resolve 时若该 ApiKey 已被 RedosTracker 标 disabled → 返回错误。R-AB filter chain 落地后再把本 filter 接进真实 routing；本 task 落地 trait + adapter，不阻塞 R-AB。

- [ ] **Step 1: 写失败测试**

```rust
// crates/gw-capability/tests/filter.rs
use gw_auth::model_permissions::{
    check_with_budget, ModelPermission, PermissionEffect, RedosTracker,
};
use gw_capability::filter::{CapabilityFilter, CapabilityFilterResult, CandidateView};
use gw_capability::types::{Capability, MatchMode, RequiredCapabilities, CapabilityOrigin};
use std::sync::Arc;

#[test]
fn key_disabled_by_redos_returns_unavailable() {
    let tracker = Arc::new(RedosTracker::new());
    for _ in 0..10 { tracker.record_timeout("ak-bad"); }
    let f = CapabilityFilter::new(tracker);
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    let candidate = CandidateView {
        api_key_id: "ak-bad",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    let r = f.evaluate(&req, &candidate);
    assert_eq!(r, CapabilityFilterResult::ApiKeyDisabled);
}

#[test]
fn compatible_mode_accepts_superset() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    req.mode = MatchMode::Compatible;
    let candidate = CandidateView {
        api_key_id: "ak1", vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    assert_eq!(f.evaluate(&req, &candidate), CapabilityFilterResult::Match);
}

#[test]
fn exclusive_mode_rejects_superset() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    req.mode = MatchMode::Exclusive;
    let candidate = CandidateView {
        api_key_id: "ak1", vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    assert_eq!(f.evaluate(&req, &candidate), CapabilityFilterResult::ExtraCaps);
}

#[test]
fn missing_required_cap_returns_missing() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Audio, CapabilityOrigin::HeaderRequire);
    let candidate = CandidateView {
        api_key_id: "ak1", vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    match f.evaluate(&req, &candidate) {
        CapabilityFilterResult::Missing(v) => assert_eq!(v[..], [Capability::Audio]),
        other => panic!("{other:?}"),
    }
}
```

- [ ] **Step 2: 实现 filter.rs**

```rust
// crates/gw-capability/src/filter.rs
use crate::types::{Capability, MatchMode, RequiredCapabilities};
use gw_auth::model_permissions::RedosTracker;
use smallvec::SmallVec;
use std::sync::Arc;

pub struct CapabilityFilter {
    redos: Arc<RedosTracker>,
}
impl CapabilityFilter {
    pub fn new(redos: Arc<RedosTracker>) -> Self { Self { redos } }

    pub fn evaluate(
        &self,
        required: &RequiredCapabilities,
        candidate: &CandidateView<'_>,
    ) -> CapabilityFilterResult {
        if self.redos.is_disabled(candidate.api_key_id) {
            return CapabilityFilterResult::ApiKeyDisabled;
        }
        let mut missing: SmallVec<[Capability; 4]> = SmallVec::new();
        for c in &required.set {
            if !candidate.detected_capabilities.contains(c) { missing.push(*c); }
        }
        if !missing.is_empty() { return CapabilityFilterResult::Missing(missing); }
        if required.mode == MatchMode::Exclusive {
            for c in &candidate.detected_capabilities {
                if !required.set.contains(c) {
                    return CapabilityFilterResult::ExtraCaps;
                }
            }
        }
        metrics::counter!("gw_capability_resolve_total", "match" => "filter_hit").increment(1);
        CapabilityFilterResult::Match
    }
}

pub struct CandidateView<'a> {
    pub api_key_id: &'a str,
    pub vendor_key_id: &'a str,
    pub detected_capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityFilterResult {
    Match,
    Missing(SmallVec<[Capability; 4]>),
    ExtraCaps,           // exclusive 模式下候选多了
    ApiKeyDisabled,      // ReDoS 熔断
}
```

`crates/gw-capability/src/lib.rs` 加 `pub mod filter;`。
`crates/gw-capability/Cargo.toml` 加 `gw-auth = { path = "../gw-auth" }`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-capability --test filter
bash scripts/check-invariants.sh
git add crates/gw-capability
git commit -m "feat(gw-capability): CapabilityFilter chain entry + ReDoS-aware (ADR-G2-8 wire)"
```

> R-AB filter chain 真接入 = R-AB spec 落地时把本 filter 注册进 FilterChain。本 plan 边界至 trait + adapter；R-AB 不在本 spec 范围。

---

### Task T5-F: OpenAI Responses ⇄ Chat ⇄ Anthropic / Gemini 跨形矩阵转换

**Files:**
- Create: `crates/gw-codec/src/openai/responses/conversion.rs`
- Modify: `crates/gw-codec/src/openai/responses/mod.rs`
- Modify: `crates/gw-codec/src/wire/any.rs`（如 WireAnyRequest 已含 OpenAiResponses 变体，加 convert 入口）
- Test: `crates/gw-codec/tests/responses_conversion.rs`

> ADR-G4-3 要求 4 向（含跨形）。S1 矩阵宏存在与否不阻塞**手写**直转的落地：v1 写 4 个 cross via 函数，矩阵宏完成后再迁移到 codegen。每 via 仅做最小可用映射（content / tool_calls / model name 直传），**不实现** thinking / multimodal 跨协议（这两条由 S2/S3 主线 spec 处理）。

- [ ] **Step 1: 写失败测试**

```rust
// crates/gw-codec/tests/responses_conversion.rs
use gw_codec::openai::responses::OpenAiResponsesRequest;
use gw_codec::openai::responses::conversion::*;
use serde_json::json;

#[test]
fn responses_to_chat_lifts_input_to_messages() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hi"}],
        "stream": true
    })).unwrap();
    let chat = responses_to_chat(&req).unwrap();
    let v = serde_json::to_value(&chat).unwrap();
    assert_eq!(v["model"], "gpt-5");
    assert_eq!(v["messages"][0]["role"], "user");
    assert_eq!(v["messages"][0]["content"], "hi");
}

#[test]
fn responses_to_anthropic_minimal() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hello"}]
    })).unwrap();
    let an = responses_to_anthropic(&req).unwrap();
    assert_eq!(an["messages"][0]["role"], "user");
}

#[test]
fn responses_to_gemini_minimal() {
    let req: OpenAiResponsesRequest = serde_json::from_value(json!({
        "model": "gpt-5",
        "input": [{"type":"input_text","text":"hello"}]
    })).unwrap();
    let g = responses_to_gemini(&req).unwrap();
    assert_eq!(g["contents"][0]["role"], "user");
}

#[test]
fn chat_to_responses_round_trip_preserves_text() {
    let chat = json!({
        "model": "gpt-5",
        "messages": [{"role":"user","content":"hi"}]
    });
    let r = chat_to_responses(&chat).unwrap();
    assert_eq!(r.input[0]["type"], "input_text");
    assert_eq!(r.input[0]["text"], "hi");
}
```

- [ ] **Step 2: 实现 conversion.rs**

```rust
// crates/gw-codec/src/openai/responses/conversion.rs
use super::OpenAiResponsesRequest;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("unsupported part: {0}")]
    Unsupported(String),
    #[error("missing field: {0}")]
    Missing(&'static str),
}

pub fn responses_to_chat(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut messages = Vec::new();
    for p in &req.input {
        let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match typ {
            "input_text" | "text" => {
                messages.push(json!({
                    "role": "user",
                    "content": p.get("text").cloned().unwrap_or(Value::Null),
                }));
            }
            other => return Err(ConvertError::Unsupported(other.into())),
        }
    }
    if let Some(instr) = &req.instructions {
        messages.insert(0, json!({"role": "system", "content": instr}));
    }
    let mut out = json!({
        "model": req.model,
        "messages": messages,
        "stream": req.stream,
    });
    if let Value::Object(ref mut m) = out {
        for (k, v) in &req.extras { m.insert(k.clone(), v.clone()); }
    }
    Ok(out)
}

pub fn chat_to_responses(chat: &Value) -> Result<OpenAiResponsesRequest, ConvertError> {
    let model = chat.get("model").and_then(|v| v.as_str())
        .ok_or(ConvertError::Missing("model"))?.to_string();
    let messages = chat.get("messages").and_then(|v| v.as_array())
        .ok_or(ConvertError::Missing("messages"))?;
    let mut input: Vec<Value> = Vec::new();
    let mut instructions: Option<String> = None;
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        if role == "system" {
            if let Value::String(s) = &content {
                instructions = Some(s.clone());
                continue;
            }
        }
        match content {
            Value::String(s) => input.push(json!({"type":"input_text","text":s})),
            Value::Array(arr) => {
                for p in arr { input.push(p); }
            }
            _ => return Err(ConvertError::Unsupported("non-text content".into())),
        }
    }
    Ok(OpenAiResponsesRequest {
        model, input,
        stream: chat.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
        instructions, previous_response_id: None,
        extras: serde_json::Map::new(),
    })
}

pub fn responses_to_anthropic(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut messages = Vec::new();
    for p in &req.input {
        let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match typ {
            "input_text" | "text" => {
                messages.push(json!({
                    "role": "user",
                    "content": p.get("text").cloned().unwrap_or(Value::Null),
                }));
            }
            other => return Err(ConvertError::Unsupported(other.into())),
        }
    }
    let mut out = json!({
        "model": req.model,
        "messages": messages,
        "stream": req.stream,
        "max_tokens": 4096,
    });
    if let Some(s) = &req.instructions {
        if let Value::Object(ref mut m) = out { m.insert("system".into(), Value::String(s.clone())); }
    }
    Ok(out)
}

pub fn responses_to_gemini(req: &OpenAiResponsesRequest) -> Result<Value, ConvertError> {
    let mut contents = Vec::new();
    for p in &req.input {
        let typ = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match typ {
            "input_text" | "text" => {
                contents.push(json!({
                    "role": "user",
                    "parts": [ { "text": p.get("text").cloned().unwrap_or(Value::Null) } ]
                }));
            }
            other => return Err(ConvertError::Unsupported(other.into())),
        }
    }
    let mut out = json!({
        "contents": contents,
    });
    if let Some(s) = &req.instructions {
        if let Value::Object(ref mut m) = out {
            m.insert("systemInstruction".into(),
                json!({"role":"system","parts":[{"text":s}]}));
        }
    }
    Ok(out)
}
```

`crates/gw-codec/src/openai/responses/mod.rs` 加 `pub mod conversion;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test responses_conversion
git add crates/gw-codec
git commit -m "feat(gw-codec): OpenAI Responses ⇄ Chat/Anthropic/Gemini conversion (ADR-G4-3 full impl)"
```



<!-- WAVE-6 -->

## Wave 6 — Filter chain 接通（顺序执行）

> 进入 Pingora filter 真实修改路径；同一文件多 task 接力，必须顺序。

### Task T6-A: filters::request 接 mgmt 路径分流 + capability 调用 + JWT validator 切换

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`（加 `mgmt: bool` / `required_caps: Option<RequiredCapabilities>`）
- Modify: `crates/gw-proxy/src/gateway.rs`（构造 LlmGateway 时把 CapabilityResolver / JwtValidator 注入）
- Test: `crates/gw-proxy/tests/filters_request_mgmt.rs`

- [ ] **Step 1: ctx 加字段**

```rust
// crates/gw-proxy/src/ctx.rs 内 RequestCtx 加：
pub mgmt: bool,
pub required_caps: Option<gw_capability::types::RequiredCapabilities>,
```

`gw-proxy/Cargo.toml` 加 `gw-capability = { path = "../gw-capability" }`、`gw-auth` 已在。

- [ ] **Step 2: 在 filters::request 入口分流**

在 filter 的最早处（路径就绪后）加：

```rust
let path = session.req_header().uri.path();
if gw_auth::mgmt::is_management_path(path) {
    ctx.mgmt = true;
    // 解析 Authorization: Mgmt <token>；缺则发 401
    let auth = session.req_header()
        .headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    if gw_auth::mgmt::parse_mgmt_authorization(auth).is_none() {
        let err = build_unauthorized_err(ctx);
        crate::error_emit::send_error(session, ctx.codec(), &err).await;
        return Ok(true);
    }
    // mgmt 路径直接放行到 upstream/handler；不跑业务 filter 后续
    return Ok(false);
}
```

`build_unauthorized_err` 是本文件 helper，构造 `ErrorIR`（401 / `GW_AUTH_INVALID_JWT`）。

- [ ] **Step 3: capability resolve 入 ctx**

在通过 ApiKey / JWT 鉴权之后、走 routing 之前：

```rust
let header_require: Vec<&str> = session.req_header()
    .headers.get_all("x-require-capability").iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|s| s.split(',').map(str::trim))
    .collect();
let req = gw_capability::resolver::ResolveRequest {
    api_key_id: api_key_id_str,
    model_alias: model_alias_str,
    header_require: if header_require.is_empty() { None } else { Some(&header_require) },
    protocol: detected_protocol,
    retry_upgrade: ctx.retry_upgrade.as_ref(),
};
let caps = match resolver.resolve(&req).await {
    Ok(c) => c,
    Err(e) => {
        tracing::warn!(error=%e, "capability resolve failed");
        let err = build_capability_failed_err(ctx);
        crate::error_emit::send_error(session, ctx.codec(), &err).await;
        return Ok(true);
    }
};
ctx.required_caps = Some(caps);
```

`resolver` 由 `LlmGateway` 构造期注入并通过 `Arc<CapabilityResolver>` 持有。

- [ ] **Step 4: 写测试**

```rust
// crates/gw-proxy/tests/filters_request_mgmt.rs
//
// 直接 unit-test mgmt branch 难搞 Pingora session；这里仅 smoke：
// 调 gw_auth::mgmt::is_management_path + parse_mgmt_authorization 双重确认 path 与 header
// 行为，配合 W1-D 已存在的 mgmt 测试就足够覆盖 filter 分支。

#[test]
fn mgmt_path_combined() {
    use gw_auth::mgmt::{is_management_path, parse_mgmt_authorization};
    assert!(is_management_path("/admin/freeze"));
    assert!(parse_mgmt_authorization("Mgmt foo").is_some());
    assert!(parse_mgmt_authorization("Bearer foo").is_none());
}
```

> 真正 filter 集成测试通过 `e2e_failure_modes` 的 wiremock 链路覆盖；本 plan 不强求每条 filter 改动都有专门 e2e（避免 plan 爆炸）。

- [ ] **Step 5: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test filters_request_mgmt
cargo build --workspace
git add crates/gw-proxy
git commit -m "feat(gw-proxy): mgmt path early branch + capability resolve in request filter (G-α.9 / G-β wire)"
```

---

### Task T6-B: filters::logging 切换到 ErrorHandler fanout

**Files:**
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`（构造 ErrorHandler 注入）
- Test: `crates/gw-proxy/tests/filters_logging_handler.rs`

- [ ] **Step 1: gateway 构造 handler**

`gw-proxy::gateway::LlmGateway` 在 `new` 时：

```rust
let handler = ErrorHandler::new(vec![
    Arc::new(KeyHealthListener::new(deps.key_health.clone())) as _,
    Arc::new(RateLimitFeedbackListener::default()) as _,
    Arc::new(OAuthListener::default()) as _,
    Arc::new(CacheInvalidateListener::default()) as _,
    Arc::new(MetricsListener::default()) as _,
    Arc::new(LedgerListener::default()) as _,
]);
self.error_handler = Arc::new(handler);
```

`LlmGatewayDeps` 加 `key_health: Arc<KeyHealthStore>`、`entity_store: Arc<EntityStore>` 字段（如尚未有）。

- [ ] **Step 2: filters::logging 调 fanout**

在 `logging` filter 末尾，把现有 freeze release / key-health record 的散点调用替换成：

```rust
if let Some(oc) = ctx.error_outcome.as_ref() {
    let hctx = HandlerCtx {
        request_id: ctx.request_id.as_str(),
        api_key_id: ctx.api_key_id.as_deref(),
        vendor_key_id: ctx.vendor_key_id.as_deref(),
        api_format: ctx.api_format,
        key_health: Some(&self.deps.key_health),
        entity_store: Some(&self.deps.entity_store),
        resilience: Some(&self.resilience),
    };
    self.error_handler.fanout(&hctx, oc);
}
```

`ctx.error_outcome` 字段在 `RequestCtx` 上加（成功路径填一个 ClassifierOutcome::default + class=Success——或新增 `ErrorClass::Success`，但通常只在错误路径填）。

- [ ] **Step 3: 写测试（smoke 覆盖）**

```rust
// crates/gw-proxy/tests/filters_logging_handler.rs
// 由于 logging filter 与 Pingora 强耦合，改测 ErrorHandler::fanout 在
// 给定 ctx 下不 panic；listener 数 = 6。
use gw_proxy::error::handler::{ErrorHandler, HandlerCtx};
use std::sync::Arc;

#[test]
fn six_listeners_registered() {
    use gw_proxy::error::listeners::{
        cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
        ledger::LedgerListener, metrics::MetricsListener,
        oauth::OAuthListener, rate_limit::RateLimitFeedbackListener,
    };
    use gw_health::KeyHealthStore;
    let store = Arc::new(KeyHealthStore::new());
    let h = ErrorHandler::new(vec![
        Arc::new(KeyHealthListener::new(store)) as _,
        Arc::new(RateLimitFeedbackListener::default()) as _,
        Arc::new(OAuthListener::default()) as _,
        Arc::new(CacheInvalidateListener::default()) as _,
        Arc::new(MetricsListener::default()) as _,
        Arc::new(LedgerListener::default()) as _,
    ]);
    let ctx = HandlerCtx::stub();
    let oc = gw_core::error::ClassifierOutcome::default();
    h.fanout(&ctx, &oc); // 不 panic 即通过
}
```

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test filters_logging_handler
cargo build --workspace
git add crates/gw-proxy
git commit -m "feat(gw-proxy): logging filter switches to ErrorHandler.fanout (G-α.2 wire)"
```

---

### Task T6-C: HMAC 强制接 request_body_filter（body-after 校验）+ 压缩入站解码

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`（仅 header-only 部分）
- Modify: `crates/gw-proxy/src/gateway.rs`（`request_body_filter` 内 EOS 时做 HMAC + 解压）
- Test: `crates/gw-proxy/tests/hmac_required_e2e.rs`（如能通过现有 e2e 框架）

- [ ] **Step 1: filter::request 设置标记**

如 ApiKey.signature_required = true，把 `ctx.signature_required = true` + 把客户端 header `x-signature` / `x-timestamp` 缓存到 ctx：

```rust
if api_key.signature_required {
    ctx.signature_required = true;
    ctx.signature_received_b64 = session.req_header().headers
        .get("x-signature").and_then(|v| v.to_str().ok())
        .map(|s| s.as_bytes().to_vec());
    ctx.signature_timestamp = session.req_header().headers
        .get("x-timestamp").and_then(|v| v.to_str().ok()).map(String::from);
}
```

- [ ] **Step 2: request_body_filter 在 EOS 时做校验**

```rust
// gateway.rs::request_body_filter
if eos && ctx.signature_required {
    // body 已完整在 ctx.cached_body
    let raw = ctx.cached_body.as_ref().map(|b| b.as_ref()).unwrap_or(&[]);
    let decoded: bytes::Bytes = match ctx.req_content_encoding {
        gw_codec::compression::Encoding::Identity => bytes::Bytes::copy_from_slice(raw),
        enc => match gw_codec::compression::decode(enc, raw) {
            Ok(b) => b,
            Err(_) => return signature_invalid(session, ctx).await,
        },
    };
    let r = gw_auth::signature::verify_required(
        true,
        ctx.signature_algo,
        ctx.api_key_secret.expose_secret().as_bytes(),
        ctx.signature_received_b64.as_deref(),
        ctx.signature_timestamp.as_deref(),
        &gw_auth::signature::SignatureInput {
            method: ctx.method.as_str(),
            path: ctx.path.as_str(),
            timestamp: ctx.signature_timestamp.as_deref().unwrap_or(""),
            body: &decoded,
        },
    );
    if !matches!(r, gw_auth::signature::SignatureCheckResult::Valid) {
        return signature_invalid(session, ctx).await;
    }
    // 通过后用 decoded 覆盖 cached_body，确保 codec parse 拿到的是解压后字节
    ctx.cached_body = Some(decoded);
}
```

`ctx.req_content_encoding` 由 `filters::request` 在解析 Content-Encoding header 时设置（默认 Identity）。

`signature_invalid` 是 helper：发 401 GW_AUTH_SIGNATURE_INVALID 错误体。

- [ ] **Step 3: 写测试**

```rust
// crates/gw-proxy/tests/hmac_required_e2e.rs
// 真 e2e 难做（依赖 wiremock + 完整 Pingora 起服务）；
// 此处 unit-test：直接调 gw_auth::signature::verify_required，模拟 body-after 行为。
use gw_auth::signature::{verify_required, SignatureCheckResult, SignatureInput};
use gw_core::entity::SignatureAlgo;

#[test]
fn body_after_compressed_then_verify_round_trip() {
    let secret = b"s";
    let body = b"{\"hi\":1}";
    let ts = format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    // 客户端用解压前/解压后哪个签名？标准是"原始 plaintext body"。
    // 模拟：客户端先 br 压再签：但服务端要先解再签，故签的应当是 plaintext。
    let sig = sign_for_test(secret, "POST", "/v1/chat", &ts, body);
    let r = verify_required(
        true, SignatureAlgo::HmacSha256, secret,
        Some(&sig), Some(&ts),
        &SignatureInput { method: "POST", path: "/v1/chat", timestamp: &ts, body },
    );
    assert_eq!(r, SignatureCheckResult::Valid);
}

fn sign_for_test(secret: &[u8], method: &str, path: &str, ts: &str, body: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac}; use sha2::{Sha256, Digest};
    let body_hash = hex::encode(Sha256::digest(body));
    let signing = format!("{method}\n{path}\n{ts}\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
```

- [ ] **Step 4: 跑测试 + 提交**

```bash
cargo test -p gw-proxy --test hmac_required_e2e
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
bash scripts/check-invariants.sh
git add crates/gw-proxy
git commit -m "feat(gw-proxy): HMAC body-after verify + compression decode in request_body_filter (G-α.8 wire)"
```

---

---

### Task T6-D: stream_policy AccumulateUpstreamStream 真实现

**Files:**
- Create: `crates/gw-codec/src/stream_accumulator.rs`
- Modify: `crates/gw-codec/src/lib.rs`
- Test: `crates/gw-codec/tests/stream_accumulator.rs`

> ADR-G4-1 要求"客户端 sync + 上游强 stream → 网关 accumulate"。本 task 实现 SSE → 单 JSON 累积器，针对三协议各自的 chunk 形态。

- [ ] **Step 1: 写失败测试**

```rust
// crates/gw-codec/tests/stream_accumulator.rs
use gw_codec::stream_accumulator::{accumulate_openai, accumulate_anthropic};

#[test]
fn openai_chat_chunks_concat_into_message() {
    let chunks = [
        r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ];
    let final_json = accumulate_openai(chunks.iter().map(|s| s.as_bytes())).unwrap();
    assert_eq!(final_json["choices"][0]["message"]["role"], "assistant");
    assert_eq!(final_json["choices"][0]["message"]["content"], "Hello");
    assert_eq!(final_json["choices"][0]["finish_reason"], "stop");
}

#[test]
fn anthropic_event_stream_concat_into_message() {
    let events = [
        r#"{"type":"message_start","message":{"id":"m1","role":"assistant","content":[]}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
    ];
    let final_json = accumulate_anthropic(events.iter().map(|s| s.as_bytes())).unwrap();
    assert_eq!(final_json["id"], "m1");
    assert_eq!(final_json["content"][0]["text"], "Hello");
    assert_eq!(final_json["stop_reason"], "end_turn");
}

#[test]
fn malformed_chunk_returns_error() {
    let chunks = ["{not json"];
    let r = accumulate_openai(chunks.iter().map(|s| s.as_bytes()));
    assert!(r.is_err());
}
```

- [ ] **Step 2: 实现**

```rust
// crates/gw-codec/src/stream_accumulator.rs
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum AccumulateError {
    #[error("parse: {0}")]
    Parse(String),
    #[error("missing field: {0}")]
    Missing(&'static str),
}

pub fn accumulate_openai<'a, I>(chunks: I) -> Result<Value, AccumulateError>
where I: IntoIterator<Item = &'a [u8]>,
{
    let mut role = None::<String>;
    let mut content = String::new();
    let mut finish_reason = None::<String>;
    let mut tool_calls = Vec::new();
    let mut model = None::<String>;
    let mut id = None::<String>;
    for raw in chunks {
        let v: Value = serde_json::from_slice(raw)
            .map_err(|e| AccumulateError::Parse(e.to_string()))?;
        if let Some(s) = v.get("model").and_then(|v| v.as_str()) { model = Some(s.into()); }
        if let Some(s) = v.get("id").and_then(|v| v.as_str()) { id = Some(s.into()); }
        if let Some(arr) = v.get("choices").and_then(|v| v.as_array()) {
            for ch in arr {
                if let Some(d) = ch.get("delta") {
                    if let Some(r) = d.get("role").and_then(|v| v.as_str()) { role = Some(r.into()); }
                    if let Some(c) = d.get("content").and_then(|v| v.as_str()) { content.push_str(c); }
                    if let Some(tc) = d.get("tool_calls").and_then(|v| v.as_array()) {
                        for t in tc { tool_calls.push(t.clone()); }
                    }
                }
                if let Some(f) = ch.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(f.into());
                }
            }
        }
    }
    Ok(json!({
        "id": id.unwrap_or_default(),
        "object": "chat.completion",
        "model": model.unwrap_or_default(),
        "choices": [{
            "index": 0,
            "message": {
                "role": role.unwrap_or_else(|| "assistant".into()),
                "content": content,
                "tool_calls": tool_calls,
            },
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".into()),
        }],
    }))
}

pub fn accumulate_anthropic<'a, I>(events: I) -> Result<Value, AccumulateError>
where I: IntoIterator<Item = &'a [u8]>,
{
    let mut id = None::<String>;
    let mut role = None::<String>;
    let mut blocks: Vec<Value> = Vec::new();
    let mut stop_reason = None::<String>;
    let mut model = None::<String>;
    for raw in events {
        let v: Value = serde_json::from_slice(raw)
            .map_err(|e| AccumulateError::Parse(e.to_string()))?;
        let typ = v.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match typ {
            "message_start" => {
                if let Some(m) = v.get("message") {
                    if let Some(s) = m.get("id").and_then(|v| v.as_str()) { id = Some(s.into()); }
                    if let Some(s) = m.get("role").and_then(|v| v.as_str()) { role = Some(s.into()); }
                    if let Some(s) = m.get("model").and_then(|v| v.as_str()) { model = Some(s.into()); }
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block = v.get("content_block").cloned().unwrap_or(json!({"type":"text","text":""}));
                while blocks.len() <= idx { blocks.push(json!({"type":"text","text":""})); }
                blocks[idx] = block;
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if idx >= blocks.len() { continue; }
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if dtype == "text_delta" {
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        if let Value::Object(ref mut m) = blocks[idx] {
                            let cur = m.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            m.insert("text".into(), Value::String(cur + t));
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(d) = v.get("delta") {
                    if let Some(s) = d.get("stop_reason").and_then(|v| v.as_str()) {
                        stop_reason = Some(s.into());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(json!({
        "id": id.unwrap_or_default(),
        "type": "message",
        "role": role.unwrap_or_else(|| "assistant".into()),
        "model": model.unwrap_or_default(),
        "content": blocks,
        "stop_reason": stop_reason.unwrap_or_default(),
    }))
}
```

`gw-codec/src/lib.rs` 加 `pub mod stream_accumulator;`。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-codec --test stream_accumulator
git add crates/gw-codec
git commit -m "feat(gw-codec): SSE accumulator for AccumulateUpstreamStream policy (ADR-G4-1 full impl)"
```

> Gemini SSE accumulator 接 codec S5 / 自身已部分被 S2 覆盖；本 task 只覆盖最常用两族。Gemini accumulator 的实现风格直接复刻 `accumulate_openai`，遇到生产路径需要时由 ADR-G4-1 落地阶段补一个分支即可。

---

### Task T6-E: usage:detailed Redis stream sink 真实现

**Files:**
- Modify: `crates/gw-ledger/src/log_sink.rs`（加 `RedisStreamSink`）
- Modify: `crates/gw-ledger/src/detailed_stream.rs`（接 verbosity gating + size cap + sink dispatch）
- Modify: `crates/gw-ledger/Cargo.toml`（加 `fred`）
- Test: `crates/gw-ledger/tests/redis_sink.rs`

> ADR-G6-4：detailed_log 写到 `usage:detailed` Redis stream，由 Go 异步消费持久化。本 task 不实现 DLQ（DLQ 是 C-δ 范围），但接通基本 XADD + size cap + drop on backpressure。

- [ ] **Step 1: 写失败测试（用 testcontainers，标 ignored）**

```rust
// crates/gw-ledger/tests/redis_sink.rs
use gw_ledger::detailed_stream::{
    redact_body, LogVerbosity, MAX_DETAILED_LOG_BYTES,
};
use gw_ledger::log_sink::{DetailedLogSink, RedisStreamSink};
use serde_json::json;

#[tokio::test]
#[ignore]
async fn writes_to_usage_detailed_stream() {
    let cli = testcontainers_modules::redis::Redis::default();
    let docker = testcontainers::clients::Cli::default();
    let node = docker.run(cli);
    let port = node.get_host_port_ipv4(6379);
    let url = format!("redis://127.0.0.1:{port}");
    let pool = build_pool(&url).await;
    let sink = RedisStreamSink::new(pool.clone(), "usage:detailed", 10_000);
    sink.write(json!({"req_id": "r1", "verbosity": "base", "model": "x"})).await;
    let len: u64 = use_fred_xlen(&pool, "usage:detailed").await;
    assert_eq!(len, 1);
}

#[tokio::test]
async fn dropped_when_payload_oversized() {
    use gw_ledger::detailed_stream::MAX_DETAILED_LOG_BYTES;
    let big = "x".repeat(MAX_DETAILED_LOG_BYTES + 1);
    let sink = MockSink::default();
    sink.write_with_size_check(json!({"big": big})).await;
    assert_eq!(sink.dropped_oversized.load(std::sync::atomic::Ordering::Acquire), 1);
}

// helpers
async fn build_pool(url: &str) -> fred::clients::RedisPool { /* ... */ unimplemented!() }
async fn use_fred_xlen(pool: &fred::clients::RedisPool, stream: &str) -> u64 {
    use fred::interfaces::StreamsInterface;
    pool.next().xlen(stream).await.unwrap()
}

#[derive(Default)]
struct MockSink {
    dropped_oversized: std::sync::atomic::AtomicU64,
}
impl MockSink {
    async fn write_with_size_check(&self, v: serde_json::Value) {
        let bytes = serde_json::to_vec(&v).unwrap();
        if bytes.len() > MAX_DETAILED_LOG_BYTES {
            self.dropped_oversized.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            metrics::counter!("gw_detailed_log_oversized_total").increment(1);
        }
    }
}
```

- [ ] **Step 2: 实现 RedisStreamSink**

```rust
// crates/gw-ledger/src/log_sink.rs (扩展 - 在文件末尾追加)
use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use fred::types::XCapKind;

pub struct RedisStreamSink {
    pool: RedisPool,
    stream: String,
    /// 最大 stream 长度（XADD MAXLEN ~ N 用于上限近似裁剪）
    max_len: u64,
}

impl RedisStreamSink {
    pub fn new(pool: RedisPool, stream: &str, max_len: u64) -> Self {
        Self { pool, stream: stream.to_string(), max_len }
    }
}

#[async_trait::async_trait]
impl DetailedLogSink for RedisStreamSink {
    async fn write(&self, payload: serde_json::Value) {
        let bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(_) => return,
        };
        if bytes.len() > crate::detailed_stream::MAX_DETAILED_LOG_BYTES {
            metrics::counter!("gw_detailed_log_oversized_total").increment(1);
            return;
        }
        let client = self.pool.next();
        let r: Result<String, _> = client.xadd(
            self.stream.as_str(),
            false,                                              // nomkstream=false
            Some((XCapKind::MaxLen, fred::types::XCapTrim::AlmostExact, self.max_len.into())),
            "*".into(),
            vec![("payload".into(), bytes.into())],
        ).await;
        if let Err(e) = r {
            // backpressure / Redis down → 丢弃 + 计数；DLQ 在 C-δ 落地
            tracing::warn!(error=%e, stream=%self.stream, "detailed log XADD failed; dropping");
            metrics::counter!("gw_detailed_log_drop_total", "reason" => "xadd_failed").increment(1);
        }
    }
}
```

`crates/gw-ledger/Cargo.toml`：

```toml
fred = { workspace = true }
metrics = { workspace = true }
```

- [ ] **Step 3: 跑测试 + 提交**

```bash
cargo test -p gw-ledger --test redis_sink
cargo test -p gw-ledger --test redis_sink -- --ignored
git add crates/gw-ledger
git commit -m "feat(gw-ledger): usage:detailed Redis stream sink + size cap (ADR-G6-4 full impl)"
```

---

### Task T6-F: 收尾——cargo feature gate / pricing_snapshot 读取 / codec convert 指标埋点

**Files:**
- Modify: `crates/gw-gateway/Cargo.toml`（features `tunnel`）
- Modify: `crates/gw-gateway/src/lib.rs`（feature 条件 use）
- Create: `crates/gw-capability/src/pricing.rs`（pricing snapshot 读取 stub）
- Modify: `crates/gw-codec/src/wire/any.rs` 或 conversion 入口（emit `gw_codec_convert_total`）
- Test: `crates/gw-gateway/tests/feature_gate.rs`（仅 metadata 校验）

> 三件小事打包一个收尾 task；本 task 必须放最后，因为依赖 Wave 2 / Wave 4 的产物。

- [ ] **Step 1: gw-gateway 加 tunnel feature**

`crates/gw-gateway/Cargo.toml`：

```toml
[features]
default = []
tunnel = ["dep:gw-tunnel-client"]

[dependencies]
gw-tunnel-client = { path = "../gw-tunnel-client", optional = true }
```

`crates/gw-gateway/src/lib.rs` 顶层：

```rust
#[cfg(feature = "tunnel")]
pub use gw_tunnel_client;
```

- [ ] **Step 2: 写 feature gate 验证测试**

`crates/gw-gateway/tests/feature_gate.rs`：

```rust
//! Verify `tunnel` feature is opt-in.
//!
//! 这个测试本身只在 default features 下编译；它通过 cargo metadata 校验
//! gw-gateway 的依赖列表中 gw-tunnel-client 是 optional。

#[test]
fn tunnel_feature_optional() {
    let manifest = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")
    ).unwrap();
    // 极简的 toml 字符串扫描，避免拉 toml crate 进 dev-deps
    assert!(manifest.contains("gw-tunnel-client"));
    assert!(manifest.contains("optional = true"));
    assert!(manifest.contains("[features]"));
}
```

- [ ] **Step 3: pricing snapshot stub**

`crates/gw-capability/src/pricing.rs`：

```rust
//! pricing_strategy_snapshot 读取（Go 端定期写；Rust 仅读）。
//! v1 仅提供 trait + Fake；真 backend 在 events:entity 扩展时由 gw-store 实现。

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct PricingSnapshot {
    pub base_input_micro_per_token: u64,
    pub base_output_micro_per_token: u64,
    pub last_updated_ms: u64,
}

#[async_trait]
pub trait PricingSnapshotReader: Send + Sync {
    async fn get(&self, model_id: &str, vendor_id: &str) -> Option<PricingSnapshot>;
}

#[derive(Default)]
pub struct InMemoryPricingReader {
    map: Mutex<HashMap<(String, String), PricingSnapshot>>,
}
impl InMemoryPricingReader {
    pub fn insert(&self, model: &str, vendor: &str, s: PricingSnapshot) {
        self.map.lock().insert((model.into(), vendor.into()), s);
    }
}
#[async_trait]
impl PricingSnapshotReader for InMemoryPricingReader {
    async fn get(&self, m: &str, v: &str) -> Option<PricingSnapshot> {
        self.map.lock().get(&(m.into(), v.into())).copied()
    }
}
```

测试：

```rust
// crates/gw-capability/tests/pricing.rs
use gw_capability::pricing::{InMemoryPricingReader, PricingSnapshot, PricingSnapshotReader};

#[tokio::test]
async fn in_memory_reader_round_trip() {
    let r = InMemoryPricingReader::default();
    let s = PricingSnapshot {
        base_input_micro_per_token: 30,
        base_output_micro_per_token: 150,
        last_updated_ms: 1,
    };
    r.insert("claude-3-opus", "anthropic", s);
    let got = r.get("claude-3-opus", "anthropic").await.unwrap();
    assert_eq!(got.base_output_micro_per_token, 150);
}
```

`gw-capability/src/lib.rs` 加 `pub mod pricing;`。

- [ ] **Step 4: codec convert 指标埋点**

打开 codec 的转换入口（v1 实际入口可能是 `WireAnyRequest::convert_to::<B>` / `passthrough.rs`），在每次成功 / 失败转换后 emit：

```rust
metrics::counter!(
    "gw_codec_convert_total",
    "direction" => "request",
    "source" => "openai_chat",
    "target" => "anthropic_messages",
    "status" => "ok",
).increment(1);
```

如 wire matrix 还是空骨架，先在 `passthrough.rs` 入口埋一行 + 失败路径埋一行；等 S1 落地后由实际转换矩阵覆盖。

- [ ] **Step 5: 跑测试 + cargo build --no-default-features 验证**

```bash
cargo test -p gw-capability --test pricing
cargo test -p gw-gateway --test feature_gate
cargo build -p gw-gateway --no-default-features
cargo build -p gw-gateway --features tunnel
git add Cargo.toml crates/gw-gateway crates/gw-capability crates/gw-codec
git commit -m "feat(gw-gateway,gw-capability,gw-codec): tunnel feature gate + pricing snapshot reader + codec convert metric (G-ε.7 / G-β.10 / G-δ.2)"
```

---

## Final verification（手动）

落地完成后跑完整 sanity：

```bash
cargo test --workspace
cargo test --workspace -- --ignored          # 跑 testcontainers 集成
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
bash scripts/check-invariants.sh
cargo build --release -p gw-gateway
```

并按 spec § 8.3 检查所有指标在 `/metrics` 上出现 cardinality < 200。

---

## Out-of-scope（明确归 spec / Aether 设计已划界的事项）

下列条目**不在本 plan**，因为 spec 已明确归 Go 或属于其他 spec 的范围；不是被本 plan 推迟：

- 计费内核（公式 / 模板 / 历史重计费 / 维度 collector） —— **永久归 Go**（CLAUDE.md 不变量）
- provider_ops（SaaS 余额查询 / 签到 / 周期巡检） —— **归 Go**（inventory A9）
- 订阅 / 支付 / license / 邮件 / LDAP / 公告 —— **归 Go**（spec § 0 不在范围）
- KEK 轮换"管理面入口 / 触发 API" —— **管理面 spec 范围**；但 KekCache dual-read 状态机本身已由 T1-E 实现（spec ADR-G1-7）
- A-T0~T6 / C-α~ε / K-A~D / R-AB~G / RL-* / S1~S5 / Task-Worker 各 spec 的**自身**内容 —— **各自 spec 落地**；本 plan 通过 trait/接口边界与之对接






