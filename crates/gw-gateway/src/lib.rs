//! `gw-gateway` — library half of the binary.
//!
//! The bin entrypoint in `main.rs` only wires the jemalloc allocator and
//! delegates to [`run`]. Splitting the runnable code out as a library makes
//! integration tests (Phase 6) able to call [`run`] without invoking the bin.
//!
//! ## Bootstrap shape
//!
//! Pingora's [`pingora::server::Server::run_forever`] consumes the server and
//! never returns (`-> !`), and it manages its own internal tokio runtimes per
//! service. Our long-lived background tasks (Ledger flusher, health server,
//! Prometheus exporter) need a tokio runtime that *outlives* the call to
//! `run_forever`, so we:
//!
//!   1. Spawn a dedicated OS thread that owns a multi-threaded tokio runtime.
//!      All async init (KMS, Redis, PgSQL, Ledger::start, health server) runs
//!      on this runtime, and its background tasks (mpsc flusher, health
//!      accept loop) continue running for the life of the process.
//!   2. Once init completes, the bootstrap thread signals readiness back to
//!      `main` via a `std::sync::mpsc` channel carrying the wired-up
//!      [`gw_proxy::LlmGateway`] dependencies.
//!   3. `main` constructs the Pingora `Server`, attaches the proxy service,
//!      and calls `run_forever()` on the main thread.
//!
//! `Ledger::try_send` (sync, just an `mpsc::Sender::try_send`) is callable
//! from pingora's worker threads even though the receiver lives on our
//! bootstrap runtime — tokio mpsc is runtime-agnostic on the send side as
//! long as the receiver's runtime is alive somewhere in the process.

pub mod config;
pub mod health;
pub mod kms;
pub mod observability;

use anyhow::Context;
use gw_auth::ratelimit::RateLimitGuard;
use gw_ledger::Ledger;
use gw_proxy::LlmGateway;
use gw_routing::{BreakerStore, RetryConfig};
use gw_store::{CounterStore, EntityStore};
use std::sync::Arc;
use std::time::Duration;

/// Bundle of `Arc`-shared collaborators handed off to Pingora as the
/// `LlmGateway` payload. The fields mirror `LlmGateway`'s constructor.
struct Wired {
    store: Arc<EntityStore>,
    breakers: Arc<BreakerStore>,
    ratelimit: Arc<RateLimitGuard>,
    ledger: Arc<Ledger>,
    counters: Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    business_port: u16,
}

/// Library entrypoint. The bin's `main` calls this after installing the
/// jemalloc global allocator.
pub fn run() -> anyhow::Result<()> {
    let cfg = config::load().context("config load failed")?;
    observability::init(&cfg).context("observability init failed")?;

    tracing::info!(
        target: "gw_gateway",
        business_port = cfg.server.business_port,
        metrics_port = cfg.server.metrics_port,
        health_port = cfg.server.health_port,
        "gw-gateway starting"
    );

    // Bootstrap thread owns the tokio runtime that hosts:
    //   - async init (KMS, Redis, PgSQL)
    //   - the Ledger flusher background task
    //   - the /livez /readyz HTTP server
    //   - (future) stream consumer, health checker, key scanner
    //
    // The handoff channel carries `Wired` so the main thread can build the
    // Pingora service once init succeeds.
    let (tx, rx) = std::sync::mpsc::sync_channel::<anyhow::Result<Wired>>(1);

    let bootstrap_handle = std::thread::Builder::new()
        .name("gw-bootstrap".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("gw-bg")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(anyhow::anyhow!("tokio runtime build failed: {e}")));
                    return;
                }
            };

            let outcome = rt.block_on(async { bootstrap(&cfg).await });

            match outcome {
                Ok(wired) => {
                    let _ = tx.send(Ok(wired));
                    // Keep the runtime parked indefinitely so the spawned
                    // background tasks (Ledger flusher, health server) keep
                    // running for the life of the process.
                    rt.block_on(async {
                        std::future::pending::<()>().await;
                    });
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        })
        .context("spawning bootstrap thread")?;

    let wired = rx
        .recv()
        .context("bootstrap thread exited before signalling")??;
    // The bootstrap thread now parks on `pending`; we intentionally leak the
    // handle. Dropping the JoinHandle does not terminate the thread.
    std::mem::drop(bootstrap_handle);

    // Pingora server lives on the main thread and never returns.
    let mut server = pingora::server::Server::new(None)
        .map_err(|e| anyhow::anyhow!("pingora Server::new failed: {e}"))?;
    server.bootstrap();

    let gw = LlmGateway::new(
        wired.store,
        wired.breakers,
        wired.ratelimit,
        wired.ledger,
        wired.counters,
        RetryConfig::default(),
        wired.global_rpm,
        wired.trust_xff,
    );

    let mut business = pingora_proxy::http_proxy_service(&server.configuration, gw);
    business.add_tcp(&format!("0.0.0.0:{}", wired.business_port));
    server.add_service(business);

    tracing::info!(
        target: "gw_gateway",
        port = wired.business_port,
        "pingora business service starting"
    );

    // Flip readiness right before `run_forever()`. `add_service` above
    // registers the service synchronously and pingora binds the listening
    // sockets prior to entering the run loop, so by the time external probes
    // reach `/readyz` the business port is accepting connections. Flipping
    // earlier (e.g. inside `bootstrap()`) would create a window where
    // `/readyz` returns 200 but the business port is still refused.
    health::READY.store(true, std::sync::atomic::Ordering::Release);

    // `run_forever` consumes the server and is `-> !`, so this call never
    // returns. The trailing `Ok(())` below is unreachable but keeps the
    // function signature `anyhow::Result<()>` for callers/tests.
    server.run_forever();
}

