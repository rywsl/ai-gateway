use bytes::Bytes;
use gw_tunnel_client::aether::http_adapter::{HttpAdapter, ResponseMetaInput};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

#[tokio::test]
async fn parses_simple_request_with_headers_and_partial_body() {
    let req = "POST /v1/echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nX-Y: 1\r\n\r\nHELLO";
    let mut reader = tokio_test::io::Builder::new().read(req.as_bytes()).build();
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "node-1".into());

    let (pre, mut remaining) = adapter.parse_request().await.unwrap();

    assert_eq!(pre.method, "POST");
    assert_eq!(pre.url.as_str(), "http://x/v1/echo");
    assert_eq!(
        pre.headers,
        vec![
            ("Host".to_string(), "x".to_string()),
            ("Content-Length".to_string(), "5".to_string()),
            ("X-Y".to_string(), "1".to_string())
        ]
    );
    let mut body = Vec::new();
    remaining.read_to_end(&mut body).await.unwrap();
    assert_eq!(&body, b"HELLO");
}

#[tokio::test]
async fn parses_request_arriving_in_two_chunks() {
    let mut reader = TwoChunk::new(b"GET / HTT".to_vec(), b"P/1.1\r\nHost: x\r\n\r\n".to_vec());
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "node".into());

    let (pre, _) = adapter.parse_request().await.unwrap();

    assert_eq!(pre.method, "GET");
    assert_eq!(pre.url.as_str(), "http://x/");
}

#[tokio::test]
async fn writes_chunked_response_when_no_content_length() {
    let mut reader = tokio_test::io::Builder::new().build();
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "n".into());
    let (body_tx, body_rx) = tokio::sync::mpsc::channel(4);
    body_tx.send(Bytes::from_static(b"hello")).await.unwrap();
    body_tx.send(Bytes::from_static(b" world")).await.unwrap();
    drop(body_tx);
    let meta = ResponseMetaInput {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain".into())],
    };

    adapter.write_response(meta, body_rx).await.unwrap();

    let out = String::from_utf8(writer).unwrap();
    assert!(out.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(out.contains("Content-Type: text/plain\r\n"));
    assert!(out.contains("Transfer-Encoding: chunked\r\n"));
    assert!(out.contains("\r\n5\r\nhello\r\n"));
    assert!(out.contains("\r\n6\r\n world\r\n"));
    assert!(out.ends_with("\r\n0\r\n\r\n"));
}

#[tokio::test]
async fn writes_plain_response_when_content_length_present() {
    let mut reader = tokio_test::io::Builder::new().build();
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "n".into());
    let (body_tx, body_rx) = tokio::sync::mpsc::channel(4);
    body_tx.send(Bytes::from_static(b"hi")).await.unwrap();
    drop(body_tx);
    let meta = ResponseMetaInput {
        status: 200,
        headers: vec![("Content-Length".into(), "2".into())],
    };

    adapter.write_response(meta, body_rx).await.unwrap();

    let out = String::from_utf8(writer).unwrap();
    assert!(!out.contains("Transfer-Encoding: chunked"));
    assert!(out.ends_with("hi"));
}

struct TwoChunk {
    first: Vec<u8>,
    second: Vec<u8>,
    index: u8,
}

impl TwoChunk {
    fn new(first: Vec<u8>, second: Vec<u8>) -> Self {
        Self {
            first,
            second,
            index: 0,
        }
    }
}

impl AsyncRead for TwoChunk {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let chunk = match self.index {
            0 => {
                self.index = 1;
                std::mem::take(&mut self.first)
            }
            1 => {
                self.index = 2;
                std::mem::take(&mut self.second)
            }
            _ => Vec::new(),
        };
        buf.put_slice(&chunk);
        std::task::Poll::Ready(Ok(()))
    }
}
