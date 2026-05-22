//! ADR-S5: TLS variant of the fake-upstream business listener (port 9443).
//!
//! At startup we generate a self-signed cert with `rcgen` and stand up a
//! `tokio_rustls::TlsAcceptor` on `0.0.0.0:9443`. The gateway's truststore
//! intentionally does NOT trust this cert — that is the whole point. When
//! the e2e test points a vendor key at `https://fake-upstream:9443`, pingora
//! dials port 9443, attempts a TLS handshake against our untrusted cert,
//! the handshake fails inside `connect_to_upstream`, pingora's
//! `fail_to_connect` fires with `ConnectErrorKind::Tls`, and the gateway
//! ticks `gw_retry_attempt_total{outcome="tls_blocked"}`.
//!
//! On the off chance a client DOES trust the cert (no e2e flow does), the
//! listener serves a trivial `HTTP/1.1 200 OK` with `{"ok":true}` directly
//! over the TLS stream. The plain-listener's full scenario engine is NOT
//! wired through here — the inject primitives in `crate::inject` operate
//! on a raw `TcpStream` (RST via `SO_LINGER`, etc.) and don't generalize
//! to a `TlsStream<TcpStream>`. Since the test path is handshake-failure,
//! a no-op served HTTP body is sufficient; the metric is the observable.

use std::sync::Arc;

use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Bind `0.0.0.0:<port>` (typically 9443) and serve a self-signed TLS
/// listener. Runs forever; the caller spawns this on the tokio runtime.
pub async fn serve_tls(addr: &str) -> std::io::Result<()> {
    // Install the ring crypto provider exactly once for this process. The
    // builder below would also pick it up via `builder_with_provider`, but
    // installing the default makes other rustls API calls (e.g. anything
    // that internally consults `CryptoProvider::get_default()`) safe.
    let _ = ring::default_provider().install_default();

    let cert = match rcgen::generate_simple_self_signed(vec!["fake-upstream".to_owned()]) {
        Ok(c) => c,
        Err(e) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("[fake-upstream] rcgen self-signed cert failed: {e}"),
            ));
        }
    };
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_pkcs8 = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let key_der = PrivateKeyDer::from(key_pkcs8);

    let server_cfg = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("[fake-upstream] rustls protocol versions: {e}"),
            )
        })?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("[fake-upstream] rustls with_single_cert: {e}"),
            )
        })?;

    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
    let listener = TcpListener::bind(addr).await?;
    eprintln!("[fake-upstream] tls business listener bound on {addr}");

    loop {
        let (sock, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[fake-upstream] tls accept error: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            match acceptor.accept(sock).await {
                // ── Handshake failed: expected for the e2e TLS-error test.
                //    pingora doesn't trust our self-signed cert, the
                //    handshake aborts here, fail_to_connect fires
                //    on the gateway side. Nothing to do server-side.
                Err(_e) => {
                    // Don't even log at info — handshake failures are the
                    // happy path for this listener.
                }
                // ── Handshake succeeded (unused by any current e2e path but
                //    keeps the listener honest if someone points a cert-
                //    trusting client at it). Write a tiny static 200.
                Ok(mut tls_stream) => {
                    let body = b"{\"ok\":true,\"note\":\"tls listener static 200\"}";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\n\
                         content-type: application/json\r\n\
                         content-length: {}\r\n\
                         connection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = tls_stream.write_all(head.as_bytes()).await;
                    let _ = tls_stream.write_all(body).await;
                    let _ = tls_stream.shutdown().await;
                }
            }
        });
    }
}
