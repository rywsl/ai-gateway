//! `gw-gateway` — library half of the binary.
//!
//! The bin entrypoint in `main.rs` only wires the jemalloc allocator and
//! delegates to [`run`]. Splitting the runnable code out as a library makes
//! integration tests (Phase 6) able to call [`run`] without invoking the bin.
//!
//! ## Bootstrap shape
//!
//! [`bootstrap`] constructs a [`gw_store::LoaderCtx`] from the KMS-backed KEK
//! cache, the Redis pool, and the PgSQL pool, then hands it to
//! [`gw_store::EntityStore::with_loader`] so cache misses on the hot path
//! cold-load from Redis L1 → PG L2. It also starts the
//! [`gw_store::stream_consumer::StreamConsumer`] so cross-pod invalidation
//! events keep caches fresh.
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
pub mod metrics_catalog;
pub mod observability;

#[cfg(feature = "tunnel")]
pub use gw_tunnel_client;

use anyhow::Context;
use gw_auth::ratelimit::RateLimitGuard;
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_proxy::LlmGateway;
use gw_routing::in_flight::InFlightStore;
use gw_store::{CounterStore, EntityStore};
use std::sync::Arc;
use std::time::Duration;

/// Bundle of `Arc`-shared collaborators handed off to Pingora as the
/// `LlmGateway` payload. The fields mirror `LlmGateway`'s constructor.
struct Wired {
    store: Arc<EntityStore>,
    health: Arc<KeyHealthStore>,
    in_flight: Arc<InFlightStore>,
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
    // Detach the bootstrap thread. Dropping its JoinHandle does not stop the
    // thread — the runtime inside it is parked on `pending()`, keeping the
    // background tasks (Ledger flusher, health server, StreamConsumer) alive
    // for the process lifetime.
    std::mem::drop(bootstrap_handle);

    // Pingora server lives on the main thread and never returns.
    let mut server = pingora::server::Server::new(None)
        .map_err(|e| anyhow::anyhow!("pingora Server::new failed: {e}"))?;
    server.bootstrap();

    let gw = LlmGateway::new(
        wired.store,
        wired.health,
        wired.in_flight,
        wired.ratelimit,
        wired.ledger,
        wired.counters,
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
    // startup. The cache is moved into LoaderCtx so vendor_key decryption
    // works on the hot path.
    let kek_cache = kms::init(cfg).await.context("KMS init failed")?;

    // 2. Storage clients.
    let redis = gw_store::redis_client::RedisCtx::connect(
        cfg.redis.url_str(),
        cfg.redis.pool_size,
    )
    .await
    .context("redis connect failed")?;

    let pg = gw_store::pg_client::connect(cfg.pgsql.url_str(), cfg.pgsql.max_connections)
        .await
        .context("postgres connect failed")?;

    // 3. EntityStore with loader. Cache misses on the *_or_load methods now
    // fall through to Redis L1 -> PG L2 -> Redis write-back.
    let loader = gw_store::LoaderCtx {
        redis: redis.clone(),
        pg,
        kek: kek_cache,
    };
    let store = Arc::new(gw_store::EntityStore::with_loader_and_timeout(
        loader,
        std::time::Duration::from_millis(cfg.loader.cold_load_timeout_ms),
    ));
    let health = Arc::new(KeyHealthStore::default());
    let in_flight = Arc::new(InFlightStore::default());
    let ratelimit = Arc::new(RateLimitGuard::new());

    let counters = Arc::new(CounterStore {
        redis: redis.clone(),
    });

    // 4. Ledger flusher.
    let ledger = Arc::new(Ledger::start(
        redis.clone(),
        cfg.ledger.channel_capacity,
        cfg.ledger.batch_size,
        Duration::from_millis(cfg.ledger.flush_interval_ms),
    ));

    // 5. Stream consumer — invalidate-on-event for cross-pod cache freshness.
    // The consumer's join handle is intentionally dropped: the task lives for
    // the life of the bootstrap runtime (which itself is parked on pending()).
    let consumer = Arc::new(gw_store::stream_consumer::StreamConsumer {
        redis: redis.clone(),
        store: store.clone(),
        instance_id: format!("gw-{}", std::process::id()),
    });
    match consumer.start().await {
        Ok(_handle) => {
            tracing::info!(
                target: "gw_gateway",
                "stream consumer started (events:entity, events:counter) with XREADGROUP invalidate-on-event"
            );
        }
        Err(e) => {
            // Non-fatal: TTL-based eviction will keep caches fresh on a longer
            // horizon. Log and proceed.
            tracing::warn!(
                target: "gw_gateway",
                error = %e,
                "stream consumer failed to start; falling back to TTL-only invalidation"
            );
        }
    }

    // 6. Health server.
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
        health,
        in_flight,
        ratelimit,
        ledger,
        counters,
        global_rpm: cfg.ratelimit.global_rpm,
        trust_xff: cfg.security.trust_xff,
        business_port: cfg.server.business_port,
    })
}
