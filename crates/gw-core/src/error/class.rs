//! ErrorClass — 客户端 / 编排层错误语义枚举。
//! 与 K-A 的 CooldownReason 故意不强耦合：ErrorClass 面向编排 / 客户端，
//! CooldownReason 面向 KeyHealthStore 内部状态。
//! 一份 ClassifierOutcome 可同时给出两者，由 classifier 负责映射。

use crate::ratelimit::RateLimitKind;

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    #[default]
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

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Retryability {
    /// 不重试
    #[default]
    No,
    /// 同 key 可重试（瞬时）
    SameKey,
    /// 跨 key 可重试（账号问题 / RPM 满）
    CrossKey,
    /// 跨 vendor 可重试
    CrossVendor,
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

#[derive(Debug, Clone)]
pub struct CooldownDirective {
    /// 与 K-A CooldownReason u16 编码同步。
    pub reason_code: u16,
    pub ttl_secs: u32,
    pub reset_consecutive: bool,
}

#[derive(Debug, Clone)]
pub struct RateLimitHint {
    pub kind: RateLimitKind,
    pub retry_after_secs: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSignal {
    pub kind: OAuthSignalKind,
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
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
    /// 客户端实际呈递的 API 形态（按 path/header 探测）
    pub api_format: crate::protocol::ProtocolKind,
    /// 上游路由 / 选 key 时使用的协议（可与 api_format 不同，如 stream-policy 桥接情形）
    pub protocol: crate::protocol::ProtocolKind,
    /// Whether the vendor key has OAuth credentials attached.
    /// When `true` and upstream returns 401, the classifier emits
    /// `OAuthSignalKind::RefreshFailed` instead of generic `AccountBlocked`.
    pub has_oauth: bool,
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
