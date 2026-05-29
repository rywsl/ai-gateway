use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use gw_egress_impersonate::handle::{EgressBind, EgressHandle};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use tls_parser::{
    parse_tls_extensions, parse_tls_plaintext, TlsExtension, TlsMessage, TlsMessageHandshake,
    TlsRecordType,
};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

const GOLDEN_CIPHERS: &[u16] = &[
    4865, 4866, 4867, 49195, 49199, 49196, 49200, 52393, 52392, 49171, 49172, 156, 157, 47, 53,
];
const GOLDEN_EXTENSIONS: &[u16] = &[
    5, 10, 11, 13, 16, 18, 23, 27, 35, 43, 45, 51, 17513, 65037, 65281,
];
const GOLDEN_GROUPS: &[u16] = &[4588, 29, 23, 24];

#[tokio::test]
async fn egress_clienthello_matches_golden() {
    let recorder = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recorder");
    let recorder_addr = recorder.local_addr().expect("recorder local_addr");
    let capture = tokio::spawn(async move { record_client_hello(recorder).await });

    let egress_addr = reserve_loopback_addr().await;
    let egress = EgressHandle::start(EgressBind::Tcp(egress_addr), 16);
    wait_for_egress(egress_addr, &egress).await;

    let send = tokio::spawn(async move {
        let _ = send_egress_request(egress_addr, recorder_addr).await;
    });

    let client_hello = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .expect("timed out waiting for ClientHello")
        .expect("recorder task panicked")
        .expect("record ClientHello");
    let observed = fingerprint_tuple(&client_hello);

    let _ = send.await;
    drop(egress);

    assert_goldens(&observed);
}

async fn reserve_loopback_addr() -> SocketAddr {
    let probe = TcpListener::bind("127.0.0.1:0").await.expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

async fn wait_for_egress(addr: SocketAddr, handle: &EgressHandle) {
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!(
        "egress listener never became ready for {:?}",
        handle.bind_addr()
    );
}

async fn send_egress_request(
    egress_addr: SocketAddr,
    recorder_addr: SocketAddr,
) -> anyhow::Result<()> {
    let stream = TcpStream::connect(egress_addr).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("POST")
        .uri("/")
        .header("X-Egress-Target", recorder_addr.to_string())
        .header("X-Egress-Profile", "claude_code_chrome131")
        .body(Full::new(Bytes::new()))?;

    let resp = sender.send_request(req).await?;
    let _ = resp.into_body().collect().await?;
    Ok(())
}

async fn record_client_hello(listener: TcpListener) -> anyhow::Result<Vec<u8>> {
    let (mut stream, _) = listener.accept().await?;
    let mut buf = vec![0; 16 * 1024];
    let mut len = 0;

    loop {
        let read = stream.read(&mut buf[len..]).await?;
        if read == 0 {
            anyhow::bail!("recorder connection closed before ClientHello was complete");
        }
        len += read;

        if has_complete_tls_record(&buf[..len]) {
            buf.truncate(len);
            return Ok(buf);
        }

        if len == buf.len() {
            anyhow::bail!("ClientHello exceeded {} bytes", buf.len());
        }
    }
}

fn has_complete_tls_record(buf: &[u8]) -> bool {
    buf.len() >= 5 && {
        let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        buf.len() >= 5 + record_len
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FingerprintTuple {
    ciphers: Vec<u16>,
    extensions: Vec<u16>,
    groups: Vec<u16>,
}

fn fingerprint_tuple(bytes: &[u8]) -> FingerprintTuple {
    let (_, plaintext) = parse_tls_plaintext(bytes).expect("parse TLS plaintext");
    assert_eq!(
        plaintext.hdr.record_type,
        TlsRecordType::Handshake,
        "expected handshake record"
    );

    for msg in plaintext.msg {
        if let TlsMessage::Handshake(TlsMessageHandshake::ClientHello(hello)) = msg {
            let ciphers = hello
                .ciphers
                .iter()
                .map(|cipher| u16::from(*cipher))
                .filter(|value| !is_grease(*value))
                .collect();
            let mut extensions = hello.ext.map(extension_ids).unwrap_or_default();
            extensions.sort_unstable();
            let mut groups = Vec::new();

            if let Some(raw_exts) = hello.ext {
                let (_, exts) =
                    parse_tls_extensions(raw_exts).expect("parse ClientHello extensions");
                for ext in exts {
                    if let TlsExtension::EllipticCurves(curves) = &ext {
                        groups.extend(
                            curves
                                .iter()
                                .map(|curve| curve.0)
                                .filter(|value| !is_grease(*value)),
                        );
                    }
                }
            }

            return FingerprintTuple {
                ciphers,
                extensions,
                groups,
            };
        }
    }

    panic!("TLS record did not contain a ClientHello");
}

fn extension_ids(mut exts: &[u8]) -> Vec<u16> {
    let mut ids = Vec::new();
    while !exts.is_empty() {
        assert!(
            exts.len() >= 4,
            "truncated ClientHello extension header: {exts:?}"
        );
        let id = u16::from_be_bytes([exts[0], exts[1]]);
        let len = u16::from_be_bytes([exts[2], exts[3]]) as usize;
        assert!(
            exts.len() >= 4 + len,
            "truncated ClientHello extension {id}: {exts:?}"
        );
        if !is_grease(id) {
            ids.push(id);
        }
        exts = &exts[4 + len..];
    }
    ids
}

fn is_grease(value: u16) -> bool {
    value & 0x0f0f == 0x0a0a && (value >> 8) == (value & 0xff)
}

#[allow(
    clippy::const_is_empty,
    reason = "keeps first-run golden bootstrap loud"
)]
fn assert_goldens(observed: &FingerprintTuple) {
    if GOLDEN_CIPHERS.is_empty() || GOLDEN_EXTENSIONS.is_empty() || GOLDEN_GROUPS.is_empty() {
        panic!(
            "bootstrap self-fingerprint goldens:\nconst GOLDEN_CIPHERS: &[u16] = &{:?};\nconst GOLDEN_EXTENSIONS: &[u16] = &{:?};\nconst GOLDEN_GROUPS: &[u16] = &{:?};",
            observed.ciphers, observed.extensions, observed.groups
        );
    }

    assert_eq!(observed.ciphers, GOLDEN_CIPHERS, "cipher suite drift");
    assert_eq!(
        observed.extensions, GOLDEN_EXTENSIONS,
        "extension set drift"
    );
    assert_eq!(observed.groups, GOLDEN_GROUPS, "supported group drift");
}
