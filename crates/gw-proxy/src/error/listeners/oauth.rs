//! OAuth listener. Emits `gw_oauth_refresh_total` with vendor + outcome
//! labels. K-C oauth-store `mark_blocked` / `mark_refresh_failed` wiring lands
//! when the K-C spec ships; for now we only surface the signal in metrics so
//! dashboards can already query refresh-failed-vs-blocked breakdowns.

use crate::error::handler::{ErrorListener, HandlerCtx};
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
    }
}
