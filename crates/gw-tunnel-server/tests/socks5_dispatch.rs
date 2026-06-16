use gw_tunnel_server::{parse_egress_url, EgressMode, TunnelServerConfig};
use std::time::Duration;

#[test]
fn config_defaults_to_direct_egress() {
    let config = TunnelServerConfig::default();

    assert!(
        matches!(config.egress, EgressMode::Direct),
        "default egress must be Direct, got {:?}",
        config.egress
    );
}

#[test]
fn egress_mode_socks5_constructs() {
    let mode = EgressMode::Socks5 {
        proxy_host: "127.0.0.1".to_owned(),
        proxy_port: 1080,
        auth: None,
        resolve_at_proxy: true,
    };

    match mode {
        EgressMode::Socks5 {
            resolve_at_proxy, ..
        } => assert!(resolve_at_proxy),
        EgressMode::Direct => panic!("expected socks5 mode"),
    }
}

#[test]
fn egress_mode_socks5_with_auth() {
    let mode = EgressMode::Socks5 {
        proxy_host: "127.0.0.1".to_owned(),
        proxy_port: 1080,
        auth: Some(("user".to_owned(), "pass".to_owned())),
        resolve_at_proxy: false,
    };

    match mode {
        EgressMode::Socks5 {
            auth: Some((user, pass)),
            ..
        } => {
            assert_eq!(user, "user");
            assert_eq!(pass, "pass");
        }
        _ => panic!("expected socks5 auth"),
    }
}

#[test]
fn parse_egress_url_none_yields_direct() {
    let mode = parse_egress_url(None).expect("parse");

    assert!(matches!(mode, EgressMode::Direct));
}

#[test]
fn parse_egress_url_socks5_basic() {
    let mode = parse_egress_url(Some("socks5://127.0.0.1:1080")).expect("parse");

    match mode {
        EgressMode::Socks5 {
            proxy_host,
            proxy_port,
            auth,
            resolve_at_proxy,
        } => {
            assert_eq!(proxy_host, "127.0.0.1");
            assert_eq!(proxy_port, 1080);
            assert!(auth.is_none());
            assert!(!resolve_at_proxy, "scheme socks5 must resolve locally");
        }
        EgressMode::Direct => panic!("expected socks5 mode"),
    }
}

#[test]
fn parse_egress_url_socks5h_sets_resolve_at_proxy() {
    let mode = parse_egress_url(Some("socks5h://192.0.2.10:9050")).expect("parse");

    match mode {
        EgressMode::Socks5 {
            resolve_at_proxy, ..
        } => assert!(resolve_at_proxy, "scheme socks5h must resolve at proxy"),
        EgressMode::Direct => panic!("expected socks5 mode"),
    }
}

#[test]
fn parse_egress_url_with_userinfo() {
    let mode = parse_egress_url(Some("socks5h://user:pass@10.0.0.5:1080")).expect("parse");

    match mode {
        EgressMode::Socks5 {
            auth: Some((user, pass)),
            ..
        } => {
            assert_eq!(user, "user");
            assert_eq!(pass, "pass");
        }
        _ => panic!("expected socks5 auth"),
    }
}

#[test]
fn parse_egress_url_percent_decodes_userinfo() {
    let mode = parse_egress_url(Some("socks5h://u%40ser:p%3Ass@10.0.0.5:1080")).expect("parse");

    match mode {
        EgressMode::Socks5 {
            auth: Some((user, pass)),
            ..
        } => {
            assert_eq!(user, "u@ser");
            assert_eq!(pass, "p:ss");
        }
        _ => panic!("expected percent-decoded socks5 auth"),
    }
}

#[test]
fn parse_egress_url_default_port_socks5() {
    let mode = parse_egress_url(Some("socks5://proxy.local")).expect("parse");

    match mode {
        EgressMode::Socks5 {
            proxy_host,
            proxy_port,
            ..
        } => {
            assert_eq!(proxy_host, "proxy.local");
            assert_eq!(proxy_port, 1080);
        }
        EgressMode::Direct => panic!("expected socks5 mode"),
    }
}

#[test]
fn parse_egress_url_rejects_unsupported_scheme() {
    for value in ["http://x:8080", "https://x:8443", "tcp://x:80", "garbage"] {
        let result = parse_egress_url(Some(value));
        assert!(result.is_err(), "expected error for {value}");
    }
}

#[test]
fn parse_egress_url_does_not_resolve_dns_at_parse_time() {
    let mode = parse_egress_url(Some("socks5h://invalid-host-no-dns.example:9050"))
        .expect("parse must not perform DNS");

    match mode {
        EgressMode::Socks5 {
            proxy_host,
            proxy_port,
            ..
        } => {
            assert_eq!(proxy_host, "invalid-host-no-dns.example");
            assert_eq!(proxy_port, 9050);
        }
        EgressMode::Direct => panic!("expected socks5 mode"),
    }
}

#[tokio::test]
async fn direct_dial_reaches_localhost_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accept_task = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
    });

    let target = format!("127.0.0.1:{port}");
    let stream = gw_tunnel_server::testing::dial_via_egress(&EgressMode::Direct, &target)
        .await
        .expect("direct dial must succeed");
    drop(stream);
    accept_task.await.unwrap();
}

#[tokio::test]
async fn socks5_dial_to_unreachable_proxy_errors() {
    let mode = EgressMode::Socks5 {
        proxy_host: "127.0.0.1".to_owned(),
        proxy_port: 1,
        auth: None,
        resolve_at_proxy: true,
    };

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        gw_tunnel_server::testing::dial_via_egress(&mode, "example.com:80"),
    )
    .await
    .expect("must not hang");

    assert!(result.is_err(), "unreachable proxy must error");
}
