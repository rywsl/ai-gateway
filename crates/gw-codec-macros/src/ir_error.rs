//! `#[derive(IrErrorConvert)]` — currently a thin re-export of `IrConvert`'s
//! expander. See crate-level docs in `lib.rs` for the v1 scope decision.

pub fn expand(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    crate::ir_convert::expand(input)
}
