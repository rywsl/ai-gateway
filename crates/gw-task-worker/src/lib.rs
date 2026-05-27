use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig as FredRedisConfig};
use gw_task::redis_contract::RedisTaskContract;
use gw_task_runtime::cancel_loop::{
    CancelLoop, CancelLoopConfig, RedisCancelSource, RunningTaskRegistry,
};
use gw_task_runtime::exec_loop::{ExecLoop, ExecLoopConfig, TaskTimeouts};
use gw_task_runtime::heartbeat_loop::{HeartbeatLoop, HeartbeatLoopConfig};
use gw_task_runtime::watchdog_loop::{RedisLeaseRecoveryStore, WatchdogLoop, WatchdogLoopConfig};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use metrics_exporter_prometheus::PrometheusBuilder;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use uuid::Uuid;

const WORKER_REGISTRATION_TTL_SECS: i64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRegistration {
    pub worker_id: String,
    pub start_time_ms: u64,
    pub hostname: String,
}

impl WorkerRegistration {
    #[must_use]
    pub fn new(worker_id: String, start_time_ms: u64, hostname: String) -> Self {
        Self {
            worker_id,
            start_time_ms,
            hostname,
        }
    }

    #[must_use]
    pub fn redis_key(&self, prefix: &str) -> String {
        format!("{prefix}entity:worker:{}", self.worker_id)
    }
}

#[derive(Clone, Debug)]
pub struct WorkerRegistrationLoopConfig {
    pub heartbeat_interval: Duration,
    pub cancellation_token: CancellationToken,
}

impl Default for WorkerRegistrationLoopConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(5),
            cancellation_token: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct WorkerRegistrationLoop {
    pool: RedisPool,
    redis_prefix: String,
    registration: WorkerRegistration,
    cfg: WorkerRegistrationLoopConfig,
}

