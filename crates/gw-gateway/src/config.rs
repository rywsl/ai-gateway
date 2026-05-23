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
use secrecy::{ExposeSecret, SecretString};
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
    #[serde(default)]
    pub security: SecurityCfg,
    #[serde(default)]
    pub loader: LoaderCfg,
    #[serde(default)]
    pub routing: RoutingCfg,
    #[serde(default)]
    pub tunnel: TunnelCfg,
    #[serde(default)]
    pub detailed_log: DetailedLogCfg,
    // TODO(phase4-wireup): the proxy hard-cap filter currently uses hardcoded
    // `HardCapCfg` defaults. Once it accepts caller-supplied caps, reintroduce
    // an `[auth]` section here and thread `hard_cap_max_input_tokens` /
    // `hard_cap_max_cost_micro` into `LlmGateway` via `Wired`.
}

#[derive(Debug, Deserialize)]
pub struct ServerCfg {
    pub business_port: u16,
    pub metrics_port: u16,
    pub health_port: u16,
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

#[derive(Debug, Clone, Deserialize)]
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

impl Default for DetailedFlusherCfg {
    fn default() -> Self {
        Self { workers: 0 }
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
}

fn default_sticky_ttl_secs() -> u64 {
    1800
}

impl Default for RoutingCfg {
    fn default() -> Self {
        Self {
            sticky_ttl_secs: default_sticky_ttl_secs(),
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
