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

/// D22 (#18): match a (lowercased) vendor id against a builtin alias using a
/// token boundary, NOT a raw substring. An id matches an alias when it equals
/// the alias exactly or extends it with a `-`/`_` separated operator suffix
/// (e.g. `kiro-prod`, `google_vertex_us`, `codex-main`). Mid-string and
/// no-boundary occurrences (e.g. `akiro`, `codexcli`, `vendor_google_vertex`)
/// no longer match — those were the misclassifications caused by the previous
/// `contains()` logic. Vendors whose ids don't follow this convention should be
/// wired explicitly via `register_adapter` / `register_error_analyzer`
/// (`for_vendor` / `error_analyzer_for` consult the explicit map first).
fn vendor_matches(id: &str, alias: &str) -> bool {
    match id.strip_prefix(alias) {
        Some("") => true,
        Some(rest) => rest.starts_with('-') || rest.starts_with('_'),
        None => false,
    }
}

/// True if the id matches any of the given aliases under [`vendor_matches`].
fn matches_any(id: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| vendor_matches(id, alias))
}

fn builtin_adapter_for(vendor: &VendorId) -> Option<Arc<dyn UpstreamAdapter>> {
    let id = vendor.as_ref().to_ascii_lowercase();
    if matches_any(&id, &["kiro"]) {
        return Some(Arc::new(KiroAdapter));
    }
    if matches_any(&id, &["vertex", "google_vertex"]) {
        return Some(Arc::new(VertexAdapter));
    }
    if matches_any(&id, &["codex", "openai_codex"]) {
        return Some(Arc::new(CodexAdapter));
    }
    None
}

fn builtin_error_analyzer_for(vendor: &VendorId) -> Option<Arc<dyn UpstreamErrorAnalyzer>> {
    let id = vendor.as_ref().to_ascii_lowercase();
    if matches_any(&id, &["codex", "openai", "openai_codex"]) {
        return Some(Arc::new(KeywordErrorAnalyzer::codex()));
    }
    if matches_any(&id, &["anthropic", "claude"]) {
        return Some(Arc::new(KeywordErrorAnalyzer::anthropic()));
    }
    if matches_any(&id, &["gemini", "google", "google_vertex"]) {
        return Some(Arc::new(KeywordErrorAnalyzer::gemini()));
    }
    None
}

#[cfg(test)]
mod vendor_match_tests {
    use super::vendor_matches;

    #[test]
    fn exact_and_separator_suffix_match() {
        assert!(vendor_matches("kiro", "kiro"));
        assert!(vendor_matches("kiro-prod", "kiro"));
        assert!(vendor_matches("kiro_backup", "kiro")); // documented: `_` suffix is allowed (see google_vertex_us)
        assert!(vendor_matches("google_vertex_us", "google_vertex"));
        assert!(vendor_matches("codex-main", "codex"));
    }

    #[test]
    fn no_boundary_does_not_match() {
        // Substring / no-boundary cases the old `contains()` wrongly matched.
        assert!(!vendor_matches("akiro", "kiro"));
        assert!(!vendor_matches("codexcli", "codex"));
        assert!(!vendor_matches("vendor_google_vertex", "google_vertex"));
        assert!(!vendor_matches("mygemini", "gemini"));
    }
}
