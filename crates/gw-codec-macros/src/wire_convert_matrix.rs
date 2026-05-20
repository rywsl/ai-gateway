//! Scaffold for the `wire_convert_matrix!` proc-macro (codec S1 v2 §ADR-S1-v2-2).
//!
//! Phase 1d: this parses the documented surface syntax and emits an empty
//! `TokenStream`. Phase 2 fills in the N² wire→wire conversions, semantic
//! trait derivation, and `WireAny` enum generation.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Ident, LitBool, Path, Token, bracketed};

/// `wire_convert_matrix! { schema: <Ident>, wires: [<Path>, ...], stream: <bool>? }`
///
/// Note: Rust strips the outer delimiters of a function-like proc-macro
/// invocation before handing the `TokenStream` to us, so we parse the
/// *content* directly. The user-facing `{ ... }` braces are still the
/// documented surface; they just don't show up in our `ParseStream`.
pub(crate) struct MatrixInput {
    pub schema: Ident,
    pub wires: Vec<Path>,
    pub stream: bool,
}

impl Parse for MatrixInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut schema: Option<Ident> = None;
        let mut wires: Option<Vec<Path>> = None;
        let mut stream = false;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "schema" => {
                    let id: Ident = input.parse()?;
                    schema = Some(id);
                }
                "wires" => {
                    let bracket_content;
                    bracketed!(bracket_content in input);
                    let paths: Punctuated<Path, Token![,]> =
                        bracket_content.parse_terminated(Path::parse, Token![,])?;
                    wires = Some(paths.into_iter().collect());
                }
                "stream" => {
                    let v: LitBool = input.parse()?;
                    stream = v.value;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown wire_convert_matrix! key `{other}` — expected one of \
                             `schema`, `wires`, `stream`"
                        ),
                    ));
                }
            }
            // Optional trailing comma between keys.
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let schema = schema.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "wire_convert_matrix! requires a `schema: <Ident>` key",
            )
        })?;
        let wires = wires.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "wire_convert_matrix! requires a `wires: [<Path>, ...]` key",
            )
        })?;
        Ok(Self { schema, wires, stream })
    }
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as MatrixInput);
    // Phase 1d: no codegen — the parsed schema / wires / stream values are
    // consumed by Phase 2 §ADR-S1-v2-2 which generates the N² wire→wire
    // conversions, semantic traits, and WireAny enum.
    //
    // We touch each parsed field once so it is exercised (rather than
    // silently dead) when Phase 2 fills in the body.
    let _ = (&parsed.schema, &parsed.wires, parsed.stream);
    let empty = TokenStream2::new();
    quote!(#empty).into()
}
