//! `#[derive(IrConvert)]` — see crate-level docs in `lib.rs` for the v1 scope
//! decision. This module parses the `#[ir(...)]` attribute grammar so users may
//! annotate wire structs without errors, but emits no code for v1.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, LitStr};

#[derive(Default)]
struct IrTargetAttr {
    target: String,
    direction: String,
}

#[derive(Default)]
#[allow(dead_code)]
struct FieldAttr {
    map: Option<String>,
    via: Option<syn::Path>,
    default: Option<String>,
    extensions: bool,
}

pub fn expand(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);

    // Parse-only walk: surface attribute syntax errors at derive time, but
    // emit no code (see crate-level docs). All conversion impls are hand-
    // written in `gw-codec`.
    let _ = parse_target_attr(&ast.attrs);
    if let syn::Data::Struct(s) = &ast.data {
        if let syn::Fields::Named(named) = &s.fields {
            for field in &named.named {
                let _ = parse_field_attr(&field.attrs);
            }
        }
    }

    let out: TokenStream2 = quote! {};
    out.into()
}

fn parse_target_attr(attrs: &[syn::Attribute]) -> IrTargetAttr {
    let mut out = IrTargetAttr {
        target: String::new(),
        direction: String::from("both"),
    };
    for a in attrs {
        if !a.path().is_ident("ir") {
            continue;
        }
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("target") {
                out.target = m.value()?.parse::<LitStr>()?.value();
            }
            if m.path.is_ident("direction") {
                out.direction = m.value()?.parse::<LitStr>()?.value();
            }
            Ok(())
        });
    }
    out
}

fn parse_field_attr(attrs: &[syn::Attribute]) -> FieldAttr {
    let mut fa = FieldAttr::default();
    for a in attrs {
        if !a.path().is_ident("ir") {
            continue;
        }
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("map") {
                fa.map = Some(m.value()?.parse::<LitStr>()?.value());
            } else if m.path.is_ident("via") {
                let s: LitStr = m.value()?.parse()?;
                fa.via = Some(syn::parse_str(&s.value())?);
            } else if m.path.is_ident("default") {
                fa.default = Some(m.value()?.parse::<LitStr>()?.value());
            } else if m.path.is_ident("extensions") {
                fa.extensions = true;
            }
            Ok(())
        });
    }
    fa
}
