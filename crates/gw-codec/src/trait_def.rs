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

    /// ADR-S1: derive `stream` from the request URL path when the wire format
    /// signals streaming via the path rather than a body field.
    ///
    /// The default returns `false`. Gemini overrides this to return `true`
    /// for paths containing `:streamGenerateContent`, since the Gemini JSON
    /// body carries no `stream` field. `request_filter` ORs this into
    /// `RequestIR.stream` after `parse_request`.
    #[inline]
    fn path_implies_stream(_path: &str) -> bool
    where
        Self: Sized,
    {
        false
    }

    /// Return the model name embedded in the request URL path, if this
    /// protocol carries it there (Gemini's `/v1[beta]/models/<MODEL>:<verb>`).
    ///
    /// Default returns `None`. `request_filter` falls back to this only when
    /// `RequestIR.model` came back empty from `parse_request`, so codecs that
    /// carry the model in the body (OpenAI, Anthropic) are unaffected.
    #[inline]
    fn extract_path_model(_path: &str) -> Option<&str>
    where
        Self: Sized,
    {
        None
    }

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>>;
    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>>;

    /// Build a fresh accumulator state for a new SSE response stream.
    ///
    /// The filter holds the `Box<dyn StreamState>` for the lifetime of the
    /// response but does not inspect its fields. Each codec's State carries
    /// codec-internal accumulation (e.g. tool_call argument deltas,
    /// thinking signatures). Called once per response in
    /// `upstream_request_filter` when `RequestIR.stream` is true (upstream
    /// codec is resolved in `rewrite_request`, so initialization cannot
    /// happen any earlier).
    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState>;

    /// Parse one already-framed SSE event chunk.
    ///
    /// CONTRACT: The caller pre-frames the upstream byte stream on `\n\n`
    /// boundaries and passes exactly one complete event to this method. The
    /// `state` argument persists across chunks so the codec can buffer
    /// partial events (e.g. tool_call.arguments deltas) and emit only when
    /// a complete logical event lands. A single chunk may therefore produce
    /// 0, 1, or multiple `StreamEventIR`s.
    ///
    /// A single framed event may decompose into more than one IR event —
    /// notably Gemini's combined final chunk carrying both `text` and
    /// `finishReason` decomposes into `[Delta, Finish]`.
    ///
    /// Codecs are expected to be tolerant of malformed `data:` payloads:
    /// they return an empty `Vec` (with a `tracing::warn!`) rather than
    /// `Err`, so that streaming is not short-circuited mid-response.
    fn parse_stream_chunk<'a>(
        &self,
        state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>>;
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

/// Placeholder `StreamState` for the T2 → T4 intermediate window. Each codec
/// returns `Box::new(EmptyStreamState)` from `new_stream_state` until T4-A/B/C
/// replaces it with a codec-specific accumulator. A functional empty type is
/// used rather than an unimplemented panic, because the proxy creates the
/// state for every streaming request — a panicking placeholder would break
/// all unrelated streaming tests.
#[derive(Default)]
pub struct EmptyStreamState;

impl gw_core::ir::StreamState for EmptyStreamState {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
