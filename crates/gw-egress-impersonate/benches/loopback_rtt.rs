//! Task 6.2 — Loopback RTT microbench.
//!
//! Measures the round-trip time of a hairpin request through the egress
//! impersonate stack (bench client → egress listener → mock upstream →
//! egress listener → bench client) for both transports:
//!
//!   - **UDS** (`EgressBind::Uds`)  — SLO target: p99 < 200 µs (spec §11 #6)
//!   - **TCP** (`EgressBind::Tcp`)  — SLO target: p99 < 500 µs (spec §11 #6)
//!
//! Verification is **manual**: criterion does not natively support a pass/fail
//! gate on p99. After running this bench, open the HTML report at
//! `target/criterion/loopback_rtt/<transport>/report/index.html` and compare
//! the p99 against the SLO. If exceeded, file an issue per spec §11 #6.
//!
//! Run with:
//!   `cargo bench -p gw-egress-impersonate --bench loopback_rtt`
//!
//! # Implementation notes
//!
//! - `EgressHandle::start` is **synchronous** but calls `tokio::spawn`
//!   internally, so it must be invoked from inside `rt.block_on(...)` (or
//!   from inside an async block driven by the same runtime).
//! - The plan's pseudocode reads back the bound port via
//!   `EgressHandle::bind_addr()`, but `bind_addr()` returns the original
//!   `EgressBind` it was started with — for the `127.0.0.1:0` ephemeral case
//!   that is *still* port 0, not the OS-assigned port. We work around this
//!   by pre-binding a throwaway `TcpListener` to discover a free loopback
//!   port, dropping it, and passing the resolved `SocketAddr` to
//!   `EgressHandle::start`. There is a small TOCTOU window, but for a
//!   loopback microbench this is acceptable.
//! - The mock upstream is a 5-line hyper service that returns an empty 200.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use http_body_util::{BodyExt, Empty, Full};
use hyper_util::rt::TokioIo;

use gw_egress_impersonate::handle::{EgressBind, EgressHandle};

// ── Mock upstream ────────────────────────────────────────────────────────────

/// Spawn a minimal hyper service on a loopback TCP socket that returns an
/// empty 200 OK for every request. Returns the bound port. Lives for the rest
/// of the process — fine for a bench harness.
fn spawn_mock_upstream(rt: &tokio::runtime::Runtime) -> u16 {
    rt.block_on(async {
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let port = listener
            .local_addr()
            .expect("mock upstream local_addr")
            .port();

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(
                            Empty::<Bytes>::new(),
                        ))
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });

        port
    })
}

// ── Egress fixtures ──────────────────────────────────────────────────────────

/// A started egress instance plus the address bench clients should dial. The
/// `_tempdir` field keeps the UDS tempdir alive for the bench duration.
struct EgressFixture {
    handle: EgressHandle,
    dial: DialAddr,
    _tempdir: Option<tempfile::TempDir>,
}

#[derive(Clone)]
enum DialAddr {
    Tcp(std::net::SocketAddr),
    Uds(PathBuf),
}

/// Start an `EgressHandle` for the given transport and return everything
/// needed to dial it. Must be called from inside a tokio runtime (we rely on
/// `EgressHandle::start` calling `tokio::spawn`).
fn start_egress(rt: &tokio::runtime::Runtime, transport: &str) -> EgressFixture {
    rt.block_on(async {
        match transport {
            "uds" => {
                let dir = tempfile::tempdir().expect("tempdir");
                let path: PathBuf = dir.path().join("egress.sock");
                let handle = EgressHandle::start(EgressBind::Uds(path.clone()), 16);
                EgressFixture {
                    handle,
                    dial: DialAddr::Uds(path),
                    _tempdir: Some(dir),
                }
            }
            "tcp" => {
                // `EgressBind::Tcp("127.0.0.1:0")` would bind successfully but
                // `EgressHandle::bind_addr()` only echoes the input port (0),
                // so we can't learn the real port. Pre-bind to find a free
                // port, drop, then bind for real.
                let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("probe bind");
                let addr = probe.local_addr().expect("probe local_addr");
                drop(probe);
                let handle = EgressHandle::start(EgressBind::Tcp(addr), 16);
                EgressFixture {
                    handle,
                    dial: DialAddr::Tcp(addr),
                    _tempdir: None,
                }
            }
            _ => unreachable!(),
        }
    })
}

/// Give the supervisor a moment to bind and enter its accept loop.
fn wait_for_ready(rt: &tokio::runtime::Runtime, fixture: &EgressFixture) {
    rt.block_on(async {
        // Poll-connect with backoff for up to ~500ms. On loopback this almost
        // always succeeds on the first try.
        for _ in 0..50 {
            let connected = match &fixture.dial {
                DialAddr::Tcp(addr) => tokio::net::TcpStream::connect(addr).await.is_ok(),
                DialAddr::Uds(path) => tokio::net::UnixStream::connect(path).await.is_ok(),
            };
            if connected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "egress listener never became ready for {:?}",
            fixture.handle.bind_addr()
        );
    });
}

// ── One request ──────────────────────────────────────────────────────────────

async fn send_once(dial: &DialAddr, req: hyper::Request<Full<Bytes>>) {
    match dial {
        DialAddr::Tcp(addr) => {
            let stream = tokio::net::TcpStream::connect(addr)
                .await
                .expect("tcp connect");
            run_h1(stream, req).await;
        }
        DialAddr::Uds(path) => {
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .expect("uds connect");
            run_h1(stream, req).await;
        }
    }
}

async fn run_h1<S>(stream: S, req: hyper::Request<Full<Bytes>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let resp = sender.send_request(req).await.expect("send_request");
    let _ = resp.into_body().collect().await.expect("drain body");
}

// ── Bench ────────────────────────────────────────────────────────────────────

fn bench_loopback(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mock_port = spawn_mock_upstream(&rt);

    let mut group = c.benchmark_group("loopback_rtt");
    group.measurement_time(Duration::from_secs(10));

    for transport in ["uds", "tcp"] {
        let fixture = start_egress(&rt, transport);
        wait_for_ready(&rt, &fixture);
        let dial = fixture.dial.clone();

        group.bench_with_input(BenchmarkId::from_parameter(transport), &(), |b, _| {
            b.to_async(&rt).iter(|| {
                let dial = dial.clone();
                async move {
                    let req = hyper::Request::builder()
                        .method("POST")
                        .uri("/")
                        .header("X-Egress-Target", format!("127.0.0.1:{mock_port}"))
                        .header("X-Egress-Profile", "claude_code_chrome131")
                        .body(Full::new(Bytes::from_static(b"")))
                        .expect("build request");
                    send_once(&dial, req).await;
                }
            });
        });

        // Keep the handle alive for the whole group; drop after measurement.
        drop(fixture);
    }

    group.finish();
}

criterion_group!(benches, bench_loopback);
criterion_main!(benches);
