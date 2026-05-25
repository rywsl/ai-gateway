//! `gw-codec::wire` — wire-level data carriers.
//!
//! - [`any`] – `WireAnyRequest` enum used by adapter `patch_request` (A-T0).

pub mod any;

pub use any::WireAnyRequest;
