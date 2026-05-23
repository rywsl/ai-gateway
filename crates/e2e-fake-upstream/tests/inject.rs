//! In-process integration tests for the failure-injection primitives.
//!
//! Each test:
//!   1. Builds a one-route scenario for the variant under test.
//!   2. Binds the business listener on `127.0.0.1:0` and spawns `serve_business`.
//!   3. Drives the server via a RAW `TcpStream` so we can observe RST /
//!      partial bytes / drip timing without hyper smoothing over them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use e2e_fake_upstream::inject::RefuseCounter;
use e2e_fake_upstream::routes::{make_counters, make_weighted_counters};
use e2e_fake_upstream::scenario::{MatchSpec, ResponseSpec, Route, Scenario};
use e2e_fake_upstream::server::{prime_refuse_counter, serve_business};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

// ── helpers ──────────────────────────────────────────────────────────────────

fn route(path_regex: &str, response: ResponseSpec) -> Route {
    Route {
        r#match: MatchSpec {
            method: Some("POST".into()),
            path_regex: Some(path_regex.into()),
            header_eq: None,
        },
        auth_check: None,
        response,
    }
}

async fn spawn_server(scenario: Scenario) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counters = make_counters(&scenario);
    let weighted_hits = make_weighted_counters(&scenario);
    let refuse = RefuseCounter::new();
    prime_refuse_counter(&scenario, &refuse);
    let scenario = Arc::new(RwLock::new(scenario));
    let counters = Arc::new(RwLock::new(counters));
    let weighted_hits = Arc::new(RwLock::new(weighted_hits));
    tokio::spawn(async move {
        serve_business(listener, scenario, counters, weighted_hits, refuse).await
    });
    addr
}

async fn send_post(stream: &mut TcpStream, path: &str) -> std::io::Result<()> {
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Read until EOF (clean OR reset), returning whatever bytes we got plus the
/// terminating io::Error kind (`UnexpectedEof` / `ConnectionReset`) if any.
async fn read_to_end_observed(stream: &mut TcpStream) -> (Vec<u8>, Option<std::io::ErrorKind>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => return (buf, None), // clean EOF
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) => return (buf, Some(e.kind())),
        }
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tcp_reset_after_headers() {
    let scenario = Scenario {
        routes: vec![route(r"^/v1/rst$", ResponseSpec::TcpResetAfterHeaders)],
    };
    let addr = spawn_server(scenario).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    send_post(&mut stream, "/v1/rst").await.unwrap();
    let (buf, err_kind) = read_to_end_observed(&mut stream).await;

    let text = String::from_utf8_lossy(&buf);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "should see headers before reset, got: {text:?}"
    );
    assert!(
        text.contains("text/event-stream"),
        "header dump should contain content-type, got: {text:?}"
    );
    // Body MUST be empty (headers only).
    let body_offset = text.find("\r\n\r\n").unwrap() + 4;
    assert_eq!(
        &buf[body_offset..],
        b"",
        "no body bytes should be sent before reset"
    );
    // We expect either an explicit ConnectionReset or a clean EOF (platform
    // dependent — see module doc).
    if let Some(k) = err_kind {
        assert!(
            matches!(
                k,
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof
            ),
            "expected ConnectionReset or UnexpectedEof, got {k:?}"
        );
    }
}

#[tokio::test]
async fn tcp_reset_mid_body() {
    let scenario = Scenario {
        routes: vec![route(
            r"^/v1/rst-mid$",
            ResponseSpec::TcpResetMidBody { after_bytes: 7 },
        )],
    };
    let addr = spawn_server(scenario).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    send_post(&mut stream, "/v1/rst-mid").await.unwrap();
    let (buf, _err_kind) = read_to_end_observed(&mut stream).await;

    let text = String::from_utf8_lossy(&buf);
    let body_offset = text.find("\r\n\r\n").unwrap() + 4;
    let body = &buf[body_offset..];
    assert_eq!(body, b"AAAAAAA", "should receive exactly 7 'A' bytes");
}

