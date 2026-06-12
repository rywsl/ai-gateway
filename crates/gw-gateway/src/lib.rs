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
use gw_runtime::{RestartPolicy, Supervisor};
use gw_store::{CounterStore, EntityStore};
use std::sync::atomic::Ordering;
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
    /// Compiled-regex cache wired into `LlmGateway` (T6.1).
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
    /// Aggregate "any Supervisor exhausted its restart budget" flag (T2.3 /
    /// T3). The `/readyz` handler ORs this into its 503 decision so an
    /// unhealthy supervised task takes the Pod out of the LB rotation. Held
    /// on `Wired` so T3 supervisors can `clone()` the Arc and `store(true)`
    /// on budget exhaustion without re-plumbing bootstrap.
    #[allow(dead_code)]
    supervisor_unhealthy: Arc<std::sync::atomic::AtomicBool>,
    /// Sharded append-only spill root. T4 migrates `EmergencyDlqWriter` and
    /// T5 the response archive on top of this.
    #[allow(dead_code)]
    spill_root: Arc<gw_runtime::SpillRoot>,
    /// Archive cap policy from `[proxy.archive]`. Wired into `LlmGateway`
    /// in `serve_gateway` so `ReqCtx::new_ctx` gets per-boot caps.
    archive_caps: gw_proxy::ctx::ArchiveCaps,
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

/// Library entrypoint. The bin's `main` calls [`run_with_signal_handler`]
/// instead; this helper remains for compatibility with callers that don't
/// want the SIGTERM/SIGINT handling layer (e.g. internal smoke tests).
pub fn run() -> anyhow::Result<()> {
    run_with_signal_handler()
}

/// Bootstrap the gateway: load config + observability, then run the async
/// `bootstrap` to construct `Wired` and the live `ShutdownCoordinator`.
///
/// Returns the wiring bundle, the coordinator (so the signal handler can
/// drive `drain()`), and the loaded `Config` (the caller needs the shutdown
/// timings to call `drain()`).
///
/// `pub(crate)` because `Wired` is private; the only intended caller is
/// `run_with_signal_handler` in this same crate. This is a `pub(crate)`
/// async function — it must be called from inside a tokio runtime, because
/// `bootstrap()` spawns long-running background tasks (Ledger flusher,
/// health server, stream consumer, etc.) that live for the life of that
/// runtime.
pub(crate) async fn bootstrap_gateway() -> anyhow::Result<(
    Wired,
    gw_runtime::ShutdownCoordinator,
    config::Config,
    Arc<gw_ledger::backlog::BillingBacklog>,
    Arc<gw_runtime::SpillRoot>,
)> {
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

    let (wired, coordinator, billing_backlog, spill_root) = bootstrap(&cfg).await?;
    Ok((wired, coordinator, cfg, billing_backlog, spill_root))
}

/// Build the Pingora `Server`, attach the LLM gateway service, and run it
/// on a dedicated OS thread. Returns the JoinHandle of that thread.
///
/// Pingora's [`pingora::server::Server::run_forever`] consumes the server
/// and is `-> !` (never returns; it manages its own internal tokio
/// runtimes per service). We dedicate an OS thread to it so the parent
/// tokio runtime stays free to handle SIGTERM/SIGINT and drive the
/// `ShutdownCoordinator`.
///
/// The thread is detached: when `run_with_signal_handler` finishes the
/// drain and returns, the process exits, terminating the Pingora thread.
/// Pingora's own internal SIGINT/SIGTERM handling is best-effort here —
/// the primary cancellation path for background tasks is the
/// `ShutdownToken` registered with the coordinator (T3 wires it through
/// the supervised spawns).
fn serve_gateway(wired: Wired) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let business_port = wired.business_port;

    let handle = std::thread::Builder::new()
        .name("gw-pingora".into())
        .spawn(move || {
            // Pingora server lives on this dedicated thread and never
            // returns. Any panic here is intentional — the supervising
            // tokio runtime will see the thread die and the process will
            // exit shortly after.
            let mut server = match pingora::server::Server::new(None) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(
                        target: "gw_gateway",
                        error = %e,
                        "pingora Server::new failed; serve thread exiting"
                    );
                    return;
                }
            };
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
                gw_routing::FailoverExecutor::with_total_budget_ms(
                    3,
                    wired.error_failover_total_budget_ms,
                ),
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
                wired.regex_cache.clone(),
                wired.backpressure_gate.clone(),
            );
            gw.body_rules = wired.body_rules;
            gw.spill_root = Some(wired.spill_root.clone());
            gw.archive_caps = wired.archive_caps;

            let mut business = pingora_proxy::http_proxy_service(&server.configuration, gw);
            business.add_tcp(&format!("0.0.0.0:{business_port}"));
            server.add_service(business);

            tracing::info!(
                target: "gw_gateway",
                port = business_port,
                "pingora business service starting"
            );

            // Flip readiness right before `run_forever()`. `add_service` above
            // registers the service synchronously and pingora binds the listening
            // sockets prior to entering the run loop, so by the time external probes
            // reach `/readyz` the business port is accepting connections.
            health::READY.store(true, std::sync::atomic::Ordering::Release);

            // `run_forever` consumes the server and is `-> !`. It manages
            // its own SIGTERM/SIGINT handling internally; the
            // `ShutdownCoordinator` in the parent runtime drives the
            // background-task drain in parallel.
            server.run_forever();
        })
        .context("spawning pingora thread")?;

    Ok(handle)
}

