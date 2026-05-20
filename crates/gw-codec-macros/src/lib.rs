//! `gw-codec-macros` — proc-macros for the `gw-codec` crate.
//!
//! This crate hosts a single `proc_macro` invoked from `gw-codec`:
//! `wire_convert_matrix!`. The matrix macro replaces the three legacy
//! `#[derive(IrConvert / IrStreamConvert / IrErrorConvert)]` macros that
//! were inert v1 scaffolding; those were removed in Phase 1d per
//! integration spec §2.1 A9.
//!
//! `wire_convert_matrix!` itself is a *framework* scaffold in Phase 1d —
//! it parses its input syntax and emits an empty `TokenStream`. Phase 2
//! (codec S1 v2 N² wire→wire codegen, §S1-v2-2) fills in the real
//! generation.

use proc_macro::TokenStream;

mod detect;
mod wire_convert_matrix;

#[proc_macro_derive(DetectRequest, attributes(detect))]
pub fn derive_detect_request(input: TokenStream) -> TokenStream {
    detect::expand(input)
}

#[proc_macro]
pub fn wire_convert_matrix(input: TokenStream) -> TokenStream {
    wire_convert_matrix::expand(input)
}
