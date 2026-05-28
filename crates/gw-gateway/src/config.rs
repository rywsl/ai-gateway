//! Configuration loading via figment.
//!
//! Sources merged low → high precedence:
//!   1. `config/default.toml`
//!   2. `config/local.toml` (optional, gitignored)
//!   3. Environment variables prefixed with `GW_`, nesting separator `__`
//!      (e.g. `GW_REDIS__URL` populates `redis.url`).
//!
//! Secret material (KEK hex, DB connection strings with embedded passwords)
//! should arrive via env vars and is kept inside `secrecy::SecretString` so
//! accidental `Debug` output redacts.

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use gw_routing::pipeline::multi_dim::DimWeights;
use gw_routing::reservation_gate::ReservationGate;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerCfg,
    #[serde(default)]
    pub admin: AdminCfg,
    pub redis: RedisCfg,
    pub pgsql: PgCfg,
    pub ratelimit: RateLimitCfg,
    pub ledger: LedgerCfg,
    pub kms: KmsCfg,
    #[serde(default)]
    pub debug: DebugCfg,
    #[serde(default)]
    pub security: SecurityCfg,
    #[serde(default)]
    pub signature: SignatureCfg,
    #[serde(default)]
    pub loader: LoaderCfg,
    #[serde(default)]
    pub routing: RoutingCfg,
    #[serde(default)]
    pub tunnel: TunnelCfg,
    #[serde(default)]
    pub egress: EgressCfg,
    #[serde(default)]
    pub detailed_log: DetailedLogCfg,
    #[serde(default)]
    pub billing: BillingCfg,
    #[serde(default)]
    pub auth: AuthCfg,
    #[serde(default)]
    pub ip_filter: IpFilterCfg,
    #[serde(default)]
    pub ip_ratelimit: IpRateLimitCfg,
    #[serde(default)]
    pub adaptive_rpm: AdaptiveRpmCfg,
    #[serde(default)]
    pub task: TaskCfg,
    #[serde(default)]
    pub error: ErrorCfg,
    #[serde(default)]
    pub body_rules: BodyRulesCfg,
    #[serde(default)]
    pub health: HealthCfg,
}