impl WorkerRegistrationLoop {
    #[must_use]
    pub fn new(
        pool: RedisPool,
        redis_prefix: String,
        registration: WorkerRegistration,
        cfg: WorkerRegistrationLoopConfig,
    ) -> Self {
        Self {
            pool,
            redis_prefix,
            registration,
            cfg,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.cfg.heartbeat_interval) => {
                    if let Err(err) = self.run_once().await {
                        tracing::warn!(target: "gw_task_worker::registration", error = %err, worker_id = %self.registration.worker_id, "worker registration heartbeat failed");
                    }
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<()> {
        self.run_once_at(now_ms()).await
    }

    pub async fn run_once_at(&self, heartbeat_ms: u64) -> anyhow::Result<()> {
        heartbeat_worker_registration(
            &self.pool,
            &self.redis_prefix,
            &self.registration,
            heartbeat_ms,
        )
        .await
    }
}

#[must_use]
pub fn generate_worker_id() -> String {
    Uuid::new_v4().to_string()
}

#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn resolve_hostname() -> anyhow::Result<String> {
    Ok(hostname::get()?.to_string_lossy().into_owned())
}

pub async fn register_worker(
    pool: &RedisPool,
    redis_prefix: &str,
    registration: &WorkerRegistration,
) -> anyhow::Result<()> {
    write_worker_registration(pool, redis_prefix, registration, registration.start_time_ms).await
}

pub async fn heartbeat_worker_registration(
    pool: &RedisPool,
    redis_prefix: &str,
    registration: &WorkerRegistration,
    heartbeat_ms: u64,
) -> anyhow::Result<()> {
    let key = registration.redis_key(redis_prefix);
    let fields: Vec<(&str, String)> = vec![
        ("worker_id", registration.worker_id.clone()),
        ("start_time_ms", registration.start_time_ms.to_string()),
        ("status", "running".to_string()),
        ("hostname", registration.hostname.clone()),
        ("last_heartbeat_ms", heartbeat_ms.to_string()),
    ];
    let _: () = pool.hset::<(), _, _>(&key, fields).await?;
    let _: () = pool
        .expire::<(), _>(&key, WORKER_REGISTRATION_TTL_SECS)
        .await?;
    Ok(())
}

async fn write_worker_registration(
    pool: &RedisPool,
    redis_prefix: &str,
    registration: &WorkerRegistration,
    heartbeat_ms: u64,
) -> anyhow::Result<()> {
    heartbeat_worker_registration(pool, redis_prefix, registration, heartbeat_ms).await
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkerConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub redis: RedisConfig,
    #[serde(default)]
    pub worker: WorkerIdentityConfig,
    #[serde(default)]
    pub loops: LoopConfig,
    #[serde(default)]
    pub concurrency: ConcurrencyConfig,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub health_port: u16,
    pub metrics_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RedisConfig {
    pub url: String,
    pub pool_size: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkerIdentityConfig {
    pub id: String,
    pub redis_prefix: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoopConfig {
    pub idle_sleep_ms: u64,
    pub cancel_block_ms: u64,
    pub watchdog_interval_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub lease_seconds: u64,
    pub recover_limit: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConcurrencyConfig {
    pub global: usize,
    pub per_vendor: usize,
    pub per_model: usize,
    pub per_key: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TimeoutConfig {
    pub total_ms: u64,
    pub submit_ms: u64,
}

impl WorkerConfig {
    pub fn load() -> Result<Self, figment::Error> {
        Figment::from(Serialized::defaults(Self::default()))
            .merge(Toml::file("config/task-default.toml"))
            .merge(Env::prefixed("GW_TASK_").split("__"))
            .extract()
    }

    pub fn from_toml_str(input: &str) -> Result<Self, figment::Error> {
        Figment::from(Serialized::defaults(Self::default()))
            .merge(Toml::string(input))
            .extract()
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.server.health_port == 0 || self.server.metrics_port == 0 {
            anyhow::bail!("worker health and metrics ports must be non-zero");
        }
        if self.server.health_port == self.server.metrics_port {
            anyhow::bail!("worker health and metrics ports must differ");
        }
        if self.redis.pool_size == 0 {
            anyhow::bail!("worker redis.pool_size must be non-zero");
        }
        if self.worker.id.trim().is_empty() {
            anyhow::bail!("worker id must be non-empty");
        }
        Ok(())
    }

    pub fn idle_sleep(&self) -> Duration {
        Duration::from_millis(self.loops.idle_sleep_ms)
    }

    pub fn cancel_block(&self) -> Duration {
        Duration::from_millis(self.loops.cancel_block_ms)
    }

    pub fn watchdog_interval(&self) -> Duration {
        Duration::from_millis(self.loops.watchdog_interval_ms)
    }

    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.loops.heartbeat_interval_ms)
    }

    pub fn lease_extension(&self) -> Duration {
        Duration::from_secs(self.loops.lease_seconds)
    }

    pub fn total_timeout(&self) -> Duration {
        Duration::from_millis(self.timeouts.total_ms)
    }

    pub fn submit_timeout(&self) -> Duration {
        Duration::from_millis(self.timeouts.submit_ms)
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            health_port: 8081,
            metrics_port: 9091,
        }
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".to_string(),
            pool_size: 8,
        }
    }
}

impl Default for WorkerIdentityConfig {
    fn default() -> Self {
        Self {
            id: "task-worker".to_string(),
            redis_prefix: String::new(),
        }
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            idle_sleep_ms: 100,
            cancel_block_ms: 5_000,
            watchdog_interval_ms: 5_000,
            heartbeat_interval_ms: 15_000,
            lease_seconds: 60,
            recover_limit: 100,
        }
    }
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            global: 128,
            per_vendor: 32,
            per_model: 32,
            per_key: 32,
        }
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            total_ms: 120_000,
            submit_ms: 30_000,
        }
    }
}