#[tokio::test]
async fn connection_refused_first_n() {
    let scenario = Scenario {
        routes: vec![
            // First route just declares the refuse semantics; n=1.
            route(
                r"^/v1/refused$",
                ResponseSpec::ConnectionRefusedFirstN { n: 1 },
            ),
            // A second route gives us a normal response after the refusal.
            route(
                r"^/v1/ok$",
                ResponseSpec::Static {
                    status: 200,
                    headers: Default::default(),
                    body: "hello".into(),
                    delay_ms: 0,
                },
            ),
        ],
    };
    let addr = spawn_server(scenario).await;

    // First connection: server drops the socket pre-handshake. We should
    // see either an immediate read error or a clean EOF with zero bytes.
    let attempt = TcpStream::connect(addr).await;
    if let Ok(mut s) = attempt {
        // Best-effort send; server may have already RST'd.
        let _ = send_post(&mut s, "/v1/ok").await;
        let (buf, err) = read_to_end_observed(&mut s).await;
        assert!(
            buf.is_empty(),
            "no HTTP response should be sent on refused connection"
        );
        // err may be Some(ConnectionReset) or None (clean EOF) — both OK.
        let _ = err;
    }

    // Second connection: counter is now 0, should be served normally.
    let mut s = TcpStream::connect(addr).await.unwrap();
    send_post(&mut s, "/v1/ok").await.unwrap();
    let (buf, _err) = read_to_end_observed(&mut s).await;
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "got: {text:?}");
    assert!(text.ends_with("hello"), "got: {text:?}");
}

#[tokio::test]
async fn slow_body_byte_per_second() {
    // 3 bytes at 20 bps → ~0.1s total (well under 2s test budget).
    let scenario = Scenario {
        routes: vec![route(
            r"^/v1/slow$",
            ResponseSpec::SlowBodyBytePerSecond {
                body: "abc".into(),
                bps: 20,
            },
        )],
    };
    let addr = spawn_server(scenario).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    send_post(&mut s, "/v1/slow").await.unwrap();

    let start = Instant::now();
    let (buf, _err) = read_to_end_observed(&mut s).await;
    let elapsed = start.elapsed();

    let text = String::from_utf8_lossy(&buf);
    let body_offset = text.find("\r\n\r\n").unwrap() + 4;
    let body = &buf[body_offset..];
    assert_eq!(body, b"abc", "should receive full body");
    // At 20 bps with 3 bytes we expect ~100ms (gaps between 2nd and 3rd).
    assert!(
        elapsed >= Duration::from_millis(80),
        "drip should take measurable time; elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "but not exceed budget; elapsed={elapsed:?}"
    );
}

#[tokio::test]
async fn partial_sse_event() {
    let scenario = Scenario {
        routes: vec![route(
            r"^/v1/partial$",
            ResponseSpec::PartialSseEvent {
                half_body: "{\"choices\":[".into(),
            },
        )],
    };
    let addr = spawn_server(scenario).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    send_post(&mut s, "/v1/partial").await.unwrap();
    let (buf, err) = read_to_end_observed(&mut s).await;

    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(text.contains("text/event-stream"));
    // Body should be `data: {"choices":[` with NO trailing `\n\n`.
    let body_offset = text.find("\r\n\r\n").unwrap() + 4;
    let body = &buf[body_offset..];
    assert_eq!(body, b"data: {\"choices\":[");
    assert!(!body.ends_with(b"\n\n"), "must NOT have SSE boundary");
    // Should be a clean EOF (None), not a reset.
    assert!(
        err.is_none(),
        "partial_sse_event must close cleanly (FIN), got {err:?}"
    );
}

#[tokio::test]
async fn status_with_retry_after() {
    let scenario = Scenario {
        routes: vec![route(
            r"^/v1/ratelimit$",
            ResponseSpec::StatusWithRetryAfter {
                status: 429,
                retry_after_secs: 7,
                body: r#"{"error":"slow down"}"#.into(),
            },
        )],
    };
    let addr = spawn_server(scenario).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    send_post(&mut s, "/v1/ratelimit").await.unwrap();
    let (buf, _err) = read_to_end_observed(&mut s).await;

    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 429"), "got: {text:?}");
    let lower = text.to_lowercase();
    assert!(lower.contains("retry-after: 7"), "got: {text:?}");
    assert!(text.ends_with(r#"{"error":"slow down"}"#), "got: {text:?}");
}

// ── happy path through the new accept loop ───────────────────────────────────

#[tokio::test]
async fn happy_path_sse_through_new_accept_loop() {
    use e2e_fake_upstream::scenario::SseEvent;
    let scenario = Scenario {
        routes: vec![route(
            r"^/v1/chat$",
            ResponseSpec::Sse {
                delay_ms: 0,
                events: vec![
                    SseEvent {
                        data: "first".into(),
                        after_ms: 0,
                    },
                    SseEvent {
                        data: "[DONE]".into(),
                        after_ms: 0,
                    },
                ],
            },
        )],
    };
    let addr = spawn_server(scenario).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    send_post(&mut s, "/v1/chat").await.unwrap();
    let (buf, _err) = read_to_end_observed(&mut s).await;

    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(text.contains("text/event-stream"));
    assert!(text.contains("data: first\n\n"));
    assert!(text.contains("data: [DONE]\n\n"));
}
