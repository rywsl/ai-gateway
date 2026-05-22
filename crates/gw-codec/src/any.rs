//! `AnyCodec` — enum dispatcher across the three supported protocols.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{RequestIR, ResponseIR, StreamEventIR};
use gw_core::protocol::ProtocolKind;

use crate::anthropic::Anthropic;
use crate::gemini::Gemini;
use crate::openai::OpenAi;
use crate::trait_def::{Headers, ProtocolCodec};

pub enum AnyCodec {
    OpenAi(OpenAi),
    Anthropic(Anthropic),
    Gemini(Gemini),
}

impl AnyCodec {
    #[inline]
    pub fn kind(&self) -> ProtocolKind {
        match self {
            AnyCodec::OpenAi(_) => ProtocolKind::OpenAi,
            AnyCodec::Anthropic(_) => ProtocolKind::Anthropic,
            AnyCodec::Gemini(_) => ProtocolKind::Gemini,
        }
    }

    /// Try each protocol's `detect_request` in declaration order.
    pub fn detect(path: &str, h: &Headers, body_head: &[u8]) -> Option<Self> {
        if let Some(c) = OpenAi::detect_request(path, h, body_head) {
            return Some(AnyCodec::OpenAi(c));
        }
        if let Some(c) = Anthropic::detect_request(path, h, body_head) {
            return Some(AnyCodec::Anthropic(c));
        }
        if let Some(c) = Gemini::detect_request(path, h, body_head) {
            return Some(AnyCodec::Gemini(c));
        }
        None
    }

    /// ADR-S1: dispatch `path_implies_stream` to the concrete codec. Only
    /// Gemini currently overrides the default (`false`); OpenAI and
    /// Anthropic inherit it.
    #[inline]
    pub fn path_implies_stream(&self, path: &str) -> bool {
        match self {
            AnyCodec::OpenAi(_) => OpenAi::path_implies_stream(path),
            AnyCodec::Anthropic(_) => Anthropic::path_implies_stream(path),
            AnyCodec::Gemini(_) => Gemini::path_implies_stream(path),
        }
    }

    /// ADR-S1b: dispatch `extract_path_model` to the concrete codec. Only
    /// Gemini currently overrides the default (`None`).
    #[inline]
    pub fn extract_path_model<'a>(&self, path: &'a str) -> Option<&'a str> {
        match self {
            AnyCodec::OpenAi(_) => OpenAi::extract_path_model(path),
            AnyCodec::Anthropic(_) => Anthropic::extract_path_model(path),
            AnyCodec::Gemini(_) => Gemini::extract_path_model(path),
        }
    }

    #[inline]
    pub fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.parse_request(body),
            AnyCodec::Anthropic(c) => c.parse_request(body),
            AnyCodec::Gemini(c) => c.parse_request(body),
        }
    }

    #[inline]
    pub fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.render_request(ir, buf),
            AnyCodec::Anthropic(c) => c.render_request(ir, buf),
            AnyCodec::Gemini(c) => c.render_request(ir, buf),
        }
    }

    #[inline]
    pub fn parse_stream_chunk<'a>(
        &self,
        chunk: &'a [u8],
    ) -> Result<smallvec::SmallVec<[StreamEventIR<'a>; 1]>, ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.parse_stream_chunk(chunk),
            AnyCodec::Anthropic(c) => c.parse_stream_chunk(chunk),
            AnyCodec::Gemini(c) => c.parse_stream_chunk(chunk),
        }
    }

    #[inline]
    pub fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.render_stream_chunk(ir, buf),
            AnyCodec::Anthropic(c) => c.render_stream_chunk(ir, buf),
            AnyCodec::Gemini(c) => c.render_stream_chunk(ir, buf),
        }
    }

    #[inline]
    pub fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.parse_response(body),
            AnyCodec::Anthropic(c) => c.parse_response(body),
            AnyCodec::Gemini(c) => c.parse_response(body),
        }
    }

    #[inline]
    pub fn render_response(
        &self,
        ir: &ResponseIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        match self {
            AnyCodec::OpenAi(c) => c.render_response(ir, buf),
            AnyCodec::Anthropic(c) => c.render_response(ir, buf),
            AnyCodec::Gemini(c) => c.render_response(ir, buf),
        }
    }

    #[inline]
    pub fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        match self {
            AnyCodec::OpenAi(c) => c.parse_error(status, body),
            AnyCodec::Anthropic(c) => c.parse_error(status, body),
            AnyCodec::Gemini(c) => c.parse_error(status, body),
        }
    }

    #[inline]
    pub fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut) {
        match self {
            AnyCodec::OpenAi(c) => c.render_error(err, buf),
            AnyCodec::Anthropic(c) => c.render_error(err, buf),
            AnyCodec::Gemini(c) => c.render_error(err, buf),
        }
    }

    #[inline]
    pub fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32 {
        match self {
            AnyCodec::OpenAi(c) => c.estimate_input_tokens(ir),
            AnyCodec::Anthropic(c) => c.estimate_input_tokens(ir),
            AnyCodec::Gemini(c) => c.estimate_input_tokens(ir),
        }
    }
}
