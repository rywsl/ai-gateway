mod routes;
mod scenario;
mod server;

use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use routes::make_counters;
use scenario::default_scenario;
use server::{handle_admin, handle_business, SharedCounters, SharedScenario};

// ── CLI / env arg parsing ─────────────────────────────────────────────────────

fn scenario_path() -> Option<String> {
    // --scenario <path> takes priority.
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--scenario" {
            if let Some(p) = args.get(i + 1) {
                return Some(p.clone());
            }
        }
    }
    // Fall back to env var.
    std::env::var("FAKE_UPSTREAM_SCENARIO").ok()
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Load scenario from file or fall back to built-in default.
    //
    // Precedence (highest → lowest):
    //   1. --scenario <path>  or  FAKE_UPSTREAM_SCENARIO env
    //   2. /etc/fake-upstream/scenario.json  (well-known default file)
    //   3. built-in default_scenario()
    let load_scenario_file = |path: &str| -> Option<scenario::Scenario> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| {
                eprintln!("[fake-upstream] cannot read scenario file {path:?}: {e}");
            })
            .ok()?;
        serde_json::from_str(&raw)
            .map_err(|e| {
                eprintln!("[fake-upstream] invalid scenario JSON in {path:?}: {e}");
            })
            .ok()
    };

    let initial_scenario = if let Some(path) = scenario_path() {
        // Explicit path (arg or env): try it; if it fails, fall through to default.
        match load_scenario_file(&path) {
            Some(s) => {
                eprintln!("[fake-upstream] loaded scenario from {path:?}");
                s
            }
            None => {
                eprintln!("[fake-upstream] falling back to built-in default");
                default_scenario()
            }
        }
    } else {
        // No explicit path — try the well-known default file.
        const WELL_KNOWN: &str = "/etc/fake-upstream/scenario.json";
        match load_scenario_file(WELL_KNOWN) {
            Some(s) => {
                eprintln!("[fake-upstream] loaded scenario from well-known path {WELL_KNOWN}");
                s
            }
            None => {
                eprintln!("[fake-upstream] no scenario file found — using built-in default");
                default_scenario()
            }
        }
    };

    let initial_counters = make_counters(&initial_scenario);

    let scenario: SharedScenario = Arc::new(RwLock::new(initial_scenario));
    let counters: SharedCounters = Arc::new(RwLock::new(initial_counters));

    // Bind both ports concurrently.
    let business_addr: SocketAddr = "0.0.0.0:9000".parse().expect("valid address");
    let admin_addr: SocketAddr = "0.0.0.0:9001".parse().expect("valid address");

    let business_listener = TcpListener::bind(business_addr).await.expect("bind :9000");
    let admin_listener = TcpListener::bind(admin_addr).await.expect("bind :9001");

    eprintln!("[fake-upstream] business server listening on {business_addr}");
    eprintln!("[fake-upstream] admin server listening on {admin_addr}");

    // Clone Arcs for the two server tasks.
    let sc_business = Arc::clone(&scenario);
    let ctr_business = Arc::clone(&counters);
    let sc_admin = Arc::clone(&scenario);
    let ctr_admin = Arc::clone(&counters);

    let business_task = tokio::spawn(async move {
        loop {
            let (stream, _peer) = match business_listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[fake-upstream] business accept error: {e}");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let sc = Arc::clone(&sc_business);
            let ctr = Arc::clone(&ctr_business);
            tokio::spawn(async move {
                if let Err(e) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            handle_business(req, Arc::clone(&sc), Arc::clone(&ctr))
                        }),
                    )
                    .await
                {
                    eprintln!("[fake-upstream] business connection error: {e}");
                }
            });
        }
    });

    let admin_task = tokio::spawn(async move {
        loop {
            let (stream, _peer) = match admin_listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[fake-upstream] admin accept error: {e}");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let sc = Arc::clone(&sc_admin);
            let ctr = Arc::clone(&ctr_admin);
            tokio::spawn(async move {
                if let Err(e) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| handle_admin(req, Arc::clone(&sc), Arc::clone(&ctr))),
                    )
                    .await
                {
                    eprintln!("[fake-upstream] admin connection error: {e}");
                }
            });
        }
    });

    // Run both servers indefinitely.
    let _ = tokio::join!(business_task, admin_task);
}