/// Run the task worker process.
///
/// This is the library-side entrypoint equivalent to the original `main()`.
/// The binary `main.rs` installs jemalloc and calls this function.
pub async fn run() -> anyhow::Result<()> {
    init_tracing()?;
    let cfg = WorkerConfig::load().context("load task worker config")?;
    cfg.validate()?;
    init_metrics(cfg.server.metrics_port)?;

    let shutdown = CancellationToken::new();
    let pool = connect_redis(&cfg.redis.url, cfg.redis.pool_size).await?;
    let worker_id = generate_worker_id();
    let consumer_name = format!("{}-{worker_id}", cfg.worker.id);
    let registration = WorkerRegistration::new(worker_id, now_ms(), resolve_hostname()?);
    register_worker(&pool, &cfg.worker.redis_prefix, &registration)
        .await
        .context("register worker in redis")?;
    let contract =
        RedisTaskContract::new_with_prefix(pool.clone(), cfg.worker.redis_prefix.clone())
            .await
            .map_err(|err| anyhow::anyhow!("initialize redis task contract: {err}"))?;

    let registry = RunningTaskRegistry::default();
    let http = reqwest::Client::new();

    let exec_loop = ExecLoop::new(
        contract.clone(),
        http,
        ExecLoopConfig {
            consumer_name: consumer_name.clone(),
            idle_sleep: cfg.idle_sleep(),
            default_timeout: TaskTimeouts {
                total: cfg.total_timeout(),
                submit: cfg.submit_timeout(),
            },
            global_concurrency: cfg.concurrency.global,
            per_vendor_concurrency: cfg.concurrency.per_vendor,
            per_model_concurrency: cfg.concurrency.per_model,
            per_key_concurrency: cfg.concurrency.per_key,
            cancellation_token: shutdown.clone(),
            ..ExecLoopConfig::default()
        },
    );

    let cancel_loop = CancelLoop::new(
        contract.clone(),
        RedisCancelSource::new_with_prefix(pool.clone(), cfg.worker.redis_prefix.clone()),
        registry.clone(),
        CancelLoopConfig {
            consumer_name: format!("{}-cancel", consumer_name),
            block_timeout: cfg.cancel_block(),
            cancellation_token: shutdown.clone(),
        },
    );

    let recovery = RedisLeaseRecoveryStore::new_with_prefix(
        pool.clone(),
        cfg.worker.redis_prefix.clone(),
        cfg.loops.lease_seconds,
    )
    .await?;
    let watchdog_loop = WatchdogLoop::new(
        contract.clone(),
        recovery,
        WatchdogLoopConfig {
            check_interval: cfg.watchdog_interval(),
            recover_limit: cfg.loops.recover_limit,
            cancellation_token: shutdown.clone(),
        },
    );
    let heartbeat_loop = HeartbeatLoop::new(
        contract,
        registry,
        HeartbeatLoopConfig {
            heartbeat_interval: cfg.heartbeat_interval(),
            lease_extension: cfg.lease_extension(),
            cancellation_token: shutdown.clone(),
        },
    );
    let registration_loop = WorkerRegistrationLoop::new(
        pool.clone(),
        cfg.worker.redis_prefix.clone(),
        registration,
        WorkerRegistrationLoopConfig {
            heartbeat_interval: cfg.heartbeat_interval(),
            cancellation_token: shutdown.clone(),
        },
    );

    let handles: Vec<JoinHandle<anyhow::Result<()>>> = vec![
        tokio::spawn(async move { exec_loop.run().await }),
        tokio::spawn(async move { cancel_loop.run().await }),
        tokio::spawn(async move { watchdog_loop.run().await }),
        tokio::spawn(async move { heartbeat_loop.run().await }),
        tokio::spawn(async move { registration_loop.run().await }),
        tokio::spawn(run_health_server(cfg.server.health_port, shutdown.clone())),
    ];

    tokio::signal::ctrl_c()
        .await
        .context("listen for shutdown signal")?;
    shutdown.cancel();
    for handle in handles {
        handle.await.context("join task worker loop")??;
    }
    Ok(())
}

fn init_tracing() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,gw_task=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_current_span(false))
        .try_init()
        .map_err(|err| anyhow::anyhow!("tracing init failed: {err}"))
}

fn init_metrics(port: u16) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .context("invalid metrics address")?;
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .context("install prometheus exporter")?;
    metrics::counter!("gw_task_worker_started_total").increment(1);
    Ok(())
}

async fn connect_redis(url: &str, pool_size: usize) -> anyhow::Result<RedisPool> {
    let cfg = FredRedisConfig::from_url(url).context("parse redis url")?;
    let pool = RedisPool::new(
        cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        pool_size,
    )?;
    pool.init().await.context("initialize redis pool")?;
    Ok(pool)
}

async fn run_health_server(port: u16, shutdown: CancellationToken) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .context("bind health listener")?;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept health connection")?;
                tokio::spawn(async move {
                    let service = service_fn(health_response);
                    if let Err(err) = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
                        tracing::warn!(error = %err, "health connection failed");
                    }
                });
            }
        }
    }
}

