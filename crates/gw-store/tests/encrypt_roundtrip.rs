//! AES-256-GCM round-trip tests (pure unit, no containers).

use gw_store::encrypt::{KekCache, decrypt, encrypt_for_test};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn aes_gcm_encrypt_decrypt_roundtrip() {
    let kek = [42u8; 32];
    let enc = encrypt_for_test("sk-test-secret", kek);
    let mut map = HashMap::new();
    map.insert("test".to_string(), kek);
    let cache = KekCache(Arc::new(map));
    let plain = decrypt(&enc, &cache).expect("decrypt should succeed");
    assert_eq!(plain.expose_secret(), "sk-test-secret");
}

#[test]
fn missing_kek_kid_fails() {
    let kek = [1u8; 32];
    let enc = encrypt_for_test("x", kek);
    let cache = KekCache(Arc::new(HashMap::new()));
    assert!(decrypt(&enc, &cache).is_err());
}

#[test]
fn wrong_kek_fails_auth() {
    let kek = [7u8; 32];
    let enc = encrypt_for_test("payload", kek);
    let mut map = HashMap::new();
    map.insert("test".to_string(), [8u8; 32]); // different kek bound to same kid
    let cache = KekCache(Arc::new(map));
    assert!(decrypt(&enc, &cache).is_err());
}
