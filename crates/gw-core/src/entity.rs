use crate::ids::*;
use crate::protocol::ProtocolKind;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::OffsetDateTime;
use url::Url;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Disabled,
    Expired,
    Depleted,
    RateLimited,
    Rotated,
    Deleted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Active,
    Banned,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyStatus {
    Active,
    Disabled,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserEntry {
    pub user_id: UserId,
    pub parent_id: Option<UserId>,
    pub tier: String,
    pub status: UserStatus,
    #[serde(default)]
    pub allowed_models: Vec<ModelName>,
    #[serde(default)]
    pub allowed_channels: Vec<KeyId>,
    #[serde(default)]
    pub allowed_groups: Vec<Group>,
    #[serde(default)]
    pub rpm_limit: u32,
    #[serde(default)]
    pub tpm_limit: u32,
    #[serde(default = "default_concurrent")]
    pub max_concurrent_streams: u32,
    #[serde(default)]
    pub daily_quota_tokens: u64,
    #[serde(default)]
    pub user_group_ids: Vec<UserGroupId>,
    #[serde(default = "default_markup")]
    pub markup: f32,
    #[serde(default)]
    pub debug_enabled: bool,
}
fn default_concurrent() -> u32 {
    100
}
fn default_markup() -> f32 {
    1.0
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserGroup {
    pub id: UserGroupId,
    pub display_name: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub allowed_models: Vec<ModelName>,
    #[serde(default)]
    pub allowed_channels: Vec<KeyId>,
    #[serde(default)]
    pub model_group_binding: Option<Group>,
    #[serde(default)]
    pub rpm_limit: u32,
    #[serde(default)]
    pub tpm_limit: u32,
    #[serde(default)]
    pub max_concurrent_streams: u32,
    #[serde(default = "default_markup")]
    pub markup: f32,
}

#[derive(Clone, Deserialize)]
pub struct ApiKeyEntry {
    pub key_id: ApiKeyId,
    pub user_id: UserId,
    pub status: ApiKeyStatus,
    pub hash: Vec<u8>,
    #[serde(default)]
    pub allowed_models: Vec<ModelName>,
    #[serde(default)]
    pub allowed_channels: Vec<KeyId>,
    #[serde(default)]
    pub rpm_limit: u32,
    #[serde(default)]
    pub tpm_limit: u32,
    #[serde(default = "default_markup")]
    pub markup: f32,
    #[serde(default)]
    pub ip_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub expires_at: Option<OffsetDateTime>,
    /// HMAC signature requirement (T1-C).
    #[serde(default)]
    pub signature_required: bool,
    /// HMAC algorithm choice (T1-C).
    #[serde(default)]
    pub signature_algo: SignatureAlgo,
    /// Capabilities the ApiKey is forced to require regardless of header (T4-B).
    #[serde(default)]
    pub force_capabilities: smallvec::SmallVec<[String; 4]>,
    /// If true, the matching mode for capability resolution is Exclusive.
    #[serde(default)]
    pub match_mode_exclusive: bool,
}

impl std::fmt::Debug for ApiKeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyEntry")
            .field("key_id", &self.key_id)
            .field("user_id", &self.user_id)
            .field("status", &self.status)
            .field("hash", &format_args!("[REDACTED; {} bytes]", self.hash.len()))
            .field("allowed_models", &self.allowed_models)
            .field("allowed_channels", &self.allowed_channels)
            .field("rpm_limit", &self.rpm_limit)
            .field("tpm_limit", &self.tpm_limit)
            .field("markup", &self.markup)
            .field("ip_allowlist", &self.ip_allowlist)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Vendor {
    pub id: VendorId,
    pub display_name: String,
    pub base_urls: Vec<Url>,
    pub auth_template: String,
    pub timeout_overrides: Option<TimeoutSpec>,
    #[serde(default)]
    pub default_proxy: Option<Url>,
    #[serde(default)]
    pub contact: Option<String>,
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
    /// Capabilities this vendor key advertises regardless of model defaults
    /// (consumed by `CapabilityResolver` — T3-C, ADR-G13-1). Stored as
    /// `Vec<String>` (parsed against `gw_capability::types::Capability` at
    /// resolve time) to avoid a supporting → business layer back-edge.
    pub forced_capabilities: Vec<String>,
    /// When true, the key only matches if every required capability is
    /// present (strict match). When false, the resolver may use a looser
    /// containment check (T3-C / ADR-G13-1).
    pub exclusive_capability_match: bool,
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
    CacheAffinity,
    FixedOrder,
    RoundRobin,
    LoadBalance,
    CostFirst,
    LatencyFirst,
    WeightedRr,
    Sticky,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelGroup {
    pub group: Group,
    pub strategy: StrategyKind,
    #[serde(default)]
    pub cross_vendor_fallback: bool,
    pub max_tokens_cap: Option<u32>,
    #[serde(default)]
    pub default_params: DefaultParams,
    #[serde(default)]
    pub forced_params: ForcedParams,
    pub vendor_key_ids: Vec<KeyId>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DefaultParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ForcedParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
}

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
pub struct JwksEntry {
    pub kid: Kid,
    pub pem: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IpBlockEntry {
    pub cidr: String,
    pub scope: IpBlockScope,
    pub reason: IpBlockReason,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpBlockScope {
    Global,
    User(UserId),
    ApiKey(ApiKeyId),
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpBlockReason {
    Manual,
    AbnormalTraffic,
    Compliance,
}

/// AuthConfig — JWT/blacklist policy. Loaded from `entity:auth_config`.
/// Used by gw-auth::jwt JwtValidator and JwtBlacklist (T1-A / T1-B).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct AuthConfig {
    pub expected_iss: smallvec::SmallVec<[String; 2]>,
    pub expected_aud: smallvec::SmallVec<[String; 2]>,
    pub leeway_secs: u32,
    pub require_nbf: bool,
    pub blacklist_fail_mode: BlacklistFailMode,
}

/// JWT-blacklist behavior when Redis is unreachable.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlacklistFailMode {
    /// Treat as revoked when backend down (most strict; default).
    #[default]
    FailClosed,
    /// Treat as not revoked when backend down (availability-first).
    FailOpen,
}

/// Signature algorithm identifier for HMAC request-signing (T1-C).
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgo {
    #[default]
    HmacSha256,
    HmacSha512,
}

/// Management token for /admin/* paths (T1-D / T6-A).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagementToken {
    pub token_id: String,
    pub secret_hash: Vec<u8>,
    pub scopes: smallvec::SmallVec<[String; 4]>,
    pub expires_at_unix_s: Option<u64>,
}

/// Per-model entity (T3-C, ADR-G13-1).
///
/// Carries the capability advertisement consumed by `CapabilityResolver`.
/// Capability strings are parsed against `gw_capability::types::Capability`
/// at resolve time — keeping the field as `Vec<String>` avoids a
/// supporting → business layer back-edge (`gw-core` cannot depend on
/// `gw-capability`; see `scripts/check-invariants.sh` §crate_layering).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub name: ModelName,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Model alias → upstream model mapping (T4-A).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelMapping {
    pub user_alias: String,
    pub protocol: crate::protocol::ProtocolKind,
    pub default_upstream_model: String,
    pub vendor_key_overrides: std::collections::BTreeMap<String, String>,
    pub availability: ModelAvailability,
    pub pricing_strategy_ref: Option<String>,
}

/// Per-model availability gate (T4-A / G-β.4).
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    #[default]
    Enabled,
    Disabled,
    ShadowOnly,
}
