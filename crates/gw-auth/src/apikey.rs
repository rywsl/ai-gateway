//! API-key verification — SHA-256 of the presented key compared in constant
//! time against the stored hash in [`EntityStore::apikey`].

use gw_core::entity::ApiKeyEntry;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::ApiKeyId;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub async fn verify_apikey(
    key: &str,
    key_id: &ApiKeyId,
    store: &EntityStore,
) -> Result<(), ErrorIR<'static>> {
    let entry = store.apikey_or_load(key_id).await.ok_or_else(err)?;
    let provided = sha2_hash(key.as_bytes());
    if provided.ct_eq(&entry.value.hash).into() {
        Ok(())
    } else {
        Err(err())
    }
}

fn sha2_hash(b: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().to_vec()
}

pub async fn resolve_apikey_by_secret(
    key: &str,
    store: &EntityStore,
) -> Result<Arc<Versioned<ApiKeyEntry>>, ErrorIR<'static>> {
    let provided = sha2_hash(key.as_bytes());
    let key_hash = bytes_to_hex(&provided);
    let entry = store
        .apikey_by_hash_or_load(&key_hash)
        .await
        .ok_or_else(err)?;
    if provided.ct_eq(&entry.value.hash).into() {
        Ok(entry)
    } else {
        Err(err())
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 401,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
        message: Cow::Borrowed("api key invalid"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

#[cfg(test)]
mod api_key_hash_tests {
    use super::*;

    // Storage contract: `api_keys.key_hash` holds the lowercase hex of
    // SHA-256(secret), identical to Go's
    // `hex.EncodeToString(sha256.Sum256(secret))`. Pin a known NIST vector
    // (SHA-256("abc")) so any future change to `sha2_hash` / `bytes_to_hex`
    // that diverges from that contract fails loudly instead of silently
    // authenticating against the wrong column.
    #[test]
    fn api_key_sha256_hex_matches_storage_contract() {
        assert_eq!(
            bytes_to_hex(&sha2_hash(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // `bytes_to_hex` must match the `hex` crate byte-for-byte (lowercase, no
    // separators) — that encoding is what the by-hash loader binds to `$1`.
    #[test]
    fn api_key_bytes_to_hex_matches_hex_crate() {
        let digest = sha2_hash(b"ak_0cYs-test");
        assert_eq!(bytes_to_hex(&digest), hex::encode(&digest));
    }
}
