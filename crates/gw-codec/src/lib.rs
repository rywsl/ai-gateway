//! `gw-codec` — protocol codecs (OpenAI / Anthropic / Gemini) bridging wire
//! formats to/from the `gw_core::ir::*` intermediate representation.
//!
//! ## Header type for `detect_request`
//!
//! The plan originally proposed `pingora::http::HeaderMap` for the
//! `detect_request` signature. To keep this crate light-weight and decoupled
//! from any concrete HTTP runtime, we instead accept `&Headers`, an alias for
//! `&[(String, String)]`. Callers in `gw-proxy` / `gw-gateway` adapt their
//! native header types to this slice form (cheap: borrow-only).

pub mod any;
pub mod anthropic;
pub(crate) mod common;
pub mod gemini;
pub mod openai;
pub mod passthrough;
pub mod trait_def;
pub mod wire;

pub use any::AnyCodec;
pub use trait_def::{Headers, ProtocolCodec};
pub use wire::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
