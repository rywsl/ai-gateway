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

#[derive(Clone, Debug, Deserialize)]
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
