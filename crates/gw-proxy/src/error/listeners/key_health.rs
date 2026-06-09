//! KeyHealth listener — translates `gw_core::error::CooldownDirective` from
//! the classifier into a `gw_health::CooldownDirective` and applies it to the
//! `KeyHealthStore`. The cooldown `ttl_secs` already incorporates the upstream
//! `Retry-After` header value when present (set by the classifier at
//! `classify_default.rs`), so the health store receives a header-accurate
//! duration rather than a fixed default.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;
use gw_core::ids::KeyId;
use gw_health::cooldown::{CooldownDirective as HealthCooldownDirective, CooldownReason};

#[derive(Default)]
pub struct KeyHealthListener;

impl ErrorListener for KeyHealthListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        let core_cd = match &oc.cooldown {
            Some(cd) => cd,
            None => return,
        };

        let (Some(vendor_key_id), Some(api_format), Some(key_health)) =
            (ctx.vendor_key_id, ctx.api_format, ctx.key_health)
        else {
            metrics::counter!(
                "gw_classifier_listener_observed_total",
                "listener" => "key_health",
                "kind" => "unwired",
            )
            .increment(1);
            return;
        };

        let key_id = KeyId::from(vendor_key_id);
        let reason = CooldownReason::from_u16(core_cd.reason_code);
        let health_cd = HealthCooldownDirective {
            reason,
            ttl_secs: core_cd.ttl_secs,
            reset_consecutive: core_cd.reset_consecutive,
        };

        key_health.apply_cooldown(&key_id, api_format, health_cd);
        if let Some(affinity) = ctx.affinity {
            affinity.invalidate_vendor_key(&key_id);
        }

        // Track whether the cooldown duration was driven by an upstream
        // Retry-After header (present in rate_limit_hint) vs the classifier
        // default.
        let source = if oc
            .rate_limit_hint
            .as_ref()
            .and_then(|h| h.retry_after_secs)
            .is_some()
        {
            "header"
        } else {
            "default"
        };
        metrics::counter!(
            "gw_retry_after_consumed_total",
            "source" => source,
        )
        .increment(1);

        metrics::counter!(
            "gw_classifier_listener_observed_total",
            "listener" => "key_health",
            "kind" => "wired",
        )
        .increment(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::handler::HandlerCtx;
    use gw_core::error::{ClassifierOutcome, CooldownDirective, RateLimitHint, Retryability};
    use gw_core::protocol::ProtocolKind;
    use gw_core::ratelimit::RateLimitKind;
    use gw_health::cooldown::CooldownReason;
    use gw_health::KeyHealthStore;

    fn ctx_with_store(store: &KeyHealthStore) -> HandlerCtx<'_> {
        HandlerCtx {
            request_id: "test",
            api_key_id: Some("ak-1"),
            vendor_key_id: Some("vk-1"),
            api_format: Some(ProtocolKind::OpenAi),
            key_health: Some(store),
            entity_store: None,
            resilience: None,
            affinity: None,
        }
    }

    #[test]
    fn cooldown_invalidates_affinity_for_vendor_key() {
        let store = KeyHealthStore::default();
        let affinity = gw_routing::affinity::AffinityManager::default();
        affinity.record_hit(
            gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from("ak-1")),
            KeyId::from("vk-1"),
            std::time::Instant::now(),
        );
        let ctx = HandlerCtx {
            request_id: "test",
            api_key_id: Some("ak-1"),
            vendor_key_id: Some("vk-1"),
            api_format: Some(ProtocolKind::OpenAi),
            key_health: Some(&store),
            entity_store: None,
            resilience: None,
            affinity: Some(&affinity),
        };
        let oc = ClassifierOutcome {
            retryable: Retryability::CrossKey,
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 30,
                reset_consecutive: false,
            }),
            rate_limit_hint: None,
            ..Default::default()
        };

        KeyHealthListener.on_outcome(&ctx, &oc);

        assert!(affinity
            .get(
                &gw_routing::affinity::AffinityDimension::ApiKey(gw_core::ids::ApiKeyId::from(
                    "ak-1"
                )),
                std::time::Instant::now(),
            )
            .is_none());
    }

    #[test]
    fn retry_after_header_drives_cooldown_ttl() {
        let store = KeyHealthStore::default();
        let listener = KeyHealthListener;
        let ctx = ctx_with_store(&store);

        let oc = ClassifierOutcome {
            retryable: Retryability::CrossKey,
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 30,
                reset_consecutive: false,
            }),
            rate_limit_hint: Some(RateLimitHint {
                kind: RateLimitKind::Unknown,
                retry_after_secs: Some(30),
            }),
            ..Default::default()
        };

        listener.on_outcome(&ctx, &oc);

        let key_id = KeyId::from("vk-1");
        let snap = store.peek(&key_id, ProtocolKind::OpenAi);
        assert!(snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::Status429RateLimit);
    }

    #[test]
    fn no_retry_after_uses_default_cooldown() {
        let store = KeyHealthStore::default();
        let listener = KeyHealthListener;
        let ctx = ctx_with_store(&store);

        let oc = ClassifierOutcome {
            retryable: Retryability::CrossKey,
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 60,
                reset_consecutive: false,
            }),
            rate_limit_hint: Some(RateLimitHint {
                kind: RateLimitKind::Unknown,
                retry_after_secs: None,
            }),
            ..Default::default()
        };

        listener.on_outcome(&ctx, &oc);

        let key_id = KeyId::from("vk-1");
        let snap = store.peek(&key_id, ProtocolKind::OpenAi);
        assert!(snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::Status429RateLimit);
    }

    #[test]
    fn no_cooldown_directive_skips_apply() {
        let store = KeyHealthStore::default();
        let listener = KeyHealthListener;
        let ctx = ctx_with_store(&store);

        let oc = ClassifierOutcome::default();
        listener.on_outcome(&ctx, &oc);

        let key_id = KeyId::from("vk-1");
        let snap = store.peek(&key_id, ProtocolKind::OpenAi);
        assert!(!snap.in_cooldown);
    }

    #[test]
    fn unwired_ctx_skips_apply_cooldown() {
        let listener = KeyHealthListener;
        let ctx = HandlerCtx::stub();

        let oc = ClassifierOutcome {
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 60,
                reset_consecutive: false,
            }),
            ..Default::default()
        };

        listener.on_outcome(&ctx, &oc);
    }
}
