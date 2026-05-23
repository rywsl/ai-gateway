use gw_tunnel_client::safe_dns::is_ip_blocked;

#[test]
fn private_v4_blocked() {
    for ip in [
        "10.0.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "192.168.1.1",
        "172.16.0.1",
        "100.64.0.1",
    ] {
        assert!(is_ip_blocked(ip.parse().unwrap()), "expected blocked: {ip}");
    }
}

#[test]
fn private_v6_blocked() {
    for ip in ["::1", "fc00::1", "fe80::1"] {
        assert!(is_ip_blocked(ip.parse().unwrap()), "expected blocked: {ip}");
    }
}

#[test]
fn public_ips_allowed() {
    for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        assert!(
            !is_ip_blocked(ip.parse().unwrap()),
            "expected allowed: {ip}"
        );
    }
}
