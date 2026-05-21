use gw_adapter::{DefaultErrorAnalyzer, ErrorOutcome, HeaderView, UpstreamErrorAnalyzer};

struct DummyHeaders;
impl HeaderView for DummyHeaders {
    fn get(&self, _: &str) -> Option<&str> {
        None
    }
}

#[test]
fn outcome_has_cooldown_enhanced_body_oauth_signal() {
    let oc = ErrorOutcome::default();
    assert!(oc.cooldown.is_none());
    assert!(oc.enhanced_body.is_none());
    assert!(oc.oauth_signal.is_none());
}

#[test]
fn default_analyzer_401_yields_oauth_signal_none_until_vendor() {
    let a = DefaultErrorAnalyzer;
    let oc = a.analyze(401, b"", &DummyHeaders);
    // Default 不知道是不是 oauth；vendor 子类才决定
    assert!(oc.oauth_signal.is_none());
}
