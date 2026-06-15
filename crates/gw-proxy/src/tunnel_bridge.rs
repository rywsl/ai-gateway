//! Loopback HTTP/1 bridge for owned tunnel traffic.

use bytes::{Buf, Bytes, BytesMut};
use gw_core::tunnel_protocol::TunnelProtocol;
use gw_tunnel_client::aether::http_adapter::{HttpAdapter, ResponseMetaInput};
use gw_tunnel_client::core::{StreamHandle, Transport};
use gw_tunnel_client::target::StreamTarget;
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
const BODY_CHUNK_BYTES: usize = 16 * 1024;

pub type TransportForNode = Arc<dyn Fn(&str) -> Option<Arc<dyn Transport>> + Send + Sync>;

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
    let mut initial = parsed.stripped_head;
    initial.extend_from_slice(&head[header_end..]);
    let (client_reader, mut client_writer) = tokio::io::split(client);

    match transport.tunnel_protocol() {
        TunnelProtocol::Cnexus => {
            let (host, port) = parse_target_host_port(&parsed.target)
                .ok_or_else(|| anyhow::anyhow!("invalid tunnel bridge target {}", parsed.target))?;
            let stream = match transport
                .open_stream(StreamTarget::Tcp {
                    host,
                    port,
                    tls: parsed.target_tls,
                })
                .await
            {
                Ok(stream) => stream,
                Err(err) => {
                    write_error_like(
                        &mut client_writer,
                        502,
                        &format!("tunnel open failed: {err}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            if parsed.target_tls {
                relay_tls(
                    parsed.host,
                    initial.freeze(),
                    client_reader,
                    client_writer,
                    stream,
                )
                .await?;
            } else {
                relay_plain(initial.freeze(), client_reader, client_writer, stream).await?;
            }
        }
        TunnelProtocol::Aether => {
            relay_aether_http(
                parsed.node_id,
                initial.freeze(),
                client_reader,
                client_writer,
                transport,
            )
            .await?;
        }
    }
    Ok(())
}

async fn relay_plain<R, W>(
    initial: Bytes,
    mut client_reader: R,
    mut client_writer: W,
    stream: StreamHandle,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let (mut tunnel_reader, mut tunnel_writer) = tokio::io::split(stream);
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

async fn relay_aether_http<R, W>(
    node_id: String,
    initial: Bytes,
    client_reader: R,
    mut client_writer: W,
    transport: Arc<dyn Transport>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut replay = ReplayReader::new(initial, client_reader);
    let (preamble, mut remaining) = {
        let mut adapter = HttpAdapter::new(&mut replay, &mut client_writer, node_id);
        adapter.parse_request().await?
    };

    let content_length = content_length(&preamble.headers);
    let (req_body_tx, req_body_rx) = tokio::sync::mpsc::channel(8);
    let (resp_body_tx, resp_body_rx) = tokio::sync::mpsc::channel(8);
    let (resp_meta_tx, resp_meta_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        pump_aether_request_body(&mut remaining, replay, content_length, req_body_tx).await;
    });

    let target = StreamTarget::Http {
        method: http::Method::from_bytes(preamble.method.as_bytes())?,
        url: preamble.url,
        headers: preamble.headers,
        body_rx: req_body_rx,
        body_tx_response: resp_body_tx,
        meta_tx_response: resp_meta_tx,
    };

    match transport.open_stream(target).await {
        Ok(_handle) => {}
        Err(err) => {
            write_error_like(
                &mut client_writer,
                502,
                &format!("tunnel open failed: {err}"),
            )
            .await?;
            return Ok(());
        }
    }

    let meta = resp_meta_rx.await?;
    let meta = ResponseMetaInput {
        status: meta.status,
        headers: meta.headers,
    };
    let mut empty = tokio::io::empty();
    let mut adapter = HttpAdapter::new(&mut empty, &mut client_writer, String::new());
    adapter.write_response(meta, resp_body_rx).await?;
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
    let (host, _) = parse_target_host_port_raw(target)?;
    let host = host.trim_matches(['[', ']']);
    if host.parse::<IpAddr>().is_ok() {
        return Some(host.to_owned());
    }
    Some(host.to_owned())
}

fn parse_target_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = parse_target_host_port_raw(target)?;
    Some((host.trim_matches(['[', ']']).to_owned(), port.parse().ok()?))
}

fn parse_target_host_port_raw(target: &str) -> Option<(&str, &str)> {
    let (host, port) = target.rsplit_once(':')?;
    Some((host, port))
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

async fn write_error_like<W>(writer: &mut W, status: u16, message: &str) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let reason = match status {
        400 => "Bad Request",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Internal Server Error",
    };
    let body = format!("{message}\n");
    writer
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

fn content_length(headers: &[(String, String)]) -> Option<usize> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
}

async fn pump_aether_request_body<R1, R2>(
    remaining: &mut R1,
    mut reader: R2,
    content_length: Option<usize>,
    tx: tokio::sync::mpsc::Sender<Bytes>,
) where
    R1: AsyncRead + Unpin,
    R2: AsyncRead + Unpin,
{
    let Some(mut bytes_left) = content_length else {
        return;
    };
    let mut buf = vec![0u8; BODY_CHUNK_BYTES];
    loop {
        if bytes_left == 0 {
            return;
        }
        let before = bytes_left;
        bytes_left = pump_aether_body_reader(remaining, bytes_left, &mut buf, &tx).await;
        if bytes_left == 0 || bytes_left == before {
            break;
        }
    }
    while bytes_left > 0 {
        let before = bytes_left;
        bytes_left = pump_aether_body_reader(&mut reader, bytes_left, &mut buf, &tx).await;
        if bytes_left == 0 || bytes_left == before {
            break;
        }
    }
}

async fn pump_aether_body_reader<R>(
    reader: &mut R,
    bytes_left: usize,
    buf: &mut [u8],
    tx: &tokio::sync::mpsc::Sender<Bytes>,
) -> usize
where
    R: AsyncRead + Unpin,
{
    let limit = bytes_left.min(buf.len());
    let n = match reader.read(&mut buf[..limit]).await {
        Ok(0) | Err(_) => return bytes_left,
        Ok(n) => n,
    };
    if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
        return 0;
    }
    bytes_left - n
}

