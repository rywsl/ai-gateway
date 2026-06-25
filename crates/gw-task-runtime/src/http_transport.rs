use async_trait::async_trait;
use base64::Engine;
use reqwest::Method;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

use crate::template_executor::{
    PollContext, TransportError, UpstreamRequest, UpstreamResponse, UpstreamTransport,
};

#[derive(Clone)]
pub struct HttpTemplateTransport {
    client: reqwest::Client,
    base_url: Option<String>,
    default_headers: BTreeMap<String, String>,
}

impl HttpTemplateTransport {
    #[must_use]
    pub fn new(client: reqwest::Client, base_url: Option<String>) -> Self {
        Self {
            client,
            base_url,
            default_headers: Default::default(),
        }
    }

    #[must_use]
    pub fn with_headers(
        client: reqwest::Client,
        base_url: Option<String>,
        default_headers: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let mut transport = Self {
            client,
            base_url,
            default_headers: Default::default(),
        };
        transport.default_headers = default_headers.into_iter().collect();
        transport
    }

    pub fn with_default_timeout(base_url: Option<String>) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(900))
            .build()?;
        Ok(Self {
            client,
            base_url,
            default_headers: Default::default(),
        })
    }

    pub fn with_default_timeout_and_headers(
        base_url: Option<String>,
        default_headers: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(900))
            .build()?;
        Ok(Self {
            client,
            base_url,
            default_headers: default_headers.into_iter().collect(),
        })
    }

    async fn send_request(
        &self,
        request: UpstreamRequest,
    ) -> Result<UpstreamResponse, TransportError> {
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|err| TransportError::Failed(format!("invalid upstream method: {err}")))?;
        let url = self.resolve_url(&request.path)?;
        let mut builder = self.client.request(method, url);
        for (name, value) in &self.default_headers {
            if !has_header_ignore_ascii_case(&request.headers, name) {
                builder = builder.header(name, value);
            }
        }
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some(form) = multipart_form(&request.body)? {
            builder = builder.multipart(form);
        } else if !request.body.is_null() {
            builder = builder.json(&request.body);
        }
        let response = builder
            .send()
            .await
            .map_err(|err| TransportError::Failed(err.to_string()))?;
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_string(), value.to_string()))
            })
            .collect();
        let text = response
            .text()
            .await
            .map_err(|err| TransportError::Failed(err.to_string()))?;
        if !status.is_success() {
            return Err(TransportError::Failed(format!(
                "upstream status {}: {}",
                status.as_u16(),
                text
            )));
        }
        let body = if text.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text)
                .map_err(|err| TransportError::Failed(format!("decode upstream json: {err}")))?
        };
        Ok(UpstreamResponse {
            status: status.as_u16(),
            headers,
            body,
        })
    }

    fn resolve_url(&self, path: &str) -> Result<String, TransportError> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Ok(path.to_string());
        }
        let Some(base_url) = self.base_url.as_deref() else {
            return Err(TransportError::Failed(format!(
                "relative upstream path {path} requires base_url"
            )));
        };
        let base = reqwest::Url::parse(base_url)
            .map_err(|err| TransportError::Failed(format!("parse base_url: {err}")))?;
        base.join(path)
            .map(|url| url.to_string())
            .map_err(|err| TransportError::Failed(format!("resolve upstream path: {err}")))
    }
}

fn has_header_ignore_ascii_case(headers: &BTreeMap<String, String>, name: &str) -> bool {
    headers
        .keys()
        .any(|header_name| header_name.eq_ignore_ascii_case(name))
}

fn multipart_form(body: &Value) -> Result<Option<reqwest::multipart::Form>, TransportError> {
    let Some(spec) = body.get("multipart") else {
        return Ok(None);
    };
    let Some(spec) = spec.as_object() else {
        return Err(TransportError::Failed(
            "multipart body must be an object".to_string(),
        ));
    };
    let mut form = reqwest::multipart::Form::new();
    if let Some(fields) = spec.get("fields").and_then(Value::as_object) {
        for (name, value) in fields {
            form = add_multipart_text(form, name, value);
        }
    }
    if let Some(files) = spec.get("files").and_then(Value::as_object) {
        for (name, value) in files {
            form = add_multipart_files(form, name, value)?;
        }
    }
    Ok(Some(form))
}

fn add_multipart_text(
    mut form: reqwest::multipart::Form,
    name: &str,
    value: &Value,
) -> reqwest::multipart::Form {
    if let Some(values) = value.as_array() {
        for item in values {
            form = form.text(name.to_string(), value_as_form_string(item));
        }
        return form;
    }
    form.text(name.to_string(), value_as_form_string(value))
}

fn add_multipart_files(
    mut form: reqwest::multipart::Form,
    name: &str,
    value: &Value,
) -> Result<reqwest::multipart::Form, TransportError> {
    if let Some(values) = value.as_array() {
        for item in values {
            form = form.part(name.to_string(), multipart_part(item)?);
        }
        return Ok(form);
    }
    Ok(form.part(name.to_string(), multipart_part(value)?))
}

fn multipart_part(value: &Value) -> Result<reqwest::multipart::Part, TransportError> {
    let obj = value.as_object().ok_or_else(|| {
        TransportError::Failed("multipart file entry must be an object".to_string())
    })?;
    let encoded = obj
        .get("bytes_b64")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TransportError::Failed("multipart file entry requires bytes_b64".to_string())
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|err| TransportError::Failed(format!("decode multipart file bytes_b64: {err}")))?;
    let mut part = reqwest::multipart::Part::bytes(bytes);
    if let Some(filename) = obj.get("filename").and_then(Value::as_str) {
        part = part.file_name(filename.to_string());
    }
    if let Some(content_type) = obj.get("content_type").and_then(Value::as_str) {
        part = part.mime_str(content_type).map_err(|err| {
            TransportError::Failed(format!("invalid multipart file content_type: {err}"))
        })?;
    }
    Ok(part)
}

fn value_as_form_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[async_trait]
impl UpstreamTransport for HttpTemplateTransport {
    async fn submit(&self, request: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        self.send_request(request).await
    }

    async fn poll(&self, ctx: PollContext) -> Result<UpstreamResponse, TransportError> {
        let Some(poll_url) = ctx.poll_url else {
            return Err(TransportError::Failed(
                "poll context does not include poll_url".to_string(),
            ));
        };
        self.send_request(UpstreamRequest {
            method: "GET".to_string(),
            path: poll_url,
            headers: Default::default(),
            body: serde_json::Value::Null,
        })
        .await
    }
}
