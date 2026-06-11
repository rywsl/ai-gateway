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
pub mod control_plane;
pub mod health;
pub mod kms;
pub mod metrics_catalog;
pub mod observability;
pub mod quota_probe_transport;

#[cfg(feature = "tunnel")]
pub use gw_tunnel_client;

use anyhow::Context;
use gw_auth::ip_allowlist::{IpAllowlistGuard, IpAllowlistMode};
use gw_auth::ip_ratelimit::IpRateLimitGuard;
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::ratelimit::adaptive::{AdaptiveCfg, AdaptiveRpmManager};
use gw_auth::ratelimit::RateLimitGuard;
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_proxy::resilience::ResilienceState;
use gw_proxy::LlmGateway;
use gw_routing::in_flight::InFlightStore;
use gw_routing::{ModelFrequencyTracker, ReservationGlobalView};
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
    reservation_view: Option<Arc<ReservationGlobalView>>,
    frequency_tracker: Option<Arc<ModelFrequencyTracker>>,
    ratelimit: Arc<RateLimitGuard>,
    ip_allowlist: Arc<IpAllowlistGuard>,
    ip_ratelimit: Arc<IpRateLimitGuard>,
    adaptive_rpm: Arc<AdaptiveRpmManager>,
    ip_allowlist_mode: IpAllowlistMode,
    ledger: Arc<Ledger>,
    counters: Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    signature_required: bool,
    task_enabled: bool,
    task_contract: Option<Arc<gw_task::redis_contract::RedisTaskContract>>,
    task_billing_config: Option<gw_billing::TaskBillingConfig>,
    business_port: u16,
    admin_upstream_url: url::Url,
    // --- Fields wired during bootstrap ---
    /// Hot-reloadable JWT claim validator (T1-E + R7b). The handle holds `None`
    /// until `AuthConfig.global` is loaded; the `events:entity ns=auth_config`
    /// arm swaps it in place so policy rotation needs no restart.
    jwt_validator: Arc<arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator>>,
    /// Shared selection-strategy instances (T0-B).
    strategies: Arc<gw_routing::strategy::set::StrategySet>,
    /// API-key affinity table consulted by proxy routing and cleared by
    /// vendor-key invalidation hooks.
    affinity: Arc<gw_routing::affinity::AffinityManager>,
    /// Capability resolver (T3-C). `None` until wired.
    capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
    /// TunnelStack (T5-B). `Some` iff `cfg.tunnel.enabled = true`.
    #[cfg(feature = "tunnel")]
    tunnel: Option<Arc<gw_proxy::tunnel_stack::TunnelStack>>,
    #[cfg(feature = "tunnel")]
    _tunnel_bridge: Option<gw_proxy::tunnel_bridge::TunnelBridgeHandle>,
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
    /// Credit-lease protection-mode config (Spec 2026-05-29-credit-lease-design).
    /// Threaded into `request_filter` so step 8.5 can decide whether to admit
    /// via the lease branch or fall through to the legacy soft-defense path.
    protection_cfg: gw_ledger::ProtectionConfig,
    classifier: Arc<dyn gw_core::error::ErrorClassifier>,
    adapter_registry: Arc<gw_adapter::AdapterRegistry>,
    oauth_runtime: Arc<dyn gw_adapter::OAuthRuntime>,
    quota_state_updates: Option<Arc<gw_store::quota_state::QuotaStateUpdateQueue>>,
    retryable_codes: Arc<std::collections::HashSet<u16>>,
    error_failover_total_budget_ms: u64,
    error_passthrough_enabled: bool,
    error_passthrough_rules: std::sync::Arc<Vec<gw_core::error::rules::PassthroughRule>>,
    body_rules: Arc<gw_proxy::filters::upstream::BodyRulesMap>,
    /// Egress impersonation loopback handle (Task 4.6). Constructed in the
    /// bootstrap runtime because `EgressHandle::start` calls `tokio::spawn`.
    egress: Arc<gw_egress_impersonate::EgressHandle>,
    /// Kill-switch for the egress-impersonate path (Task 5.2). When `true`,
    /// vendors with `egress_mode = Impersonate` are excluded from candidate
    /// selection; metric `gw_egress_disabled_total` ticks per dropped vendor.
    egress_disabled: bool,
    /// Identity-persist queue (Task 4.6). Spawns a background flush task, so
    /// it must be built inside the bootstrap runtime.
    identity_persist: Arc<gw_store::identity_persist::IdentityPersistQueue>,
    /// Per-conversation turn-state cache (Task 4.6).
    turn_state_cache: Arc<gw_core::impersonate::turn_state::TurnStateCache>,
    pg: sqlx::PgPool,
    // --- Resilience-fixes T2.1 runtime primitives ---
    /// Compiled-regex cache shared with future ModelPermission / passthrough
    /// rule consumers (T6). Held in `Wired` so filter wiring in later tasks
    /// can pick it up without re-plumbing bootstrap.
    #[allow(dead_code)]
    regex_cache: Arc<gw_runtime::RegexCache>,
    /// Force-majeure backpressure gate. T7.1 will consult this in early.rs;
    /// for now it just rides along so the wiring exists.
    #[allow(dead_code)]
    backpressure_gate: Arc<gw_runtime::BackpressureGate>,
    /// Two-phase drain token. T2.2 will install the SIGTERM handler that
    /// triggers `ShutdownCoordinator::drain`; long-running tasks supervised
    /// in T3 will observe this token.
    #[allow(dead_code)]
    shutdown_token: gw_runtime::ShutdownToken,
    /// Snapshot of the current shutdown phase. T2.3 will read this from
    /// `/readyz` to flip the listener to 503 during phase 1.
    #[allow(dead_code)]
    phase_handle: gw_runtime::PhaseHandle,
    /// Sharded append-only spill root. T4 migrates `EmergencyDlqWriter` and
    /// T5 the response archive on top of this.
    #[allow(dead_code)]
    spill_root: Arc<gw_runtime::SpillRoot>,
    /// Reporter handle for the RedisOomHook. T3 will pass this into the
    /// fred wrappers in `gw-store` / `gw-ledger` so they can `record()` on
    /// `OOM command not allowed when used memory > 'maxmemory'`.
    #[allow(dead_code)]
    redis_oom_reporter: gw_runtime::health::redis_oom::RedisOomReporter,
}

