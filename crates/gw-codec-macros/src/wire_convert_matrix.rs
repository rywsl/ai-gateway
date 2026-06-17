//! Scaffold for the `wire_convert_matrix!` proc-macro (codec S1 v2 §ADR-S1-v2-2).
//!
//! Phase 1d: this parses the documented surface syntax and emits an empty
//! `TokenStream`. Phase 2 fills in the N² wire→wire conversions, semantic
//! trait derivation, and `WireAny` enum generation.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{bracketed, Ident, LitBool, Path, Token};

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
        Ok(Self {
            schema,
            wires,
            stream,
        })
    }
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as MatrixInput);
    // D20/D21 (#18): Phase 2 codegen (§ADR-S1-v2-2 N² wire→wire conversions,
    // semantic traits, WireAny enum) is not implemented. Rather than expand to
    // an empty TokenStream — which silently produces NOTHING and traps a future
    // developer who expects generated impls — validate the surface syntax (done
    // by the `parse_macro_input!` above) and then emit a `compile_error!` so any
    // invocation fails loudly until the codegen lands.
    let _ = (&parsed.schema, &parsed.wires, parsed.stream);
    let msg = "`wire_convert_matrix!` codegen is not implemented yet (codec S1 v2 Phase 2, \
               §ADR-S1-v2-2). The macro currently only validates its surface syntax; \
               implement the N² wire→wire generation before invoking it.";
    quote! {
        ::core::compile_error!(#msg);
    }
    .into()
}
