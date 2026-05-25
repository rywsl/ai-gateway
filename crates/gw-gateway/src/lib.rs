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
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::ratelimit::RateLimitGuard;
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_proxy::resilience::ResilienceState;
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
    latency_tracker: Arc<gw_routing::LatencyTracker>,
    ratelimit: Arc<RateLimitGuard>,
    ledger: Arc<Ledger>,
    counters: Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    signature_required: bool,
    business_port: u16,
    admin_upstream_url: url::Url,
    // --- Fields wired during bootstrap ---
    /// JWT claim validator (T1-E). `None` until T1-E builds it from
    /// `AuthConfig.global`.
    jwt_validator: Option<Arc<gw_auth::jwt::JwtValidator>>,
    /// Shared selection-strategy instances (T0-B).
    strategies: Arc<gw_routing::strategy::set::StrategySet>,
    /// Capability resolver (T3-C). `None` until wired.
    capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
    /// TunnelStack (T5-B). `Some` iff `cfg.tunnel.enabled = true`.
    #[cfg(feature = "tunnel")]
    tunnel: Option<Arc<gw_proxy::tunnel_stack::TunnelStack>>,
    /// Management-token blacklist + JWT blacklist (T3-Pre wiring). Threaded
    /// through `LlmGateway`'s request filter and the admin `/healthz/*`
    /// surface so T3-B / T3-C can implement the verify chain.
    mgmt_blacklist: Arc<RedisJwtBlacklist>,
    /// Resilience state (T3-A error-handler listeners; T3-Pre wiring).
    /// Constructed once in bootstrap so the `/healthz/resilience` snapshot
    /// (T3-C) and the proxy error fanout (T3-A) share a single instance.
    resilience: Arc<ResilienceState>,
    /// Detailed-log archive (T10). Flusher pool started during bootstrap.
    detailed_archive: Arc<gw_ledger::detailed_archive::DetailedArchive>,
    /// InflightLedger (W2-T6). Tracks in-flight requests in Redis for
    /// crash-safety billing recovery (W4 reconciler reads the residuals).
    inflight: Arc<gw_ledger::inflight::InflightLedger>,
    /// Response deadline (W3-T3). From `cfg.billing.response_deadline_ms`.
    response_deadline_ms: u64,
    /// Cancel grace window (W3-T5). From `cfg.billing.cancel_grace_ms`.
    cancel_grace_ms: u64,
    /// Hard-cap configuration (ADR-024). From `cfg.auth.*`.
    hard_cap_cfg: gw_auth::hard_cap::HardCapCfg,
}