struct ReplayReader<R> {
    remaining: Bytes,
    reader: R,
}

impl<R> ReplayReader<R> {
    fn new(remaining: Bytes, reader: R) -> Self {
        Self { remaining, reader }
    }
}

impl<R> AsyncRead for ReplayReader<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.remaining.is_empty() {
            let n = self.remaining.len().min(buf.remaining());
            buf.put_slice(&self.remaining[..n]);
            self.remaining.advance(n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::tunnel_protocol::TunnelProtocol;
    use gw_tunnel_client::cnexus::transport::{TransportConfig, WsTransport};
    use gw_tunnel_client::core::{StreamHandle, Transport, TransportError, TransportFuture};
    use gw_tunnel_client::target::StreamTarget;
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

    #[tokio::test]
    async fn bridge_routes_aether_as_http_target() {
        struct FakeAetherTransport {
            seen: Arc<StdMutex<Option<String>>>,
        }

        impl Transport for FakeAetherTransport {
            fn open_stream<'a>(&'a self, target: StreamTarget) -> TransportFuture<'a> {
                Box::pin(async move {
                    let StreamTarget::Http {
                        method,
                        url,
                        headers,
                        mut body_rx,
                        body_tx_response,
                        meta_tx_response,
                    } = target
                    else {
                        return Err(TransportError::UnsupportedTarget);
                    };
                    let host = headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default();
                    let mut body = Vec::new();
                    while let Some(chunk) = body_rx.recv().await {
                        body.extend_from_slice(&chunk);
                    }
                    *self.seen.lock().unwrap() = Some(format!(
                        "{method} {url} host={host} body={}",
                        String::from_utf8_lossy(&body)
                    ));
                    meta_tx_response
                        .send(gw_tunnel_client::aether::meta::ResponseMeta {
                            status: 201,
                            headers: vec![("content-type".into(), "text/plain".into())],
                        })
                        .map_err(|_| TransportError::Closed)?;
                    body_tx_response
                        .send(Bytes::from_static(b"aether-ok"))
                        .await
                        .map_err(|_| TransportError::Closed)?;
                    let (stream, _peer) = tokio::io::duplex(1);
                    Ok(Box::new(stream) as StreamHandle)
                })
            }

            fn alive(&self) -> bool {
                true
            }

            fn last_heartbeat_age(&self) -> Duration {
                Duration::from_millis(0)
            }

            fn tunnel_protocol(&self) -> TunnelProtocol {
                TunnelProtocol::Aether
            }

            fn close(&self) {}
        }

        let seen = Arc::new(StdMutex::new(None));
        let transport: Arc<dyn Transport> = Arc::new(FakeAetherTransport { seen: seen.clone() });
        let bridge = spawn(TunnelBridgeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            transport_for_node: Arc::new(move |node_id| {
                (node_id == "node-a").then_some(transport.clone())
            }),
        })
        .await
        .unwrap();

        let mut client = TcpStream::connect(bridge.local_addr).await.unwrap();
        let request = format!(
            "POST /v1/chat HTTP/1.1\r\nHost: aether-upstream.internal\r\n{HDR_NODE_ID}: node-a\r\n{HDR_TARGET}: aether-upstream.internal:80\r\n{HDR_TARGET_TLS}: false\r\nContent-Length: 4\r\nConnection: close\r\n\r\nping",
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.contains("201 Created"),
            "unexpected bridge response: {response_text}"
        );
        assert!(response_text.ends_with("9\r\naether-ok\r\n0\r\n\r\n"));
        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("POST http://aether-upstream.internal/v1/chat host=aether-upstream.internal body=ping")
        );
        bridge.shutdown().await;
    }
}