/// Library entrypoint with SIGTERM/SIGINT handling (resilience-fixes T2.2).
///
/// Builds a multi-threaded tokio runtime that:
///   1. Installs SIGTERM + SIGINT handlers FIRST, before anything else.
///      `tokio::signal::unix::signal` only queues signals delivered *after*
///      the call returns, so arming the listeners before `bootstrap_gateway`
///      is critical — bootstrap can spend seconds on PG preloads / Redis
///      cold-loads, and a SIGTERM during that window would otherwise hit
///      the default disposition (terminate) instead of our graceful drain.
///   2. Calls [`bootstrap_gateway`] to construct `Wired` + the live
///      `ShutdownCoordinator`. All background tasks spawned in `bootstrap`
///      (Ledger flusher, health server, stream consumer, etc.) live on
///      this runtime.
///   3. Spawns the Pingora server on a dedicated OS thread via
///      [`serve_gateway`] and wraps the resulting `std::thread::JoinHandle`
///      in `spawn_blocking` so its termination is observable as a future.
///      If the Pingora thread dies (Server::new error, run_forever panic),
///      the select! arm returns `Err` rather than hanging on `recv()`.
///   4. Waits on `tokio::select!` for the first signal, the Pingora thread
///      to die, or — once drain is running — the drain task to complete.
///      On a second signal during drain, the process exits immediately
///      with code 1 (the spawned drain task cannot be `emergency_abort`-ed
///      in place after the coordinator was moved into the task;
///      `hard_deadline_ms` still bounds the in-flight drain). The
///      `ShutdownCoordinator::emergency_abort` API is still covered by
///      integration tests.
///   5. Returns `Ok(())` on clean drain (exit code 0) or `Err(...)`
///      with `std::process::exit(1)` on forced drain.
pub fn run_with_signal_handler() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("gw-main")
        .build()
        .context("tokio runtime build failed")?;

    rt.block_on(async {
        // Step 1 — arm signal handlers FIRST. `tokio::signal::unix::signal`
        // only catches signals delivered after this call returns. Bootstrap
        // does heavy synchronous-feeling work (PG preload, Redis cold-load,
        // KMS init) that can take seconds; a SIGTERM landing during that
        // window must not fall through to the default disposition.
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("installing SIGINT handler")?;

        let (wired, coordinator, cfg, billing_backlog, spill_root) = bootstrap_gateway().await?;

        // Capture shutdown timings before `cfg` is moved or borrowed
        // elsewhere — `coordinator.drain` takes them as Duration args.
        let phase1_ms = cfg.runtime.shutdown.phase1_accept_stop_ms;
        let phase2_ms = cfg.runtime.shutdown.phase2_task_drain_ms;
        let hard_deadline_ms = cfg.runtime.shutdown.hard_deadline_ms;

        // Spawn Pingora on its own OS thread (run_forever is sync + `-> !`).
        // Wrap the std::thread::JoinHandle in `spawn_blocking` so we get a
        // future we can `select!` on — without this, a thread death during
        // Server::new error or run_forever panic would silently leave the
        // parent hanging on `sigterm.recv()`.
        let pingora_thread = serve_gateway(wired)?;
        let mut pingora_done =
            tokio::task::spawn_blocking(move || pingora_thread.join());

        // Step 2 — wait for the first signal (or Pingora dying first).
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!(target: "gw_gateway", "SIGTERM received; initiating graceful shutdown");
            }
            _ = sigint.recv() => {
                tracing::info!(target: "gw_gateway", "SIGINT received; initiating graceful shutdown");
            }
            res = &mut pingora_done => {
                tracing::error!(
                    target: "gw_gateway",
                    result = ?res,
                    "Pingora thread exited unexpectedly before any shutdown signal"
                );
                return Err(anyhow::anyhow!("pingora thread died before shutdown"));
            }
        }

        // Step 3 — run coordinator.drain in a task and race it against a
        // second signal / Pingora death. A second SIGTERM/SIGINT during
        // drain triggers an immediate `std::process::exit(1)` because
        // `drain` consumed the coordinator by value and we can't call
        // `emergency_abort` on the moved value; `hard_deadline_ms` still
        // upper-bounds the in-flight drain.
        let drain_fut = coordinator.drain(
            Duration::from_millis(phase1_ms),
            Duration::from_millis(phase2_ms),
            Duration::from_millis(hard_deadline_ms),
        );
        let drain_handle = tokio::spawn(drain_fut);

        let outcome = tokio::select! {
            res = drain_handle => {
                res.context("drain task joined with panic")?
            }
            _ = sigterm.recv() => {
                tracing::warn!(
                    target: "gw_gateway",
                    "second SIGTERM during drain; emergency exit"
                );
                std::process::exit(1);
            }
            _ = sigint.recv() => {
                tracing::warn!(
                    target: "gw_gateway",
                    "second SIGINT during drain; emergency exit"
                );
                std::process::exit(1);
            }
            res = &mut pingora_done => {
                tracing::error!(
                    target: "gw_gateway",
                    result = ?res,
                    "Pingora thread exited during drain"
                );
                std::process::exit(1);
            }
        };

        match outcome {
            gw_runtime::DrainOutcome::Clean => {
                tracing::info!(target: "gw_gateway", "shutdown drain complete (Clean)");
            }
            gw_runtime::DrainOutcome::Forced => {
                tracing::warn!(
                    target: "gw_gateway",
                    "shutdown drain complete (Forced); exiting with code 1"
                );
            }
        }

        // T4.5 — Dump any un-drained billing backlog to disk so it survives a
        // process restart. This MUST run even on Forced drain — the backlog
        // may still contain envelopes that the drainer didn't consume before
        // the hard deadline. A dump failure is fatal (exit 2) because billing
        // data must never be silently dropped.
        match billing_backlog.dump_to_shutdown_file(&spill_root).await {
            Ok(n) if n > 0 => {
                tracing::info!(
                    target: "gw_gateway",
                    n,
                    "shutdown: backlog dumped to disk"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(
                    target: "gw_gateway",
                    error = %e,
                    "shutdown: backlog dump FAILED; exiting 2"
                );
                metrics::counter!("gw_billing_backlog_shutdown_dump_failed_total").increment(1);
                std::process::exit(2);
            }
        }

        match outcome {
            gw_runtime::DrainOutcome::Clean => Ok(()),
            gw_runtime::DrainOutcome::Forced => std::process::exit(1),
        }
    })
}