#[derive(Debug, Deserialize)]
pub struct ServerCfg {
    pub business_port: u16,
    pub metrics_port: u16,
    pub health_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminCfg {
    #[serde(default = "default_admin_upstream_url")]
    pub upstream_url: String,
}

fn default_admin_upstream_url() -> String {
    "http://localhost:3000".to_string()
}

impl Default for AdminCfg {
    fn default() -> Self {
        Self {
            upstream_url: default_admin_upstream_url(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RedisCfg {
    /// Connection URL including credentials. Wrapped in `SecretString` so a
    /// stray `{:?}` print of the surrounding `Config` keeps the password
    /// redacted.
    pub url: SecretString,
    pub pool_size: usize,
}

#[derive(Debug, Deserialize)]
pub struct PgCfg {
    /// Connection URL including credentials. Wrapped in `SecretString` for the
    /// same reason as `RedisCfg::url`.
    pub url: SecretString,
    pub max_connections: u32,
}

#[derive(Debug, Deserialize)]
pub struct RateLimitCfg {
    pub global_rpm: u32,
}

#[derive(Debug, Deserialize)]
pub struct LedgerCfg {
    pub channel_capacity: usize,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct KmsCfg {
    /// Hex-encoded 32-byte KEK supplied via `GW_KMS__KEK_HEX`. Wrapped in
    /// `SecretString` so a stray `{:?}` print stays redacted.
    ///
    /// Note: figment's env provider auto-coerces all-digit values to integers.
    /// A real 32-byte random KEK encoded as hex will almost always contain
    /// `a-f` characters so this is not an issue in practice; the CI test
    /// fixture and the dev-mode docs both use realistic hex strings.
    #[serde(default)]
    pub kek_hex: Option<SecretString>,
    pub key_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedLogCfg {
    #[serde(default = "default_detailed_enabled")]
    pub enabled: bool,
    #[serde(default = "default_detailed_stream")]
    pub stream: String,
    #[serde(default = "default_detailed_maxlen")]
    pub maxlen: u64,
    #[serde(default)]
    pub compression: DetailedCompressionCfg,
    #[serde(default)]
    pub channel: DetailedChannelCfg,
    #[serde(default)]
    pub flusher: DetailedFlusherCfg,
    #[serde(default)]
    pub limits: DetailedLimitsCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedCompressionCfg {
    #[serde(default = "default_small_threshold")]
    pub small_threshold_bytes: usize,
    #[serde(default = "default_medium_threshold")]
    pub medium_threshold_bytes: usize,
    #[serde(default = "default_zstd_level")]
    pub zstd_level: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedChannelCfg {
    #[serde(default = "default_channel_capacity")]
    pub capacity: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DetailedFlusherCfg {
    #[serde(default)]
    pub workers: usize, // 0 = auto
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedLimitsCfg {
    #[serde(default = "default_single_payload_soft_cap")]
    pub single_payload_soft_cap_bytes: usize,
}

fn default_detailed_enabled() -> bool {
    true
}
fn default_detailed_stream() -> String {
    "usage:detailed".to_string()
}
fn default_detailed_maxlen() -> u64 {
    10_000_000
}
fn default_small_threshold() -> usize {
    1024
}
fn default_medium_threshold() -> usize {
    65_536
}
fn default_zstd_level() -> i32 {
    3
}
fn default_channel_capacity() -> usize {
    65_536
}
fn default_single_payload_soft_cap() -> usize {
    8 * 1024 * 1024
}

impl Default for DetailedCompressionCfg {
    fn default() -> Self {
        Self {
            small_threshold_bytes: default_small_threshold(),
            medium_threshold_bytes: default_medium_threshold(),
            zstd_level: default_zstd_level(),
        }
    }
}

impl Default for DetailedChannelCfg {
    fn default() -> Self {
        Self {
            capacity: default_channel_capacity(),
        }
    }
}

impl Default for DetailedLimitsCfg {
    fn default() -> Self {
        Self {
            single_payload_soft_cap_bytes: default_single_payload_soft_cap(),
        }
    }
}

impl Default for DetailedLogCfg {
    fn default() -> Self {
        Self {
            enabled: default_detailed_enabled(),
            stream: default_detailed_stream(),
            maxlen: default_detailed_maxlen(),
            compression: DetailedCompressionCfg::default(),
            channel: DetailedChannelCfg::default(),
            flusher: DetailedFlusherCfg::default(),
            limits: DetailedLimitsCfg::default(),
        }
    }
}

/// Reserved table; historical `sampling_rate` / `include_*_body` knobs are
/// gone — detailed-log behaviour now lives under `[detailed_log]`. Kept as
/// an empty struct to keep figment happy when a stale `local.toml` still
/// contains a `[debug]` header.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugCfg {}

/// Transport-layer security settings.
///
/// `trust_xff` opts the gateway into parsing client IPs from the
/// `X-Forwarded-For` header. Only enable when the gateway sits behind a
/// trusted L7 load balancer that strips client-supplied XFF headers — a
/// public-internet deployment with this flag on lets any client forge the
/// header and trivially bypass IP blocklists. The default is `false`.
#[derive(Debug, Deserialize, Default)]
pub struct SecurityCfg {
    #[serde(default)]
    pub trust_xff: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct SignatureCfg {
    #[serde(default)]
    pub required: bool,
}

/// Cold-loader (EntityStore) settings.
///
/// `cold_load_timeout_ms` bounds how long an `EntityStore::*_or_load` call
/// will wait on the Redis L1 → Postgres L2 → write-back loader future before
/// giving up. Without a deadline, a stalled Postgres (network glitch, lock
/// contention, etc.) makes every cache miss block indefinitely; with one,
/// the miss resolves to `None` (treated as "entity not found") and the
/// `gateway_cold_load_timeout_total{ns}` counter increments so an operator
/// can see the storm. The default 2000 ms is generous for healthy
/// deployments and tight enough to keep tail latency bounded under failure.
#[derive(Debug, Clone, Deserialize)]
pub struct LoaderCfg {
    /// Per-cold-load deadline in milliseconds. After this, the cache miss
    /// resolves to `None` (treated as "entity not found") and a
    /// `gateway_cold_load_timeout_total{ns}` counter increments. Default
    /// 2000 ms.
    #[serde(default = "default_cold_load_timeout_ms")]
    pub cold_load_timeout_ms: u64,
}

fn default_cold_load_timeout_ms() -> u64 {
    2_000
}

impl Default for LoaderCfg {
    fn default() -> Self {
        Self {
            cold_load_timeout_ms: default_cold_load_timeout_ms(),
        }
    }
}

/// Routing-layer knobs.
///
/// `sticky_ttl_secs` controls how long the `Sticky` strategy keeps a
/// `conversation_id → vendor_key_id` binding in its moka cache. Production
/// default is 1800 s (30 min) — long enough to outlive most chat sessions,
/// short enough that abandoned conversations don't pin keys forever. The
/// e2e suite overrides this to 2 s via `GW_ROUTING__STICKY_TTL_SECS=2` so
/// the TTL-expiry leg is exercisable inside CI without sleeping for half
/// an hour. ADR-S4.
#[derive(Debug, Clone, Deserialize)]
pub struct RoutingCfg {
    #[serde(default = "default_sticky_ttl_secs")]
    pub sticky_ttl_secs: u64,
    #[serde(default = "default_routing_presets")]
    pub presets: HashMap<String, RoutingPresetCfg>,
    #[serde(default)]
    pub reservation: ReservationCfg,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RoutingPresetCfg {
    #[serde(default)]
    pub health: f64,
    #[serde(default)]
    pub latency: f64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub inflight: f64,
    #[serde(default)]
    pub error_rate: f64,
    #[serde(default)]
    pub reservation: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReservationCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_reservation_interval_secs")]
    pub interval_secs: u64,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub thresholds: HashMap<String, u64>,
}

fn default_reservation_interval_secs() -> u64 {
    5
}

impl Default for ReservationCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_reservation_interval_secs(),
            instance_id: None,
            thresholds: HashMap::new(),
        }
    }
}

impl ReservationCfg {
    pub fn gate(&self) -> ReservationGate {
        ReservationGate::new(self.enabled, self.thresholds.clone())
    }
}

impl From<RoutingPresetCfg> for DimWeights {
    fn from(value: RoutingPresetCfg) -> Self {
        DimWeights::raw(
            value.health,
            value.latency,
            value.cost,
            value.inflight,
            value.error_rate,
        )
        .with_reservation(value.reservation)
    }
}

fn default_sticky_ttl_secs() -> u64 {
    1800
}

fn default_routing_presets() -> HashMap<String, RoutingPresetCfg> {
    HashMap::from([
        (
            "health_first".to_string(),
            RoutingPresetCfg {
                health: 0.6,
                latency: 0.2,
                cost: 0.1,
                inflight: 0.1,
                error_rate: 0.0,
                reservation: 0.0,
            },
        ),
        (
            "latency_first".to_string(),
            RoutingPresetCfg {
                health: 0.2,
                latency: 0.5,
                cost: 0.1,
                inflight: 0.2,
                error_rate: 0.0,
                reservation: 0.0,
            },
        ),
        (
            "cost_first".to_string(),
            RoutingPresetCfg {
                health: 0.2,
                latency: 0.1,
                cost: 0.5,
                inflight: 0.1,
                error_rate: 0.1,
                reservation: 0.0,
            },
        ),
        (
            "balanced".to_string(),
            RoutingPresetCfg {
                health: 0.25,
                latency: 0.25,
                cost: 0.25,
                inflight: 0.15,
                error_rate: 0.1,
                reservation: 0.0,
            },
        ),
    ])
}

impl RoutingCfg {
    pub fn preset_weights(&self) -> HashMap<String, DimWeights> {
        self.presets
            .iter()
            .map(|(name, cfg)| (name.clone(), (*cfg).into()))
            .collect()
    }
}

impl Default for RoutingCfg {
    fn default() -> Self {
        Self {
            sticky_ttl_secs: default_sticky_ttl_secs(),
            presets: default_routing_presets(),
            reservation: ReservationCfg::default(),
        }
    }
}

/// Tunnel subsystem knobs (ADR-T-5). `enabled = false` (default) keeps the
/// gateway on the byte-identical Direct path. Boot-time only — flipping
/// requires SIGHUP / restart, not a hot-reload, because constructing the
/// stack requires DNS + transport setup at startup.
#[derive(Debug, Clone, Deserialize)]
pub struct TunnelCfg {
    #[serde(default)]
    pub enabled: bool,
    /// DoH provider name. v1 only `cloudflare` is supported; the loader
    /// errors at startup on any other value to keep the surface explicit.
    #[serde(default = "default_doh")]
    pub doh: String,
    #[serde(default = "default_dns_cache_ttl_secs")]
    pub dns_cache_ttl_secs: u32,
    /// `host:port` strings the gateway is allowed to tunnel to. Mapped
    /// into `gw_tunnel_client::target_filter::set_whitelist` at boot.
    #[serde(default)]
    pub target_whitelist: Vec<String>,
    #[serde(default = "default_heartbeat_secs")]
    pub heartbeat_interval_secs: u32,
    #[serde(default = "default_missed_pong")]
    pub missed_pong_threshold: u32,
    #[serde(default = "default_backoff_min_secs")]
    pub backoff_min_secs: u32,
    #[serde(default = "default_backoff_max_secs")]
    pub backoff_max_secs: u32,
    #[serde(default = "default_max_streams")]
    pub max_streams_per_conn: u32,
}

fn default_doh() -> String {
    "cloudflare".into()
}
fn default_dns_cache_ttl_secs() -> u32 {
    600
}
fn default_heartbeat_secs() -> u32 {
    30
}
fn default_missed_pong() -> u32 {
    3
}
fn default_backoff_min_secs() -> u32 {
    1
}
fn default_backoff_max_secs() -> u32 {
    60
}
fn default_max_streams() -> u32 {
    256
}

impl Default for TunnelCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            doh: default_doh(),
            dns_cache_ttl_secs: default_dns_cache_ttl_secs(),
            target_whitelist: Vec::new(),
            heartbeat_interval_secs: default_heartbeat_secs(),
            missed_pong_threshold: default_missed_pong(),
            backoff_min_secs: default_backoff_min_secs(),
            backoff_max_secs: default_backoff_max_secs(),
            max_streams_per_conn: default_max_streams(),
        }
    }
}

/// Egress-impersonation subsystem knobs (Phase 4 / Task 5.2).
///
/// Controls where the in-process loopback egress listener binds, its
/// concurrency cap, the TurnStateCache TTL, and a kill-switch (`disabled`)
/// that excludes Impersonate vendors from candidate selection without touching
/// the Rustls path.
///
/// Boot-time only — changing these fields requires a restart.
#[derive(Debug, Clone, Deserialize)]
pub struct EgressCfg {
    /// When `true`, vendors with `egress_mode = Impersonate { .. }` are
    /// excluded from `pick_peer` candidate selection and the metric
    /// `gw_egress_disabled_total` increments per dropped vendor. Rustls
    /// vendors are unaffected. Default `false`.
    #[serde(default)]
    pub disabled: bool,
    /// Unix-domain socket path the loopback egress listener binds. Mutually
    /// exclusive with `bind_tcp`. Defaults to `/tmp/gw-egress.sock`.
    #[serde(default)]
    pub bind_uds: Option<std::path::PathBuf>,
    /// Loopback TCP address the egress listener binds. Mutually exclusive
    /// with `bind_uds`. When set, `bind_uds` must be `None`.
    #[serde(default)]
    pub bind_tcp: Option<std::net::SocketAddr>,
    /// Maximum concurrent loopback sessions before back-pressure. Default 64.
    #[serde(default = "default_egress_max_sessions")]
    pub max_sessions: usize,
    /// TTL for the `TurnStateCache` (Codex per-turn state). Default 300 s.
    #[serde(default = "default_egress_turn_state_ttl_secs")]
    pub turn_state_ttl_secs: u64,
}

fn default_egress_max_sessions() -> usize {
    64
}
fn default_egress_turn_state_ttl_secs() -> u64 {
    300
}

impl Default for EgressCfg {
    fn default() -> Self {
        Self {
            disabled: false,
            bind_uds: Some(std::path::PathBuf::from("/tmp/gw-egress.sock")),
            bind_tcp: None,
            max_sessions: default_egress_max_sessions(),
            turn_state_ttl_secs: default_egress_turn_state_ttl_secs(),
        }
    }
}

impl EgressCfg {
    /// Resolve the configured bind address to an `EgressBind`.
    ///
    /// Returns an error when both `bind_uds` and `bind_tcp` are set, or when
    /// neither is set.
    pub fn bind(&self) -> anyhow::Result<gw_egress_impersonate::EgressBind> {
        match (&self.bind_uds, &self.bind_tcp) {
            (Some(_), Some(_)) => {
                anyhow::bail!("[egress] both bind_uds and bind_tcp are set; pick exactly one")
            }
            (None, None) => {
                anyhow::bail!("[egress] neither bind_uds nor bind_tcp is set; one is required")
            }
            (Some(path), None) => {
                Ok(gw_egress_impersonate::EgressBind::Uds(path.clone()))
            }
            (None, Some(addr)) => {
                Ok(gw_egress_impersonate::EgressBind::Tcp(*addr))
            }
        }
    }
}

/// Auth-layer knobs. Proxy hard-cap filter values are boot-time only.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthCfg {
    /// Maximum estimated input tokens per single request (ADR-024).
    #[serde(default = "default_hard_cap_max_input_tokens")]
    pub hard_cap_max_input_tokens: u32,
    /// Maximum estimated cost (micro-units) per single request (ADR-024).
    #[serde(default = "default_hard_cap_max_cost_micro")]
    pub hard_cap_max_cost_micro: u64,
}

fn default_hard_cap_max_input_tokens() -> u32 {
    1_000_000
}
fn default_hard_cap_max_cost_micro() -> u64 {
    5_000_000_000
}

impl Default for AuthCfg {
    fn default() -> Self {
        Self {
            hard_cap_max_input_tokens: default_hard_cap_max_input_tokens(),
            hard_cap_max_cost_micro: default_hard_cap_max_cost_micro(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub enum IpFilterAllowlistMode {
    #[default]
    Off,
    Enforce,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpFilterCfg {
    #[serde(default)]
    pub allowlist_mode: IpFilterAllowlistMode,
}

impl Default for IpFilterCfg {
    fn default() -> Self {
        Self {
            allowlist_mode: IpFilterAllowlistMode::Off,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpRateLimitCfg {
    #[serde(default = "default_global_per_ip_rpm")]
    pub global_per_ip_rpm: u32,
    #[serde(default = "default_ip_ratelimit_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_ip_ratelimit_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
}

fn default_global_per_ip_rpm() -> u32 {
    600
}
fn default_ip_ratelimit_max_entries() -> usize {
    100_000
}
fn default_ip_ratelimit_sweep_interval_secs() -> u64 {
    300
}

impl Default for IpRateLimitCfg {
    fn default() -> Self {
        Self {
            global_per_ip_rpm: default_global_per_ip_rpm(),
            max_entries: default_ip_ratelimit_max_entries(),
            sweep_interval_secs: default_ip_ratelimit_sweep_interval_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdaptiveRpmCfg {
    #[serde(default = "default_adaptive_rpm_enforcement_threshold")]
    pub enforcement_threshold: f64,
    #[serde(default = "default_adaptive_rpm_confidence_decay_per_min")]
    pub confidence_decay_per_min: f64,
    #[serde(default = "default_adaptive_rpm_min_header_observations")]
    pub min_header_observations: u32,
    #[serde(default = "default_adaptive_rpm_min_no_header_observations")]
    pub min_no_header_observations: u32,
    #[serde(default = "default_adaptive_rpm_buffer_ratio")]
    pub buffer_ratio: f64,
    #[serde(default = "default_adaptive_rpm_probe_after_min")]
    pub probe_after_min: u64,
    #[serde(default = "default_adaptive_rpm_probe_increment")]
    pub probe_increment: u32,
    #[serde(default = "default_adaptive_rpm_min_rpm")]
    pub min_rpm: u32,
    #[serde(default = "default_adaptive_rpm_max_rpm")]
    pub max_rpm: u32,
}

fn default_adaptive_rpm_enforcement_threshold() -> f64 {
    0.6
}
fn default_adaptive_rpm_confidence_decay_per_min() -> f64 {
    0.02
}
fn default_adaptive_rpm_min_header_observations() -> u32 {
    2
}
fn default_adaptive_rpm_min_no_header_observations() -> u32 {
    3
}
fn default_adaptive_rpm_buffer_ratio() -> f64 {
    0.85
}
fn default_adaptive_rpm_probe_after_min() -> u64 {
    30
}
fn default_adaptive_rpm_probe_increment() -> u32 {
    5
}
fn default_adaptive_rpm_min_rpm() -> u32 {
    5
}
fn default_adaptive_rpm_max_rpm() -> u32 {
    10_000
}

impl Default for AdaptiveRpmCfg {
    fn default() -> Self {
        Self {
            enforcement_threshold: default_adaptive_rpm_enforcement_threshold(),
            confidence_decay_per_min: default_adaptive_rpm_confidence_decay_per_min(),
            min_header_observations: default_adaptive_rpm_min_header_observations(),
            min_no_header_observations: default_adaptive_rpm_min_no_header_observations(),
            buffer_ratio: default_adaptive_rpm_buffer_ratio(),
            probe_after_min: default_adaptive_rpm_probe_after_min(),
            probe_increment: default_adaptive_rpm_probe_increment(),
            min_rpm: default_adaptive_rpm_min_rpm(),
            max_rpm: default_adaptive_rpm_max_rpm(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_task_queue_stream")]
    pub queue_stream: String,
    #[serde(default = "default_task_cancel_stream")]
    pub cancel_stream: String,
    #[serde(default = "default_task_billing_base_micro")]
    pub billing_base_micro: i64,
    #[serde(default = "default_task_billing_vendor_multiplier")]
    pub billing_vendor_multiplier: f64,
    #[serde(default = "default_task_billing_estimated_duration_secs")]
    pub billing_estimated_duration_secs: u64,
}

fn default_task_queue_stream() -> String {
    "tasks:queue".to_string()
}
fn default_task_cancel_stream() -> String {
    "tasks:cancel".to_string()
}
const fn default_task_billing_base_micro() -> i64 {
    1_000
}
const fn default_task_billing_vendor_multiplier() -> f64 {
    1.0
}
const fn default_task_billing_estimated_duration_secs() -> u64 {
    60
}

impl Default for TaskCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            queue_stream: default_task_queue_stream(),
            cancel_stream: default_task_cancel_stream(),
            billing_base_micro: default_task_billing_base_micro(),
            billing_vendor_multiplier: default_task_billing_vendor_multiplier(),
            billing_estimated_duration_secs: default_task_billing_estimated_duration_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ErrorCfg {
    #[serde(default)]
    pub classification: ErrorClassificationCfg,
    #[serde(default)]
    pub passthrough: ErrorPassthroughCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorClassificationCfg {
    #[serde(default = "default_retryable_status_codes")]
    pub retryable_status_codes: Vec<u16>,
}

fn default_retryable_status_codes() -> Vec<u16> {
    vec![429, 500, 502, 503, 504]
}

impl Default for ErrorClassificationCfg {
    fn default() -> Self {
        Self {
            retryable_status_codes: default_retryable_status_codes(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ErrorPassthroughCfg {
    #[serde(default)]
    pub enabled: bool,
}

/// Per-endpoint body-rule defaults. Each key is an endpoint path pattern
/// (exact match, e.g. `"/v1/chat/completions"`), and the value is a flat
/// JSON object whose fields are injected into the upstream request body
/// with **set-if-absent** semantics (existing client-supplied fields are
/// never overridden).
///
/// ```toml
/// [body_rules."/v1/chat/completions"]
/// temperature = 0.7
/// top_p = 0.9
/// ```
///
/// At boot time the inner `serde_json::Value`s are converted to
/// `Box<RawValue>` for zero-copy injection into the IR passthrough
/// overrides map.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BodyRulesCfg {
    /// Endpoint path → flat key-value defaults.
    #[serde(flatten)]
    pub rules:
        std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>,
}

/// Health subsystem knobs.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HealthCfg {
    #[serde(default)]
    pub quota_probe: QuotaProbeCfg,
}

/// Optional quota probe that checks cooldown keys at a fixed interval.
/// Default: disabled. Boot-time only.
#[derive(Debug, Clone, Deserialize)]
pub struct QuotaProbeCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_quota_probe_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_quota_probe_endpoint")]
    pub probe_endpoint: String,
}

fn default_quota_probe_interval_secs() -> u64 {
    30
}

fn default_quota_probe_endpoint() -> String {
    "/v1/models".to_string()
}

impl Default for QuotaProbeCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_quota_probe_interval_secs(),
            probe_endpoint: default_quota_probe_endpoint(),
        }
    }
}

/// Billing-robustness knobs. Spec:
/// [`docs/superpowers/specs/2026-05-24-billing-robustness-design.md`].
#[derive(Debug, Clone, Deserialize)]
pub struct BillingCfg {
    /// Window (ms) after a cancel trigger during which the gateway continues
    /// reading upstream body to catch a final usage frame. Spec §5.3.
    #[serde(default = "default_cancel_grace_ms")]
    pub cancel_grace_ms: u64,
    /// Total upstream response timeout (ms). Spec §5.3 / L7.
    #[serde(default = "default_response_deadline_ms")]
    pub response_deadline_ms: u64,
    /// InflightLedger T0 watchdog scan period (seconds). Spec §5.4.2.
    #[serde(default = "default_reaper_interval_secs")]
    pub reaper_interval_secs: u64,
    /// Threshold for emit-lag (seconds since `ts_settled_ms`) that triggers a
    /// T0 incident. Spec §5.4.2.
    #[serde(default = "default_emit_lag_t0_secs")]
    pub emit_lag_t0_secs: u64,
    /// Heartbeat key renewal cadence (seconds). Spec §5.4.1.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// Heartbeat key TTL (seconds). Must be >> heartbeat_interval_secs.
    #[serde(default = "default_heartbeat_ttl_secs")]
    pub heartbeat_ttl_secs: u64,
}

fn default_cancel_grace_ms() -> u64 {
    200
}
fn default_response_deadline_ms() -> u64 {
    300_000
}
fn default_reaper_interval_secs() -> u64 {
    30
}
fn default_emit_lag_t0_secs() -> u64 {
    60
}
fn default_heartbeat_interval_secs() -> u64 {
    60
}
fn default_heartbeat_ttl_secs() -> u64 {
    600
}

impl Default for BillingCfg {
    fn default() -> Self {
        Self {
            cancel_grace_ms: default_cancel_grace_ms(),
            response_deadline_ms: default_response_deadline_ms(),
            reaper_interval_secs: default_reaper_interval_secs(),
            emit_lag_t0_secs: default_emit_lag_t0_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            heartbeat_ttl_secs: default_heartbeat_ttl_secs(),
        }
    }
}

impl RedisCfg {
    /// Expose the connection URL for the redis client. The string is only
    /// consumed at startup by the connect call.
    pub fn url_str(&self) -> &str {
        self.url.expose_secret()
    }
}

impl PgCfg {
    /// Expose the connection URL for the pg client. The string is only
    /// consumed at startup by the connect call.
    pub fn url_str(&self) -> &str {
        self.url.expose_secret()
    }
}

impl Config {
    /// Validate cross-field invariants and numeric ranges. Called from
    /// [`load`] right after deserialisation so misconfiguration surfaces at
    /// startup rather than at first use.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.server.business_port == 0
            || self.server.metrics_port == 0
            || self.server.health_port == 0
        {
            anyhow::bail!("invalid config: server ports must be non-zero");
        }
        if self.server.business_port == self.server.metrics_port
            || self.server.business_port == self.server.health_port
            || self.server.metrics_port == self.server.health_port
        {
            anyhow::bail!(
                "invalid config: server.business_port, server.metrics_port \
                 and server.health_port must all differ"
            );
        }

        if self.redis.pool_size == 0 {
            anyhow::bail!("invalid config: redis.pool_size must be > 0");
        }
        if self.pgsql.max_connections == 0 {
            anyhow::bail!("invalid config: pgsql.max_connections must be > 0");
        }
        if self.routing.presets.is_empty() {
            anyhow::bail!("invalid config: routing.presets must not be empty");
        }
        for (name, preset) in &self.routing.presets {
            if name.trim().is_empty() {
                anyhow::bail!("invalid config: routing.presets contains an empty preset name");
            }
            for (field, value) in [
                ("health", preset.health),
                ("latency", preset.latency),
                ("cost", preset.cost),
                ("inflight", preset.inflight),
                ("error_rate", preset.error_rate),
            ] {
                if !value.is_finite() || value < 0.0 {
                    anyhow::bail!(
                        "invalid config: routing.presets.{name}.{field} must be finite and >= 0.0"
                    );
                }
            }
        }

        if self.ledger.batch_size == 0 {
            anyhow::bail!("invalid config: ledger.batch_size must be > 0");
        }
        if self.ledger.channel_capacity == 0 {
            anyhow::bail!("invalid config: ledger.channel_capacity must be > 0");
        }
        if self.ledger.flush_interval_ms == 0 {
            anyhow::bail!("invalid config: ledger.flush_interval_ms must be > 0");
        }

        if self.tunnel.enabled {
            if self.tunnel.doh != "cloudflare" {
                anyhow::bail!(
                    "invalid config: tunnel.doh = {:?} (only 'cloudflare' supported in v1)",
                    self.tunnel.doh
                );
            }
            if self.tunnel.backoff_min_secs == 0 || self.tunnel.backoff_max_secs == 0 {
                anyhow::bail!("invalid config: tunnel.backoff_* must be > 0");
            }
            if self.tunnel.backoff_min_secs > self.tunnel.backoff_max_secs {
                anyhow::bail!("invalid config: tunnel.backoff_min_secs > backoff_max_secs");
            }
            if self.tunnel.missed_pong_threshold == 0 {
                anyhow::bail!("invalid config: tunnel.missed_pong_threshold must be > 0");
            }
        }

        if self.auth.hard_cap_max_input_tokens == 0 {
            anyhow::bail!("invalid config: auth.hard_cap_max_input_tokens must be > 0");
        }
        if self.auth.hard_cap_max_cost_micro == 0 {
            anyhow::bail!("invalid config: auth.hard_cap_max_cost_micro must be > 0");
        }
        if self.task.billing_base_micro <= 0 {
            anyhow::bail!("invalid config: task.billing_base_micro must be > 0");
        }
        if !self.task.billing_vendor_multiplier.is_finite()
            || self.task.billing_vendor_multiplier <= 0.0
        {
            anyhow::bail!("invalid config: task.billing_vendor_multiplier must be finite and > 0");
        }

        if self.billing.cancel_grace_ms == 0 {
            anyhow::bail!("invalid config: billing.cancel_grace_ms must be > 0");
        }
        if self.billing.response_deadline_ms == 0 {
            anyhow::bail!("invalid config: billing.response_deadline_ms must be > 0");
        }
        if self.billing.heartbeat_interval_secs == 0 {
            anyhow::bail!("invalid config: billing.heartbeat_interval_secs must be > 0");
        }
        if self.billing.heartbeat_ttl_secs == 0 {
            anyhow::bail!("invalid config: billing.heartbeat_ttl_secs must be > 0");
        }
        if self.billing.heartbeat_interval_secs * 10 > self.billing.heartbeat_ttl_secs {
            anyhow::bail!(
                "invalid config: billing.heartbeat_interval_secs ({}) * 10 must be <= heartbeat_ttl_secs ({})",
                self.billing.heartbeat_interval_secs,
                self.billing.heartbeat_ttl_secs,
            );
        }

        if self.ip_ratelimit.global_per_ip_rpm == 0 {
            anyhow::bail!("invalid config: ip_ratelimit.global_per_ip_rpm must be > 0");
        }
        if self.ip_ratelimit.max_entries == 0 {
            anyhow::bail!("invalid config: ip_ratelimit.max_entries must be > 0");
        }
        if self.ip_ratelimit.sweep_interval_secs == 0 {
            anyhow::bail!("invalid config: ip_ratelimit.sweep_interval_secs must be > 0");
        }

        if !(0.0..=1.0).contains(&self.adaptive_rpm.enforcement_threshold) {
            anyhow::bail!(
                "invalid config: adaptive_rpm.enforcement_threshold must be between 0.0 and 1.0"
            );
        }
        if !(0.0..=1.0).contains(&self.adaptive_rpm.confidence_decay_per_min) {
            anyhow::bail!(
                "invalid config: adaptive_rpm.confidence_decay_per_min must be between 0.0 and 1.0"
            );
        }
        if !(0.0..=1.0).contains(&self.adaptive_rpm.buffer_ratio) {
            anyhow::bail!("invalid config: adaptive_rpm.buffer_ratio must be between 0.0 and 1.0");
        }
        if self.adaptive_rpm.min_header_observations == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.min_header_observations must be > 0");
        }
        if self.adaptive_rpm.min_no_header_observations == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.min_no_header_observations must be > 0");
        }
        if self.adaptive_rpm.probe_after_min == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.probe_after_min must be > 0");
        }
        if self.adaptive_rpm.probe_increment == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.probe_increment must be > 0");
        }
        if self.adaptive_rpm.min_rpm == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.min_rpm must be > 0");
        }
        if self.adaptive_rpm.max_rpm == 0 {
            anyhow::bail!("invalid config: adaptive_rpm.max_rpm must be > 0");
        }
        if self.adaptive_rpm.min_rpm > self.adaptive_rpm.max_rpm {
            anyhow::bail!("invalid config: adaptive_rpm.min_rpm must be <= max_rpm");
        }

        if self.task.queue_stream.is_empty() {
            anyhow::bail!("invalid config: task.queue_stream must be non-empty");
        }
        if self.task.cancel_stream.is_empty() {
            anyhow::bail!("invalid config: task.cancel_stream must be non-empty");
        }

        if self.health.quota_probe.enabled {
            if self.health.quota_probe.interval_secs == 0 {
                anyhow::bail!("invalid config: health.quota_probe.interval_secs must be > 0");
            }
            if self.health.quota_probe.probe_endpoint.is_empty() {
                anyhow::bail!(
                    "invalid config: health.quota_probe.probe_endpoint must be non-empty"
                );
            }
        }

        Ok(())
    }
}

pub fn load() -> anyhow::Result<Config> {
    let cfg: Config = Figment::new()
        .merge(Toml::file("config/default.toml"))
        .merge(Toml::file("config/local.toml"))
        .merge(Env::prefixed("GW_").split("__"))
        .extract()?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod egress_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn egress_default_values() {
        let cfg = EgressCfg::default();
        assert!(!cfg.disabled, "disabled should default to false");
        assert_eq!(
            cfg.bind_uds,
            Some(std::path::PathBuf::from("/tmp/gw-egress.sock")),
            "bind_uds should default to /tmp/gw-egress.sock"
        );
        assert!(cfg.bind_tcp.is_none(), "bind_tcp should default to None");
        assert_eq!(cfg.max_sessions, 64, "max_sessions should default to 64");
        assert_eq!(
            cfg.turn_state_ttl_secs, 300,
            "turn_state_ttl_secs should default to 300"
        );
    }

    #[test]
    fn egress_bind_returns_uds_for_default() {
        let cfg = EgressCfg::default();
        let bind = cfg.bind().expect("bind() should succeed for default");
        assert!(
            matches!(bind, gw_egress_impersonate::EgressBind::Uds(_)),
            "default bind should be Uds"
        );
    }

    #[test]
    fn egress_bind_returns_tcp_when_set() {
        let cfg = EgressCfg {
            disabled: false,
            bind_uds: None,
            bind_tcp: Some("127.0.0.1:18080".parse().unwrap()),
            max_sessions: 64,
            turn_state_ttl_secs: 300,
        };
        let bind = cfg.bind().expect("bind() should succeed for tcp config");
        assert!(
            matches!(bind, gw_egress_impersonate::EgressBind::Tcp(_)),
            "bind should be Tcp when bind_tcp is set and bind_uds is None"
        );
    }

    #[test]
    fn egress_bind_errors_when_both_set() {
        let cfg = EgressCfg {
            disabled: false,
            bind_uds: Some(std::path::PathBuf::from("/tmp/gw-egress.sock")),
            bind_tcp: Some("127.0.0.1:18080".parse().unwrap()),
            max_sessions: 64,
            turn_state_ttl_secs: 300,
        };
        assert!(
            cfg.bind().is_err(),
            "bind() should error when both bind_uds and bind_tcp are set"
        );
    }

    #[test]
    fn egress_bind_errors_when_neither_set() {
        let cfg = EgressCfg {
            disabled: false,
            bind_uds: None,
            bind_tcp: None,
            max_sessions: 64,
            turn_state_ttl_secs: 300,
        };
        assert!(
            cfg.bind().is_err(),
            "bind() should error when neither bind_uds nor bind_tcp is set"
        );
    }

    #[test]
    fn egress_section_from_default_toml() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("default.toml should parse with [egress] section");
        cfg.validate().expect("default.toml should validate");
        assert!(!cfg.egress.disabled);
        assert_eq!(cfg.egress.max_sessions, 64);
        assert_eq!(cfg.egress.turn_state_ttl_secs, 300);
        assert_eq!(
            cfg.egress.bind_uds,
            Some(std::path::PathBuf::from("/tmp/gw-egress.sock"))
        );
        assert!(cfg.egress.bind_tcp.is_none());
    }
}

#[cfg(test)]
mod tunnel_cfg_tests {
    use super::*;
    use figment::{
        providers::{Format, Toml},
        Figment,
    };

    fn extract(toml: &str) -> anyhow::Result<Config> {
        Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .map_err(Into::into)
    }

    #[test]
    fn tunnel_disabled_by_default() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = extract(toml).unwrap();
        cfg.validate().unwrap();
        assert!(!cfg.tunnel.enabled);
    }

    #[test]
    fn tunnel_rejects_unknown_doh_provider() {
        // Build config from defaults, then override just the tunnel fields via
        // a second TOML string that Figment merges on top (higher precedence).
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[tunnel]\nenabled = true\ndoh = \"google\"\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tunnel_rejects_zero_backoff_min() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[tunnel]\nenabled = true\nbackoff_min_secs = 0\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tunnel_rejects_min_greater_than_max() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[tunnel]\nenabled = true\nbackoff_min_secs = 60\nbackoff_max_secs = 1\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tunnel_rejects_zero_missed_pong() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[tunnel]\nenabled = true\nmissed_pong_threshold = 0\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tunnel_validates_when_enabled_with_defaults() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[tunnel]\nenabled = true\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_ok());
    }
}

#[cfg(test)]
mod billing_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn billing_section_defaults_match_spec() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("default.toml parses");
        assert_eq!(cfg.billing.cancel_grace_ms, 200);
        assert_eq!(cfg.billing.response_deadline_ms, 300_000);
        assert_eq!(cfg.billing.reaper_interval_secs, 30);
        assert_eq!(cfg.billing.emit_lag_t0_secs, 60);
        assert_eq!(cfg.billing.heartbeat_interval_secs, 60);
        assert_eq!(cfg.billing.heartbeat_ttl_secs, 600);
    }

    #[test]
    fn billing_validate_rejects_zero_cancel_grace() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[billing]\ncancel_grace_ms = 0\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn billing_validate_rejects_zero_response_deadline() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[billing]\nresponse_deadline_ms = 0\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn billing_validate_rejects_heartbeat_interval_too_close_to_ttl() {
        // heartbeat_interval_secs * 10 must be <= heartbeat_ttl_secs
        // Setting interval=100, ttl=600: 100*10=1000 > 600 → reject
        let defaults = include_str!("../../../config/default.toml");
        let overrides = "[billing]\nheartbeat_interval_secs = 100\nheartbeat_ttl_secs = 600\n";
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn billing_env_override() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GW_BILLING__CANCEL_GRACE_MS", "500");
            let defaults = include_str!("../../../config/default.toml");
            let cfg: Config = Figment::new()
                .merge(Toml::string(defaults))
                .merge(figment::providers::Env::prefixed("GW_").split("__"))
                .extract()
                .expect("env override extracts");
            assert_eq!(cfg.billing.cancel_grace_ms, 500);
            Ok(())
        });
    }
}

#[cfg(test)]
mod routing_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn routing_presets_default_to_four_builtins() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("default.toml parses");
        cfg.validate().unwrap();
        for name in ["health_first", "latency_first", "cost_first", "balanced"] {
            assert!(cfg.routing.presets.contains_key(name));
        }
        assert_eq!(cfg.routing.presets["health_first"].health, 0.6);
    }

    #[test]
    fn routing_presets_custom_weights_parse() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = r#"
[routing.presets]
custom = { health = 0.1, latency = 0.2, cost = 0.3, inflight = 0.4, error_rate = 0.0 }
"#;
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .expect("routing preset parse");
        cfg.validate().unwrap();
        assert_eq!(cfg.routing.presets["custom"].inflight, 0.4);
    }

    #[test]
    fn routing_presets_reject_negative_weight() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = r#"
[routing.presets]
bad = { health = -1.0, latency = 0.0, cost = 0.0, inflight = 0.0, error_rate = 0.0 }
"#;
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .expect("routing preset parse");
        assert!(cfg.validate().is_err());
    }
}

#[cfg(test)]
mod error_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn error_section_defaults_match_spec() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("default.toml parses");
        assert_eq!(
            cfg.error.classification.retryable_status_codes,
            vec![429, 500, 502, 503, 504]
        );
        assert!(!cfg.error.passthrough.enabled);
    }

