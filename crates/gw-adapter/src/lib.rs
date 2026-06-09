//! `gw-adapter` — vendor-specific upstream rewrite hooks.
//!
//! Per integration spec §3 C-1 this is a **business-layer** crate. It
//! provides the `UpstreamAdapter` v2 trait (4 hooks + `transport_profile`),
//! an `AdapterCtx` runtime-agnostic carrier, a `NoopAdapter` default that
//! preserves the legacy `x-api-key` injection behavior, an `AdapterRegistry`
//! for vendor lookup, and an `UpstreamErrorAnalyzer` trait skeleton (full
//! K-B vendor analyzers ship in Phase 3).
//!
//! Spec: `docs/spec/07-vendor-integration.md`.

pub mod analyzer;
pub mod ctx;
pub mod error;
pub mod kiro;
pub mod noop;
pub mod oauth_runtime;
pub mod registry;
pub mod trait_def;
pub mod vertex;

pub use analyzer::{
    BuiltinAnalyzerKind, DefaultErrorAnalyzer, ErrorKind, ErrorOutcome, HeaderView,
    KeywordErrorAnalyzer, UpstreamErrorAnalyzer,
};
pub use ctx::{
    AdapterCtx, HeadersAccessor, HeadersMut, OAuthRuntime, OAuthTokenResult, UrlAccessor, UrlMut,
};
pub use error::AdapterError;
pub use kiro::KiroAdapter;
pub use noop::NoopAdapter;
pub use registry::AdapterRegistry;
pub use trait_def::{ChromeVersion, TransportProfile, UpstreamAdapter};
pub use vertex::{VertexAdapter, VertexJwtClaims};
