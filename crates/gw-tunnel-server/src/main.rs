use gw_tunnel_server::{spawn, EgressMode, TunnelServerConfig};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut bind: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let mut auth_token = None;
    let mut max_streams_per_conn = 256usize;
    let mut allowed_targets = Vec::new();
    let mut allow_private_targets = false;
    let mut proxy_url = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "gw-tunnel-server --bind 127.0.0.1:9001 [--auth-token TOKEN] [--max-streams N] [--allow-target host:port] [--allow-private-targets] [--proxy-url socks5[h]://[user:pass@]host:port]\nenv: PROXY_URL (fallback for --proxy-url)"
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
            "--proxy-url" => {
                proxy_url = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--proxy-url requires a url"))?,
                );
            }
            other => return Err(anyhow::anyhow!("unknown argument {other}")),
        }
    }

    let proxy_url = proxy_url.or_else(|| std::env::var("PROXY_URL").ok());
    let egress = gw_tunnel_server::parse_egress_url(proxy_url.as_deref())?;
    log_egress_mode(&egress);
    spawn_proxy_probe(&egress);

    let handle = spawn(TunnelServerConfig {
        bind,
        auth_token,
        max_streams_per_conn,
        node_id: "gw-tunnel-server".to_owned(),
        allowed_targets,
        allow_private_targets,
        egress,
    })
    .await?;
    println!("gw-tunnel-server listening on {}", handle.local_addr);
    tokio::signal::ctrl_c().await?;
    handle.shutdown().await;
    Ok(())
}

fn log_egress_mode(egress: &EgressMode) {
    match egress {
        EgressMode::Direct => tracing::info!("proxy egress: direct"),
        EgressMode::Socks5 {
            proxy_host,
            proxy_port,
            auth,
            resolve_at_proxy,
        } => {
            let scheme = if *resolve_at_proxy {
                "socks5h"
            } else {
                "socks5"
            };
            let auth_marker = if auth.is_some() { " (auth)" } else { "" };
            let dns_at = if *resolve_at_proxy { "proxy" } else { "local" };
            tracing::info!(
                "proxy egress: {scheme}://{proxy_host}:{proxy_port}{auth_marker} (DNS at {dns_at})"
            );
        }
    }
}

fn spawn_proxy_probe(egress: &EgressMode) {
    let EgressMode::Socks5 {
        proxy_host,
        proxy_port,
        ..
    } = egress
    else {
        return;
    };
    let host = proxy_host.clone();
    let port = *proxy_port;
    tokio::spawn(async move {
        use tokio::time::{timeout, Duration};

        let Some(addr) = tokio::net::lookup_host((host.as_str(), port))
            .await
            .ok()
            .and_then(|mut addrs| addrs.next())
        else {
            tracing::warn!("proxy probe: DNS resolution failed for {host}:{port}");
            return;
        };
        match timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(_)) => tracing::info!("proxy reachable: {host}:{port}"),
            Ok(Err(err)) => tracing::warn!(
                "proxy unreachable at startup: {err}; relays will fail until proxy is up"
            ),
            Err(_) => {
                tracing::warn!("proxy probe timed out (5s); relays will fail until proxy is up")
            }
        }
    });
}