/// Library entrypoint. The bin's `main` calls this after installing the
/// jemalloc global allocator.
pub fn run() -> anyhow::Result<()> {
    // Pin rustls to the `ring` crypto provider. Both `aws-lc-rs` (rustls
    // 0.23's default feature) and `ring` (pingora-rustls direct dep) end up
    // linked in this build, so rustls refuses to auto-select at handshake
    // time and panics. Pick `ring` to match pingora-rustls + the
    // `e2e-fake-upstream` listener. `install_default` is set-once and
    // returns `Err` on a second call — ignored.
    let _ = rustls::crypto::ring::default_provider().install_default();

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
        wired.latency_tracker,
        wired.ratelimit,
        wired.ledger,
        wired.counters,
        wired.global_rpm,
        wired.admin_upstream_url,
        wired.trust_xff,
        wired.signature_required,
        wired.jwt_validator,
        wired.strategies,
        gw_routing::FailoverExecutor::default(),
        wired.capability_resolver,
        wired.mgmt_blacklist,
        wired.resilience,
        #[cfg(feature = "tunnel")]
        wired.tunnel,
        wired.detailed_archive,
        wired.inflight,
        wired.response_deadline_ms,
        wired.cancel_grace_ms,
        wired.hard_cap_cfg,
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
    let admin_upstream_url = url::Url::parse(&cfg.admin.upstream_url)
        .with_context(|| format!("invalid admin.upstream_url: {}", cfg.admin.upstream_url))?;
    match admin_upstream_url.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("admin.upstream_url must use http or https, got {scheme}"),
    }
    if admin_upstream_url.host_str().is_none() {
        anyhow::bail!("admin.upstream_url must include a host");
    }

    // 1. KMS — load and validate KEK material so config errors surface at
    // startup. The cache is moved into LoaderCtx so vendor_key decryption
    // works on the hot path.
    let kek_cache = kms::init(cfg).await.context("KMS init failed")?;

    // 2. Storage clients.
    let redis = gw_store::redis_client::RedisCtx::connect(cfg.redis.url_str(), cfg.redis.pool_size)
        .await
        .context("redis connect failed")?;

    let pg = gw_store::pg_client::connect(cfg.pgsql.url_str(), cfg.pgsql.max_connections)
        .await
        .context("postgres connect failed")?;

    // 3. EntityStore with loader. Cache misses on the *_or_load methods now
    // fall through to Redis L1 -> PG L2 -> Redis write-back.
    let loader = gw_store::LoaderCtx {
        redis: redis.clone(),
        pg: pg.clone(),
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

    let mut preloaded_ip_blocks = 0usize;
    let mut ip_block_offset = 0i64;
    loop {
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM ip_blocks WHERE deleted_at IS NULL ORDER BY id LIMIT 100 OFFSET $1",
        )
        .bind(ip_block_offset)
        .fetch_all(&pg)
        .await
        .context("preloading ip_blocks from postgres")?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            store
                .ipblock_or_load(id)
                .await
                .ok_or_else(|| anyhow::anyhow!("ip_block preload failed for id {id}"))?;
            preloaded_ip_blocks += 1;
        }
        ip_block_offset += ids.len() as i64;
    }
    tracing::info!("preloaded {} ip_block entries", preloaded_ip_blocks);

    // T3-Pre — build the mgmt-token / JWT blacklist on the same Redis pool
    // used by CounterStore and Ledger. Threaded onto Wired so both the
    // proxy request filter (T3-B) and the admin /healthz/* surface (T3-C)
    // observe a single instance. fred's RedisPool is Clone (Arc internally),
    // so unwrapping the Arc with `(*pool).clone()` hands the blacklist its
    // own pool handle without an extra refcount layer.
    let mgmt_blacklist = Arc::new(RedisJwtBlacklist::new((*redis.pool).clone()));

    // T3-Pre — single shared ResilienceState. The proxy error fanout
    // (T3-A) and the /healthz/resilience snapshot (T3-C) must observe the
    // same counters, so construction happens once here and the Arc is
    // threaded through LlmGateway + HealthState.
    let resilience = Arc::new(ResilienceState::default());

    // 4. Ledger flusher.
    let ledger = Arc::new(Ledger::start(
        redis.clone(),
        cfg.ledger.channel_capacity,
        cfg.ledger.batch_size,
        Duration::from_millis(cfg.ledger.flush_interval_ms),
    ));

    // 4b. Detailed-log archive (T10). Workers is auto when configured to 0:
    // use 1/4 of available parallelism, minimum 1.
    let archive_cfg = gw_ledger::detailed_archive::DetailedArchiveCfg {
        stream: cfg.detailed_log.stream.clone(),
        maxlen: cfg.detailed_log.maxlen,
        channel_capacity: cfg.detailed_log.channel.capacity,
        workers: if cfg.detailed_log.flusher.workers == 0 {
            (std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                / 4)
            .max(1)
        } else {
            cfg.detailed_log.flusher.workers
        },
        single_payload_soft_cap_bytes: cfg.detailed_log.limits.single_payload_soft_cap_bytes,
        compression: gw_ledger::encoding::CompressionCfg {
            thresholds: gw_ledger::encoding::TierThresholds {
                small_threshold_bytes: cfg.detailed_log.compression.small_threshold_bytes,
                medium_threshold_bytes: cfg.detailed_log.compression.medium_threshold_bytes,
            },
            zstd_level: cfg.detailed_log.compression.zstd_level,
        },
    };
    let detailed_archive = Arc::new(gw_ledger::detailed_archive::DetailedArchive::start(
        (*redis.pool).clone(),
        archive_cfg,
    ));

    // 4c. DEDUCT DashMap GC (S-ledger T3). Periodically removes zero-state
    // entries from the in-memory DEDUCT map to prevent unbounded growth.
    // The JoinHandle is intentionally dropped: the spawned task lives for the
    // life of the bootstrap runtime (which is parked on pending()).
    let _deduct_gc_handle = gw_ledger::DeductGc::start_default();

    // 4d. Billing instance_id + InflightLedger (W2-T6).
    // instance_id is stable across restarts on the same host (stored in Redis
    // under `gateway:instance:<hostname>`). The heartbeat task keeps the TTL
    // alive so the W4 reconciler knows the gateway is live.
    let hostname = hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());
    let instance_id = gw_ledger::instance_id::resolve(&redis.pool, &hostname)
        .await
        .context("billing instance_id resolve failed")?;
    tracing::info!(
        target: "gw_gateway",
        %instance_id,
        %hostname,
        "billing instance_id resolved"
    );
    gw_ledger::instance_id::spawn_heartbeat_task((*redis.pool).clone(), instance_id.clone());
    gw_ledger::inflight_reaper::spawn(
        (*redis.pool).clone(),
        instance_id.clone(),
        cfg.billing.reaper_interval_secs,
        cfg.billing.emit_lag_t0_secs,
    );
    let inflight = Arc::new(gw_ledger::inflight::InflightLedger::new(
        (*redis.pool).clone(),
        instance_id.clone(),
    ));

    // W4-T4 — Startup reconciler: scan billing:inflight:<instance_id>:* for
    // residuals from the previous boot and emit CrashFallback UsageRecords.
    // Runs before traffic starts (bootstrap completes, then Pingora starts).
    {
        let pool_for_recon = (*redis.pool).clone();
        let inst_for_recon = instance_id.clone();
        let emit_ledger = ledger.clone();
        let emit = move |r: gw_ledger::usage::UsageRecord| {
            let l = emit_ledger.clone();
            Box::pin(async move {
                l.emit(r).await;
                Ok(())
            })
                as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
        };
        match gw_ledger::inflight_reconciler::run(&pool_for_recon, &inst_for_recon, emit).await {
            Ok(n) if n > 0 => tracing::warn!(
                target: "gw_gateway",
                recovered = n,
                "InflightReconciler: emitted CrashFallback records from prior boot"
            ),
            Ok(_) => tracing::info!(
                target: "gw_gateway",
                "InflightReconciler: no residuals from prior boot"
            ),
            Err(e) => tracing::error!(
                target: "gw_gateway",
                error = ?e,
                "InflightReconciler startup failed"
            ),
        }
    }

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

    // T1-E (G6) — build the hybrid JwtValidator if AuthConfig.global is
    // available. Absent → validator stays None and request_filter falls back
    // to the legacy `verify_jwt` so deployments without AuthConfig keep
    // working with v1 semantics (exp-only, JWKS-by-kid).
    let jwt_validator: Option<Arc<gw_auth::jwt::JwtValidator>> = match store
        .auth_config_or_load("global")
        .await
    {
        Some(cfg_v) => {
            let vcfg = gw_auth::jwt::JwtValidatorConfig {
                static_secret_hs256: cfg_v.value.static_secret_hs256.clone(),
                static_pem_rs256: cfg_v.value.static_pem_rs256.clone(),
                expected_iss: cfg_v.value.expected_iss.clone(),
                expected_aud: cfg_v.value.expected_aud.clone(),
                leeway_secs: cfg_v.value.leeway_secs,
                require_nbf: cfg_v.value.require_nbf,
            };
            let mut v = gw_auth::jwt::JwtValidator::new(vcfg);
            v.attach_jwks(Arc::new(gw_auth::jwt::EntityStoreJwksResolver {
                store: store.clone(),
            }));
            tracing::info!(
                target: "gw_gateway",
                "JwtValidator wired from AuthConfig.global (G6, T1-E)"
            );
            Some(Arc::new(v))
        }
        None => {
            tracing::warn!(
                target: "gw_gateway",
                "auth_configs.global missing; falling back to legacy JWT exp-only validation. \
                 Populate the `global` row via the management plane to enable iss/aud/nbf checks (0006_auth_configs)."
            );
            None
        }
    };

    // G13 (T3-C) — production capability resolver backed by the EntityStore.
    // `Model.capabilities` / `ApiKeyEntry.force_capabilities` /
    // `ApiKeyEntry.match_mode_exclusive` drive the resolved requirement set.
    let capability_resolver = Some(Arc::new(gw_capability::CapabilityResolver::new(Arc::new(
        gw_capability::EntityStoreResolveBackend::new(store.clone()),
    ))));

    // T5 / ADR-T-2 — Build TunnelStack iff `cfg.tunnel.enabled`. Skipping the
    // entire stack on `false` keeps the byte-identical Direct path: pick_peer
    // sees `tunnel = None` and never branches.
    #[cfg(feature = "tunnel")]
    let tunnel: Option<Arc<gw_proxy::tunnel_stack::TunnelStack>> = if cfg.tunnel.enabled {
        use gw_tunnel_client::pool::PoolConfig;
        use gw_tunnel_client::resolver::ProxyNodeView;
        use gw_tunnel_client::safe_dns::SafeDnsResolver;
        use gw_tunnel_client::target_filter;
        use gw_tunnel_client::transport::TransportConfig;

        // Whitelist mapping: "host:port" strings → Vec<(String, u16)>.
        let mut wl = Vec::with_capacity(cfg.tunnel.target_whitelist.len());
        for entry in &cfg.tunnel.target_whitelist {
            // IPv6 hosts must be bracketed: "[::1]:443" parses correctly because
            // rsplit_once finds the last ':'. Bare "::1:443" would parse to host
            // "::1" — not supported in v1.
            let (h, p) = entry.rsplit_once(':').ok_or_else(|| {
                anyhow::anyhow!("tunnel.target_whitelist entry missing ':' — {entry}")
            })?;
            let port: u16 = p.parse().map_err(|_| {
                anyhow::anyhow!("tunnel.target_whitelist entry has non-numeric port — {entry}")
            })?;
            wl.push((h.to_string(), port));
        }
        target_filter::set_whitelist(wl);

        let pool_cfg = PoolConfig {
            heartbeat_interval: Duration::from_secs(cfg.tunnel.heartbeat_interval_secs as u64),
            max_streams_per_conn: cfg.tunnel.max_streams_per_conn,
            backoff_min: Duration::from_secs(cfg.tunnel.backoff_min_secs as u64),
            backoff_max: Duration::from_secs(cfg.tunnel.backoff_max_secs as u64),
            backoff_jitter: 0.2,
        };
        let transport_template = TransportConfig {
            url: "ws://placeholder:0/".parse().unwrap(), // overwritten per node in peer_for_node
            heartbeat_interval: pool_cfg.heartbeat_interval,
            missed_pong_threshold: cfg.tunnel.missed_pong_threshold,
            max_streams_per_conn: cfg.tunnel.max_streams_per_conn,
        };
        let dns = Arc::new(SafeDnsResolver::cloudflare_default());

        fn node_to_view(
            n: gw_core::entity::ProxyNode,
        ) -> gw_tunnel_client::resolver::ProxyNodeView {
            gw_tunnel_client::resolver::ProxyNodeView {
                id: n.id,
                ws_url: n.ws_url,
                enabled: n.enabled,
                latency_ms: n.latency_ms,
                allowed_provider_ids: n.allowed_provider_ids,
                allowed_vendor_key_ids: n.allowed_vendor_key_ids,
            }
        }

        // Initial node snapshot. Plan defines ProxyNode -> ProxyNodeView mapping;
        // including `ws_url` (added in T3-A).
        let initial_nodes: Vec<ProxyNodeView> = store
            .proxy_nodes_list_or_load()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(node_to_view)
            .collect();

        let stack = Arc::new(gw_proxy::tunnel_stack::TunnelStack::new(
            initial_nodes,
            dns,
            pool_cfg,
            transport_template,
        ));

        // Refresh-on-invalidation: spawn a 5s polling watcher. The stream
        // consumer in gw-store already invalidates the Moka cache on
        // `proxy_node` events; this watcher pushes the refreshed snapshot
        // into the ArcSwap inside TunnelStack. v1 uses polling because
        // `stream_consumer` does not yet expose a per-namespace callback.
        let stack_clone = stack.clone();
        let store_clone = store.clone();
        tokio::spawn(async move {
            let mut last_signature: u64 = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Some(nodes) = store_clone.proxy_nodes_list_or_load().await {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    // DefaultHasher uses a randomised per-process seed (since Rust 1.36), so
                    // the signature is intra-process only — it'll spuriously reload once on
                    // every restart, which is harmless.
                    let sig = nodes.iter().fold(0u64, |acc, n| {
                        let mut h = DefaultHasher::new();
                        n.id.hash(&mut h);
                        n.enabled.hash(&mut h);
                        n.latency_ms.hash(&mut h);
                        acc ^ h.finish()
                    });
                    if sig != last_signature {
                        last_signature = sig;
                        let view: Vec<gw_tunnel_client::resolver::ProxyNodeView> =
                            nodes.into_iter().map(node_to_view).collect();
                        stack_clone.reload_nodes(view);
                    }
                }
            }
        });

        let _probe_handle = stack.start_probe_loop(Duration::from_secs(5));

        tracing::info!(target: "gw_gateway", "TunnelStack constructed (tunnel.enabled=true)");
        Some(stack)
    } else {
        tracing::info!(target: "gw_gateway", "tunnel.enabled=false; skipping TunnelStack");
        None
    };

    // 6. Latency tracker — extracted early to share with HealthState.
    let latency_tracker = Arc::new(gw_routing::LatencyTracker::new());

    // 7. Health server (positioned after TunnelStack so `tunnel` is in scope).
    let health_port = cfg.server.health_port;
    let health_state = health::HealthState {
        key_health: health.clone(),
        resilience: resilience.clone(),
        latency_tracker: latency_tracker.clone(),
        tunnel_enabled: cfg.tunnel.enabled,
        #[cfg(feature = "tunnel")]
        tunnel_stack: tunnel.clone(),
    };
    let health_store = store.clone();
    let health_blacklist = mgmt_blacklist.clone();
    tokio::spawn(async move {
        if let Err(e) =
            health::run_health_server(health_port, health_state, health_store, health_blacklist)
                .await
        {
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
        latency_tracker,
        ratelimit,
        ledger,
        counters,
        global_rpm: cfg.ratelimit.global_rpm,
        admin_upstream_url,
        trust_xff: cfg.security.trust_xff,
        signature_required: cfg.signature.required,
        business_port: cfg.server.business_port,
        jwt_validator,
        strategies: Arc::new(gw_routing::strategy::set::StrategySet::new(
            Duration::from_secs(cfg.routing.sticky_ttl_secs),
        )),
        capability_resolver,
        #[cfg(feature = "tunnel")]
        tunnel,
        mgmt_blacklist,
        resilience,
        detailed_archive,
        inflight,
        response_deadline_ms: cfg.billing.response_deadline_ms,
        cancel_grace_ms: cfg.billing.cancel_grace_ms,
        hard_cap_cfg: gw_auth::hard_cap::HardCapCfg {
            max_input_tokens: cfg.auth.hard_cap_max_input_tokens,
            max_cost_micro: cfg.auth.hard_cap_max_cost_micro,
        },
    })
}
