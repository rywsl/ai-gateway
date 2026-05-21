//! CacheInvalidate listener. T5-A wires the `EntityStore` handle into
//! `HandlerCtx`. On `AuthDenied` / `AccountBlocked`, the listener fires a
//! best-effort `tokio::spawn` that invalidates the api_key cache entry so
//! the next request reloads from Postgres. The spawn is fire-and-forget; the
//! current request still proceeds even if the runtime drops the future.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::{ClassifierOutcome, ErrorClass};

#[derive(Default)]
pub struct CacheInvalidateListener;

impl ErrorListener for CacheInvalidateListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        let trigger = matches!(
            oc.class,
            ErrorClass::AuthDenied | ErrorClass::AccountBlocked(_)
        );
        if !trigger {
            return;
        }
        if let (Some(es), Some(ak)) = (ctx.entity_store, ctx.api_key_id) {
            let cache = es.apikey.clone();
            let key = gw_core::ids::ApiKeyId::from(ak.to_string());
            tokio::spawn(async move {
                cache.invalidate(&key).await;
            });
        }
        metrics::counter!(
            "gw_classifier_listener_observed_total",
            "listener" => "cache",
        )
        .increment(1);
    }
}
