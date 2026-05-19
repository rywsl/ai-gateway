//! API-key verification — SHA-256 of the presented key compared in constant
//! time against the stored hash in [`EntityStore::apikey`].

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::ApiKeyId;
use gw_store::EntityStore;
use std::borrow::Cow;
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