/// Build a hybrid [`gw_auth::jwt::JwtValidator`] from an `AuthConfig`, wiring
/// the production `EntityStoreJwksResolver`. Shared by boot wiring and the R7b
/// hot-reload path so both produce an identically-constructed validator.
fn build_validator_from_auth_config(
    store: &Arc<EntityStore>,
    cfg: &gw_core::entity::AuthConfig,
) -> gw_auth::jwt::JwtValidator {
    let vcfg = gw_auth::jwt::JwtValidatorConfig {
        static_secret_hs256: cfg.static_secret_hs256.clone(),
        static_pem_rs256: cfg.static_pem_rs256.clone(),
        expected_iss: cfg.expected_iss.clone(),
        expected_aud: cfg.expected_aud.clone(),
        leeway_secs: cfg.leeway_secs,
        require_nbf: cfg.require_nbf,
    };
    let mut v = gw_auth::jwt::JwtValidator::new(vcfg);
    v.attach_jwks(Arc::new(gw_auth::jwt::EntityStoreJwksResolver {
        store: store.clone(),
    }));
    v
}

/// R7b hot-reload: reload `auth_config` `id`, rebuild the validator, and swap
/// it into `handle`. KEEP-OLD-ON-FAIL — a reload miss / parse error / cold-load
/// timeout (all surfaced as `None`) leaves the existing validator in place and
/// only warns; the handle is never reset to `None` on a transient failure. A
/// genuinely deleted config is indistinguishable from a miss at this layer
/// (`load_auth_config` filters `deleted_at IS NULL`), so keep-old covers it too.
async fn rebuild_jwt_validator_and_swap(
    store: &Arc<EntityStore>,
    id: &str,
    handle: &arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator>,
) {
    match store.auth_config_or_load(id).await {
        Some(cfg_v) => {
            handle.store(Some(Arc::new(build_validator_from_auth_config(
                store,
                &cfg_v.value,
            ))));
            tracing::info!(
                target: "gw_gateway",
                id = %id,
                "JwtValidator hot-reloaded from auth_config (R7b)"
            );
        }
        None => {
            tracing::warn!(
                target: "gw_gateway",
                id = %id,
                "auth_config reload returned None (miss/parse/timeout/deleted); keeping existing JwtValidator (R7b keep-old-on-fail)"
            );
        }
    }
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

    let mut gw = LlmGateway::new(
        wired.store,
        wired.health,
        wired.in_flight,
        wired.latency_tracker,
        wired.ratelimit,
        wired.ip_allowlist,
        wired.ip_ratelimit,
        wired.adaptive_rpm,
        wired.ip_allowlist_mode,
        wired.ledger,
        wired.counters,
        wired.global_rpm,
        wired.admin_upstream_url,
        wired.trust_xff,
        wired.signature_required,
        wired.task_enabled,
        wired.task_contract,
        wired.task_billing_config,
        wired.jwt_validator,
        wired.strategies,
        wired.affinity,
        gw_routing::FailoverExecutor::with_total_budget_ms(3, wired.error_failover_total_budget_ms),
        wired.capability_resolver,
        wired.mgmt_blacklist,
        wired.resilience,
        #[cfg(feature = "tunnel")]
        wired.tunnel,
        wired.egress,
        wired.egress_disabled,
        wired.identity_persist,
        wired.turn_state_cache,
        wired.detailed_archive,
        wired.inflight,
        wired.response_deadline_ms,
        wired.cancel_grace_ms,
        wired.hard_cap_cfg,
        wired.protection_cfg.clone(),
        wired.classifier,
        wired.adapter_registry,
        wired.oauth_runtime,
        wired.quota_state_updates,
        wired.retryable_codes,
        wired.error_passthrough_enabled,
        wired.error_passthrough_rules,
        wired.reservation_view,
        wired.frequency_tracker,
        wired.pg.clone(),
    );
    gw.body_rules = wired.body_rules;

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

    // 0. Shared runtime primitives (resilience-fixes T2.1).
    //
    // Stand up the `gw-runtime` primitives early so subsequent bootstrap
    // steps (and the `Wired` handoff) can reference them. T2.1 is plumbing
    // only — the SIGTERM handler (T2.2), `/readyz` integration (T2.3), and
    // BackpressureGate admit check (T7.1) land in later tasks. Long-running
    // tasks spawned below still use bare `tokio::spawn`; T3 wraps them in
    // `Supervisor` and threads the `ShutdownToken` through.
    let (
        shutdown_token,
        phase_handle,
        spill_root,
        regex_cache,
        backpressure_gate,
        redis_oom_reporter,
        _monitor_handle,
    ) = {
        use gw_runtime::health::{
            disk::SpillDiskFullHook, fd::FdPressureHook, memory::MemoryPressureHook,
            redis_oom::RedisOomHook,
        };
        use gw_runtime::{
            BackpressureGate, RegexCache, RegexCacheConfig as RuntimeRegexCacheConfig,
            ShutdownCoordinator, SpillRoot, SpillRootConfig, SystemHealthMonitor,
        };

        let mut shutdown = ShutdownCoordinator::new();
        let shutdown_token = shutdown.token();
        let phase_handle = shutdown.phase_handle();

        let spill_root = Arc::new(SpillRoot::new(SpillRootConfig {
            root_dir: cfg.runtime.spill_io.root_dir.clone(),
            max_total_bytes: cfg.runtime.spill_io.max_total_bytes,
            buffer_size: cfg.runtime.spill_io.buffer_size,
        }));
        spill_root
            .ensure_dir()
            .await
            .with_context(|| {
                format!(
                    "ensuring spill root {:?} exists",
                    cfg.runtime.spill_io.root_dir
                )
            })?;

        let regex_cache = Arc::new(RegexCache::new(RuntimeRegexCacheConfig {
            capacity: cfg.runtime.regex_cache.capacity,
            tti: Duration::from_secs(cfg.runtime.regex_cache.tti_secs),
        }));

        let redis_oom_hook = RedisOomHook::new();
        let redis_oom_reporter = redis_oom_hook.handle();

        let mut monitor = SystemHealthMonitor::new(cfg.runtime.health.hysteresis_healthy_ticks);
        monitor
            .add_hook(MemoryPressureHook::new())
            .add_hook(FdPressureHook::new())
            .add_hook(SpillDiskFullHook::new(spill_root.root_dir().to_path_buf()))
            .add_hook(redis_oom_hook);

        let backpressure_state = monitor.state_handle();
        let backpressure_gate = Arc::new(BackpressureGate::from_state(backpressure_state));
        let monitor_handle = monitor.into_loop(Duration::from_millis(
            cfg.runtime.health.evaluate_interval_ms,
        ));
        shutdown.register(monitor_handle);

        // `shutdown` (the ShutdownCoordinator) is intentionally dropped here.
        // T2.2 will refactor `bootstrap()` to return it to the caller so the
        // signal handler can drive `drain()`. For T2.1 we only need the
        // token + phase handle; the monitor task continues running because
        // its JoinHandle was registered with `shutdown` before drop, and
        // dropping a ShutdownCoordinator without calling `drain` does NOT
        // abort registered handles — they live for the bootstrap runtime.
        std::mem::drop(shutdown);

        (
            shutdown_token,
            phase_handle,
            spill_root,
            regex_cache,
            backpressure_gate,
            redis_oom_reporter,
            (),
        )
    };

    // 1. KMS — load and validate KEK material so config errors surface at
    // startup. The cache is moved into LoaderCtx so vendor_key decryption
    // works on the hot path.
    let kek_cache = kms::init(cfg).await.context("KMS init failed")?;

    // 2. Storage clients.
    let redis = gw_store::redis_client::RedisCtx::connect(cfg.redis.url_str(), cfg.redis.pool_size)
        .await
        .context("redis connect failed")?;
    // Keep EntityStore cold loads on a small, non-blocking Redis pool. The
    // main RedisCtx is shared by long BLOCKing XREADGROUP loops
    // (events:entity, commands:gateway, task streams). If the loader's Redis
    // L1 HGETALL lands on one of those blocked connections, a real PG row can
    // be reported as "not found" after cold_load_timeout_ms.
    let loader_redis = gw_store::redis_client::RedisCtx::connect(cfg.redis.url_str(), 2)
        .await
        .context("redis loader connect failed")?;

    let pg = gw_store::pg_client::connect(cfg.pgsql.url_str(), cfg.pgsql.max_connections)
        .await
        .context("postgres connect failed")?;

    // 3. EntityStore with loader. Cache misses on the *_or_load methods now
    // fall through to Redis L1 -> PG L2 -> Redis write-back.
    let loader = gw_store::LoaderCtx {
        redis: loader_redis,
        pg: pg.clone(),
        kek: kek_cache,
    };
    let oauth_kek = loader.kek.clone();
    #[cfg(feature = "tunnel")]
    let tunnel_kek = loader.kek.clone();
    let adaptive_rpm = Arc::new(AdaptiveRpmManager::new(map_adaptive_rpm_cfg(
        &cfg.adaptive_rpm,
    )));
    let affinity = Arc::new(gw_routing::affinity::AffinityManager::default());
    let adapter_registry = Arc::new(gw_adapter::AdapterRegistry::new());
    spawn_decay_loop(
        adaptive_rpm.clone(),
        cfg.adaptive_rpm.probe_after_min.saturating_mul(60),
    );

    let mut store_inner = gw_store::EntityStore::with_loader_and_timeout(
        loader,
        std::time::Duration::from_millis(cfg.loader.cold_load_timeout_ms),
    );
    {
        let adaptive_for_evict = adaptive_rpm.clone();
        store_inner.on_vendor_key_invalidate(Arc::new(move |key_id| {
            adaptive_for_evict.evict(key_id);
        }));
    }
    {
        let affinity_for_evict = affinity.clone();
        store_inner.on_vendor_key_invalidate(Arc::new(move |key_id| {
            affinity_for_evict.invalidate_vendor_key(key_id);
        }));
    }
    // R7b: hot-reloadable JWT validator. The handle is shared between the
    // request path (`LlmGateway.jwt_validator`) and the rebuild hook below; the
    // boot-time validator is stored into it once `AuthConfig.global` loads.
    let jwt_validator: Arc<arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator>> =
        Arc::new(arc_swap::ArcSwapOption::new(None));
    {
        let swap_for_hook = jwt_validator.clone();
        store_inner.on_auth_config_rebuild(Arc::new(
            move |store: Arc<EntityStore>,
                  id: String|
                  -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
                let swap = swap_for_hook.clone();
                Box::pin(async move {
                    rebuild_jwt_validator_and_swap(&store, &id, &swap).await;
                })
            },
        ));
    }
    let store = Arc::new(store_inner);
    let oauth_writer = Arc::new(gw_store::oauth_refresh::PgOAuthRefreshWriter::new(
        pg.clone(),
        redis.clone(),
        oauth_kek,
    ));
    let oauth_runtime: Arc<dyn gw_adapter::OAuthRuntime> =
        Arc::new(gw_adapter::oauth_runtime::RustOAuthRuntime::new(
            store.clone(),
            oauth_writer,
            redis.clone(),
            reqwest::Client::new(),
            gw_adapter::oauth_runtime::OAuthRuntimeCfg {
                lock_ttl_ms: cfg.oauth_refresh.lock_ttl_ms,
                refresh_skew_secs: cfg.oauth_refresh.refresh_skew_secs,
                request_timeout_ms: cfg.oauth_refresh.request_timeout_ms,
                wait_after_lock_lost_ms: cfg.oauth_refresh.wait_after_lock_lost_ms,
            },
        ));
    let quota_state_updates = {
        let writer = Arc::new(gw_store::quota_state::QuotaStateWriter::new(
            pg.clone(),
            redis.clone(),
        ));
        Arc::new(gw_store::quota_state::QuotaStateUpdateQueue::new(
            writer,
            cfg.quota_state_probe.max_pending,
        ))
    };
    if cfg.quota_state_probe.enabled {
        gw_store::quota_state::QuotaStateProbeRunner {
            store: store.clone(),
            queue: quota_state_updates.clone(),
            client: reqwest::Client::new(),
            cfg: gw_store::quota_state::QuotaStateProbeCfg {
                enabled: cfg.quota_state_probe.enabled,
                interval_secs: cfg.quota_state_probe.interval_secs,
                probe_endpoint: cfg.quota_state_probe.probe_endpoint.clone(),
                max_pending: cfg.quota_state_probe.max_pending,
            },
        }
        .start();
    }
    let health = Arc::new(KeyHealthStore::default());
    let in_flight = Arc::new(InFlightStore::default());
    let ratelimit = Arc::new(RateLimitGuard::new());
    let ip_allowlist_mode = map_ip_allowlist_mode(cfg.ip_filter.allowlist_mode);
    let ip_allowlist = Arc::new(IpAllowlistGuard::new(ip_allowlist_mode));
    let ip_ratelimit = Arc::new(IpRateLimitGuard::new(
        cfg.ip_ratelimit.global_per_ip_rpm,
        cfg.ip_ratelimit.max_entries,
    ));

    let counters = Arc::new(CounterStore {
        redis: redis.clone(),
    });

    spawn_ip_ratelimit_sweep(ip_ratelimit.clone(), cfg.ip_ratelimit.sweep_interval_secs);

    {
        let probe_cfg = gw_health::QuotaProbeCfg {
            enabled: cfg.health.quota_probe.enabled,
            interval_secs: cfg.health.quota_probe.interval_secs,
            probe_endpoint: cfg.health.quota_probe.probe_endpoint.clone(),
        };
        if probe_cfg.enabled {
            let transport = Arc::new(quota_probe_transport::HttpProbeTransport {
                store: store.clone(),
                client: reqwest::Client::new(),
            });
            let runner = gw_health::QuotaProbeRunner {
                health: health.clone(),
                transport,
                cfg: probe_cfg,
            };
            let _ = runner.spawn();
        }
    }

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
    let task_contract = if cfg.task.enabled {
        Some(Arc::new(
            gw_task::redis_contract::RedisTaskContract::new((*redis.pool).clone())
                .await
                .context("initializing task redis contract")?,
        ))
    } else {
        None
    };

    // T3-Pre — single shared ResilienceState. The proxy error fanout
    // (T3-A) and the /healthz/resilience snapshot (T3-C) must observe the
    // same counters, so construction happens once here and the Arc is
    // threaded through LlmGateway + HealthState.
    let resilience = Arc::new(ResilienceState::default());

    // 4. Billing instance_id — resolved before Ledger::start so both the
    // flusher (inflight DEL key path, Task 4) and InflightLedger share the
    // same stable value. instance_id is stable across restarts on the same
    // host (stored in Redis under `gateway:instance:<hostname>`). The
    // heartbeat task keeps the TTL alive so the W4 reconciler knows the
    // gateway is live.
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

    // 4a. Ledger flusher.
    let ledger = Arc::new(Ledger::start_with_emergency_dir(
        redis.clone(),
        instance_id.clone(),
        cfg.ledger.channel_capacity,
        cfg.ledger.batch_size,
        Duration::from_millis(cfg.ledger.flush_interval_ms),
        cfg.ledger.emergency_dlq_dir.clone(),
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

    // Egress impersonation loopback + identity persistence (Task 4.6, configured by
    // `[egress]` in Task 5.2). Constructed in the bootstrap runtime because
    // EgressHandle::start and IdentityPersistQueue::new both call tokio::spawn.
    let egress_bind = cfg.egress.bind().context("invalid [egress] bind config")?;
    let egress = Arc::new(gw_egress_impersonate::EgressHandle::start(
        egress_bind,
        cfg.egress.max_sessions,
    ));
    let identity_persist = Arc::new(gw_store::identity_persist::IdentityPersistQueue::new(
        Arc::new(gw_store::identity_persist::PgSnapshotWriter::new(
            pg.clone(),
        )),
        2000,
    ));
    let turn_state_cache = Arc::new(gw_core::impersonate::turn_state::TurnStateCache::new(
        Duration::from_secs(cfg.egress.turn_state_ttl_secs),
        8192,
    ));

    // 4c. DEDUCT DashMap GC (S-ledger T3). Periodically removes zero-state
    // entries from the in-memory DEDUCT map to prevent unbounded growth.
    // The JoinHandle is intentionally dropped: the spawned task lives for the
    // life of the bootstrap runtime (which is parked on pending()).
    let _deduct_gc_handle = gw_ledger::DeductGc::start_default();

    // 4d. InflightLedger + background tasks (W2-T6).
    // instance_id was resolved above (before Ledger::start) and is reused here.
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
                l.emit(gw_ledger::flusher::LedgerItem {
                    record: r,
                    clear: None,
                })
                .await;
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
    //
    // Install the credit-lease counter handler BEFORE start() so the very
    // first events:counter delivery has a registered handler. OnceCell-backed,
    // so any later install_counter_handler() calls are silent no-ops.
    gw_ledger::install_counter_handler();
    let consumer = Arc::new(gw_store::stream_consumer::StreamConsumer {
        redis: redis.clone(),
        store: store.clone(),
        instance_id: format!("gw-{}", std::process::id()),
    });
    // start() is sync and infallible: Supervisor::spawn handles ensure() on boot
    // and automatically recreates groups on NOGROUP (e.g. after Redis FLUSHALL).
    let _consumer_handle = consumer.start();
    tracing::info!(
        target: "gw_gateway",
        "stream consumer started (events:entity, events:counter) with XREADGROUP invalidate-on-event"
    );

    if cfg.control_plane.enabled {
        let _control_plane = control_plane::start(
            redis.clone(),
            store.clone(),
            health.clone(),
            in_flight.clone(),
            instance_id.clone(),
            &cfg.control_plane,
        );
        tracing::info!(
            target: "gw_gateway",
            %instance_id,
            "control-plane state writer and command consumer started"
        );
    } else {
        tracing::info!(
            target: "gw_gateway",
            "control-plane disabled by config"
        );
    }

    // T1-E (G6) + R7b — populate the hot-reloadable validator handle from
    // AuthConfig.global. Absent → handle stays None and request_filter falls
    // back to the legacy `verify_jwt` (exp-only, JWKS-by-kid); a later
    // `events:entity ns=auth_config id=global` then hot-loads it without a
    // restart. Construction is identical to the rebuild path.
    match store.auth_config_or_load("global").await {
        Some(cfg_v) => {
            jwt_validator.store(Some(Arc::new(build_validator_from_auth_config(
                &store,
                &cfg_v.value,
            ))));
            tracing::info!(
                target: "gw_gateway",
                "JwtValidator wired from AuthConfig.global (G6, T1-E); hot-reloadable via events:entity ns=auth_config (R7b)"
            );
        }
        None => {
            tracing::warn!(
                target: "gw_gateway",
                "auth_configs.global missing; falling back to legacy JWT exp-only validation. \
                 Populate the `global` row via the management plane to enable iss/aud/nbf checks (0006_auth_configs). \
                 The validator will hot-load on the next events:entity ns=auth_config id=global (R7b)."
            );
        }
    }

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
    let mut tunnel_bridge: Option<gw_proxy::tunnel_bridge::TunnelBridgeHandle> = None;

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
            auth_token: None,
            heartbeat_interval: pool_cfg.heartbeat_interval,
            missed_pong_threshold: cfg.tunnel.missed_pong_threshold,
            max_streams_per_conn: cfg.tunnel.max_streams_per_conn,
        };
        let dns = Arc::new(SafeDnsResolver::cloudflare_default());

        fn node_auth_token(
            enc: Option<gw_core::entity::entity_helpers::EncryptedBlob>,
            kek: &gw_store::encrypt::KekCache,
        ) -> anyhow::Result<Option<secrecy::SecretString>> {
            let Some(enc) = enc else {
                return Ok(None);
            };
            let key = gw_store::encrypt::EncryptedKey {
                ciphertext: enc.ciphertext,
                nonce: enc.nonce,
                auth_tag: enc.auth_tag,
                kek_kid: enc.kek_kid,
            };
            gw_store::encrypt::decrypt(&key, kek).map(Some)
        }

        fn node_to_view(
            n: gw_core::entity::ProxyNode,
            kek: &gw_store::encrypt::KekCache,
        ) -> anyhow::Result<gw_tunnel_client::resolver::ProxyNodeView> {
            Ok(gw_tunnel_client::resolver::ProxyNodeView {
                id: n.id,
                ws_url: n.ws_url,
                auth_token: node_auth_token(n.auth_token_enc, kek)?,
                enabled: n.enabled,
                latency_ms: n.latency_ms,
                allowed_provider_ids: n.allowed_provider_ids,
                allowed_vendor_key_ids: n.allowed_vendor_key_ids,
            })
        }

        // Initial node snapshot. Plan defines ProxyNode -> ProxyNodeView mapping;
        // including `ws_url` (added in T3-A).
        let initial_nodes: Vec<ProxyNodeView> = store
            .proxy_nodes_list_or_load()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|n| node_to_view(n, &tunnel_kek))
            .collect::<anyhow::Result<Vec<_>>>()?;

        let stack = Arc::new(gw_proxy::tunnel_stack::TunnelStack::new(
            initial_nodes,
            dns,
            pool_cfg,
            transport_template,
        ));

        let weak_stack = Arc::downgrade(&stack);
        let bridge = gw_proxy::tunnel_bridge::spawn(gw_proxy::tunnel_bridge::TunnelBridgeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            transport_for_node: Arc::new(move |node_id: &str| {
                weak_stack
                    .upgrade()
                    .and_then(|stack| stack.current_transport(node_id))
            }),
        })
        .await?;
        stack.set_bridge_addr(bridge.local_addr);
        tracing::info!(
            target: "gw_gateway",
            bridge_addr = %bridge.local_addr,
            "tunnel loopback bridge started"
        );
        tunnel_bridge = Some(bridge);

        // Refresh-on-invalidation: spawn a 5s polling watcher. The stream
        // consumer in gw-store already invalidates the Moka cache on
        // `proxy_node` events; this watcher pushes the refreshed snapshot
        // into the ArcSwap inside TunnelStack. v1 uses polling because
        // `stream_consumer` does not yet expose a per-namespace callback.
        let stack_clone = stack.clone();
        let store_clone = store.clone();
        let tunnel_kek = tunnel_kek.clone();
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
                        let view: Vec<gw_tunnel_client::resolver::ProxyNodeView> = nodes
                            .into_iter()
                            .filter_map(|n| match node_to_view(n, &tunnel_kek) {
                                Ok(view) => Some(view),
                                Err(err) => {
                                    tracing::error!(
                                        target: "gw_gateway::tunnel",
                                        error = %err,
                                        "dropping proxy_node with undecryptable auth_token_enc"
                                    );
                                    None
                                }
                            })
                            .collect();
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

    let (reservation_view, frequency_tracker) = if cfg.routing.reservation.enabled {
        let view = Arc::new(ReservationGlobalView::with_gate(
            cfg.routing.reservation.gate(),
        ));
        let tracker = Arc::new(ModelFrequencyTracker::new());
        let instance_id = cfg
            .routing
            .reservation
            .instance_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        view.clone().spawn(
            redis.clone(),
            tracker.clone(),
            instance_id.clone(),
            Duration::from_secs(cfg.routing.reservation.interval_secs.max(1)),
        );
        tracing::info!(
            target: "gw_gateway",
            %instance_id,
            interval_secs = cfg.routing.reservation.interval_secs.max(1),
            "reservation global-view task started"
        );
        (Some(view), Some(tracker))
    } else {
        tracing::info!(target: "gw_gateway", "routing.reservation.enabled=false; skipping reservation global-view task");
        (None, None)
    };

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
        reservation_view,
        frequency_tracker,
        ratelimit,
        ip_allowlist,
        ip_ratelimit,
        adaptive_rpm,
        ip_allowlist_mode,
        ledger,
        counters,
        global_rpm: cfg.ratelimit.global_rpm,
        admin_upstream_url,
        trust_xff: cfg.security.trust_xff,
        signature_required: cfg.signature.required,
        task_enabled: cfg.task.enabled,
        task_contract,
        task_billing_config: Some(gw_billing::TaskBillingConfig {
            base_micro: cfg.task.billing_base_micro,
            vendor_multiplier: cfg.task.billing_vendor_multiplier,
            estimated_duration_secs: cfg.task.billing_estimated_duration_secs,
        }),
        business_port: cfg.server.business_port,
        jwt_validator,
        strategies: Arc::new(gw_routing::strategy::set::StrategySet::new_with_presets(
            Duration::from_secs(cfg.routing.sticky_ttl_secs),
            cfg.routing.preset_weights(),
        )),
        affinity,
        capability_resolver,
        #[cfg(feature = "tunnel")]
        tunnel,
        #[cfg(feature = "tunnel")]
        _tunnel_bridge: tunnel_bridge,
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
        protection_cfg: cfg.protection.clone(),
        classifier: Arc::new(gw_core::error::DefaultClassifier),
        adapter_registry,
        oauth_runtime,
        quota_state_updates: Some(quota_state_updates),
        retryable_codes: {
            Arc::new(
                cfg.error
                    .classification
                    .retryable_status_codes
                    .iter()
                    .copied()
                    .collect(),
            )
        },
        error_failover_total_budget_ms: cfg.error.classification.failover_total_budget_ms,
        error_passthrough_enabled: cfg.error.passthrough.enabled,
        error_passthrough_rules: std::sync::Arc::new(cfg.error.passthrough.compiled_rules()),
        body_rules: Arc::new(convert_body_rules(&cfg.body_rules)),
        egress,
        egress_disabled: cfg.egress.disabled,
        identity_persist,
        turn_state_cache,
        pg,
        regex_cache,
        backpressure_gate,
        shutdown_token,
        phase_handle,
        spill_root,
        redis_oom_reporter,
    })
}

