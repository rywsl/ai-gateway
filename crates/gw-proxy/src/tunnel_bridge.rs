//! Loopback HTTP/1 bridge for owned tunnel traffic.

use bytes::{Bytes, BytesMut};
use gw_tunnel_client::cnexus::transport::{TunnelStreamReader, TunnelStreamWriter, WsTransport};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

pub const HDR_NODE_ID: &str = "X-Gateway-Tunnel-Node-Id";
pub const HDR_TARGET: &str = "X-Gateway-Tunnel-Target";
pub const HDR_TARGET_TLS: &str = "X-Gateway-Tunnel-Target-Tls";

const MAX_HEADER_BYTES: usize = 64 * 1024;

pub type TransportForNode = Arc<dyn Fn(&str) -> Option<Arc<WsTransport>> + Send + Sync>;

pub struct TunnelBridgeHandle {
    pub local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TunnelBridgeHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for TunnelBridgeHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub struct TunnelBridgeConfig {
    pub bind: SocketAddr,
    pub transport_for_node: TransportForNode,
}

pub async fn spawn(config: TunnelBridgeConfig) -> anyhow::Result<TunnelBridgeHandle> {
    let listener = TcpListener::bind(config.bind).await?;
    let local_addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let transport_for_node = config.transport_for_node;
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else {
                        continue;
                    };
                    let transport_for_node = transport_for_node.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_conn(stream, transport_for_node).await {
                            tracing::debug!(error = ?err, "tunnel bridge connection ended");
                        }
                    });
                }
            }
        }
    });
    Ok(TunnelBridgeHandle {
        local_addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    })
}

async fn handle_conn(
    mut client: TcpStream,
    transport_for_node: TransportForNode,
) -> anyhow::Result<()> {
    let mut head = BytesMut::new();
    let header_end = loop {
        if let Some(pos) = find_header_end(&head) {
            break pos;
        }
        if head.len() >= MAX_HEADER_BYTES {
            write_error(&mut client, 431, "tunnel bridge header too large").await?;
            return Ok(());
        }
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    };

    let parsed = match parse_and_strip(&head[..header_end]) {
        Ok(parsed) => parsed,
        Err(status) => {
            write_error(&mut client, status, "invalid tunnel bridge request").await?;
            return Ok(());
        }
    };
    let Some(transport) = (transport_for_node)(&parsed.node_id) else {
        write_error(&mut client, 502, "tunnel node not live").await?;
        return Ok(());
    };
    let stream = match transport.open_stream(&parsed.target).await {
        Ok(stream) => stream,
        Err(err) => {
            write_error(&mut client, 502, &format!("tunnel open failed: {err}")).await?;
            return Ok(());
        }
    };
    let mut initial = parsed.stripped_head;
    initial.extend_from_slice(&head[header_end..]);
    let (tunnel_reader, tunnel_writer) = stream.split();
    let (client_reader, client_writer) = tokio::io::split(client);

    if parsed.target_tls {
        relay_tls(
            parsed.host,
            initial.freeze(),
            client_reader,
            client_writer,
            TunnelIo {
                reader: tunnel_reader,
                writer: tunnel_writer,
            },
        )
        .await?;
    } else {
        relay_plain(
            initial.freeze(),
            client_reader,
            client_writer,
            tunnel_reader,
            tunnel_writer,
        )
        .await?;
    }
    Ok(())
}

async fn relay_plain<R, W>(
    initial: Bytes,
    mut client_reader: R,
    mut client_writer: W,
    mut tunnel_reader: TunnelStreamReader,
    mut tunnel_writer: TunnelStreamWriter,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    if !initial.is_empty() {
        tunnel_writer.write_all(&initial).await?;
    }
    let request_task = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_reader, &mut tunnel_writer).await;
        let _ = tunnel_writer.shutdown().await;
    });
    let response = tokio::io::copy(&mut tunnel_reader, &mut client_writer).await;
    request_task.abort();
    response?;
    Ok(())
}

async fn relay_tls<R, W, S>(
    host: String,
    initial: Bytes,
    mut client_reader: R,
    mut client_writer: W,
    stream: S,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.clone())?;
    let tls = connector.connect(server_name, stream).await?;
    let (mut tls_reader, mut tls_writer) = tokio::io::split(tls);
    if !initial.is_empty() {
        tls_writer.write_all(&initial).await?;
    }
    let request_task = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_reader, &mut tls_writer).await;
        let _ = tls_writer.shutdown().await;
    });
    let response = tokio::io::copy(&mut tls_reader, &mut client_writer).await;
    request_task.abort();
    response?;
    Ok(())
}

struct TunnelIo {
    reader: TunnelStreamReader,
    writer: TunnelStreamWriter,
}

impl AsyncRead for TunnelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl AsyncWrite for TunnelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

struct ParsedRequest {
    node_id: String,
    target: String,
    host: String,
    target_tls: bool,
    stripped_head: BytesMut,
}

