use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use gw_egress_impersonate::{EgressBind, EgressHandle};
use http_body_util::{BodyExt, Empty};
use hyper::Request;
use hyper_util::rt::TokioIo;
use tls_parser::{
    parse_tls_extensions, parse_tls_plaintext, TlsExtension, TlsMessage, TlsMessageHandshake,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PROFILE: &str = "claude_code_chrome131";

// Chrome/BoringSSL permutes extension order. Pin the GREASE-filtered extension
// set, while preserving order for cipher suites and supported groups.
const EXPECTED_CIPHERS: &[u16] = &[
    4865, 4866, 4867, 49195, 49199, 49196, 49200, 52393, 52392, 49171, 49172, 156, 157, 47, 53,
];
const EXPECTED_EXTENSIONS: &[u16] = &[
    5, 10, 11, 13, 16, 18, 23, 27, 35, 43, 45, 51, 17513, 65037, 65281,
];
const EXPECTED_SUPPORTED_GROUPS: &[u16] = &[4588, 29, 23, 24];

#[tokio::test]
async fn loopback_emits_stable_claude_code_chrome131_client_hello() {
    let upstream = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream capture listener");
    let upstream_addr = upstream.local_addr().expect("upstream local_addr");
    let capture = tokio::spawn(capture_client_hello(upstream));

    let egress_addr = reserve_loopback_addr().await;
    let handle = EgressHandle::start(EgressBind::Tcp(egress_addr), 16);
    wait_for_egress(egress_addr).await;

    let request = Request::builder()
        .method("GET")
        .uri("/")
        .header("Host", "localhost")
        .header("X-Egress-Target", upstream_addr.to_string())
        .header("X-Egress-Profile", PROFILE)
        .body(Empty::<Bytes>::new())
        .expect("valid loopback request");
    let egress_request = tokio::spawn(send_h1(egress_addr, request));

    let raw = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .expect("timed out waiting for upstream ClientHello")
        .expect("capture task panicked")
        .expect("capture ClientHello");

    let fingerprint = parse_client_hello(&raw);
    assert_eq!(fingerprint.ciphers, EXPECTED_CIPHERS);
    assert_eq!(fingerprint.extensions, EXPECTED_EXTENSIONS);
    assert_eq!(fingerprint.supported_groups, EXPECTED_SUPPORTED_GROUPS);

    let _ = egress_request.await;
    handle.shutdown(Duration::from_secs(1)).await;
}

async fn reserve_loopback_addr() -> SocketAddr {
    let probe = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind egress probe");
    let addr = probe.local_addr().expect("egress probe local_addr");
    drop(probe);
    addr
}

async fn wait_for_egress(addr: SocketAddr) {
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("egress listener never became ready at {addr}");
}

async fn send_h1(
    addr: SocketAddr,
    request: Request<Empty<Bytes>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stream = TcpStream::connect(addr).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    match sender.send_request(request).await {
        Ok(response) => {
            let _ = response.into_body().collect().await;
        }
        Err(_e) => {}
    }
    Ok(())
}

async fn capture_client_hello(
    listener: TcpListener,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let (mut stream, _) = listener.accept().await?;
    let mut header = [0_u8; 5];
    stream.read_exact(&mut header).await?;
    let record_len = u16::from_be_bytes([header[3], header[4]]) as usize;
    let mut payload = vec![0_u8; record_len];
    stream.read_exact(&mut payload).await?;

    let mut raw = header.to_vec();
    raw.extend_from_slice(&payload);
    let _ = stream.shutdown().await;
    Ok(raw)
}

#[derive(Debug)]
struct Fingerprint {
    ciphers: Vec<u16>,
    extensions: Vec<u16>,
    supported_groups: Vec<u16>,
}

fn parse_client_hello(raw: &[u8]) -> Fingerprint {
    let (_, plaintext) = parse_tls_plaintext(raw).expect("parse TLS plaintext");
    let hello = plaintext
        .msg
        .iter()
        .find_map(|msg| match msg {
            TlsMessage::Handshake(TlsMessageHandshake::ClientHello(hello)) => Some(hello),
            _ => None,
        })
        .expect("TLS record contains ClientHello");

    let ext_raw = hello.ext.expect("ClientHello extensions present");
    let (_, extensions) = parse_tls_extensions(ext_raw).expect("parse ClientHello extensions");

    let supported_groups = extensions
        .iter()
        .find_map(|ext| match ext {
            TlsExtension::EllipticCurves(groups) => Some(
                groups
                    .iter()
                    .map(|group| group.0)
                    .filter(|v| !is_grease(*v))
                    .collect(),
            ),
            _ => None,
        })
        .expect("supported_groups extension present");

    let mut extension_ids: Vec<u16> = extension_ids(ext_raw)
        .into_iter()
        .filter(|v| !is_grease(*v))
        .collect();
    extension_ids.sort_unstable();

    Fingerprint {
        ciphers: hello
            .ciphers
            .iter()
            .map(|cipher| cipher.0)
            .filter(|v| !is_grease(*v))
            .collect(),
        extensions: extension_ids,
        supported_groups,
    }
}

fn extension_ids(mut raw: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    while raw.len() >= 4 {
        let id = u16::from_be_bytes([raw[0], raw[1]]);
        let len = u16::from_be_bytes([raw[2], raw[3]]) as usize;
        ids.push(id);
        raw = raw
            .get(4 + len..)
            .expect("extension length stays inside ClientHello extension block");
    }
    assert!(raw.is_empty(), "trailing partial extension bytes: {raw:?}");
    ids
}

fn is_grease(value: u16) -> bool {
    (value & 0x0f0f) == 0x0a0a && (value >> 8) == (value & 0x00ff)
}
