use gw_tunnel_client::target_filter::{assert_target_allowed, set_whitelist, ResetGuard};
use url::Url;

#[test]
fn whitelisted_443_passes() {
    let _g = ResetGuard::new();
    set_whitelist(vec![("api.anthropic.com".into(), 443)]);
    assert!(assert_target_allowed(&Url::parse("https://api.anthropic.com/v1").unwrap()).is_ok());
}

#[test]
fn non_whitelisted_denied() {
    let _g = ResetGuard::new();
    set_whitelist(vec![("api.anthropic.com".into(), 443)]);
    assert!(assert_target_allowed(&Url::parse("https://evil.example.com/").unwrap()).is_err());
}

#[test]
fn http_80_denied_unless_in_whitelist() {
    let _g = ResetGuard::new();
    set_whitelist(vec![]);
    assert!(assert_target_allowed(&Url::parse("http://api.anthropic.com/").unwrap()).is_err());
}

#[test]
fn http_80_passes_when_explicitly_whitelisted() {
    let _g = ResetGuard::new();
    set_whitelist(vec![("status.example.com".into(), 80)]);
    assert!(assert_target_allowed(&Url::parse("http://status.example.com/health").unwrap()).is_ok());
}
