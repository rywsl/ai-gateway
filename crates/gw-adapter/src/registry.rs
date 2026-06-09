//! `AdapterRegistry` — vendor-id → adapter + error-analyzer lookup.
//!
//! Built at gateway / worker bootstrap; one shared instance per process.

use crate::analyzer::{DefaultErrorAnalyzer, KeywordErrorAnalyzer, UpstreamErrorAnalyzer};
use crate::codex::CodexAdapter;
use crate::kiro::KiroAdapter;
use crate::noop::NoopAdapter;
use crate::trait_def::UpstreamAdapter;
use crate::vertex::VertexAdapter;
use dashmap::DashMap;
use gw_core::ids::VendorId;
use std::sync::Arc;

pub struct AdapterRegistry {
    by_vendor: DashMap<VendorId, Arc<dyn UpstreamAdapter>>,
    default_adapter: Arc<NoopAdapter>,
    error_analyzers: DashMap<VendorId, Arc<dyn UpstreamErrorAnalyzer>>,
    default_error_analyzer: Arc<DefaultErrorAnalyzer>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            by_vendor: DashMap::new(),
            default_adapter: Arc::new(NoopAdapter),
            error_analyzers: DashMap::new(),
            default_error_analyzer: Arc::new(DefaultErrorAnalyzer),
        }
    }

    pub fn register_adapter(&self, vendor: VendorId, adapter: Arc<dyn UpstreamAdapter>) {
        self.by_vendor.insert(vendor, adapter);
    }

    pub fn register_error_analyzer(
        &self,
        vendor: VendorId,
        analyzer: Arc<dyn UpstreamErrorAnalyzer>,
    ) {
        self.error_analyzers.insert(vendor, analyzer);
    }

    pub fn for_vendor(&self, v: &VendorId) -> Arc<dyn UpstreamAdapter> {
        self.by_vendor
            .get(v)
            .map(|e| e.value().clone())
            .or_else(|| builtin_adapter_for(v))
            .unwrap_or_else(|| (self.default_adapter.clone()) as Arc<dyn UpstreamAdapter>)
    }

    pub fn error_analyzer_for(&self, v: &VendorId) -> Arc<dyn UpstreamErrorAnalyzer> {
        self.error_analyzers
            .get(v)
            .map(|e| e.value().clone())
            .or_else(|| builtin_error_analyzer_for(v))
            .unwrap_or_else(|| {
                (self.default_error_analyzer.clone()) as Arc<dyn UpstreamErrorAnalyzer>
            })
    }
}

fn builtin_adapter_for(vendor: &VendorId) -> Option<Arc<dyn UpstreamAdapter>> {
    let id = vendor.as_ref().to_ascii_lowercase();
    if id.contains("kiro") {
        return Some(Arc::new(KiroAdapter));
    }
    if id.contains("vertex") || id.contains("google_vertex") {
        return Some(Arc::new(VertexAdapter));
    }
    if id.contains("codex") {
        return Some(Arc::new(CodexAdapter));
    }
    None
}

fn builtin_error_analyzer_for(vendor: &VendorId) -> Option<Arc<dyn UpstreamErrorAnalyzer>> {
    let id = vendor.as_ref().to_ascii_lowercase();
    if id.contains("codex") || id.contains("openai") {
        return Some(Arc::new(KeywordErrorAnalyzer::codex()));
    }
    if id.contains("anthropic") || id.contains("claude") {
        return Some(Arc::new(KeywordErrorAnalyzer::anthropic()));
    }
    if id.contains("gemini") || id.contains("google") {
        return Some(Arc::new(KeywordErrorAnalyzer::gemini()));
    }
    None
}
