//! `gw-codec-macros` — derive macros for the gw-codec crate.
//!
//! ## v1 scope note
//!
//! The plan (ADR-019 / `docs/superpowers/plans/2026-05-18-llm-gateway-implementation.md`)
//! prescribes a fairly elaborate `#[derive(IrConvert)]` family that parses
//! `#[ir(target = "...", map = "...", via = "...", default = "...", extensions)]`
//! attributes and emits both `From<Wire<'a>> for RequestIR<'a>` and
//! `TryFrom<&RequestIR<'a>> for Wire<'a>` impls.
//!
//! For the v1 milestone, generating that codegen reliably across the OpenAI /
//! Anthropic / Gemini wire structs (each of which has bespoke shape, lifetimes,
//! and nested enums) is a non-trivial undertaking that is not on the critical
//! path: the `gw-codec` crate consumes the resulting `From`/`TryFrom` impls
//! through ordinary trait resolution, so the impls can equally well be
//! hand-written.
//!
//! Therefore the derive macros in this crate are deliberately **scaffolded but
//! inert** for v1:
//! - they exist and compile,
//! - they parse `#[ir(...)]` and `#[detect(...)]` attributes (so wire structs
//!   may still be annotated for documentation purposes), but
//! - they emit empty token streams; `From`/`TryFrom` impls are hand-written in
//!   `gw-codec/src/<protocol>/via.rs`.
//!
//! Once the wire structs stabilise we can graduate the macros to full codegen
//! without touching downstream call sites.

use proc_macro::TokenStream;

mod detect;
mod ir_convert;
mod ir_error;
mod ir_stream;

#[proc_macro_derive(IrConvert, attributes(ir))]
pub fn derive_ir_convert(input: TokenStream) -> TokenStream {
    ir_convert::expand(input)
}

#[proc_macro_derive(IrStreamConvert, attributes(ir))]
pub fn derive_ir_stream_convert(input: TokenStream) -> TokenStream {
    ir_stream::expand(input)
}

#[proc_macro_derive(IrErrorConvert, attributes(ir))]
pub fn derive_ir_error_convert(input: TokenStream) -> TokenStream {
    ir_error::expand(input)
}

#[proc_macro_derive(DetectRequest, attributes(detect))]
pub fn derive_detect_request(input: TokenStream) -> TokenStream {
    detect::expand(input)
}
