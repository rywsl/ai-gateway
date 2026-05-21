//! `fake_upstream` binary — fixed ports 9000 (business) / 9001 (admin).
//!
//! The library crate exposes [`serve_business`] / [`serve_admin`] so that
//! integration tests can bind ephemeral ports without going through main.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::RwLock;

use e2e_fake_upstream::inject::RefuseCounter;
use e2e_fake_upstream::routes::make_counters;
use e2e_fake_upstream::scenario::{self, default_scenario};
use e2e_fake_upstream::server::{prime_refuse_counter, serve_admin, serve_business};

// ── CLI / env arg parsing ─────────────────────────────────────────────────────

fn scenario_path() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--scenario" {
            if let Some(p) = args.get(i + 1) {
                return Some(p.clone());
            }
        }
    }
    std::env::var("FAKE_UPSTREAM_SCENARIO").ok()
}

#[tokio::main]
async fn main() {
    let load_scenario_file = |path: &str| -> Option<scenario::Scenario> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| eprintln!("[fake-upstream] cannot read scenario file {path:?}: {e}"))
            .ok()?;
        serde_json::from_str(&raw)
            .map_err(|e| eprintln!("[fake-upstream] invalid scenario JSON in {path:?}: {e}"))
            .ok()
    };

    let initial_scenario = if let Some(path) = scenario_path() {
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
    let refuse = RefuseCounter::new();
    prime_refuse_counter(&initial_scenario, &refuse);

    let scenario = Arc::new(RwLock::new(initial_scenario));
    let counters = Arc::new(RwLock::new(initial_counters));

    let business_addr: SocketAddr = "0.0.0.0:9000".parse().expect("valid address");
    let admin_addr: SocketAddr = "0.0.0.0:9001".parse().expect("valid address");

    let business_listener = TcpListener::bind(business_addr).await.expect("bind :9000");
    let admin_listener = TcpListener::bind(admin_addr).await.expect("bind :9001");

    eprintln!("[fake-upstream] business server listening on {business_addr}");
    eprintln!("[fake-upstream] admin server listening on {admin_addr}");

    let sc_b = Arc::clone(&scenario);
    let ctr_b = Arc::clone(&counters);
    let refuse_b = refuse.clone();
    let sc_a = Arc::clone(&scenario);
    let ctr_a = Arc::clone(&counters);
    let refuse_a = refuse.clone();

    let business_task =
        tokio::spawn(async move { serve_business(business_listener, sc_b, ctr_b, refuse_b).await });
    let admin_task =
        tokio::spawn(async move { serve_admin(admin_listener, sc_a, ctr_a, refuse_a).await });

    let _ = tokio::join!(business_task, admin_task);
}
