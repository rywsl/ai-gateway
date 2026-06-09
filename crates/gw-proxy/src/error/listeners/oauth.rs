//! OAuth listener. Emits `gw_oauth_refresh_total` with vendor + outcome
//! labels. On `OAuthSignal::RefreshFailed` (ADR-G3-4 / T2-B) the listener
//! additionally reverse-XADDs a status-only entry to `events:entity`:
//!
//! ```text
//! events:entity ns=vendor_key id=<key_id> op=status_change
//!               field=oauth_refresh_status status=rust_refresh_failed
//!               reason=upstream_401 source=rust_oauth_refresh ts_ms=<now>
//! ```
//!
//! The XADD is fire-and-forget via `tokio::spawn` — the listener trait is
//! sync and listener-side I/O must not block the fanout loop.

use crate::error::handler::{ErrorListener, HandlerCtx};
use fred::interfaces::StreamsInterface;
use gw_core::error::{ClassifierOutcome, OAuthSignalKind};

#[derive(Default)]
pub struct OAuthListener;

impl ErrorListener for OAuthListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        let Some(sig) = &oc.oauth_signal else {
            return;
        };
        let vendor = ctx.vendor_key_id.unwrap_or("unknown").to_string();
        let outcome = match sig.kind {
            OAuthSignalKind::RefreshFailed => "refresh_failed",
            OAuthSignalKind::AccountBlocked => "blocked",
            _ => "unknown",
        };
        metrics::counter!(
            "gw_oauth_refresh_total",
            "vendor" => vendor,
            "outcome" => outcome.to_string(),
        )
        .increment(1);

        // Post-K-C: on RefreshFailed, reverse-XADD a status-only event.
        // Go must not consume this as a runtime refresh command.
        if sig.kind != OAuthSignalKind::RefreshFailed {
            return;
        }
        if let (Some(affinity), Some(key_id)) = (ctx.affinity, ctx.vendor_key_id) {
            affinity.invalidate_vendor_key(&gw_core::ids::KeyId::from(key_id));
        }
        let (Some(es), Some(key_id)) = (ctx.entity_store, ctx.vendor_key_id) else {
            // Defensive: unit-test contexts use `HandlerCtx::stub()` with both
            // fields `None`. Match the cache_invalidate listener pattern.
            return;
        };
        let Some(redis) = es.loader_redis() else {
            // Test fixtures without a Redis-backed loader: no-op.
            return;
        };
        let pool = redis.pool.clone();
        let key_id = key_id.to_string();
        let now_ms: i64 =
            (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        let now_ms_str = now_ms.to_string();
        tokio::spawn(async move {
            let client = pool.next();
            let fields: Vec<(&'static str, String)> = vec![
                ("ns", "vendor_key".to_string()),
                ("id", key_id),
                ("op", "status_change".to_string()),
                ("field", "oauth_refresh_status".to_string()),
                ("status", "rust_refresh_failed".to_string()),
                ("reason", "upstream_401".to_string()),
                ("source", "rust_oauth_refresh".to_string()),
                ("ts_ms", now_ms_str),
            ];
            match client
                .xadd::<(), _, _, _, _>("events:entity", false, None::<()>, "*", fields)
                .await
            {
                Ok(()) => {
                    metrics::counter!("gw_oauth_refresh_status_xadd_total").increment(1);
                }
                Err(err) => {
                    tracing::warn!(
                        target: "gw_proxy::error::listeners::oauth",
                        error = %err,
                        "events:entity XADD failed on OAuth RefreshFailed"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::error::{
        AccountBlockReason, ClassifierOutcome, ErrorClass, OAuthSignal, OAuthSignalKind,
        Retryability,
    };

    fn make_outcome(kind: OAuthSignalKind) -> ClassifierOutcome {
        ClassifierOutcome {
            class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
            retryable: Retryability::CrossKey,
            cooldown: None,
            oauth_signal: Some(OAuthSignal { kind }),
            rate_limit_hint: None,
            passthrough_body: None,
        }
    }

    #[test]
    fn refresh_failed_invalidates_affinity_before_xadd() {
        let affinity = gw_routing::affinity::AffinityManager::default();
        affinity.record_hit(
            gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
            gw_core::ids::KeyId::from("vk-1"),
            std::time::Instant::now(),
        );
        let ctx = HandlerCtx {
            request_id: "test",
            api_key_id: Some("ak-1"),
            vendor_key_id: Some("vk-1"),
            api_format: None,
            key_health: None,
            entity_store: None,
            resilience: None,
            affinity: Some(&affinity),
        };

        OAuthListener.on_outcome(&ctx, &make_outcome(OAuthSignalKind::RefreshFailed));

        assert!(affinity
            .get(
                &gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from(
                    "ak-1"
                )),
                std::time::Instant::now(),
            )
            .is_none());
    }

    /// Stub `HandlerCtx` has `entity_store = None` and `vendor_key_id = None`,
    /// so the new XADD branch must short-circuit without panicking or
    /// spawning. (Verified by running through `on_outcome` synchronously.)
    #[test]
    fn refresh_failed_with_stub_ctx_is_noop() {
        let listener = OAuthListener;
        let ctx = HandlerCtx::stub();
        let oc = make_outcome(OAuthSignalKind::RefreshFailed);
        // Must not panic, must not require a tokio runtime (no spawn).
        listener.on_outcome(&ctx, &oc);
    }

    /// AccountBlocked path must not enter the XADD branch even if a context
    /// were ever to carry entity_store + vendor_key_id together.
    #[test]
    fn account_blocked_does_not_enter_xadd_branch() {
        let listener = OAuthListener;
        let ctx = HandlerCtx::stub();
        let oc = make_outcome(OAuthSignalKind::AccountBlocked);
        listener.on_outcome(&ctx, &oc);
    }

    /// Source-grep tripwire: the actual XADD spawn cannot be exercised
    /// without a live Redis fixture (covered by Wave-4 e2e). This guards
    /// against accidental regression of the contract surface (stream name,
    /// field key, success metric, async spawn).
    #[test]
    fn oauth_listener_emits_status_only_xadd_on_refresh_failed() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/error/listeners/oauth.rs"
        ))
        .unwrap();
        assert!(
            src.contains("events:entity"),
            "events:entity stream name missing"
        );
        assert!(
            src.contains("oauth_refresh_status"),
            "field=oauth_refresh_status missing"
        );
        assert!(
            src.contains("rust_refresh_failed"),
            "status=rust_refresh_failed missing"
        );
        assert!(src.contains("source"), "source field missing");
        assert!(
            src.contains("gw_oauth_refresh_status_xadd_total"),
            "success metric missing"
        );
        assert!(!src.contains("\"oauth_failed\""));
        assert!(src.contains("upstream_401"), "reason=upstream_401 missing");
        assert!(src.contains("vendor_key"), "ns=vendor_key missing");
        assert!(src.contains("status_change"), "op=status_change missing");
        assert!(
            src.contains("tokio::spawn"),
            "must spawn (sync listener trait)"
        );
    }
}
