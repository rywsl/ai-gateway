//! `RequestBodyView` — explicit raw/decoded/outbound state.
//!
//! See `docs/architecture.md`.
//!
//! Note on `max_bytes`: the cap is enforced **inside** `gw_codec::compression::decode`
//! via `Read::take`, so peak allocation is bounded even on a compression bomb.
//! `decoded()` therefore propagates the codec-layer `TooLarge` directly; the
//! identity path keeps its own size guard since no decoder runs.

use bytes::Bytes;
use gw_codec::compression::Encoding;
use once_cell::sync::OnceCell;

/// Errors that can occur when decompressing a request body.
#[derive(Debug)]
pub enum DecodeErr {
    TooLarge { limit: usize, actual: usize },
    FormatError(String),
}

/// Three-state body lifecycle: raw wire bytes → decoded plaintext (lazy,
/// OnceCell) → outbound bytes after codec rewrite.
pub struct RequestBodyView {
    raw: Bytes,
    req_encoding: Encoding,
    decoded: OnceCell<Result<Bytes, DecodeErr>>,
    outbound: Option<Bytes>,
}

impl RequestBodyView {
    /// Construct from the raw (possibly compressed) downstream bytes.
    pub fn from_raw(raw: Bytes, req_encoding: Encoding) -> Self {
        Self {
            raw,
            req_encoding,
            decoded: OnceCell::new(),
            outbound: None,
        }
    }

    /// The raw wire bytes as received from the client.
    pub fn raw(&self) -> &Bytes {
        &self.raw
    }

    /// The content-encoding the client declared.
    pub fn req_encoding(&self) -> Encoding {
        self.req_encoding
    }

    /// Decoded (decompressed) plaintext bytes. Decompression runs at most
    /// once; subsequent calls return the cached result. `max_bytes` is enforced
    /// inside the codec layer via `Read::take`, so a compression bomb is
    /// rejected before any oversized allocation occurs. `DecodeErr::TooLarge`
    /// is returned on the *first* call and cached.
    pub fn decoded(&self, max_bytes: usize) -> Result<&Bytes, &DecodeErr> {
        self.decoded
            .get_or_init(|| decompress(&self.raw, self.req_encoding, max_bytes))
            .as_ref()
    }

    /// Replace the outbound bytes with the codec-rewritten payload.
    pub fn set_outbound(&mut self, body: Bytes) {
        self.outbound = Some(body);
    }

    /// The bytes to forward upstream: the codec-rewritten payload if set,
    /// otherwise the original raw bytes.
    pub fn outbound_or_raw(&self) -> &Bytes {
        self.outbound.as_ref().unwrap_or(&self.raw)
    }

    /// Pre-populate the decoded OnceCell with bytes already decoded by another
    /// path (e.g. HMAC verify which decodes independently). Must be called before
    /// any other consumer reads `decoded()`; later consumers will see this value.
    /// A no-op if the cell was already initialised.
    pub fn prefill_decoded(&self, decoded: Bytes) {
        // `set` returns Err if already set; ignore.
        let _ = self.decoded.set(Ok(decoded));
    }
}

fn decompress(raw: &Bytes, enc: Encoding, max_bytes: usize) -> Result<Bytes, DecodeErr> {
    // Codec-layer cap enforces max_bytes in-stream via Read::take, so a
    // brotli/gzip bomb cannot inflate past the cap. We map its `TooLarge`
    // to our caller-shaped `DecodeErr::TooLarge`.
    match enc {
        Encoding::Identity => {
            if raw.len() > max_bytes {
                return Err(DecodeErr::TooLarge {
                    limit: max_bytes,
                    actual: raw.len(),
                });
            }
            Ok(raw.clone())
        }
        other => gw_codec::compression::decode(other, raw, max_bytes).map_err(|e| match e {
            gw_codec::compression::CompressionError::TooLarge { limit } => DecodeErr::TooLarge {
                limit,
                // Exact decoded size is unknown — the decoder is cut off at
                // limit + 1 — so report a sentinel `limit + 1` to communicate
                // "at least one byte past the limit".
                actual: limit.saturating_add(1),
            },
            other => DecodeErr::FormatError(other.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip_encode(payload: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn decoded_returns_plaintext_for_gzip_body() {
        let plain = b"{\"hello\":\"world\"}";
        let body = Bytes::from(gzip_encode(plain));
        let v = RequestBodyView::from_raw(body, Encoding::Gzip);
        let d1 = v.decoded(64 * 1024).expect("decompresses");
        assert_eq!(d1.as_ref(), &plain[..]);
    }

    #[test]
    fn decoded_only_decompresses_once() {
        let plain = b"hello once";
        let body = Bytes::from(gzip_encode(plain));
        let v = RequestBodyView::from_raw(body, Encoding::Gzip);
        let p1: *const Bytes = v.decoded(64 * 1024).expect("ok 1");
        let p2: *const Bytes = v.decoded(64 * 1024).expect("ok 2");
        let p3: *const Bytes = v.decoded(64 * 1024).expect("ok 3");
        assert_eq!(p1, p2);
        assert_eq!(p2, p3);
    }

    #[test]
    fn identity_encoding_returns_raw_clone() {
        let plain = b"plain payload";
        let v = RequestBodyView::from_raw(Bytes::from(&plain[..]), Encoding::Identity);
        assert_eq!(v.decoded(64 * 1024).unwrap().as_ref(), &plain[..]);
    }

    #[test]
    fn outbound_overrides_raw() {
        let mut v = RequestBodyView::from_raw(Bytes::from_static(b"raw"), Encoding::Identity);
        assert_eq!(v.outbound_or_raw().as_ref(), b"raw");
        v.set_outbound(Bytes::from_static(b"rewritten"));
        assert_eq!(v.outbound_or_raw().as_ref(), b"rewritten");
    }

    #[test]
    fn too_large_returns_error() {
        let plain = b"hello world this is a long payload";
        let v = RequestBodyView::from_raw(Bytes::from(&plain[..]), Encoding::Identity);
        let result = v.decoded(5);
        assert!(matches!(result, Err(DecodeErr::TooLarge { limit: 5, .. })));
    }
}
