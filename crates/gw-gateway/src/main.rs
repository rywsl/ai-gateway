//! gw-gateway binary entrypoint.
//!
//! Installs jemalloc as the global allocator (the gateway is high-throughput
//! and jemalloc's arena layout gives us tighter tail latency than glibc
//! malloc), then delegates everything else to
//! [`gw_gateway::run_with_signal_handler`], which wraps bootstrap +
//! Pingora-serve with SIGTERM/SIGINT-driven `ShutdownCoordinator::drain`
//! (resilience-fixes T2.2).

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    gw_gateway::run_with_signal_handler()
}
