use std::convert::Infallible;
use std::net::SocketAddr;

use anyhow::Context;
use fred::clients::RedisPool;
use fred::interfaces::ClientLike;
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
use gw_task::redis_contract::RedisTaskContract;
use gw_task_runtime::cancel_loop::{
    CancelLoop, CancelLoopConfig, RedisCancelSource, RunningTaskRegistry,
};
use gw_task_runtime::exec_loop::{ExecLoop, ExecLoopConfig, TaskTimeouts};
use gw_task_runtime::heartbeat_loop::{HeartbeatLoop, HeartbeatLoopConfig};
use gw_task_runtime::watchdog_loop::{RedisLeaseRecoveryStore, WatchdogLoop, WatchdogLoopConfig};
use gw_task_worker::{
    generate_worker_id, now_ms, register_worker, resolve_hostname, WorkerConfig,
    WorkerRegistration, WorkerRegistrationLoop, WorkerRegistrationLoopConfig,
};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let cfg = RedisConfig::from_url(url).context("parse redis url")?;
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
