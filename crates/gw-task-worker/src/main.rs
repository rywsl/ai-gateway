#[tokio::main]
async fn main() -> anyhow::Result<()> {
    gw_task_worker::run().await
}
