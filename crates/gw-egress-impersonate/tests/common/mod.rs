//! Shared test infrastructure for gw-egress-impersonate integration tests.
//!
//! `mock_upstream` starts a plain HTTP/1.1 server on an OS-assigned port that
//! echoes back the request headers it received as a JSON object. Tests for
//! Task 3.5 (request handler), 3.6 (listener), and 3.7 (supervisor) all use
//! this shared fixture.

pub mod mock_upstream {
    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Full};
    use hyper::body::Incoming;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Start a mock HTTP/1.1 upstream on 127.0.0.1 with an OS-assigned port.
    ///
    /// Returns the bound address. The server runs as a background task for the
    /// lifetime of the test runtime. Each request is handled by echoing the
    /// received headers back as a JSON map in the response body.
    ///
    /// The response body has the shape:
    /// ```json
    /// { "Header-Name": "header-value", ... }
    /// ```
    /// Header names are returned in their original capitalisation as sent over
    /// the wire (as parsed by hyper).
    pub async fn start() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[mock_upstream] accept error: {e}");
                        continue;
                    }
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    if let Err(e) = http1::Builder::new()
                        .serve_connection(io, service_fn(echo_headers))
                        .await
                    {
                        // Connection errors are expected in tests (client drops).
                        let _ = e;
                    }
                });
            }
        });

        addr
    }

    /// Echo back the request's headers as a JSON object.
    ///
    /// The returned `HeaderMap` is the *received* headers — internal egress
    /// headers stripped by the Handler must not appear here.
    async fn echo_headers(
        req: Request<Incoming>,
    ) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
        let mut map: HashMap<String, String> = HashMap::new();
        for (name, value) in req.headers() {
            if let Ok(v) = value.to_str() {
                // Use the canonical header name string (lowercase, per HTTP/1.1
                // normalisation that hyper applies on parsing).
                map.insert(name.to_string(), v.to_owned());
            }
        }
        let body_json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_owned());
        let response = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body_json)).boxed())
            .expect("valid echo response");
        Ok(response)
    }
}
