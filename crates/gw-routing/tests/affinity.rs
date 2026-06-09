use gw_core::ids::{ApiKeyId, KeyId};
use gw_routing::affinity::{AffinityDimension, AffinityManager};
use std::time::{Duration, Instant};

fn api_key(id: &str) -> ApiKeyId {
    ApiKeyId::from(id)
}

fn vendor_key(id: &str) -> KeyId {
    KeyId::from(id)
}

#[test]
fn affinity_record_hit_then_get_returns_vendor_key() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(60));

    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-1")),
        vendor_key("vk-1"),
        now,
    );

    let record = manager
        .get(&AffinityDimension::ApiKey(api_key("ak-1")), now)
        .expect("affinity record should exist");
    assert_eq!(record.vendor_key_id, vendor_key("vk-1"));
    assert_eq!(record.dimension, AffinityDimension::ApiKey(api_key("ak-1")));
    assert_eq!(record.expires_at, now + Duration::from_secs(60));
}

#[test]
fn affinity_get_drops_expired_record_deterministically() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(10));
    let dimension = AffinityDimension::ApiKey(api_key("ak-expire"));

    manager.record_hit(dimension.clone(), vendor_key("vk-1"), now);

    assert!(manager
        .get(&dimension, now + Duration::from_secs(9))
        .is_some());
    assert!(manager
        .get(&dimension, now + Duration::from_secs(10))
        .is_none());
    assert_eq!(manager.len(), 0);
}

#[test]
fn affinity_later_hit_refreshes_vendor_key_and_expiry() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(30));
    let dimension = AffinityDimension::ApiKey(api_key("ak-refresh"));

    manager.record_hit(dimension.clone(), vendor_key("vk-old"), now);
    manager.record_hit(
        dimension.clone(),
        vendor_key("vk-new"),
        now + Duration::from_secs(5),
    );

    let record = manager
        .get(&dimension, now + Duration::from_secs(34))
        .expect("refreshed affinity should still be active");
    assert_eq!(record.vendor_key_id, vendor_key("vk-new"));
    assert_eq!(record.expires_at, now + Duration::from_secs(35));
}

#[test]
fn affinity_invalidate_vendor_key_removes_all_matching_records() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(60));

    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-1")),
        vendor_key("vk-remove"),
        now,
    );
    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-2")),
        vendor_key("vk-remove"),
        now,
    );
    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-3")),
        vendor_key("vk-keep"),
        now,
    );

    let removed = manager.invalidate_vendor_key(&vendor_key("vk-remove"));

    assert_eq!(removed, 2);
    assert!(manager
        .get(&AffinityDimension::ApiKey(api_key("ak-1")), now)
        .is_none());
    assert!(manager
        .get(&AffinityDimension::ApiKey(api_key("ak-2")), now)
        .is_none());
    assert_eq!(
        manager
            .get(&AffinityDimension::ApiKey(api_key("ak-3")), now)
            .expect("non-matching record remains")
            .vendor_key_id,
        vendor_key("vk-keep")
    );
}

#[test]
fn affinity_sweep_expired_removes_only_expired_records() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(10));

    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-expired")),
        vendor_key("vk-1"),
        now,
    );
    manager.record_hit(
        AffinityDimension::ApiKey(api_key("ak-active")),
        vendor_key("vk-2"),
        now + Duration::from_secs(5),
    );

    let removed = manager.sweep_expired(now + Duration::from_secs(10));

    assert_eq!(removed, 1);
    assert_eq!(manager.len(), 1);
    assert!(manager
        .get(
            &AffinityDimension::ApiKey(api_key("ak-active")),
            now + Duration::from_secs(10)
        )
        .is_some());
}

#[test]
fn affinity_dimensions_are_isolated() {
    let now = Instant::now();
    let manager = AffinityManager::new(Duration::from_secs(60));

    manager.record_hit(
        AffinityDimension::ApiKey(api_key("shared-id")),
        vendor_key("vk-api-key"),
        now,
    );
    manager.record_hit(
        AffinityDimension::Custom {
            name: "tenant".into(),
            value: "shared-id".into(),
        },
        vendor_key("vk-tenant"),
        now,
    );

    assert_eq!(
        manager
            .get(&AffinityDimension::ApiKey(api_key("shared-id")), now)
            .expect("api-key dimension remains independent")
            .vendor_key_id,
        vendor_key("vk-api-key")
    );
    assert_eq!(
        manager
            .get(
                &AffinityDimension::Custom {
                    name: "tenant".into(),
                    value: "shared-id".into(),
                },
                now
            )
            .expect("custom dimension remains independent")
            .vendor_key_id,
        vendor_key("vk-tenant")
    );
}