fn map_ip_allowlist_mode(mode: config::IpFilterAllowlistMode) -> IpAllowlistMode {
    match mode {
        config::IpFilterAllowlistMode::Off => IpAllowlistMode::Off,
        config::IpFilterAllowlistMode::Enforce => IpAllowlistMode::Enforce,
    }
}

fn map_adaptive_rpm_cfg(cfg: &config::AdaptiveRpmCfg) -> AdaptiveCfg {
    AdaptiveCfg {
        enforcement_threshold: cfg.enforcement_threshold as f32,
        confidence_decay_per_min: cfg.confidence_decay_per_min as f32,
        min_header_observations: cfg.min_header_observations,
        min_no_header_observations: cfg.min_no_header_observations,
        buffer_ratio: cfg.buffer_ratio as f32,
        probe_after_min: cfg.probe_after_min,
        probe_increment: cfg.probe_increment,
        min_rpm: cfg.min_rpm,
        max_rpm: cfg.max_rpm,
    }
}

fn convert_body_rules(cfg: &config::BodyRulesCfg) -> gw_proxy::filters::upstream::BodyRulesMap {
    cfg.rules
        .iter()
        .filter_map(|(endpoint, fields)| {
            let converted: std::collections::HashMap<String, Box<serde_json::value::RawValue>> =
                fields
                    .iter()
                    .filter_map(|(key, val)| {
                        let raw_str = serde_json::to_string(val).ok()?;
                        let raw = serde_json::value::RawValue::from_string(raw_str).ok()?;
                        Some((key.clone(), raw))
                    })
                    .collect();
            if converted.is_empty() {
                None
            } else {
                Some((endpoint.clone(), converted))
            }
        })
        .collect()
}

