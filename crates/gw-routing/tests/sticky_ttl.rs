//! T2-C / G12: moka-backed Sticky cache exercises.
//!
//! Verifies the TTL eviction semantics + capacity that DashMap lacked.

use gw_core::entity::*;
use gw_core::ids::*;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::strategy::sticky::{Sticky, DEFAULT_STICKY_TTL_SECS};
use gw_routing::strategy::*;
use secrecy::SecretString;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn fake_key(id: &str) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
    })
}

fn pick_with_conv(sticky: &Sticky, keys: &[Arc<VendorKey>], conv: &str) {
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let user = UserId::from("u");
    let model = ModelName::from("m");
    let ctx = PickCtx {
        user_id: &user,
        model: &model,
        conversation_id: Some(conv),
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };
    let _ = sticky.pick(keys, &h, &f, &ctx);
}

#[test]
fn default_ttl_is_30_min() {
    assert_eq!(DEFAULT_STICKY_TTL_SECS, 30 * 60);
}

#[test]
fn bindings_active_grows_on_insert() {
    let sticky = Sticky::default();
    let keys = vec![fake_key("a"), fake_key("b"), fake_key("c")];
    assert_eq!(sticky.bindings_active(), 0);

    pick_with_conv(&sticky, &keys, "conv-1");
    pick_with_conv(&sticky, &keys, "conv-2");
    pick_with_conv(&sticky, &keys, "conv-3");
    // Force moka to count entries deterministically.
    sticky.invalidate_expired();
    assert_eq!(sticky.bindings_active(), 3);

    // Re-pick existing conv -> hit path, no new binding.
    pick_with_conv(&sticky, &keys, "conv-1");
    sticky.invalidate_expired();
    assert_eq!(sticky.bindings_active(), 3);
}

#[test]
fn evicts_after_ttl() {
    let sticky = Sticky::new_with_ttl(Duration::from_millis(50), 100);
    let keys = vec![fake_key("a"), fake_key("b")];
    pick_with_conv(&sticky, &keys, "ephemeral");
    sticky.invalidate_expired();
    assert_eq!(sticky.bindings_active(), 1);

    thread::sleep(Duration::from_millis(120));
    sticky.invalidate_expired();
    assert_eq!(
        sticky.bindings_active(),
        0,
        "entry past TTL should have been evicted by moka"
    );
}

#[test]
fn shared_instance_across_threads() {
    let sticky = Arc::new(Sticky::default());
    let keys = Arc::new(vec![fake_key("a"), fake_key("b"), fake_key("c")]);

    let mut handles = Vec::new();
    for i in 0..8u32 {
        let s = Arc::clone(&sticky);
        let k = Arc::clone(&keys);
        handles.push(thread::spawn(move || {
            pick_with_conv(&s, &k, &format!("conv-{}", i));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    sticky.invalidate_expired();
    assert_eq!(
        sticky.bindings_active(),
        8,
        "all 8 threads should have inserted into the same shared cache"
    );
}

#[test]
fn rebinds_when_bound_key_disappears() {
    // Hit path looks up the binding, but if the bound key_id is no longer
    // in the candidate slice, Sticky falls through to bind a new one.
    let sticky = Sticky::default();
    let keys_initial = vec![fake_key("a"), fake_key("b")];
    pick_with_conv(&sticky, &keys_initial, "stable-conv");
    sticky.invalidate_expired();
    assert_eq!(sticky.bindings_active(), 1);

    // Different candidate set, none of which match the previous binding's
    // key_id. The hit path misses, the bind path runs, and the entry is
    // overwritten — still one entry.
    let keys_new = vec![fake_key("x"), fake_key("y")];
    pick_with_conv(&sticky, &keys_new, "stable-conv");
    sticky.invalidate_expired();
    assert_eq!(sticky.bindings_active(), 1);
}
