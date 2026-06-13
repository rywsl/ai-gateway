use gw_store::encrypt::{decrypt, encrypt_for_test_only, KekCache, KekState};
use std::collections::HashMap;

#[test]
fn dual_read_can_decrypt_old_and_new() {
    let kek_v1 = [1u8; 32];
    let kek_v2 = [2u8; 32];
    let mut map = HashMap::new();
    map.insert("v1".into(), kek_v1);
    let cache = KekCache::with_active(map, Some("v1"));
    let mut enc_v1 = encrypt_for_test_only("plain", kek_v1);
    enc_v1.kek_kid = "v1".into();

    cache.enter_dual_read("v2".into(), kek_v2).unwrap();
    assert_eq!(cache.active_kid(), "v2");
    assert!(matches!(cache.state(), KekState::DualRead { .. }));

    // Old kid still decryptable
    let _ = decrypt(&enc_v1, &cache).unwrap();
}

#[test]
fn cannot_drop_last_kek() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let c = KekCache::with_active(map, Some("v1"));
    c.enter_dual_read("v2".into(), [2u8; 32]).unwrap();
    let r1 = c.drop_old_kek("v1");
    assert!(r1.is_ok(), "should be able to drop v1 leaving v2");
    let r2 = c.drop_old_kek("v2");
    assert!(r2.is_err(), "should refuse to drop the only remaining kek");
}

#[test]
fn cannot_enter_dual_read_twice() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let c = KekCache::with_active(map, Some("v1"));
    c.enter_dual_read("v2".into(), [2u8; 32]).unwrap();
    let err = c.enter_dual_read("v3".into(), [3u8; 32]);
    assert!(err.is_err(), "max 2 KEKs concurrent");
}

#[test]
fn drop_old_kek_returns_to_healthy() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let c = KekCache::with_active(map, Some("v1"));
    c.enter_dual_read("v2".into(), [2u8; 32]).unwrap();
    c.drop_old_kek("v1").unwrap();
    assert!(matches!(c.state(), KekState::Healthy));
    assert_eq!(c.active_kid(), "v2");
}

#[test]
fn empty_cache_active_kid_is_blank() {
    let c = KekCache::empty();
    assert_eq!(c.active_kid(), "");
}

/// Regression for the `&mut self` rebuild bug. Prior to the single-ArcSwap
/// refactor, `enter_dual_read(&mut self)` replaced the inner Arc pointer on
/// the local clone only — every other clone (loader.kek, oauth_kek,
/// tunnel_kek in the bootstrap) kept the stale snapshot and would fail
/// `kek not found: v2` on the next decrypt of a re-encrypted row. After the
/// fix, all clones share one `ArcSwap<KekSnapshot>` so the update is
/// universally visible.
#[test]
fn enter_dual_read_propagates_through_clones() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let a = KekCache::with_active(map, Some("v1"));
    let b = a.clone();
    let c = a.clone();

    a.enter_dual_read("v2".into(), [2u8; 32]).unwrap();

    assert_eq!(b.active_kid(), "v2", "clone b must see new active kid");
    assert!(matches!(b.state(), KekState::DualRead { .. }));
    assert_eq!(c.active_kid(), "v2", "clone c must see new active kid");

    // Decrypt round-trip through the *clone* to confirm v2 is actually
    // resident — the bug surfaced as `kek not found: v2`.
    let mut enc = encrypt_for_test_only("payload", [2u8; 32]);
    enc.kek_kid = "v2".into();
    let pt = decrypt(&enc, &b).expect("clone b must resolve v2");
    use secrecy::ExposeSecret;
    assert_eq!(pt.expose_secret(), "payload");
}

/// Same propagation guarantee for `drop_old_kek` and `mark_reencrypting`.
#[test]
fn drop_old_kek_and_mark_reencrypting_propagate_through_clones() {
    let mut map = HashMap::new();
    map.insert("v1".into(), [1u8; 32]);
    let a = KekCache::with_active(map, Some("v1"));
    let b = a.clone();

    a.enter_dual_read("v2".into(), [2u8; 32]).unwrap();
    a.mark_reencrypting();
    assert_eq!(
        b.state(),
        KekState::DualRead { reencrypting: true },
        "clone must observe mark_reencrypting",
    );

    a.drop_old_kek("v1").unwrap();
    assert!(matches!(b.state(), KekState::Healthy));
    assert_eq!(b.active_kid(), "v2");
}
