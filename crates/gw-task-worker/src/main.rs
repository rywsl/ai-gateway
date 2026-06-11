#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let shutdown = gw_runtime::ShutdownToken::new();
    gw_task_worker::run(shutdown).await
}
