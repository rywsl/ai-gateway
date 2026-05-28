use crate::ctx::ReqCtx;
use async_trait::async_trait;
use bytes::Bytes;
use gw_codec::gemini::files::{FileInfo, FilesCtx, GeminiFilesGateway};
use gw_core::entity::{KeyStatus, ModelGroup, Pricing, VendorKey};
use gw_core::error::codes;
use gw_core::ids::{ApiKeyId, Group, KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::strategy::set::StrategySet;
use gw_routing::{PickCtx, Strategy};
use gw_store::EntityStore;
use pingora::http::ResponseHeader;
use pingora::Result;
use pingora_proxy::Session;
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

const GEMINI_FILES_MODEL: &str = "gemini-files";
const DEFAULT_GEMINI_FILES_BASE: &str = "https://generativelanguage.googleapis.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiFilesAction {
    Upload,
    List,
    Get,
    Delete,
}

pub fn detect_gemini_files_route(method: &str, path: &str) -> Option<GeminiFilesAction> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segments.as_slice()) {
        ("POST", ["v1beta", "files"]) => Some(GeminiFilesAction::Upload),
        ("GET", ["v1beta", "files"]) => Some(GeminiFilesAction::List),
        ("GET", ["v1beta", "files", file_id]) if !file_id.is_empty() => {
            Some(GeminiFilesAction::Get)
        }
        ("DELETE", ["v1beta", "files", file_id]) if !file_id.is_empty() => {
            Some(GeminiFilesAction::Delete)
        }
        _ => None,
    }
}