fn parse_and_strip(head: &[u8]) -> Result<ParsedRequest, u16> {
    let text = std::str::from_utf8(head).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(400u16)?;
    let mut out = BytesMut::new();
    out.extend_from_slice(first.as_bytes());
    out.extend_from_slice(b"\r\n");

    let mut node_id = None;
    let mut target = None;
    let mut target_tls = false;
    for line in lines {
        if line.is_empty() {
            out.extend_from_slice(b"\r\n");
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(400u16);
        };
        if name.eq_ignore_ascii_case(HDR_NODE_ID) {
            node_id = Some(value.trim().to_owned());
            continue;
        }
        if name.eq_ignore_ascii_case(HDR_TARGET) {
            target = Some(value.trim().to_owned());
            continue;
        }
        if name.eq_ignore_ascii_case(HDR_TARGET_TLS) {
            target_tls = value.trim().eq_ignore_ascii_case("true");
            continue;
        }
        if name.to_ascii_lowercase().starts_with("x-gateway-tunnel-") {
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }

    let node_id = node_id.filter(|s| !s.is_empty()).ok_or(400u16)?;
    let target = target.filter(|s| !s.is_empty()).ok_or(400u16)?;
    let host = parse_target_host(&target).ok_or(400u16)?;
    Ok(ParsedRequest {
        node_id,
        target,
        host,
        target_tls,
        stripped_head: out,
    })
}

fn parse_target_host(target: &str) -> Option<String> {
    let (host, _) = target.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    if host.parse::<IpAddr>().is_ok() {
        return Some(host.to_owned());
    }
    Some(host.to_owned())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

async fn write_error(client: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Internal Server Error",
    };
    let body = format!("{message}\n");
    client
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_tunnel_client::cnexus::transport::{TransportConfig, WsTransport};
    use gw_tunnel_client::target_filter::{set_whitelist, ResetGuard};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    #[test]
    fn parse_and_strip_removes_internal_headers() {
        let parsed = parse_and_strip(
            b"POST /v1 HTTP/1.1\r\nHost: bridge\r\nX-Gateway-Tunnel-Node-Id: n1\r\nX-Gateway-Tunnel-Target: api.example.com:443\r\nX-Gateway-Tunnel-Target-Tls: true\r\nX-Gateway-Tunnel-Other: nope\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let out = String::from_utf8(parsed.stripped_head.to_vec()).unwrap();
        assert!(out.contains("Host: bridge\r\n"));
        assert!(out.contains("Content-Length: 0\r\n"));
        assert!(!out.to_ascii_lowercase().contains("x-gateway-tunnel-"));
        assert_eq!(parsed.node_id, "n1");
        assert_eq!(parsed.target, "api.example.com:443");
        assert!(parsed.target_tls);
    }

    #[tokio::test]
    async fn bridge_strips_internal_headers_and_relays_plain_http() {
        let _guard = ResetGuard::new();
        let upstream_seen = Arc::new(StdMutex::new(String::new()));
        let upstream_seen_task = upstream_seen.clone();
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            *upstream_seen_task.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        set_whitelist(vec![("127.0.0.1".to_owned(), upstream_port)]);
        let server = gw_tunnel_server::spawn(gw_tunnel_server::TunnelServerConfig {
            allowed_targets: vec![("127.0.0.1".to_owned(), upstream_port)],
            allow_private_targets: true,
            ..gw_tunnel_server::TunnelServerConfig::default()
        })
        .await
        .unwrap();
        let transport = Arc::new(
            WsTransport::connect(TransportConfig {
                url: format!("ws://{}/", server.local_addr).parse().unwrap(),
                heartbeat_interval: Duration::from_secs(60),
                missed_pong_threshold: 5,
                ..TransportConfig::default()
            })
            .await
            .unwrap(),
        );
        let transport_for_node = transport.clone();
        let bridge = spawn(TunnelBridgeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            transport_for_node: Arc::new(move |node_id| {
                (node_id == "node-a").then_some(transport_for_node.clone())
            }),
        })
        .await
        .unwrap();
        let mut client = TcpStream::connect(bridge.local_addr).await.unwrap();
        let request = format!(
            "GET /plain HTTP/1.1\r\nHost: bridge\r\n{HDR_NODE_ID}: node-a\r\n{HDR_TARGET}: 127.0.0.1:{upstream_port}\r\n{HDR_TARGET_TLS}: false\r\nX-Gateway-Tunnel-Other: secret\r\nConnection: close\r\n\r\n",
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("204 No Content"),
            "unexpected bridge response: {response_text}"
        );
        upstream_task.await.unwrap();
        let seen = upstream_seen.lock().unwrap().clone();
        assert!(seen.starts_with("GET /plain HTTP/1.1"));
        assert!(!seen.to_ascii_lowercase().contains("x-gateway-tunnel-"));
        bridge.shutdown().await;
        server.shutdown().await;
    }
}
