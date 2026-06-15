use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;
use url::Url;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const READ_CHUNK_SIZE: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPreamble {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseMetaInput {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

pub struct HttpAdapter<'a, R, W> {
    reader: &'a mut R,
    writer: &'a mut W,
    node_id: String,
}

impl<'a, R, W> HttpAdapter<'a, R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: &'a mut R, writer: &'a mut W, node_id: String) -> Self {
        Self {
            reader,
            writer,
            node_id,
        }
    }

    pub async fn parse_request(
        &mut self,
    ) -> Result<(RequestPreamble, RemainingBody), AdapterError> {
        let mut buf = Vec::with_capacity(READ_CHUNK_SIZE);
        let header_len = loop {
            if let Some(pos) = find_header_end(&buf) {
                break pos;
            }
            if buf.len() >= MAX_HEADER_BYTES {
                return Err(AdapterError::HeadersTooLarge {
                    node_id: self.node_id.clone(),
                    len: buf.len(),
                });
            }
            let mut chunk = [0u8; READ_CHUNK_SIZE];
            let n = self.reader.read(&mut chunk).await?;
            if n == 0 {
                return Err(AdapterError::UnexpectedEof);
            }
            buf.extend_from_slice(&chunk[..n]);
        };

        let preamble = parse_preamble(&buf[..header_len])?;
        let body = Bytes::copy_from_slice(&buf[header_len..]);
        Ok((preamble, RemainingBody::new(body)))
    }

    pub async fn write_response(
        &mut self,
        meta: ResponseMetaInput,
        mut body_rx: mpsc::Receiver<Bytes>,
    ) -> Result<(), AdapterError> {
        let chunked = !has_header(&meta.headers, "content-length")
            && !has_header(&meta.headers, "transfer-encoding");

        self.writer
            .write_all(
                format!(
                    "HTTP/1.1 {} {}\r\n",
                    meta.status,
                    reason_phrase(meta.status)
                )
                .as_bytes(),
            )
            .await?;
        for (name, value) in &meta.headers {
            self.writer
                .write_all(format!("{name}: {value}\r\n").as_bytes())
                .await?;
        }
        if chunked {
            self.writer
                .write_all(b"Transfer-Encoding: chunked\r\n")
                .await?;
        }
        self.writer.write_all(b"\r\n").await?;

        while let Some(chunk) = body_rx.recv().await {
            if chunked {
                self.writer
                    .write_all(format!("{:X}\r\n", chunk.len()).as_bytes())
                    .await?;
                self.writer.write_all(&chunk).await?;
                self.writer.write_all(b"\r\n").await?;
            } else {
                self.writer.write_all(&chunk).await?;
            }
        }
        if chunked {
            self.writer.write_all(b"0\r\n\r\n").await?;
        }
        self.writer.flush().await?;
        Ok(())
    }
}

pub struct RemainingBody {
    buf: Bytes,
}

impl RemainingBody {
    fn new(buf: Bytes) -> Self {
        Self { buf }
    }
}

impl AsyncRead for RemainingBody {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        dst: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let n = self.buf.len().min(dst.remaining());
        if n > 0 {
            dst.put_slice(&self.buf[..n]);
            self.buf.advance(n);
        }
        std::task::Poll::Ready(Ok(()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP parse error: {0}")]
    Parse(#[from] httparse::Error),
    #[error("incomplete HTTP request")]
    Incomplete,
    #[error("unexpected EOF before HTTP request headers completed")]
    UnexpectedEof,
    #[error("HTTP request headers too large for node {node_id}: {len} bytes")]
    HeadersTooLarge { node_id: String, len: usize },
    #[error("request missing Host header")]
    MissingHost,
    #[error("invalid request target URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn parse_preamble(buf: &[u8]) -> Result<RequestPreamble, AdapterError> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    let status = req.parse(buf)?;
    if status.is_partial() {
        return Err(AdapterError::Incomplete);
    }

    let method = req.method.ok_or(AdapterError::Incomplete)?.to_string();
    let path = req.path.ok_or(AdapterError::Incomplete)?;
    let headers = req
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).to_string(),
            )
        })
        .collect::<Vec<_>>();

    let url = if path.starts_with("http://") || path.starts_with("https://") {
        Url::parse(path)?
    } else {
        let host = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.as_str())
            .ok_or(AdapterError::MissingHost)?;
        Url::parse(&format!("http://{host}{path}"))?
    };

    Ok(RequestPreamble {
        method,
        url,
        headers,
    })
}

fn has_header(headers: &[(String, String)], needle: &str) -> bool {
    headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(needle))
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}
