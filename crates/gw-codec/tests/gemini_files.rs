#![allow(deprecated)]

use bytes::Bytes;
use gw_codec::gemini::files::{
    FakeFilesGateway, FileInfo, FilesCtx, GeminiFilesGateway, UploadCtx,
};

#[tokio::test]
async fn dedup_same_sha_only_once() {
    let g = FakeFilesGateway::default();
    let ctx = UploadCtx {
        user_id: "u1".into(),
    };
    let bytes = b"image-bytes";
    let r1 = g.ensure_uploaded(&ctx, bytes, "image/png").await.unwrap();
    let r2 = g.ensure_uploaded(&ctx, bytes, "image/png").await.unwrap();
    assert_eq!(r1.file_uri, r2.file_uri);
    assert_eq!(g.upload_count(), 1);
}

#[tokio::test]
async fn different_user_uploads_independently() {
    let g = FakeFilesGateway::default();
    let bytes = b"image-bytes";
    g.ensure_uploaded(
        &UploadCtx {
            user_id: "u1".into(),
        },
        bytes,
        "image/png",
    )
    .await
    .unwrap();
    g.ensure_uploaded(
        &UploadCtx {
            user_id: "u2".into(),
        },
        bytes,
        "image/png",
    )
    .await
    .unwrap();
    assert_eq!(g.upload_count(), 2);
}

#[tokio::test]
async fn different_bytes_upload_independently() {
    let g = FakeFilesGateway::default();
    let ctx = UploadCtx {
        user_id: "u1".into(),
    };
    g.ensure_uploaded(&ctx, b"image-1", "image/png")
        .await
        .unwrap();
    g.ensure_uploaded(&ctx, b"image-2", "image/png")
        .await
        .unwrap();
    assert_eq!(g.upload_count(), 2);
}

#[tokio::test]
async fn file_uri_format_is_files_prefix() {
    let g = FakeFilesGateway::default();
    let r = g
        .ensure_uploaded(
            &UploadCtx {
                user_id: "u1".into(),
            },
            b"x",
            "image/png",
        )
        .await
        .unwrap();
    assert!(r.file_uri.starts_with("files/"));
}

#[tokio::test]
async fn ref_carries_mime_and_size() {
    let g = FakeFilesGateway::default();
    let r = g
        .ensure_uploaded(
            &UploadCtx {
                user_id: "u1".into(),
            },
            b"hello",
            "image/jpeg",
        )
        .await
        .unwrap();
    assert_eq!(r.mime_type, "image/jpeg");
    assert_eq!(r.size_bytes, 5);
    assert!(r.expire_unix_s > 0);
}

struct SignatureOnlyGateway;

#[async_trait::async_trait]
impl GeminiFilesGateway for SignatureOnlyGateway {
    async fn upload(&self, _ctx: &FilesCtx, body: Bytes, mime: &str) -> anyhow::Result<FileInfo> {
        Ok(FileInfo {
            file_uri: "files/signature".to_string(),
            mime_type: mime.to_string(),
            size_bytes: body.len() as u64,
            expire_unix_s: 1,
        })
    }

    async fn list(&self, _ctx: &FilesCtx) -> anyhow::Result<Vec<FileInfo>> {
        Ok(Vec::new())
    }

    async fn get(&self, _ctx: &FilesCtx, _file_id: &str) -> anyhow::Result<FileInfo> {
        Ok(FileInfo {
            file_uri: "files/signature".to_string(),
            mime_type: "application/octet-stream".to_string(),
            size_bytes: 0,
            expire_unix_s: 1,
        })
    }

    async fn delete(&self, _ctx: &FilesCtx, _file_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn ensure_uploaded(
        &self,
        _ctx: &UploadCtx,
        bytes: &[u8],
        mime: &str,
    ) -> anyhow::Result<FileInfo> {
        Ok(FileInfo {
            file_uri: "files/deprecated".to_string(),
            mime_type: mime.to_string(),
            size_bytes: bytes.len() as u64,
            expire_unix_s: 1,
        })
    }
}

#[test]
fn new_trait_is_implementable() {
    fn assert_gateway<T: GeminiFilesGateway>() {}
    assert_gateway::<SignatureOnlyGateway>();
}
