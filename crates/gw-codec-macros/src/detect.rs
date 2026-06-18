//! `#[derive(DetectRequest)]` — parses `#[detect(path_prefix = "...",
//! header_contains = "...")]` attributes. For v1 the actual `detect` method is
//! hand-written in `gw-codec/src/<protocol>/codec.rs`, so the real codegen is
//! not implemented yet.
//!
//! D20/D21 (#18): rather than silently expanding to an empty `TokenStream`
//! (which lets a future developer `#[derive(DetectRequest)]` and get NOTHING
//! with no warning), the derive validates its attribute syntax and then emits a
//! `compile_error!` so misuse fails loudly at compile time until codegen lands.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, LitStr};

pub fn expand(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let mut path_prefix = String::new();
    let mut header_contains: Option<String> = None;
    for a in &ast.attrs {
        if !a.path().is_ident("detect") {
            continue;
        }
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("path_prefix") {
                path_prefix = m.value()?.parse::<LitStr>()?.value();
            } else if m.path.is_ident("header_contains") {
                header_contains = Some(m.value()?.parse::<LitStr>()?.value());
            }
            Ok(())
        });
    }
    // Discard parsed values; attribute parsing is the only validation side effect.
    let _ = (path_prefix, header_contains);

    // Fail loudly: codegen is not implemented. Emitting `compile_error!` (instead
    // of an empty TokenStream) ensures any `#[derive(DetectRequest)]` is rejected
    // at compile time rather than silently producing nothing.
    let _ = &ast.ident;
    let msg = "`#[derive(DetectRequest)]` is not implemented yet (codec v1 hand-writes \
               `detect` in gw-codec/src/<protocol>/codec.rs). Implement the codegen in \
               gw-codec-macros before using this derive.";
    let out: TokenStream2 = quote! {
        ::core::compile_error!(#msg);
    };
    out.into()
}
