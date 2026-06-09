use gw_tunnel_server::{spawn, TunnelServerConfig};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut bind: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let mut auth_token = None;
    let mut max_streams_per_conn = 256usize;
    let mut allowed_targets = Vec::new();
    let mut allow_private_targets = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "gw-tunnel-server --bind 127.0.0.1:9001 [--auth-token TOKEN] [--max-streams N] [--allow-target host:port] [--allow-private-targets]"
                );
                return Ok(());
            }
            "--bind" => {
                bind = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bind requires an address"))?
                    .parse()?;
            }
            "--auth-token" => {
                auth_token = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--auth-token requires a value"))?,
                );
            }
            "--max-streams" => {
                max_streams_per_conn = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-streams requires a value"))?
                    .parse()?;
            }
            "--allow-target" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--allow-target requires host:port"))?;
                let (host, port) = value
                    .rsplit_once(':')
                    .ok_or_else(|| anyhow::anyhow!("--allow-target requires host:port"))?;
                allowed_targets.push((host.trim_matches(['[', ']']).to_owned(), port.parse()?));
            }
            "--allow-private-targets" => {
                allow_private_targets = true;
            }
            other => return Err(anyhow::anyhow!("unknown argument {other}")),
        }
    }

    let handle = spawn(TunnelServerConfig {
        bind,
        auth_token,
        max_streams_per_conn,
        node_id: "gw-tunnel-server".to_owned(),
        allowed_targets,
        allow_private_targets,
    })
    .await?;
    println!("gw-tunnel-server listening on {}", handle.local_addr);
    tokio::signal::ctrl_c().await?;
    handle.shutdown().await;
    Ok(())
}
