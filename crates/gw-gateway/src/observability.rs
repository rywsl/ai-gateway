//! Tracing + metrics bootstrap.
//!
//! - `tracing_subscriber` is installed with JSON output and `EnvFilter`
//!   (defaulting to `info,gw=debug` when `RUST_LOG` is unset).
//! - `metrics_exporter_prometheus` exposes a `/` scrape endpoint on
//!   `cfg.server.metrics_port`. The exporter installs itself globally so
//!   any crate emitting `metrics::counter!` / `histogram!` macros lights up
//!   without further plumbing.

use crate::config::Config;
use anyhow::Context;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::SocketAddr;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init(cfg: &Config) -> anyhow::Result<()> {
    init_tracing()?;
    init_metrics(cfg)?;
    Ok(())
}

fn init_tracing() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,gw=debug"));

    let fmt_layer = fmt::layer().json().with_current_span(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing init failed: {e}"))?;

    Ok(())
}

fn init_metrics(cfg: &Config) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.server.metrics_port)
        .parse()
        .context("invalid metrics listener address")?;

    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .context("failed to install prometheus exporter")?;

    // Emit a baseline counter at startup so `/metrics` is non-empty even
    // before any traffic completes. Without at least one `counter!`/`gauge!`
    // call, `metrics-exporter-prometheus` renders the registry as an empty
    // body — which makes a healthy 200 response indistinguishable from a
    // broken exporter during ops triage.
    metrics::counter!("gateway_started_total").increment(1);

    crate::metrics_catalog::describe_all();

    // Page-on-call billing counters fire only on an incident, so without a boot
    // zero-init they are absent from `/metrics` until the bad event — leaving the
    // Go-side `billing-rules.yml` alerts pointed at an absent series. Materialize
    // them at `0` so the alert rules always have a healthy baseline.
    crate::metrics_catalog::zero_init_page_on_call();

    Ok(())
}