pub fn file_id_from_path(path: &str) -> Option<&str> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["v1beta", "files", file_id] if !file_id.is_empty() => Some(file_id),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeminiFilesHttpResponse {
    pub status: u16,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiFilesHttpError {
    pub status: u16,
    pub code: &'static str,
    pub message: Cow<'static, str>,
}

impl GeminiFilesHttpError {
    pub fn missing_context(message: &'static str) -> Self {
        Self {
            status: 503,
            code: codes::GW_BACKEND_UNAVAILABLE,
            message: Cow::Borrowed(message),
        }
    }

    fn bad_request(message: &'static str) -> Self {
        Self {
            status: 400,
            code: codes::GW_REQUEST_CONTENT_REJECTED,
            message: Cow::Borrowed(message),
        }
    }

    fn gateway(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 502,
            code: codes::GW_UPSTREAM_5XX,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 404,
            code: codes::GW_REQUEST_MODEL_NOT_FOUND,
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct HttpGeminiFilesGateway {
    client: reqwest::Client,
    pg: sqlx::PgPool,
    base_url_override: Option<url::Url>,
    store: Arc<EntityStore>,
    health: Arc<KeyHealthStore>,
    in_flight: Arc<InFlightStore>,
    strategies: Arc<StrategySet>,
    latency_tracker: Arc<LatencyTracker>,
}

impl HttpGeminiFilesGateway {
    pub fn new(
        pg: sqlx::PgPool,
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        strategies: Arc<StrategySet>,
        latency_tracker: Arc<LatencyTracker>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            pg,
            base_url_override: None,
            store,
            health,
            in_flight,
            strategies,
            latency_tracker,
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn with_client(
        client: reqwest::Client,
        pg: sqlx::PgPool,
        base_url_override: Option<url::Url>,
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        strategies: Arc<StrategySet>,
        latency_tracker: Arc<LatencyTracker>,
    ) -> Self {
        Self {
            client,
            pg,
            base_url_override,
            store,
            health,
            in_flight,
            strategies,
            latency_tracker,
        }
    }

    async fn pick_key(&self, ctx: &FilesCtx) -> anyhow::Result<Arc<VendorKey>> {
        let group = self
            .store
            .model_group_or_load(&ctx.model_group)
            .await
            .ok_or_else(|| anyhow::anyhow!("model_group not found for Gemini Files"))?;
        let model = ModelName::from(GEMINI_FILES_MODEL);
        let candidates = gw_routing::candidates::get_candidate_keys(
            &group.value,
            &model,
            ProtocolKind::Gemini,
            &files_perm(ctx, &group.value),
            &self.in_flight,
            &self.store,
            1024,
            &[],
            None,
        )
        .await;
        if candidates.is_empty() {
            anyhow::bail!("no Gemini Files vendor key available");
        }
        let pricing = self.store.pricing_or_load(&model).await;
        let pick_ctx = PickCtx {
            user_id: &UserId::from(ctx.api_key_id.as_ref()),
            model: &model,
            conversation_id: None,
            message_hash: None,
            api_format: ProtocolKind::Gemini,
            pricing: pricing.as_ref().map(|p| &p.value as &Pricing),
            latency_tracker: self.latency_tracker.as_ref(),
            reservation_view: None,
            frequency_tracker: None,
        };
        self.strategies
            .pick_for(&group.value.strategy)
            .pick(&candidates, &self.health, &self.in_flight, &pick_ctx)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Gemini Files routing strategy returned no key"))
    }

    async fn key_for_existing_file(&self, file_id: &str) -> anyhow::Result<Arc<VendorKey>> {
        let row = sqlx::query_scalar::<_, String>(
            "SELECT vendor_key_id FROM gemini_file_mapping WHERE file_uri = $1",
        )
        .bind(file_id)
        .fetch_optional(&self.pg)
        .await?;
        let key_id = row.ok_or_else(|| anyhow::anyhow!(GeminiFileNotFound(file_id.to_owned())))?;
        self.store
            .vendor_key_or_load(&KeyId::from(key_id.as_str()))
            .await
            .map(|v| clone_vendor_key(&v.value))
            .ok_or_else(|| anyhow::anyhow!("mapped Gemini Files vendor key missing"))
    }

    async fn proxy_json(
        &self,
        key: &VendorKey,
        method: reqwest::Method,
        url: String,
        body: Option<Bytes>,
        content_type: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut request = self
            .client
            .request(method, url)
            .query(&[("key", key.api_key.expose_secret())]);
        if let Some(content_type) = content_type {
            request = request.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let value = response
            .json::<serde_json::Value>()
            .await
            .unwrap_or_else(|_| json!({}));
        if status.is_success() {
            Ok(value)
        } else if status.as_u16() == 404 {
            Err(anyhow::anyhow!(GeminiFileNotFound(
                "upstream 404".to_owned()
            )))
        } else if status.is_client_error() {
            Err(anyhow::anyhow!("Gemini Files upstream 4xx: {status}"))
        } else {
            Err(anyhow::anyhow!("Gemini Files upstream failed: {status}"))
        }
    }

    async fn record_mapping(&self, file: &FileInfo, key: &VendorKey) -> anyhow::Result<()> {
        let sha = sha_for_mapping(file.file_uri.as_bytes());
        sqlx::query(
            "INSERT INTO gemini_file_mapping (sha256_hex, user_id, file_uri, mime_type, size_bytes, expire_unix_s, vendor_key_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (sha256_hex, user_id) DO UPDATE SET \
             file_uri = EXCLUDED.file_uri, mime_type = EXCLUDED.mime_type, size_bytes = EXCLUDED.size_bytes, \
             expire_unix_s = EXCLUDED.expire_unix_s, vendor_key_id = EXCLUDED.vendor_key_id",
        )
        .bind(sha)
        .bind(key.key_id.as_ref())
        .bind(&file.file_uri)
        .bind(&file.mime_type)
        .bind(file.size_bytes as i64)
        .bind(file.expire_unix_s as i64)
        .bind(key.key_id.as_ref())
        .execute(&self.pg)
        .await?;
        Ok(())
    }
}

#[derive(Debug)]
struct GeminiFileNotFound(String);

impl std::fmt::Display for GeminiFileNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gemini file not found: {}", self.0)
    }
}

impl std::error::Error for GeminiFileNotFound {}

#[async_trait]
impl GeminiFilesGateway for HttpGeminiFilesGateway {
    async fn upload(&self, ctx: &FilesCtx, body: Bytes, mime: &str) -> anyhow::Result<FileInfo> {
        let key = self.pick_key(ctx).await?;
        let value = self
            .proxy_json(
                &key,
                reqwest::Method::POST,
                gemini_files_url(
                    &self.store,
                    self.base_url_override.as_ref(),
                    &key,
                    "/upload/v1beta/files",
                )
                .await?,
                Some(body),
                Some(mime),
            )
            .await?;
        let file = parse_file_info(value.get("file").unwrap_or(&value))?;
        self.record_mapping(&file, &key).await?;
        Ok(file)
    }

    async fn list(&self, ctx: &FilesCtx) -> anyhow::Result<Vec<FileInfo>> {
        let key = self.pick_key(ctx).await?;
        let value = self
            .proxy_json(
                &key,
                reqwest::Method::GET,
                gemini_files_url(
                    &self.store,
                    self.base_url_override.as_ref(),
                    &key,
                    "/v1beta/files",
                )
                .await?,
                None,
                None,
            )
            .await?;
        Ok(value
            .get("files")
            .and_then(serde_json::Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|v| parse_file_info(v).ok())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn get(&self, _ctx: &FilesCtx, file_id: &str) -> anyhow::Result<FileInfo> {
        let key = self.key_for_existing_file(file_id).await?;
        let value = self
            .proxy_json(
                &key,
                reqwest::Method::GET,
                gemini_files_url(
                    &self.store,
                    self.base_url_override.as_ref(),
                    &key,
                    &format!("/v1beta/files/{file_id}"),
                )
                .await?,
                None,
                None,
            )
            .await?;
        parse_file_info(value.get("file").unwrap_or(&value))
    }

    async fn delete(&self, _ctx: &FilesCtx, file_id: &str) -> anyhow::Result<()> {
        let key = self.key_for_existing_file(file_id).await?;
        self.proxy_json(
            &key,
            reqwest::Method::DELETE,
            gemini_files_url(
                &self.store,
                self.base_url_override.as_ref(),
                &key,
                &format!("/v1beta/files/{file_id}"),
            )
            .await?,
            None,
            None,
        )
        .await?;
        sqlx::query("DELETE FROM gemini_file_mapping WHERE file_uri = $1")
            .bind(file_id)
            .execute(&self.pg)
            .await?;
        Ok(())
    }

    #[allow(deprecated)]
    async fn ensure_uploaded(
        &self,
        _ctx: &gw_codec::gemini::files::UploadCtx,
        _bytes: &[u8],
        _mime: &str,
    ) -> anyhow::Result<gw_codec::gemini::files::GeminiFileRef> {
        anyhow::bail!("legacy Gemini Files ensure_uploaded is not supported by HTTP proxy")
    }
}

pub async fn handle_endpoint<G: GeminiFilesGateway + Sync + ?Sized>(
    gateway: &G,
    action: GeminiFilesAction,
    ctx: &FilesCtx,
    body: Bytes,
    mime: &str,
    file_id: Option<&str>,
) -> std::result::Result<GeminiFilesHttpResponse, GeminiFilesHttpError> {
    match action {
        GeminiFilesAction::Upload => {
            let file = gateway.upload(ctx, body, mime).await.map_err(|err| {
                GeminiFilesHttpError::gateway(format!("file upload failed: {err}"))
            })?;
            Ok(GeminiFilesHttpResponse {
                status: 200,
                body: json!({ "file": file_to_json(&file) }),
            })
        }
        GeminiFilesAction::List => {
            let files = gateway
                .list(ctx)
                .await
                .map_err(|err| GeminiFilesHttpError::gateway(format!("file list failed: {err}")))?;
            Ok(GeminiFilesHttpResponse {
                status: 200,
                body: json!({ "files": files.iter().map(file_to_json).collect::<Vec<_>>() }),
            })
        }
        GeminiFilesAction::Get => {
            let file_id =
                file_id.ok_or_else(|| GeminiFilesHttpError::bad_request("file id missing"))?;
            let file = gateway.get(ctx, file_id).await.map_err(map_gateway_error)?;
            Ok(GeminiFilesHttpResponse {
                status: 200,
                body: json!({ "file": file_to_json(&file) }),
            })
        }
        GeminiFilesAction::Delete => {
            let file_id =
                file_id.ok_or_else(|| GeminiFilesHttpError::bad_request("file id missing"))?;
            gateway
                .delete(ctx, file_id)
                .await
                .map_err(map_gateway_error)?;
            Ok(GeminiFilesHttpResponse {
                status: 200,
                body: json!({}),
            })
        }
    }
}

pub async fn select_files_vendor_key(
    ctx: &ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    latency_tracker: &Arc<LatencyTracker>,
) -> std::result::Result<Arc<VendorKey>, GeminiFilesHttpError> {
    let group_id = ctx
        .merged_perm
        .as_ref()
        .and_then(|perm| {
            perm.forced_model_group
                .clone()
                .or_else(|| perm.allowed_groups.iter().next().cloned())
        })
        .ok_or_else(|| {
            GeminiFilesHttpError::missing_context("no model_group configured for Gemini Files")
        })?;
    let group = store.model_group_or_load(&group_id).await.ok_or_else(|| {
        GeminiFilesHttpError::missing_context("Gemini Files model_group not found")
    })?;
    let model = ModelName::from(GEMINI_FILES_MODEL);
    let perm = ctx.merged_perm.as_ref().ok_or_else(|| {
        GeminiFilesHttpError::missing_context("permissions missing for Gemini Files")
    })?;
    let candidates = gw_routing::candidates::get_candidate_keys(
        &group.value,
        &model,
        ProtocolKind::Gemini,
        perm,
        in_flight,
        store,
        1024,
        &ctx.retry_blacklist,
        ctx.required_caps.as_ref(),
    )
    .await;
    if candidates.is_empty() {
        return Err(GeminiFilesHttpError {
            status: 503,
            code: codes::GW_ROUTING_NO_AVAILABLE_CHANNEL,
            message: Cow::Borrowed("no available Gemini Files channel"),
        });
    }
    let strategy = strategies.pick_for(&group.value.strategy);
    let pick_ctx = PickCtx {
        user_id: &perm.effective_user_id,
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: ProtocolKind::Gemini,
        pricing: ctx.pricing.as_ref().map(|p| &p.value as &Pricing),
        latency_tracker: latency_tracker.as_ref(),
        reservation_view: ctx.reservation_view.as_deref(),
        frequency_tracker: ctx.frequency_tracker.as_deref(),
    };
    strategy
        .pick(&candidates, health, in_flight, &pick_ctx)
        .cloned()
        .ok_or_else(|| {
            GeminiFilesHttpError::missing_context("Gemini Files routing strategy returned no key")
        })
}

pub async fn write_response(
    session: &mut Session,
    response: GeminiFilesHttpResponse,
) -> Result<()> {
    let mut header = ResponseHeader::build(response.status, None)?;
    header.insert_header("Content-Type", "application/json")?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session
        .write_response_body(Some(Bytes::from(response.body.to_string())), true)
        .await?;
    Ok(())
}

pub fn build_files_ctx(
    req_ctx: &ReqCtx,
    vendor_key: Arc<VendorKey>,
) -> std::result::Result<FilesCtx, GeminiFilesHttpError> {
    let api_key_id = req_ctx
        .api_key_id
        .clone()
        .unwrap_or_else(|| ApiKeyId::from("jwt"));
    let model_group = req_ctx
        .merged_perm
        .as_ref()
        .and_then(|perm| {
            perm.forced_model_group
                .clone()
                .or_else(|| perm.allowed_groups.iter().next().cloned())
        })
        .unwrap_or_else(|| Group::from("gemini-files"));
    Ok(FilesCtx {
        vendor_key,
        api_key_id,
        model_group,
    })
}

pub fn skeleton_vendor_key() -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from("gemini-files-skeleton"),
        vendor_id: VendorId::from("gemini"),
        api_key: SecretString::from("skeleton"),
        accessible_models: Vec::new(),
        model_mapping: HashMap::new(),
        accessible_endpoints: vec![ProtocolKind::Gemini],
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
        tags: Vec::new(),
        forced_capabilities: Vec::new(),
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
    })
}

fn map_gateway_error(err: anyhow::Error) -> GeminiFilesHttpError {
    if err.downcast_ref::<GeminiFileNotFound>().is_some() {
        GeminiFilesHttpError::not_found("Gemini file not found")
    } else {
        GeminiFilesHttpError::gateway(format!("Gemini Files proxy failed: {err}"))
    }
}

fn files_perm(ctx: &FilesCtx, group: &ModelGroup) -> MergedPermissions {
    MergedPermissions {
        user_id: UserId::from(ctx.api_key_id.as_ref()),
        effective_user_id: UserId::from(ctx.api_key_id.as_ref()),
        client_ip: std::net::IpAddr::from([0, 0, 0, 0]),
        allowed_models: std::collections::HashSet::new(),
        allowed_channels: std::collections::HashSet::new(),
        allowed_groups: [group.group.clone()].into_iter().collect(),
        forced_model_group: Some(group.group.clone()),
        rpm_limit: 0,
        tpm_limit: 0,
        max_concurrent_streams: 0,
        markup_chain: Default::default(),
        model_whitelist_regex: None,
    }
}

async fn gemini_files_url(
    store: &EntityStore,
    override_base: Option<&url::Url>,
    key: &VendorKey,
    path: &str,
) -> anyhow::Result<String> {
    let base = if let Some(base) = override_base {
        base.clone()
    } else {
        store
            .vendor_or_load(&key.vendor_id)
            .await
            .and_then(|v| v.value.base_urls.first().cloned())
            .unwrap_or_else(|| {
                url::Url::parse(DEFAULT_GEMINI_FILES_BASE).expect("valid default URL")
            })
    };
    Ok(base.join(path.trim_start_matches('/'))?.to_string())
}

fn parse_file_info(value: &serde_json::Value) -> anyhow::Result<FileInfo> {
    let file_uri = value
        .get("name")
        .or_else(|| value.get("uri"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Gemini Files response missing file name"))?
        .to_owned();
    let mime_type = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("application/octet-stream")
        .to_owned();
    let size_bytes = value
        .get("sizeBytes")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v.as_u64())
        })
        .unwrap_or(0);
    let expire_unix_s = value
        .get("expirationTime")
        .and_then(|v| v.as_str())
        .and_then(parse_expiration_time)
        .unwrap_or_else(|| now_unix() + 48 * 3600);
    Ok(FileInfo {
        file_uri,
        mime_type,
        size_bytes,
        expire_unix_s,
    })
}

fn parse_expiration_time(value: &str) -> Option<u64> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|ts| ts.unix_timestamp().max(0) as u64)
}

