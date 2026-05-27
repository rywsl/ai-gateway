//! Concrete [`gw_health::ProbeTransport`] wired in bootstrap.
//!
//! Resolves the vendor base URL + API key from [`EntityStore`], then
//! sends `GET <base_url><probe_endpoint>` with `Authorization: Bearer
//! <api_key>`. Returns [`ProbeResult::Success`] on 2xx, [`ProbeResult::Failure`]
//! otherwise.

use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;
use gw_health::quota_probe::{ProbeResult, ProbeTransport};
use gw_store::EntityStore;
use secrecy::ExposeSecret;
use std::sync::Arc;

pub struct HttpProbeTransport {
    pub store: Arc<EntityStore>,
    pub client: reqwest::Client,
}

#[async_trait::async_trait]
impl ProbeTransport for HttpProbeTransport {
    async fn probe(&self, key_id: &KeyId, _fmt: ApiFormat, probe_endpoint: &str) -> ProbeResult {
        let vk = match self.store.vendor_key_or_load(key_id).await {
            Some(v) => v,
            None => {
                tracing::warn!(
                    target: "gw_gateway::quota_probe",
                    %key_id,
                    "vendor key not found — skipping probe"
                );
                return ProbeResult::Failure;
            }
        };

        let vendor = match self.store.vendor_or_load(&vk.value.vendor_id).await {
            Some(v) => v,
            None => {
                tracing::warn!(
                    target: "gw_gateway::quota_probe",
                    %key_id,
                    vendor_id = %vk.value.vendor_id.as_ref(),
                    "vendor not found — skipping probe"
                );
                return ProbeResult::Failure;
            }
        };

        let base_url = match vendor.value.base_urls.first() {
            Some(u) => u.clone(),
            None => {
                tracing::warn!(
                    target: "gw_gateway::quota_probe",
                    %key_id,
                    "vendor has no base_urls — skipping probe"
                );
                return ProbeResult::Failure;
            }
        };

        let url = format!(
            "{}{}",
            base_url.as_str().trim_end_matches('/'),
            probe_endpoint,
        );

        let api_key = vk.value.api_key.expose_secret();

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => ProbeResult::Success,
            Ok(r) => {
                tracing::debug!(
                    target: "gw_gateway::quota_probe",
                    %key_id,
                    status = r.status().as_u16(),
                    "probe returned non-2xx"
                );
                ProbeResult::Failure
            }
            Err(e) => {
                tracing::debug!(
                    target: "gw_gateway::quota_probe",
                    %key_id,
                    error = %e,
                    "probe request failed"
                );
                ProbeResult::Failure
            }
        }
    }
}
