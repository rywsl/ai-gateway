use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONTROL_PLANE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayCapabilitySnapshot {
    pub schema_version: u32,
    pub instance_id: String,
    pub gateway_version: String,
    pub generated_at_ms: i64,
    pub started_at_ms: i64,
    pub protocols: Vec<String>,
    pub api_shapes: Vec<String>,
    pub api_shape_capabilities: Vec<ApiShapeCapability>,
    pub cli_variants: Vec<String>,
    pub routing_strategies: Vec<String>,
    pub features: BTreeMap<String, bool>,
    pub impersonate_identities: Vec<String>,
    pub commands: Vec<GatewayCommandCapability>,
    pub config_namespaces: Vec<String>,
    pub readiness: GatewayReadiness,
    pub kiro_cli: KiroCliReadiness,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiShapeCapability {
    pub api_shape: String,
    pub readiness: CapabilityReadiness,
    pub production_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityReadiness {
    Ready,
    Partial,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayCommandCapability {
    pub action: GatewayCommandAction,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRuntimeSnapshot {
    pub schema_version: u32,
    pub instance_id: String,
    pub generated_at_ms: i64,
    pub ts_ms: i64,
    pub uptime_ms: u64,
    pub active_requests: u64,
    pub inflight_tokens: u64,
    #[serde(default)]
    pub provider_health: Vec<ProviderHealthSummary>,
    #[serde(default)]
    pub key_health: Vec<KeyHealthSummary>,
    pub command_consumer: CommandConsumerState,
    #[serde(default)]
    pub tunnel: Vec<serde_json::Value>,
    #[serde(default)]
    pub oauth_rotation: Vec<serde_json::Value>,
    #[serde(default)]
    pub impersonate_active: serde_json::Value,
    pub inflight_total: u64,
    #[serde(default)]
    pub stream_lag: Vec<StreamLagSnapshot>,
    pub config_sample: RuntimeConfigSample,
    #[serde(default)]
    pub readiness: GatewayReadiness,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHealthSummary {
    pub provider: String,
    pub healthy: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyHealthSummary {
    pub key_id: String,
    pub format: String,
    pub available: bool,
    pub circuit: String,
    pub in_cooldown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<u64>,
    pub consecutive_failures: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandConsumerState {
    pub stream: String,
    pub group: String,
    pub consumer: String,
    pub last_seen_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamLagSnapshot {
    pub stream: String,
    pub group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lag: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfigSample {
    pub runtime_push_interval_ms: u64,
    pub snapshot_ttl_ms: u64,
    pub command_block_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayReadiness {
    #[serde(default)]
    pub capability_snapshot: bool,
    #[serde(default)]
    pub runtime_snapshot: bool,
    #[serde(default)]
    pub command_consumer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command_stream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command_ack_ms: Option<i64>,
    #[serde(default)]
    pub kiro_cli: KiroCliReadiness,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KiroCliReadiness {
    #[serde(default)]
    pub advertised: bool,
    #[serde(default)]
    pub detector_ready: bool,
    #[serde(default)]
    pub production_enabled: bool,
    #[serde(default)]
    pub usage_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GatewayCommandDeliveryEntity {
    pub command_id: String,
    pub delivery_id: String,
    pub action: GatewayCommandAction,
    pub target_scope: GatewayCommandTargetScope,
    pub target_instance_id: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub delivery_mode: CommandDeliveryMode,
    pub issued_at_ms: i64,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default, alias = "requested_by")]
    pub issued_by: Option<String>,
}

impl GatewayCommandDeliveryEntity {
    pub fn validate_stream_entry(
        &self,
        entry: &GatewayCommandStreamEntry,
    ) -> Result<(), GatewayCommandMismatch> {
        if self.command_id != entry.command_id {
            return Err(GatewayCommandMismatch::CommandId);
        }
        if self.delivery_id != entry.delivery_id {
            return Err(GatewayCommandMismatch::DeliveryId);
        }
        if self.target_instance_id != entry.target_instance_id {
            return Err(GatewayCommandMismatch::TargetInstanceId);
        }
        if self.delivery_mode != entry.delivery_mode {
            return Err(GatewayCommandMismatch::DeliveryMode);
        }
        if self.issued_at_ms != entry.issued_at_ms {
            return Err(GatewayCommandMismatch::IssuedAtMs);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayCommandStreamEntry {
    pub command_id: String,
    pub delivery_id: String,
    pub target_instance_id: String,
    pub delivery_mode: CommandDeliveryMode,
    pub issued_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayCommandAction {
    ReloadConfig,
    CooldownKey,
    ClearCooldown,
    ResetCost,
    ClearCache,
    SetLogLevel,
    RefreshOauth,
    Drain,
    Undrain,
    RestartInstance,
    RotateKek,
}

impl GatewayCommandAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReloadConfig => "reload-config",
            Self::CooldownKey => "cooldown-key",
            Self::ClearCooldown => "clear-cooldown",
            Self::ResetCost => "reset-cost",
            Self::ClearCache => "clear-cache",
            Self::SetLogLevel => "set-log-level",
            Self::RefreshOauth => "refresh-oauth",
            Self::Drain => "drain",
            Self::Undrain => "undrain",
            Self::RestartInstance => "restart-instance",
            Self::RotateKek => "rotate-kek",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GatewayCommandTargetScope {
    All,
    Instance,
}

impl GatewayCommandTargetScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Instance => "instance",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandDeliveryMode {
    Durable,
    Ephemeral,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayCommandAck {
    pub command_id: String,
    pub delivery_id: String,
    pub instance_id: String,
    pub status: GatewayCommandStatus,
    pub message: String,
    pub finished_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GatewayCommandStatus {
    Ok,
    Failed,
    Skipped,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GatewayCommandMismatch {
    #[error("command_id_mismatch")]
    CommandId,
    #[error("delivery_id_mismatch")]
    DeliveryId,
    #[error("target_instance_id_mismatch")]
    TargetInstanceId,
    #[error("delivery_mode_mismatch")]
    DeliveryMode,
    #[error("issued_at_ms_mismatch")]
    IssuedAtMs,
}

pub fn default_gateway_capability(
    instance_id: impl Into<String>,
    generated_at_ms: i64,
) -> GatewayCapabilitySnapshot {
    gateway_capability(
        instance_id,
        env!("CARGO_PKG_VERSION"),
        generated_at_ms,
        generated_at_ms,
    )
}

pub fn gateway_capability(
    instance_id: impl Into<String>,
    gateway_version: impl Into<String>,
    generated_at_ms: i64,
    started_at_ms: i64,
) -> GatewayCapabilitySnapshot {
    let api_shape_capabilities = default_api_shape_capabilities();
    let api_shapes = api_shape_capabilities
        .iter()
        .map(|capability| capability.api_shape.clone())
        .collect();
    let kiro_cli = default_kiro_cli_readiness();
    GatewayCapabilitySnapshot {
        schema_version: CONTROL_PLANE_SCHEMA_VERSION,
        instance_id: instance_id.into(),
        gateway_version: gateway_version.into(),
        generated_at_ms,
        started_at_ms,
        protocols: vec![
            "openai".to_string(),
            "anthropic".to_string(),
            "gemini".to_string(),
            "kiro".to_string(),
        ],
        api_shapes,
        api_shape_capabilities,
        cli_variants: vec![
            "claude:cli".to_string(),
            "openai:cli".to_string(),
            "openai:compact".to_string(),
            "gemini:cli".to_string(),
            "kiro:cli".to_string(),
        ],
        routing_strategies: vec![
            "FixedOrder".to_string(),
            "RoundRobin".to_string(),
            "LoadBalance".to_string(),
            "WeightedRr".to_string(),
            "CostFirst".to_string(),
            "LatencyFirst".to_string(),
            "CacheAffinity".to_string(),
            "Sticky".to_string(),
        ],
        features: default_gateway_features(),
        impersonate_identities: vec![
            "claude_code_chrome131".to_string(),
            "openai_codex".to_string(),
            "gemini_cli".to_string(),
        ],
        commands: default_gateway_command_capabilities(),
        config_namespaces: vec![
            "config_routing".to_string(),
            "config_ratelimit".to_string(),
            "config_auth".to_string(),
            "config_billing".to_string(),
            "config_protection".to_string(),
            "config_control_plane".to_string(),
        ],
        readiness: GatewayReadiness {
            capability_snapshot: true,
            kiro_cli: kiro_cli.clone(),
            ..Default::default()
        },
        kiro_cli,
    }
}

pub fn default_api_shape_capabilities() -> Vec<ApiShapeCapability> {
    vec![
        ready_api_shape("openai:chat"),
        ready_api_shape("openai:responses"),
        ready_api_shape("anthropic:messages"),
        ready_api_shape("gemini:generate"),
        ready_api_shape("kiro:eventstream"),
        partial_api_shape("kiro:cli", "requires_real_cli_sample"),
        ready_api_shape("claude:cli"),
        ready_api_shape("openai:cli"),
        ready_api_shape("openai:compact"),
        ready_api_shape("gemini:cli"),
    ]
}

pub fn default_gateway_command_capabilities() -> Vec<GatewayCommandCapability> {
    use GatewayCommandAction::*;
    vec![
        enabled_command(ReloadConfig),
        enabled_command(CooldownKey),
        enabled_command(ClearCooldown),
        enabled_command(ResetCost),
        enabled_command(ClearCache),
        disabled_command(SetLogLevel, "runtime_log_reload_hook_missing"),
        disabled_command(RefreshOauth, "oauth_force_refresh_hook_missing"),
        disabled_command(Drain, "graceful_drain_hook_missing"),
        disabled_command(Undrain, "graceful_drain_hook_missing"),
        disabled_command(RestartInstance, "graceful_restart_hook_missing"),
        disabled_command(RotateKek, "kek_rotation_command_hook_missing"),
    ]
}

pub fn command_ack_hash_key(instance_id: &str) -> String {
    format!("gateway:instance:{instance_id}:cmd_ack")
}

pub fn command_delivery_entity_key(delivery_id: &str) -> String {
    format!("entity:gateway_command_delivery:{delivery_id}")
}

pub fn gateway_capability_key(instance_id: &str) -> String {
    format!("gateway:instance:{instance_id}:capability")
}

pub fn gateway_runtime_key(instance_id: &str) -> String {
    format!("gateway:instance:{instance_id}:runtime")
}

fn ready_api_shape(api_shape: &str) -> ApiShapeCapability {
    ApiShapeCapability {
        api_shape: api_shape.to_string(),
        readiness: CapabilityReadiness::Ready,
        production_ready: true,
        reason: None,
    }
}

fn partial_api_shape(api_shape: &str, reason: &str) -> ApiShapeCapability {
    ApiShapeCapability {
        api_shape: api_shape.to_string(),
        readiness: CapabilityReadiness::Partial,
        production_ready: false,
        reason: Some(reason.to_string()),
    }
}

pub fn default_kiro_cli_readiness() -> KiroCliReadiness {
    KiroCliReadiness {
        advertised: true,
        detector_ready: false,
        production_enabled: false,
        usage_ready: false,
        reason: Some("requires_real_cli_sample".to_string()),
    }
}

fn default_gateway_features() -> BTreeMap<String, bool> {
    BTreeMap::from([
        ("tunnel".to_string(), false),
        ("egress_impersonate".to_string(), true),
        ("credit_lease".to_string(), true),
        ("detailed_log".to_string(), true),
        ("task_worker".to_string(), false),
    ])
}

fn enabled_command(action: GatewayCommandAction) -> GatewayCommandCapability {
    GatewayCommandCapability {
        action,
        enabled: true,
        reason: None,
    }
}

fn disabled_command(action: GatewayCommandAction, reason: &str) -> GatewayCommandCapability {
    GatewayCommandCapability {
        action,
        enabled: false,
        reason: Some(reason.to_string()),
    }
}
