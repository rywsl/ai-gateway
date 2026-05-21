//! Gemini Files API gateway.
//!
//! Gemini multimodal payloads use a two-step protocol: upload then reference.
//! v1 defines the trait + an in-memory Fake suitable for unit tests; the real
//! HTTP-backed implementation is injected by the gemini_cli adapter (A-T2).
//!
//! Dedup key is `(user_id, sha256(bytes))`. Mappings carry a default 48h TTL
//! matching Gemini's documented file expiry.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct GeminiFileRef {
    pub file_uri: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub expire_unix_s: u64,
}

pub struct UploadCtx {
    pub user_id: String,
}

#[async_trait]
pub trait GeminiFilesGateway: Send + Sync {
    async fn ensure_uploaded(
        &self,
        ctx: &UploadCtx,
        bytes: &[u8],
        mime: &str,
    ) -> anyhow::Result<GeminiFileRef>;
}

/// In-memory test double. Production code uses a HTTP-backed impl injected
/// by the gemini_cli adapter.
#[derive(Default)]
pub struct FakeFilesGateway {
    map: Mutex<HashMap<(String, String), GeminiFileRef>>,
    uploads: AtomicU64,
}

impl FakeFilesGateway {
    pub fn upload_count(&self) -> u64 {
        self.uploads.load(Ordering::Acquire)
    }
}

#[async_trait]
impl GeminiFilesGateway for FakeFilesGateway {
    async fn ensure_uploaded(
        &self,
        ctx: &UploadCtx,
        bytes: &[u8],
        mime: &str,
    ) -> anyhow::Result<GeminiFileRef> {
        use sha2::{Digest, Sha256};
        let sha = hex::encode(Sha256::digest(bytes));
        let key = (ctx.user_id.clone(), sha);
        if let Some(r) = self.map.lock().get(&key) {
            return Ok(r.clone());
        }
        self.uploads.fetch_add(1, Ordering::AcqRel);
        let r = GeminiFileRef {
            file_uri: format!("files/{}", &key.1[..8]),
            mime_type: mime.into(),
            size_bytes: bytes.len() as u64,
            expire_unix_s: now() + 48 * 3600,
        };
        self.map.lock().insert(key, r.clone());
        Ok(r)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