async fn bootstrap(
    cfg: &config::Config,
) -> anyhow::Result<(
    Wired,
    gw_runtime::ShutdownCoordinator,
    Arc<gw_ledger::backlog::BillingBacklog>,
    Arc<gw_runtime::SpillRoot>,
)> {
    let admin_upstream_url = url::Url::parse(&cfg.admin.upstream_url)
        .with_context(|| format!("invalid admin.upstream_url: {}", cfg.admin.upstream_url))?;
    match admin_upstream_url.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("admin.upstream_url must use http or https, got {scheme}"),
    }
    if admin_upstream_url.host_str().is_none() {
        anyhow::bail!("admin.upstream_url must include a host");
    }

    // 0. Shared runtime primitives (resilience-fixes T2.1 / T2.2).
    //
    // Stand up the `gw-runtime` primitives early so subsequent bootstrap
    // steps (and the `Wired` handoff) can reference them. T2.1 added the
    // plumbing; T2.2 returns the `ShutdownCoordinator` so the SIGTERM
    // handler in `run_with_signal_handler` can drive `drain()`. Long-running
    // tasks spawned below are wrapped in `Supervisor` (T3.4) and the
    // returned coordinator drains them on shutdown.
    let (
        mut shutdown,
        shutdown_token,
        phase_handle,
        spill_root,
        regex_cache,
        backpressure_gate,
        redis_oom_reporter,
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
        spill_root.ensure_dir().await.with_context(|| {
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

        (
            shutdown,
            shutdown_token,
            phase_handle,
            spill_root,
            regex_cache,
            backpressure_gate,
            redis_oom_reporter,
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
    let decay_interval_secs = cfg.adaptive_rpm.probe_after_min.saturating_mul(60);
    let decay_unhealthy = {
        let sv = Supervisor::new(
            "rpm_decay",
            RestartPolicy::default(),
            shutdown_token.clone(),
        );
        let flag = sv.unhealthy_flag();
        let manager = adaptive_rpm.clone();
        let handle = sv.spawn(move |tok| {
            let manager = manager.clone();
            let mut interval = tokio::time::interval(Duration::from_secs(decay_interval_secs));
            async move {
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = interval.tick() => {
                            manager.tick_decay();
                        }
                    }
                }
            }
        });
        shutdown.register(handle);
        flag
    };

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

    let ip_sweep_unhealthy = {
        let sv = Supervisor::new(
            "ip_ratelimit_sweep",
            RestartPolicy::default(),
            shutdown_token.clone(),
        );
        let flag = sv.unhealthy_flag();
        let guard = ip_ratelimit.clone();
        let sweep_secs = cfg.ip_ratelimit.sweep_interval_secs;
        let handle = sv.spawn(move |tok| {
            let guard = guard.clone();
            let mut interval = tokio::time::interval(Duration::from_secs(sweep_secs));
            async move {
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => return Ok(()),
                        _ = interval.tick() => {
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
                    }
                }
            }
        });
        shutdown.register(handle);
        flag
    };

    let auth_failure_ip_limiter = Arc::new(
        gw_auth::auth_ip_ratelimit::AuthFailureIpLimiter::new(5, 100_000, true),
    );
    {
        let handle = spawn_auth_failure_ip_sweep(
            auth_failure_ip_limiter.clone(),
            300, // 5 min sweep interval; TODO: expose as config knob
            shutdown_token.clone(),
        );
        shutdown.register(handle);
    }

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
    //
    // `Ledger::start_with_spill_root` returns the JoinHandles of all four
    // supervised workers (flusher, dlq-retry, frozen-delta, backlog-drainer).
    // They MUST be registered with `shutdown` so the coordinator awaits their
    // cancel-drain — otherwise revenue records emitted during phase-1 of
    // shutdown can be lost. The drain budget caps how long each worker
    // keeps recv-ing after the cancel signal; it MUST be < phase2 timeout.
    //
    // The `BillingBacklog` is constructed here so future bootstrap stages
    // (T4.4 `BacklogRecovery`) can pre-populate it from on-disk spill files
    // before the drainer task starts pulling from it.
    let billing_backlog = Arc::new(gw_ledger::backlog::BillingBacklog::new());

    // T4.4 — BacklogRecovery: load any DLQ / backlog-shutdown files left
    // from a prior process into the in-memory backlog BEFORE the drainer
    // task starts pulling from it. Corrupt files are quarantined so they
    // don't block subsequent boots.
    {
        let recovery = gw_ledger::backlog::BacklogRecovery::new(spill_root.clone());
        match recovery.load_into(&billing_backlog).await {
            Ok(loaded) => tracing::info!(
                target: "gw_gateway",
                loaded,
                "billing backlog recovery complete"
            ),
            Err(e) => tracing::error!(
                target: "gw_gateway",
                error = %e,
                "backlog recovery failed; continuing with empty backlog"
            ),
        }
    }

    let (ledger_inner, ledger_handles) = Ledger::start_with_spill_root(
        redis.clone(),
        instance_id.clone(),
        cfg.ledger.channel_capacity,
        cfg.ledger.batch_size,
        Duration::from_millis(cfg.ledger.flush_interval_ms),
        spill_root.clone(),
        cfg.runtime.spill_io.dlq_shards,
        billing_backlog.clone(),
        shutdown_token.clone(),
        Duration::from_millis(cfg.runtime.shutdown.billing_drain_ms),
    );
    for h in ledger_handles {
        shutdown.register(h);
    }
    let ledger = Arc::new(ledger_inner);

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
    // DeductGc is now Supervisor-wrapped (gw_runtime::Supervisor) and
    // registered with the ShutdownCoordinator so it survives panics and
    // exits cleanly on shutdown.
    let deduct_gc_handle = gw_ledger::DeductGc::spawn(shutdown_token.clone());
    shutdown.register(deduct_gc_handle);

    // ledger_sweep: periodic sweep of LEASE_POOL (Unprotected/idle) +
    // COEF_STAGING (orphan versions). Supervisor-wrapped; registered with
    // the ShutdownCoordinator like the other ledger-side background tasks.
    let ledger_sweep_handle = gw_ledger::ledger_sweep::spawn(shutdown_token.clone());
    shutdown.register(ledger_sweep_handle);

    // 4d. InflightLedger + background tasks (W2-T6).
    // instance_id was resolved above (before Ledger::start) and is reused here.
    gw_ledger::instance_id::spawn_heartbeat_task((*redis.pool).clone(), instance_id.clone());
    let reaper_handle = gw_ledger::inflight_reaper::spawn(
        (*redis.pool).clone(),
        instance_id.clone(),
        cfg.billing.reaper_interval_secs,
        cfg.billing.emit_lag_t0_secs,
        shutdown_token.clone(),
    );
    shutdown.register(reaper_handle);
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
    let mut tunnel_refresher_unhealthy: Option<Arc<std::sync::atomic::AtomicBool>> = None;

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

        // Refresh-on-invalidation: supervised 5s polling watcher. The stream
        // consumer in gw-store already invalidates the Moka cache on
        // `proxy_node` events; this watcher pushes the refreshed snapshot
        // into the ArcSwap inside TunnelStack. v1 uses polling because
        // `stream_consumer` does not yet expose a per-namespace callback.
        tunnel_refresher_unhealthy = Some({
            let sv = Supervisor::new(
                "tunnel_refresher",
                RestartPolicy::default(),
                shutdown_token.clone(),
            );
            let flag = sv.unhealthy_flag();
            let stack_clone = stack.clone();
            let store_clone = store.clone();
            let tunnel_kek = tunnel_kek.clone();
            let handle = sv.spawn(move |tok| {
                let stack = stack_clone.clone();
                let store = store_clone.clone();
                let kek = tunnel_kek.clone();
                let mut last_signature: u64 = 0;
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                async move {
                    loop {
                        tokio::select! {
                            _ = tok.cancelled() => return Ok(()),
                            _ = tick.tick() => {
                                if let Some(nodes) = store.proxy_nodes_list_or_load().await {
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
                                            .filter_map(|n| match node_to_view(n, &kek) {
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
                                        stack.reload_nodes(view);
                                    }
                                }
                            }
                        }
                    }
                }
            });
            shutdown.register(handle);
            flag
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
    // T2.3: `/readyz` reflects shutdown phase, backpressure, and supervisor
    // unhealthy. `backpressure_state` is derived from the BackpressureGate
    // that wraps the SystemHealthMonitor's shared AtomicU8. The
    // `supervisor_unhealthy` flag is OR-aggregated from per-supervisor
    // unhealthy flags by the unhealthy_aggregator task (T3.4).
    let health_backpressure_state = backpressure_gate.state_handle();
    let health_phase_handle = phase_handle.clone();
    let supervisor_unhealthy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let health_supervisor_unhealthy = supervisor_unhealthy.clone();
    let health_server_unhealthy = {
        let sv = Supervisor::new(
            "health_server",
            RestartPolicy::default(),
            shutdown_token.clone(),
        );
        let flag = sv.unhealthy_flag();
        let handle = sv.spawn(move |tok| {
            let state = health_state.clone();
            let store = health_store.clone();
            let blacklist = health_blacklist.clone();
            let phase = health_phase_handle.clone();
            let bp = health_backpressure_state.clone();
            let unhealthy = health_supervisor_unhealthy.clone();
            async move {
                tokio::select! {
                    _ = tok.cancelled() => Ok(()),
                    result = health::run_health_server(
                        health_port,
                        state,
                        store,
                        blacklist,
                        phase,
                        bp,
                        unhealthy,
                    ) => result,
                }
            }
        });
        shutdown.register(handle);
        flag
    };

    // T3.4: Unhealthy aggregator — periodically ORs per-supervisor unhealthy
    // flags into the single `supervisor_unhealthy` Arc<AtomicBool> that the
    // health server reads for `/readyz`. Only T3.4's four gateway-internal
    // supervisors (health_server, tunnel_refresher, rpm_decay,
    // ip_ratelimit_sweep) are aggregated here; earlier-wave supervisors in
    // gw-ledger / gw-task-worker / gw-egress-impersonate do not expose their
    // flags externally yet. T3.5+ refactor can revisit to unify all flags.
    let unhealthy_flags: Vec<Arc<std::sync::atomic::AtomicBool>> = {
        #[cfg_attr(not(feature = "tunnel"), allow(unused_mut))]
        let mut v: Vec<Arc<std::sync::atomic::AtomicBool>> =
            vec![health_server_unhealthy, decay_unhealthy, ip_sweep_unhealthy];
        #[cfg(feature = "tunnel")]
        if let Some(f) = tunnel_refresher_unhealthy {
            v.push(f);
        }
        v
    };
    let supervisor_unhealthy_clone = supervisor_unhealthy.clone();
    let aggregator_handle = Supervisor::new(
        "unhealthy_aggregator",
        RestartPolicy::default(),
        shutdown_token.clone(),
    )
    .spawn(move |tok| {
        let flags = unhealthy_flags.clone();
        let aggregate = supervisor_unhealthy_clone.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = tick.tick() => {
                        let any = flags.iter().any(|f| f.load(Ordering::Acquire));
                        aggregate.store(any, Ordering::Release);
                    }
                }
            }
        }
    });
    shutdown.register(aggregator_handle);

    Ok((
        Wired {
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
            supervisor_unhealthy,
            spill_root: spill_root.clone(),
            archive_caps: gw_proxy::ctx::ArchiveCaps {
                soft_cap_bytes: cfg.proxy.archive.soft_cap_bytes,
                hard_cap_bytes: cfg.proxy.archive.hard_cap_bytes,
            },
            redis_oom_reporter,
        },
        shutdown,
        billing_backlog,
        spill_root,
    ))
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
    token: gw_runtime::ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("rpm_decay", RestartPolicy::default(), token).spawn(move |tok| {
        let manager = manager.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        manager.tick_decay();
                    }
                }
            }
        }
    })
}

