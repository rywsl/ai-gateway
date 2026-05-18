//! Configuration loading via figment.
//!
//! Sources merged low → high precedence:
//!   1. `config/default.toml`
//!   2. `config/local.toml` (optional, gitignored)
//!   3. Environment variables prefixed with `GW_`, nesting separator `__`
//!      (e.g. `GW_REDIS__URL` populates `redis.url`).
//!
//! Secret material (KEK hex, DB passwords) should arrive via env vars and is
//! kept inside `secrecy::SecretString` so accidental `Debug` output redacts.

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use secrecy::SecretString;
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
    pub auth: AuthCfg,
}

#[derive(Debug, Deserialize)]
pub struct ServerCfg {
    pub business_port: u16,
    pub metrics_port: u16,
    pub health_port: u16,
}

#[derive(Debug, Deserialize)]
pub struct RedisCfg {
    pub url: String,
    pub pool_size: usize,
}

#[derive(Debug, Deserialize)]
pub struct PgCfg {
    pub url: String,
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

#[derive(Debug, Deserialize)]
pub struct AuthCfg {
    pub hard_cap_max_input_tokens: u32,
    pub hard_cap_max_cost_micro: u64,
}

pub fn load() -> anyhow::Result<Config> {
    let cfg: Config = Figment::new()
        .merge(Toml::file("config/default.toml"))
        .merge(Toml::file("config/local.toml"))
        .merge(Env::prefixed("GW_").split("__"))
        .extract()?;
    Ok(cfg)
}
