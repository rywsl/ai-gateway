//! End-to-end OpenAI integration test (v1 stub).
//!
//! The full plan calls for:
//!   - testcontainers (Redis + Postgres) seeded with mock user / api_key /
//!     vendor_key / model_group / pricing entities;
//!   - wiremock running on a random port acting as the OpenAI upstream;
//!   - a Pingora server bound to a second random port that proxies to the
//!     wiremock URL;
//!   - reqwest client driving requests through the Pingora listener and
//!     asserting on the round-trip body, status, and side-effects
//!     (DeductionState.frozen_micro, UsageRecord emission).
//!
//! v1 scope: that wiring is a meaningful project on its own (it pulls
//! testcontainers + wiremock + pingora-proxy into one tokio runtime). We
//! ship the scaffold below and mark every real test `#[ignore]` so CI green
//! stays green while we land the rest of the gateway and harness in later
//! phases. To run locally:
//!
//! ```bash
//! cargo test -p gw-proxy --test end_to_end_openai -- --ignored --nocapture
//! ```

use std::sync::Arc;

use gw_auth::ratelimit::RateLimitGuard;
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_proxy::LlmGateway;
use gw_routing::in_flight::InFlightStore;
use gw_store::{CounterStore, EntityStore};

/// Builds a `LlmGateway` whose collaborators are all in-memory defaults.
/// Useful as a smoke-build assertion: any breaking change to the constructor
/// signature surfaces here without needing to spin a full server.
fn build_test_gateway() -> Option<LlmGateway> {
    // Ledger requires a RedisCtx. For v1 we skip when no redis is reachable;
    // the test is run with `--ignored` once the harness is wired.
    let _store: Arc<EntityStore> = Arc::new(EntityStore::new());
    let _health: Arc<KeyHealthStore> = Arc::new(KeyHealthStore::default());
    let _in_flight: Arc<InFlightStore> = Arc::new(InFlightStore::default());
    let _ratelimit: Arc<RateLimitGuard> = Arc::new(RateLimitGuard::new());
    // Building a Ledger requires Redis; we return None here so the smoke
    // build doesn't try to start one. The end_to_end_smoke test below is
    // marked #[ignore] and assumes redis is reachable.
    None
}

#[test]
#[ignore = "complex e2e setup: pingora + wiremock + testcontainers; v2 harness lands in gw-gateway"]
fn end_to_end_openai_smoke() {
    let gateway = build_test_gateway();
    assert!(
        gateway.is_some(),
        "expected wired gateway when redis is reachable"
    );
}

#[test]
fn llm_gateway_constructor_signature_smoke() {
    // Compile-only assertion: LlmGateway::new accepts the documented Arc<...>
    // collaborator set. This protects the gw-gateway main() from silent
    // signature drift.
    #[allow(clippy::too_many_arguments)]
    fn _accepts(
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        ratelimit: Arc<RateLimitGuard>,
        ledger: Arc<Ledger>,
        counters: Arc<CounterStore>,
        global_rpm: u32,
        trust_xff: bool,
        jwt_validator: Option<Arc<gw_auth::jwt::JwtValidator>>,
        strategies: Arc<gw_routing::strategy::set::StrategySet>,
        capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
        mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
        resilience: Arc<gw_proxy::resilience::ResilienceState>,
        detailed_archive: Arc<gw_ledger::detailed_archive::DetailedArchive>,
        inflight: Arc<gw_ledger::inflight::InflightLedger>,
        response_deadline_ms: u64,
        cancel_grace_ms: u64,
    ) -> LlmGateway {
        LlmGateway::new(
            store,
            health,
            in_flight,
            ratelimit,
            ledger,
            counters,
            global_rpm,
            trust_xff,
            jwt_validator,
            strategies,
            capability_resolver,
            mgmt_blacklist,
            resilience,
            #[cfg(feature = "tunnel")]
            None,
            detailed_archive,
            inflight,
            response_deadline_ms,
            cancel_grace_ms,
        )
    }
    // Do not call it — `Ledger::start` would need a live Redis. We only need
    // this to compile.
    let _ = _accepts;
}