async fn health_response(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<http_body_util::Full<Bytes>>, Infallible> {
    let mut response = match req.uri().path() {
        "/livez" | "/readyz" => {
            Response::new(http_body_util::Full::new(Bytes::from_static(b"ok\n")))
        }
        _ => {
            let mut not_found = Response::new(http_body_util::Full::new(Bytes::from_static(
                b"not found\n",
            )));
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            not_found
        }
    };
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use testcontainers_modules::redis::Redis;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    #[test]
    fn test_worker_config_deserialize() {
        let cfg = WorkerConfig::from_toml_str(
            r#"
            [server]
            health_port = 8081
            metrics_port = 9091

            [redis]
            url = "redis://localhost:6379"
            pool_size = 4

            [worker]
            id = "worker-a"
            redis_prefix = "test:"

            [loops]
            idle_sleep_ms = 25
            cancel_block_ms = 1000
            watchdog_interval_ms = 5000
            heartbeat_interval_ms = 15000
            lease_seconds = 60
            recover_limit = 100

            [concurrency]
            global = 128
            per_vendor = 32
            per_model = 16
            per_key = 8

            [timeouts]
            total_ms = 120000
            submit_ms = 30000
            "#,
        )
        .expect("worker config deserializes");

        assert_eq!(cfg.server.health_port, 8081);
        assert_eq!(cfg.server.metrics_port, 9091);
        assert_eq!(cfg.redis.pool_size, 4);
        assert_eq!(cfg.worker.id, "worker-a");
        assert_eq!(cfg.worker.redis_prefix, "test:");
        assert_eq!(cfg.concurrency.global, 128);
        assert_eq!(cfg.timeouts.total_ms, 120_000);
    }

    #[test]
    fn config_struct_defaults_match_adr_t10_ports() {
        let cfg = WorkerConfig::default();

        assert_eq!(cfg.server.health_port, 8081);
        assert_eq!(cfg.server.metrics_port, 9091);
        assert_eq!(cfg.redis.pool_size, 8);
        assert_eq!(cfg.worker.redis_prefix, "");
        assert_eq!(cfg.loops.lease_seconds, 60);
    }

    async fn docker_pool() -> RedisPool {
        let node = Redis::default().start().await.unwrap();
        let port = node.get_host_port_ipv4(6379).await.unwrap();
        let cfg = FredRedisConfig::from_url(&format!("redis://127.0.0.1:{port}")).unwrap();
        let pool = RedisPool::new(
            cfg,
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::default()),
            1,
        )
        .unwrap();
        pool.init().await.unwrap();
        std::mem::forget(node);
        pool
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers Redis)"]
    async fn test_worker_registers_on_startup() {
        let pool = docker_pool().await;
        let registration = WorkerRegistration::new(
            "worker-test".to_string(),
            1_700_000_000_000,
            "host-a".to_string(),
        );

        register_worker(&pool, "", &registration).await.unwrap();

        let hash: HashMap<String, String> = pool.hgetall(registration.redis_key("")).await.unwrap();
        assert_eq!(hash.get("worker_id"), Some(&"worker-test".to_string()));
        assert_eq!(
            hash.get("start_time_ms"),
            Some(&"1700000000000".to_string())
        );
        assert_eq!(hash.get("status"), Some(&"running".to_string()));
        assert_eq!(hash.get("hostname"), Some(&"host-a".to_string()));
        assert_eq!(
            hash.get("last_heartbeat_ms"),
            Some(&"1700000000000".to_string())
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers Redis)"]
    async fn test_worker_heartbeat_updates() {
        let pool = docker_pool().await;
        let registration = WorkerRegistration::new(
            "worker-test".to_string(),
            1_700_000_000_000,
            "host-a".to_string(),
        );
        register_worker(&pool, "", &registration).await.unwrap();
        let loop_ = WorkerRegistrationLoop::new(
            pool.clone(),
            "".to_string(),
            registration.clone(),
            WorkerRegistrationLoopConfig::default(),
        );

        loop_.run_once_at(1_700_000_000_111).await.unwrap();

        let hash: HashMap<String, String> = pool.hgetall(registration.redis_key("")).await.unwrap();
        assert_eq!(hash.get("worker_id"), Some(&"worker-test".to_string()));
        assert_eq!(
            hash.get("last_heartbeat_ms"),
            Some(&"1700000000111".to_string())
        );
    }
}
