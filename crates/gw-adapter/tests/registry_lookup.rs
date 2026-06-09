//! AdapterRegistry returns Noop for unregistered vendors and returns the
//! exact registered adapter when present.

use async_trait::async_trait;
use gw_adapter::analyzer::{ErrorOutcome, HeaderView};
use gw_adapter::{
    AdapterCtx, AdapterError, AdapterRegistry, UpstreamAdapter, UpstreamErrorAnalyzer,
};
use gw_core::ids::VendorId;
use std::sync::Arc;

struct MarkerAdapter;

#[async_trait]
impl UpstreamAdapter for MarkerAdapter {
    fn name(&self) -> &'static str {
        "marker"
    }
    async fn rewrite_auth(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

struct MarkerAnalyzer;
impl UpstreamErrorAnalyzer for MarkerAnalyzer {
    fn analyze(&self, _status: u16, _body: &[u8], _h: &dyn HeaderView) -> ErrorOutcome {
        ErrorOutcome::default()
    }
}

#[test]
fn lookup_returns_noop_when_unregistered() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("unknown".to_string());
    assert_eq!(r.for_vendor(&v).name(), "noop");
}

#[test]
fn lookup_returns_registered_adapter() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("marker-vendor".to_string());
    r.register_adapter(v.clone(), Arc::new(MarkerAdapter));
    assert_eq!(r.for_vendor(&v).name(), "marker");
}

#[test]
fn builtin_vendor_adapters_match_known_vendor_ids() {
    let r = AdapterRegistry::default();
    assert_eq!(r.for_vendor(&VendorId::from("kiro-prod")).name(), "kiro");
    assert_eq!(
        r.for_vendor(&VendorId::from("google_vertex_us")).name(),
        "vertex"
    );
}

#[test]
fn analyzer_lookup_falls_back_to_default() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("unknown".to_string());
    let analyzer = r.error_analyzer_for(&v);
    struct Empty;
    impl HeaderView for Empty {
        fn get(&self, _: &str) -> Option<&str> {
            None
        }
    }
    let outcome = analyzer.analyze(503, &[], &Empty);
    assert!(outcome.kind.is_some());
}

#[test]
fn registered_analyzer_overrides_default() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("marker-vendor".to_string());
    r.register_error_analyzer(v.clone(), Arc::new(MarkerAnalyzer));
    let analyzer = r.error_analyzer_for(&v);
    struct Empty;
    impl HeaderView for Empty {
        fn get(&self, _: &str) -> Option<&str> {
            None
        }
    }
    let outcome = analyzer.analyze(503, &[], &Empty);
    assert_eq!(outcome.kind, None);
}
