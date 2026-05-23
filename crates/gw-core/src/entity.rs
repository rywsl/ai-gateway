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
    /// Tunnel routing preference (ADR-T-4). `None` => follow the
    /// global default policy (which is "Direct" until a node matches).
    /// Always `None` in seed data; existing rows do not require backfill
    /// per the spec's "non-breaking on rollout" guarantee.
    pub tunnel_preference: Option<TunnelPreference>,
    /// G3 / go-onboarding-gaps T0: OAuth credentials for this vendor key.
    /// `None` when the key uses a static API key (the common case).
    /// Populated by `load_vendor_key` after decrypting the `oauth` sub-object
    /// from the JSONB payload. `None` for all existing rows until backfilled.
    pub oauth: Option<OAuthCredentials>,
}

impl std::fmt::Debug for VendorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VendorKey")
            .field("key_id", &self.key_id)
            .field("vendor_id", &self.vendor_id)
            .field("api_key", &"[REDACTED]")
            .field("status", &self.status)
            .field(
                "oauth",
                &self.oauth.as_ref().map(|_| "<OAuthCredentials: REDACTED>"),
            )
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
    /// HS256 shared-secret bytes for the client-bearer JWT path (G6, T1-E).
    /// Absent means the HS256 path is disabled.
    #[serde(default)]
    pub static_secret_hs256: Option<Vec<u8>>,
    /// Static RS256 public-key PEM for the client-bearer JWT path (G6, T1-E).
    /// Acts as fallback when the JWKS resolver returns `None` or the token has
    /// no `kid` header. Absent means the RS256 static path is disabled.
    #[serde(default)]
    pub static_pem_rs256: Option<Vec<u8>>,
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

/// OAuth credentials for a vendor key — decrypted form held in-process.
///
/// The encrypted wire shape (`OAuthCredentialsRow`) lives in
/// `gw-store::loader` so the back-edge `gw-core → gw-store` is avoided.
/// Populated by `load_vendor_key` when the JSONB payload contains an `oauth`
/// sub-object (G3 / go-onboarding-gaps T0).
#[derive(Clone)]
pub struct OAuthCredentials {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    pub access_expires_at: OffsetDateTime,
    pub token_endpoint: String,
    pub scope: Vec<String>,
}

impl std::fmt::Debug for OAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredentials")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("access_expires_at", &self.access_expires_at)
            .field("token_endpoint", &self.token_endpoint)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Per-vendor-key tunnel routing preference (ADR-T-4).
///
/// Sits in `gw-core` rather than `gw-tunnel-client` because `VendorKey`
/// (also in `gw-core`) embeds it and `gw-core` is supporting-layer — it
/// cannot depend on `gw-tunnel-client` (business-layer). The tunnel
/// resolver re-exports a thin alias via `pub use gw_core::entity::TunnelPreference`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TunnelPreference {
    /// Optional list of preferred node ids; empty = "any allowed node".
    #[serde(default)]
    pub preferred_node_ids: smallvec::SmallVec<[String; 4]>,
    /// When true, `pick_peer` MUST tunnel — no available node => hard error
    /// `GW_TUNNEL_NO_NODE` (no silent Direct fallback). See spec ADR-T-4.
    #[serde(default)]
    pub require_tunnel: bool,
    /// Sort filtered nodes by ascending `latency_ms` before picking the first.
    #[serde(default)]
    pub prefer_low_latency: bool,
}

/// Helpers for entity types that carry encrypted blobs without a back-edge
/// to `gw-store` (which owns the AES-256-GCM `EncryptedKey` type).
/// The wire shape is intentionally isomorphic to `gw_store::encrypt::EncryptedKey`
/// so JSONB payloads round-trip between the two crates without re-encoding.
pub mod entity_helpers {
    /// AES-256-GCM ciphertext blob stored in a JSONB payload column.
    ///
    /// Field layout mirrors `gw_store::encrypt::EncryptedKey` exactly
    /// (`ciphertext`, `nonce`, `auth_tag`, `kek_kid`). `gw-core` cannot
    /// back-edge to `gw-store`, so this parallel type is defined here;
    /// `gw-store`'s loader converts between them at decode time.
    ///
    /// `Vec<u8>` fields are serialised by `serde_json` as JSON arrays of
    /// integers (the default; no `serde_with::base64`). This matches the
    /// on-disk JSONB shape of `gw_store::encrypt::EncryptedKey` exactly.
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct EncryptedBlob {
        pub ciphertext: Vec<u8>,
        pub nonce: Vec<u8>,
        pub auth_tag: Vec<u8>,
        pub kek_kid: String,
    }
}

