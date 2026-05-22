//! Tests for /admin/chaos-stats (and Task 4's /admin/hits-total).

use std::sync::Arc;
use std::time::Duration;

use e2e_fake_upstream::chaos::set_enabled_for_test;
use e2e_fake_upstream::inject::RefuseCounter;
use e2e_fake_upstream::routes::{make_counters, make_weighted_counters};
use e2e_fake_upstream::scenario::{
    MatchSpec, ResponseSpec, Route, Scenario, SseEvent, WeightedVariant,
};
use e2e_fake_upstream::server::{serve_admin, serve_business};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

async fn drive_once(addr: std::net::SocketAddr, path: &str) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
}

async fn fetch_json(addr: std::net::SocketAddr, path: &str) -> serde_json::Value {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let raw = String::from_utf8_lossy(&buf);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("bad json: {e}\nraw:\n{raw}"))
}

#[tokio::test(flavor = "current_thread")]
async fn chaos_stats_counts_variants() {
    set_enabled_for_test(true);

    let scenario = Scenario {
        routes: vec![Route {
            r#match: MatchSpec {
                method: Some("POST".into()),
                path_regex: Some("^/x$".into()),
                header_eq: None,
            },
            auth_check: None,
            response: ResponseSpec::Weighted {
                variants: vec![
                    WeightedVariant {
                        weight: 1,
                        spec: Box::new(ResponseSpec::Sse {
                            delay_ms: 0,
                            events: vec![SseEvent {
                                data: "HAPPY".into(),
                                after_ms: 0,
                            }],
                        }),
                    },
                    WeightedVariant {
                        weight: 1,
                        spec: Box::new(ResponseSpec::Static {
                            status: 500,
                            headers: Default::default(),
                            body: "FAULT".into(),
                            delay_ms: 0,
                        }),
                    },
                ],
            },
        }],
    };

    let counters = make_counters(&scenario);
    let weighted = make_weighted_counters(&scenario);
    let business = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let baddr = business.local_addr().unwrap();
    let aaddr = admin.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    let wh = Arc::new(RwLock::new(weighted));
    let refuse = RefuseCounter::new();
    tokio::spawn(serve_business(
        business,
        Arc::clone(&s),
        Arc::clone(&c),
        Arc::clone(&wh),
        refuse.clone(),
    ));
    tokio::spawn(serve_admin(admin, s, c, wh, refuse));

    for _ in 0..50 {
        drive_once(baddr, "/x").await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stats = fetch_json(aaddr, "/admin/chaos-stats").await;
    assert_eq!(stats["enabled"], serde_json::json!(true));
    let variants = stats["variants"].as_array().expect("variants array");
    assert_eq!(variants.len(), 2);
    let sum: u64 = variants.iter().map(|v| v["hits"].as_u64().unwrap()).sum();
    assert_eq!(sum, 50, "every request must increment exactly one variant");
}

#[tokio::test(flavor = "current_thread")]
async fn hits_total_sums_routes_and_refused() {
    let scenario = Scenario {
        routes: vec![Route {
            r#match: MatchSpec {
                method: Some("POST".into()),
                path_regex: Some("^/x$".into()),
                header_eq: None,
            },
            auth_check: None,
            response: ResponseSpec::ConnectionRefusedFirstN { n: 3 },
        }],
    };
    let counters = make_counters(&scenario);
    let weighted = e2e_fake_upstream::routes::make_weighted_counters(&scenario);
    let business = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let baddr = business.local_addr().unwrap();
    let aaddr = admin.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    let wh = Arc::new(RwLock::new(weighted));
    let refuse = RefuseCounter::new();
    e2e_fake_upstream::server::prime_refuse_counter(&*s.read().await, &refuse);
    tokio::spawn(serve_business(
        business,
        Arc::clone(&s),
        Arc::clone(&c),
        Arc::clone(&wh),
        refuse.clone(),
    ));
    tokio::spawn(serve_admin(admin, s, c, wh, refuse));

    // 3 refused (counter primed to 3), then 2 served (route hit).
    for _ in 0..5 {
        drive_once(baddr, "/x").await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let v = fetch_json(aaddr, "/admin/hits-total").await;
    assert_eq!(v["refused_hit_sum"], serde_json::json!(3));
    assert_eq!(v["routes_hit_sum"], serde_json::json!(2));
    assert_eq!(v["total"], serde_json::json!(5));
}