pub fn spawn_decay_loop(
    manager: Arc<AdaptiveRpmManager>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        loop {
            interval.tick().await;
            manager.tick_decay();
        }
    })
}

fn spawn_ip_ratelimit_sweep(guard: Arc<IpRateLimitGuard>, sweep_interval_secs: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(sweep_interval_secs));
        loop {
            interval.tick().await;
            let removed = guard.sweep();
            if removed > 0 {
                tracing::info!(
                    target: "gw_gateway",
                    removed,
                    active = guard.len(),
                    "ip ratelimit sweep evicted idle entries"
                );
            }
        }
    });
}

#[cfg(test)]
mod adaptive_rpm_tests {
    use super::*;
    use gw_auth::ratelimit::adaptive::{
        AdaptiveCfg, AdaptiveRpmManager, Confidence, KeyAdaptiveState,
    };
    use gw_core::ids::KeyId;

    #[tokio::test(start_paused = true)]
    async fn test_decay_loop_ticks() {
        let manager = Arc::new(AdaptiveRpmManager::new(AdaptiveCfg {
            confidence_decay_per_min: 0.5,
            enforcement_threshold: 0.6,
            probe_after_min: 30,
            ..AdaptiveCfg::default()
        }));
        let key_id = KeyId::from("vk-decay-loop");
        manager.state.insert(
            key_id.clone(),
            Arc::new(KeyAdaptiveState {
                configured_rpm: 1_000,
                learned_rpm: Some(100),
                confidence: Confidence::new(1.0, 0),
                last_429_at_ms: 0,
                consistent_observations: 1,
                last_observed_limit: Some(100),
            }),
        );

        let handle = spawn_decay_loop(manager.clone(), 60);
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        handle.abort();

        let state = manager.state.get(&key_id).expect("state retained");
        assert!(state.confidence.value < 1.0);
    }
}

