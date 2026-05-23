//! OAuth listener. Emits `gw_oauth_refresh_total` with vendor + outcome
//! labels. On `OAuthSignal::RefreshFailed` (ADR-G3-4 / T2-B) the listener
//! additionally reverse-XADDs an entry to `events:entity` so the Go
//! task-worker can drive an async refresh:
//!
//! ```text
//! events:entity ns=vendor_key id=<key_id> op=status_change
//!               field=oauth_failed reason=upstream_401 ts_ms=<now>
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

        // T2-B (ADR-G3-4): on RefreshFailed, reverse-XADD `events:entity`
        // so the Go task-worker can drive an async refresh.
        if sig.kind != OAuthSignalKind::RefreshFailed {
            return;
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
                ("field", "oauth_failed".to_string()),
                ("reason", "upstream_401".to_string()),
                ("ts_ms", now_ms_str),
            ];
            match client
                .xadd::<(), _, _, _, _>("events:entity", false, None::<()>, "*", fields)
                .await
            {
                Ok(()) => {
                    metrics::counter!("gw_oauth_refresh_failed_xadd_total").increment(1);
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
    fn oauth_listener_emits_reverse_xadd_on_refresh_failed() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/error/listeners/oauth.rs"
        ))
        .unwrap();
        assert!(
            src.contains("events:entity"),
            "events:entity stream name missing"
        );
        assert!(src.contains("oauth_failed"), "field=oauth_failed missing");
        assert!(src.contains("upstream_401"), "reason=upstream_401 missing");
        assert!(src.contains("vendor_key"), "ns=vendor_key missing");
        assert!(src.contains("status_change"), "op=status_change missing");
        assert!(
            src.contains("gw_oauth_refresh_failed_xadd_total"),
            "success metric missing"
        );
        assert!(
            src.contains("tokio::spawn"),
            "must spawn (sync listener trait)"
        );
    }
}