fn spawn_ip_ratelimit_sweep(
    guard: Arc<IpRateLimitGuard>,
    sweep_interval_secs: u64,
    token: gw_runtime::ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("ip_ratelimit_sweep", RestartPolicy::default(), token).spawn(move |tok| {
        let guard = guard.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(sweep_interval_secs));
        async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = interval.tick() => {
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
                }
            }
        }
    })
}

fn spawn_auth_failure_ip_sweep(
    guard: Arc<gw_auth::auth_ip_ratelimit::AuthFailureIpLimiter>,
    sweep_interval_secs: u64,
    token: gw_runtime::ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("auth_failure_ip_sweep", RestartPolicy::default(), token).spawn(move |tok| {
        let guard = guard.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(sweep_interval_secs));
        async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        let removed = guard.sweep();
                        if removed > 0 {
                            tracing::info!(
                                target: "gw_gateway",
                                removed,
                                "auth failure IP limiter sweep evicted idle entries"
                            );
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod ip_sweep_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ip_ratelimit_sweep_respects_shutdown_token() {
        let coord = gw_runtime::ShutdownCoordinator::default();
        let token = coord.token();
        let guard = Arc::new(IpRateLimitGuard::default());
        let handle = spawn_ip_ratelimit_sweep(guard, 60, token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "ip ratelimit sweep should exit within 5s of shutdown");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_failure_ip_sweep_respects_shutdown_token() {
        let coord = gw_runtime::ShutdownCoordinator::default();
        let token = coord.token();
        let guard = Arc::new(gw_auth::auth_ip_ratelimit::AuthFailureIpLimiter::new(5, 1000, true));
        let handle = spawn_auth_failure_ip_sweep(guard, 60, token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "auth failure IP sweep should exit within 5s of shutdown");
    }
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

        let handle = spawn_decay_loop(manager.clone(), 60, gw_runtime::ShutdownToken::new());
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
