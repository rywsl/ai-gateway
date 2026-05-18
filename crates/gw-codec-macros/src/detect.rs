//! `#[derive(DetectRequest)]` — parses `#[detect(path_prefix = "...",
//! header_contains = "...")]` attributes. For v1 the actual `detect` method is
//! hand-written in `gw-codec/src/<protocol>/codec.rs`, so this derive only
//! validates attribute syntax without emitting code. See crate-level docs in
//! `lib.rs` for the v1 scope decision.

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
    // Discard parsed values; attribute parsing is the only side effect.
    let _ = (path_prefix, header_contains);

    let out: TokenStream2 = quote! {};
    out.into()
}
