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

#[derive(Debug, Deserialize)]
pub struct DebugCfg {
    pub sampling_rate: f64,
    pub include_request_body: bool,
    pub include_response_body: bool,
}

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

        if !(0.0..=1.0).contains(&self.debug.sampling_rate) {
            anyhow::bail!(
                "invalid config: debug.sampling_rate must be in [0.0, 1.0], got {}",
                self.debug.sampling_rate
            );
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
