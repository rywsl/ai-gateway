use std::thread;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Parser)]
#[command(name = "ailinkmax-gateway", version, about = "Unified LLM gateway binary")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the LLM proxy gateway
    Gateway,
    /// Start the async task worker
    Worker,
    /// Start both gateway and worker in the same process
    All,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Gateway => gw_gateway::run(),
        Command::Worker => build_tokio_runtime()?.block_on(gw_task_worker::run()),
        Command::All => run_all(),
    }
}

fn run_all() -> anyhow::Result<()> {
    let gw_handle = thread::Builder::new()
        .name("gateway".into())
        .spawn(gw_gateway::run)
        .context("failed to spawn gateway thread")?;

    let worker_result = build_tokio_runtime()?.block_on(gw_task_worker::run());

    if let Err(e) = &worker_result {
        eprintln!("[unified] worker exited with error: {e:#}");
    }

    match gw_handle.join() {
        Ok(gw_result) => {
            if let Err(e) = &gw_result {
                eprintln!("[unified] gateway exited with error: {e:#}");
            }
            worker_result.and(gw_result)
        }
        Err(panic_payload) => {
            eprintln!("[unified] gateway thread panicked");
            std::panic::resume_unwind(panic_payload);
        }
    }
}

fn build_tokio_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")
}