async fn bootstrap(cfg: &config::Config) -> anyhow::Result<Wired> {
    // 1. KMS — load and validate KEK material so config errors surface at
    // startup. The cache is dropped at the end of this function; when the
    // loader path is wired to consume encrypted vendor keys, promote
    // `kek_cache` into the `Wired` struct so it survives across `bootstrap()`.
    let _kek_cache = kms::init(cfg).await.context("KMS init failed")?;

    // 2. Storage clients. Both calls validate connectivity before returning.
    let redis = gw_store::redis_client::RedisCtx::connect(
        cfg.redis.url_str(),
        cfg.redis.pool_size,
    )
    .await
    .context("redis connect failed")?;

    let pg = gw_store::pg_client::connect(cfg.pgsql.url_str(), cfg.pgsql.max_connections)
        .await
        .context("postgres connect failed")?;
    // Pg pool is held for future loader use; loader runs on demand from
    // proxy filters and will pull a connection from the pool there.
    let _ = pg;

    // 3. In-memory caches and guards.
    let store = Arc::new(EntityStore::new());
    let breakers = Arc::new(BreakerStore::default());
    let ratelimit = Arc::new(RateLimitGuard::new());

    // Counter store — shared Redis-backed read/write surface for the balance
    // enforcement check in `request_filter` and the per-user TPM / RPM
    // counters the ledger flusher debits. Cloning `RedisCtx` is cheap (just
    // bumps an `Arc<RedisPool>`).
    let counters = Arc::new(CounterStore {
        redis: redis.clone(),
    });

    // 4. Ledger flusher — spawns a long-lived `tokio::spawn` on the current
    // runtime. The runtime is parked (not dropped) once bootstrap completes
    // so this task continues running.
    let ledger = Arc::new(Ledger::start(
        redis.clone(),
        cfg.ledger.channel_capacity,
        cfg.ledger.batch_size,
        Duration::from_millis(cfg.ledger.flush_interval_ms),
    ));

    // 5. Health server. Spawned before returning so /livez is reachable
    // immediately; /readyz will return 503 until the main thread flips
    // `health::READY` right before `server.run_forever()`.
    let health_port = cfg.server.health_port;
    tokio::spawn(async move {
        if let Err(e) = health::run_health_server(health_port).await {
            tracing::error!(
                target: "gw_gateway::health",
                error = %e,
                "health server exited unexpectedly"
            );
        }
    });

    Ok(Wired {
        store,
        breakers,
        ratelimit,
        ledger,
        counters,
        global_rpm: cfg.ratelimit.global_rpm,
        trust_xff: cfg.security.trust_xff,
        business_port: cfg.server.business_port,
    })
}
