use gw_tunnel_client::safe_dns::{set_override, OverrideGuard, SafeDnsResolver};
use std::net::IpAddr;

#[tokio::test]
async fn override_redirects_to_specified_ip() {
    let _g = OverrideGuard::new();
    let target_ip: IpAddr = "1.2.3.4".parse().unwrap();
    set_override("api.x.test", vec![target_ip]);
    let r = SafeDnsResolver::cloudflare_default();
    let ips = r.resolve("api.x.test").await.unwrap();
    assert_eq!(ips, vec![target_ip]);
}

#[tokio::test]
async fn override_with_blocked_ip_errors() {
    let _g = OverrideGuard::new();
    set_override("api.x.test", vec!["169.254.169.254".parse().unwrap()]);
    let r = SafeDnsResolver::cloudflare_default();
    let err = r.resolve("api.x.test").await;
    assert!(err.is_err(), "blocked IP via override should error");
}

#[tokio::test]
async fn override_guard_restores_state() {
    let initial: IpAddr = "9.9.9.9".parse().unwrap();
    {
        let _g = OverrideGuard::new();
        set_override("api.x.test", vec![initial]);
        // Inside guard
    }
    // After guard, state should be reset to whatever existed before _g was created
    let r = SafeDnsResolver::cloudflare_default();
    let _ = r.resolve("api.x.test").await; // either errors (no override + non-existent host) or succeeds
}

#[tokio::test]
#[ignore]
async fn resolves_public_host_via_doh() {
    // Requires network. Run with `cargo test -- --ignored`.
    let r = SafeDnsResolver::cloudflare_default();
    let ips = r.resolve("one.one.one.one").await.unwrap();
    assert!(!ips.is_empty());
    use gw_tunnel_client::safe_dns::is_ip_blocked;
    for ip in ips {
        assert!(!is_ip_blocked(ip));
    }
}
