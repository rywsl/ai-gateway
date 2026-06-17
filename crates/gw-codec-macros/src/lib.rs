//! `gw-codec-macros` — proc-macros for the `gw-codec` crate.
//!
//! This crate hosts a single `proc_macro` invoked from `gw-codec`:
//! `wire_convert_matrix!`. The matrix macro replaces the three legacy
//! `#[derive(IrConvert / IrStreamConvert / IrErrorConvert)]` macros that
//! were inert v1 scaffolding; those were removed in Phase 1d per
//! integration spec §2.1 A9.
//!
//! `wire_convert_matrix!` itself is a *framework* scaffold whose Phase 2 codegen
//! (codec S1 v2 N² wire→wire generation, §S1-v2-2) is not implemented. D20/D21
//! (#18): rather than silently expanding to an empty `TokenStream` — which traps
//! a future developer who expects generated impls — both `wire_convert_matrix!`
//! and `#[derive(DetectRequest)]` validate their surface syntax and then emit a
//! `compile_error!`, so any invocation fails loudly at compile time until the
//! codegen lands.

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