#[cfg(test)]
mod r7b_tests {
    use super::*;

    #[tokio::test]
    async fn rebuild_swaps_in_new_validator_on_success() {
        // Pre-populate the auth_config cache so `auth_config_or_load` hits
        // without a wired loader, standing in for a fresh cold-load after a
        // rotation event.
        let store = Arc::new(EntityStore::new());
        let cfg = gw_core::entity::AuthConfig {
            leeway_secs: 45,
            static_secret_hs256: Some(b"r7b-rotated-secret".to_vec()),
            ..Default::default()
        };
        store
            .auth_config
            .insert(
                "global".to_string(),
                Arc::new(gw_core::versioned::Versioned {
                    version: 7,
                    value: Arc::new(cfg),
                }),
            )
            .await;

        let handle: arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator> =
            arc_swap::ArcSwapOption::new(None);
        assert!(
            handle.load_full().is_none(),
            "pre-condition: no validator installed yet"
        );

        rebuild_jwt_validator_and_swap(&store, "global", &handle).await;

        assert!(
            handle.load_full().is_some(),
            "successful reload must install the rebuilt validator (R7b hot-reload)"
        );
    }

    #[tokio::test]
    async fn rebuild_keeps_old_validator_on_reload_failure() {
        // A loaderless store makes `auth_config_or_load` always return `None`,
        // standing in for a transient reload failure / cache miss.
        let store = Arc::new(EntityStore::new());
        let old = Arc::new(gw_auth::jwt::JwtValidator::new(
            gw_auth::jwt::JwtValidatorConfig::default(),
        ));
        let old_ptr = Arc::as_ptr(&old);
        let handle: arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator> =
            arc_swap::ArcSwapOption::new(Some(old));

        rebuild_jwt_validator_and_swap(&store, "global", &handle).await;

        let current = handle
            .load_full()
            .expect("validator must be retained on reload failure (keep-old-on-fail)");
        assert_eq!(
            Arc::as_ptr(&current),
            old_ptr,
            "transient reload failure must NOT swap the validator (keep-old-on-fail)"
        );
    }
}