/// Proxy-node entity (ADR-T-3). A WS-reachable remote relay that
/// `TunnelStack` can route upstream traffic through. Cached by
/// `EntityStore::proxy_node` (1800s TTL, matching vendor_key).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProxyNode {
    pub id: String,
    /// `ws[s]://host:port/path` — base WebSocket URL the gateway dials.
    pub ws_url: url::Url,
    #[serde(default = "proxy_node_default_enabled")]
    pub enabled: bool,
    /// Most recent latency observation from the periodic health prober.
    #[serde(default)]
    pub latency_ms: u32,
    #[serde(default)]
    pub allowed_provider_ids: Vec<String>,
    /// Empty = unrestricted across keys for the allowed providers.
    #[serde(default)]
    pub allowed_vendor_key_ids: Vec<String>,
    /// AES-256-GCM ciphertext of the node-auth bearer token (encrypted
    /// under the active KEK). `None` means the node accepts unauthenticated
    /// CONNECT frames.
    #[serde(default)]
    pub auth_token_enc: Option<entity_helpers::EncryptedBlob>,
}

fn proxy_node_default_enabled() -> bool {
    true
}

// --- Management-token persisted entity (B2 / ADR-B2-1) ---
//
// `ManagementToken` (above) was the v1.0 in-flight shape for `/admin/*` and is
// retained for backwards compatibility with that surface. The Go-onboarding
// blocker B2 introduces a parallel, narrower entity persisted under
// `management_tokens` (migration 0007) and consumed by request_filter on the
// management-path branch (T3-A/B/C). Scope is a single enum value rather than
// a free-form string set so the verification helper can return a typed answer.

/// Scope granted by a `ManagementTokenEntry`. Ordered loosely by privilege —
/// `AdminFull` ⊇ `AdminRead` ⊇ `UserSelf`. Verification call sites compare
/// the token's scope against the route-required scope via `covers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MgmtScope {
    UserSelf,
    AdminRead,
    AdminFull,
}

impl MgmtScope {
    /// `true` iff `self` is authorised to act under the `required` scope.
    pub fn covers(self, required: MgmtScope) -> bool {
        use MgmtScope::*;
        matches!(
            (self, required),
            (AdminFull, _) | (AdminRead, AdminRead | UserSelf) | (UserSelf, UserSelf)
        )
    }
}

/// Persisted management-token entity (B2). Loaded from the `management_tokens`
/// table by `gw-store` (T1-D) and cached by `EntityStore::management_token`.
/// Verification: SHA-256 of the presented bearer is compared constant-time
/// against `hash` after the entity is fetched by `token_id`.
///
/// `hash` mirrors `ApiKeyEntry.hash` — plain `Vec<u8>`, serialised by
/// `serde_json` as a JSON integer array (no `serde_with::base64` in tree).
/// `Debug` is implemented manually below to redact `hash` (SHA-256 of the
/// bearer secret) so `tracing::debug!(?entry)` cannot leak it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ManagementTokenEntry {
    pub token_id: String,
    pub user_id: crate::ids::UserId,
    pub scope: MgmtScope,
    pub hash: Vec<u8>,
    #[serde(default)]
    pub expires_at: Option<time::OffsetDateTime>,
    #[serde(default = "default_active_status")]
    pub status: KeyStatus,
    pub display_name: String,
}

impl std::fmt::Debug for ManagementTokenEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementTokenEntry")
            .field("token_id", &self.token_id)
            .field("user_id", &self.user_id)
            .field("scope", &self.scope)
            .field("hash", &format_args!("[REDACTED; {} bytes]", self.hash.len()))
            .field("expires_at", &self.expires_at)
            .field("status", &self.status)
            .field("display_name", &self.display_name)
            .finish()
    }
}

fn default_active_status() -> KeyStatus {
    KeyStatus::Active
}

#[cfg(test)]
mod mgmt_scope_tests {
    use super::MgmtScope::*;

    #[test]
    fn admin_full_covers_all() {
        assert!(AdminFull.covers(AdminFull));
        assert!(AdminFull.covers(AdminRead));
        assert!(AdminFull.covers(UserSelf));
    }

    #[test]
    fn admin_read_covers_admin_read_and_user_self() {
        assert!(AdminRead.covers(AdminRead));
        assert!(AdminRead.covers(UserSelf));
        assert!(!AdminRead.covers(AdminFull));
    }

    #[test]
    fn user_self_covers_only_self() {
        assert!(UserSelf.covers(UserSelf));
        assert!(!UserSelf.covers(AdminRead));
        assert!(!UserSelf.covers(AdminFull));
    }
}

#[cfg(test)]
mod tests {
    use super::entity_helpers::EncryptedBlob;

    /// Verifies that `EncryptedBlob` round-trips through `serde_json`. `Vec<u8>`
    /// fields serialise as JSON integer arrays (serde_json default) and
    /// deserialise back to the same bytes.
    #[test]
    fn encrypted_blob_serde_roundtrip() {
        let original = EncryptedBlob {
            ciphertext: vec![0xde, 0xad, 0xbe, 0xef],
            nonce: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            auth_tag: vec![0xaa; 16],
            kek_kid: "test-kid-1".to_string(),
        };
        let serialised = serde_json::to_string(&original).expect("serialise");
        let restored: EncryptedBlob =
            serde_json::from_str(&serialised).expect("deserialise");
        assert_eq!(restored.ciphertext, original.ciphertext);
        assert_eq!(restored.nonce, original.nonce);
        assert_eq!(restored.auth_tag, original.auth_tag);
        assert_eq!(restored.kek_kid, original.kek_kid);
    }
}