    #[test]
    fn error_env_override_passthrough() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GW_ERROR__PASSTHROUGH__ENABLED", "true");
            let defaults = include_str!("../../../config/default.toml");
            let cfg: Config = Figment::new()
                .merge(Toml::string(defaults))
                .merge(figment::providers::Env::prefixed("GW_").split("__"))
                .extract()
                .expect("env override extracts");
            assert!(cfg.error.passthrough.enabled);
            Ok(())
        });
    }
}

#[cfg(test)]
mod body_rules_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn default_body_rules_empty() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("default.toml parses");
        assert!(cfg.body_rules.rules.is_empty());
    }

    #[test]
    fn body_rules_parse_from_toml() {
        let defaults = include_str!("../../../config/default.toml");
        let overrides = r#"
[body_rules."/v1/chat/completions"]
temperature = 0.7
top_p = 0.9
"#;
        let cfg: Config = Figment::new()
            .merge(Toml::string(defaults))
            .merge(Toml::string(overrides))
            .extract()
            .expect("body_rules parse");
        cfg.validate().unwrap();
        let rules = cfg.body_rules.rules.get("/v1/chat/completions").unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules.get("temperature").unwrap(), &serde_json::json!(0.7));
        assert_eq!(rules.get("top_p").unwrap(), &serde_json::json!(0.9));
    }
}
