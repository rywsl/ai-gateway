//! Gemini Files API gateway.
//!
//! Gemini multimodal payloads use a two-step protocol: upload then reference.
//! v1 defines the trait + an in-memory Fake suitable for unit tests; the real
//! HTTP-backed implementation is injected by the gemini_cli adapter (A-T2).
//!
//! Dedup key is `(user_id, sha256(bytes))`. Mappings carry a default 48h TTL
//! matching Gemini's documented file expiry.

use async_trait::async_trait;
use bytes::Bytes;
use gw_core::entity::VendorKey;
use gw_core::ids::{ApiKeyId, Group};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct GeminiFileRef {
    pub file_uri: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub expire_unix_s: u64,
}

pub type FileInfo = GeminiFileRef;

pub struct UploadCtx {
    pub user_id: String,
}

#[derive(Clone, Debug)]
pub struct FilesCtx {
    pub vendor_key: Arc<VendorKey>,
    pub api_key_id: ApiKeyId,
    pub model_group: Group,
}

#[async_trait]
pub trait GeminiFilesGateway: Send + Sync {
    async fn upload(&self, ctx: &FilesCtx, body: Bytes, mime: &str) -> anyhow::Result<FileInfo>;

    async fn list(&self, ctx: &FilesCtx) -> anyhow::Result<Vec<FileInfo>>;

    async fn get(&self, ctx: &FilesCtx, file_id: &str) -> anyhow::Result<FileInfo>;

    async fn delete(&self, ctx: &FilesCtx, file_id: &str) -> anyhow::Result<()>;

    #[deprecated(note = "use upload/list/get/delete with FilesCtx vendor-key routing context")]
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
    async fn upload(&self, ctx: &FilesCtx, body: Bytes, mime: &str) -> anyhow::Result<FileInfo> {
        self.insert_or_get(ctx.api_key_id.as_ref().to_owned(), body.as_ref(), mime)
    }

    async fn list(&self, ctx: &FilesCtx) -> anyhow::Result<Vec<FileInfo>> {
        let owner = ctx.api_key_id.as_ref();
        Ok(self
            .map
            .lock()
            .iter()
            .filter(|((stored_owner, _), _)| stored_owner == owner)
            .map(|(_, file)| file.clone())
            .collect())
    }

    async fn get(&self, ctx: &FilesCtx, file_id: &str) -> anyhow::Result<FileInfo> {
        let owner = ctx.api_key_id.as_ref();
        self.map
            .lock()
            .iter()
            .find(|((stored_owner, _), file)| stored_owner == owner && file.file_uri == file_id)
            .map(|(_, file)| file.clone())
            .ok_or_else(|| anyhow::anyhow!("file not found"))
    }

    async fn delete(&self, ctx: &FilesCtx, file_id: &str) -> anyhow::Result<()> {
        let owner = ctx.api_key_id.as_ref();
        self.map
            .lock()
            .retain(|(stored_owner, _), file| !(stored_owner == owner && file.file_uri == file_id));
        Ok(())
    }

    #[allow(deprecated)]
    async fn ensure_uploaded(
        &self,
        ctx: &UploadCtx,
        bytes: &[u8],
        mime: &str,
    ) -> anyhow::Result<GeminiFileRef> {
        self.insert_or_get(ctx.user_id.clone(), bytes, mime)
    }
}

impl FakeFilesGateway {
    fn insert_or_get(
        &self,
        owner_id: String,
        bytes: &[u8],
        mime: &str,
    ) -> anyhow::Result<GeminiFileRef> {
        use sha2::{Digest, Sha256};
        let sha = hex::encode(Sha256::digest(bytes));
        let key = (owner_id, sha);
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