fn sha_for_mapping(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn clone_vendor_key(key: &VendorKey) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: key.key_id.clone(),
        vendor_id: key.vendor_id.clone(),
        api_key: SecretString::from(key.api_key.expose_secret().to_owned()),
        accessible_models: key.accessible_models.clone(),
        model_mapping: key.model_mapping.clone(),
        accessible_endpoints: key.accessible_endpoints.clone(),
        protocol_conversion: key.protocol_conversion,
        priority: key.priority,
        weight: key.weight,
        rpm_limit: key.rpm_limit,
        tpm_limit: key.tpm_limit,
        max_usage_count: key.max_usage_count,
        proxy: key.proxy.clone(),
        ip_allowlist: key.ip_allowlist.clone(),
        expires_at: key.expires_at,
        status: key.status,
        tags: key.tags.clone(),
        forced_capabilities: key.forced_capabilities.clone(),
        exclusive_capability_match: key.exclusive_capability_match,
        tunnel_preference: key.tunnel_preference.clone(),
        oauth: key.oauth.clone(),
        identity_snapshot: key.identity_snapshot.clone(),
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_to_json(file: &FileInfo) -> serde_json::Value {
    json!({
        "name": file.file_uri,
        "uri": file.file_uri,
        "mimeType": file.mime_type,
        "sizeBytes": file.size_bytes.to_string(),
        "expirationTime": file.expire_unix_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_codec::gemini::files::FakeFilesGateway;

    fn ctx() -> FilesCtx {
        FilesCtx {
            vendor_key: skeleton_vendor_key(),
            api_key_id: ApiKeyId::from("ak-test"),
            model_group: Group::from("gemini-default"),
        }
    }

    #[test]
    fn detects_all_gemini_files_routes() {
        assert_eq!(
            detect_gemini_files_route("POST", "/v1beta/files"),
            Some(GeminiFilesAction::Upload)
        );
        assert_eq!(
            detect_gemini_files_route("GET", "/v1beta/files"),
            Some(GeminiFilesAction::List)
        );
        assert_eq!(
            detect_gemini_files_route("GET", "/v1beta/files/files-1"),
            Some(GeminiFilesAction::Get)
        );
        assert_eq!(
            detect_gemini_files_route("DELETE", "/v1beta/files/files-1"),
            Some(GeminiFilesAction::Delete)
        );
        assert_eq!(detect_gemini_files_route("PATCH", "/v1beta/files"), None);
    }

    #[tokio::test]
    async fn fake_gateway_handles_upload_list_get_delete() {
        let gateway = FakeFilesGateway::default();
        let ctx = ctx();
        let uploaded = handle_endpoint(
            &gateway,
            GeminiFilesAction::Upload,
            &ctx,
            Bytes::from_static(b"hello"),
            "text/plain",
            None,
        )
        .await
        .unwrap();
        let file_id = uploaded.body["file"]["name"].as_str().unwrap().to_string();

        let listed = handle_endpoint(
            &gateway,
            GeminiFilesAction::List,
            &ctx,
            Bytes::new(),
            "application/octet-stream",
            None,
        )
        .await
        .unwrap();
        assert_eq!(listed.body["files"].as_array().unwrap().len(), 1);

        let fetched = handle_endpoint(
            &gateway,
            GeminiFilesAction::Get,
            &ctx,
            Bytes::new(),
            "application/octet-stream",
            Some(&file_id),
        )
        .await
        .unwrap();
        assert_eq!(fetched.body["file"]["mimeType"], "text/plain");

        handle_endpoint(
            &gateway,
            GeminiFilesAction::Delete,
            &ctx,
            Bytes::new(),
            "application/octet-stream",
            Some(&file_id),
        )
        .await
        .unwrap();
        assert_eq!(gateway.list(&ctx).await.unwrap().len(), 0);
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers Postgres)"]
    async fn http_gateway_crud_lifecycle_records_and_cleans_mapping() {
        use gw_core::entity::{ModelGroup, StrategyKind, Vendor};
        use gw_core::ids::ModelName;
        use gw_core::versioned::Versioned;
        use testcontainers_modules::postgres::Postgres;
        use testcontainers_modules::testcontainers::runners::AsyncRunner;

        let pg_node = Postgres::default().start().await.unwrap();
        let pg_port = pg_node.get_host_port_ipv4(5432).await.unwrap();
        let pg = gw_store::pg_client::connect(
            &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
            4,
        )
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../gw-store/migrations/0002_gemini_file_mapping.sql"
        ))
        .execute(&pg)
        .await
        .unwrap();

        let upstream = TestGeminiFilesServer::start().await;

        let store = Arc::new(EntityStore::new());
        let key = VendorKey {
            key_id: KeyId::from("vk-gemini-files"),
            vendor_id: VendorId::from("gemini"),
            api_key: SecretString::from("real-secret"),
            accessible_models: vec![ModelName::from(GEMINI_FILES_MODEL)],
            model_mapping: HashMap::new(),
            accessible_endpoints: vec![ProtocolKind::Gemini],
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
            tags: Vec::new(),
            forced_capabilities: Vec::new(),
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
        };
        store
            .vendor_key
            .insert(key.key_id.clone(), Arc::new(Versioned::new(1, key)))
            .await;
        store
            .vendor
            .insert(
                VendorId::from("gemini"),
                Arc::new(Versioned::new(
                    1,
                    Vendor {
                        id: VendorId::from("gemini"),
                        display_name: "Gemini".to_string(),
                        base_urls: vec![url::Url::parse(&upstream.base_url).unwrap()],
                        auth_template: "key".to_string(),
                        timeout_overrides: None,
                        default_proxy: None,
                        contact: None,
                        egress_mode: Default::default(),
                    },
                )),
            )
            .await;
        store
            .model_group
            .insert(
                Group::from("gemini-default"),
                Arc::new(Versioned::new(
                    1,
                    ModelGroup {
                        group: Group::from("gemini-default"),
                        strategy: StrategyKind::FixedOrder,
                        cross_vendor_fallback: false,
                        max_tokens_cap: None,
                        default_params: Default::default(),
                        forced_params: Default::default(),
                        vendor_key_ids: vec![KeyId::from("vk-gemini-files")],
                    },
                )),
            )
            .await;

        let gateway = HttpGeminiFilesGateway::with_client(
            reqwest::Client::new(),
            pg.clone(),
            Some(url::Url::parse(&upstream.base_url).unwrap()),
            store,
            Arc::new(KeyHealthStore::default()),
            Arc::new(InFlightStore::default()),
            Arc::new(StrategySet::new_default()),
            Arc::new(LatencyTracker::default()),
        );
        let ctx = ctx();

        let uploaded = gateway
            .upload(&ctx, Bytes::from_static(b"hello"), "text/plain")
            .await
            .unwrap();
        assert_eq!(uploaded.file_uri, "files/crud-1");
        let mapped: Option<String> =
            sqlx::query_scalar("SELECT vendor_key_id FROM gemini_file_mapping WHERE file_uri = $1")
                .bind("files/crud-1")
                .fetch_optional(&pg)
                .await
                .unwrap();
        assert_eq!(mapped.as_deref(), Some("vk-gemini-files"));

        assert_eq!(gateway.list(&ctx).await.unwrap().len(), 1);
        assert_eq!(
            gateway.get(&ctx, "files/crud-1").await.unwrap().mime_type,
            "text/plain"
        );
        gateway.delete(&ctx, "files/crud-1").await.unwrap();
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM gemini_file_mapping")
            .fetch_one(&pg)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        let err = gateway.get(&ctx, "files/crud-1").await.unwrap_err();
        assert!(err.downcast_ref::<GeminiFileNotFound>().is_some());
    }

    struct TestGeminiFilesServer {
        base_url: String,
    }

    impl TestGeminiFilesServer {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut buf = vec![0u8; 8192];
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let first = req.lines().next().unwrap_or_default();
                        let body = if first
                            .starts_with("POST /upload/v1beta/files?key=real-secret ")
                        {
                            json!({"file":{"name":"files/crud-1","mimeType":"text/plain","sizeBytes":"5","expirationTime":"4102444800"}})
                        } else if first.starts_with("GET /v1beta/files?key=real-secret ") {
                            json!({"files":[{"name":"files/crud-1","mimeType":"text/plain","sizeBytes":"5"}]})
                        } else if first.starts_with("GET /v1beta/files/crud-1?key=real-secret ") {
                            json!({"name":"files/crud-1","mimeType":"text/plain","sizeBytes":"5"})
                        } else if first.starts_with("DELETE /v1beta/files/crud-1?key=real-secret ")
                        {
                            json!({})
                        } else {
                            json!({"error":"not found"})
                        };
                        let status = if body.get("error").is_some() {
                            "404 Not Found"
                        } else {
                            "200 OK"
                        };
                        let body = body.to_string();
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            });
            Self {
                base_url: format!("http://{addr}"),
            }
        }
    }

    #[test]
    fn request_filter_keeps_gemini_files_after_business_auth() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/request.rs"
        ))
        .expect("read request filter");
        let auth_needle = "let (user_id, api_key_id, ip_allowlist_for_request) = user_id_res?;";
        let files_needle = "if ctx.is_gemini_files";
        assert!(src.find(auth_needle).unwrap() < src.find(files_needle).unwrap());
    }
}
