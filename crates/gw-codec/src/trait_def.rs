//! `ProtocolCodec` — the trait every wire-format codec implements.
//!
//! See `lib.rs` for the choice of `Headers = &[(String, String)]` over the
//! plan's original `pingora::http::HeaderMap`.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{RequestIR, ResponseIR, StreamEventIR};

/// Lightweight, runtime-agnostic header view used by `detect_request`.
///
/// Callers convert their concrete header type (e.g. `pingora::http::HeaderMap`,
/// `http::HeaderMap`, or a raw `Vec<(String, String)>`) into this borrow.
pub type Headers = [(String, String)];

pub trait ProtocolCodec: Send + Sync + 'static {
    /// Return `Some(Self)` if `(path, headers, body_head)` looks like a request
    /// in this protocol. Implementations should be branch-free and never
    /// allocate.
    fn detect_request(path: &str, headers: &Headers, body_head: &[u8]) -> Option<Self>
    where
        Self: Sized;

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>>;
    fn render_request(&self, ir: &RequestIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;

    /// Parse one already-framed SSE event chunk.
    ///
    /// CONTRACT: The caller pre-frames the upstream byte stream on `\n\n`
    /// boundaries and passes exactly one complete event to this method. The
    /// codec implementation does NOT split or accumulate across calls; it
    /// interprets exactly one event and returns an empty `SmallVec` if the
    /// chunk contains no usable IR payload (e.g. an SSE comment line,
    /// keep-alive, or unknown event).
    ///
    /// A single framed event may decompose into more than one IR event —
    /// notably Gemini's combined final chunk carrying both `text` and
    /// `finishReason` decomposes into `[Delta, Finish]`. The common case
    /// is one IR event per framed chunk and stays inline (no heap) thanks
    /// to the `SmallVec<[_; 1]>` storage.
    ///
    /// Codecs are expected to be tolerant of malformed `data:` payloads:
    /// they return an empty `SmallVec` (with a `tracing::warn!`) rather
    /// than `Err`, so that streaming is not short-circuited mid-response.
    fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<smallvec::SmallVec<[StreamEventIR<'a>; 1]>, ErrorIR<'static>>;
    fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>>;

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>>;
    fn render_response(
        &self,
        ir: &ResponseIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>>;

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a>;
    fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut);

    fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32;
}
