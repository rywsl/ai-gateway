//! `gw-codec::wire` — wire-level data carriers.
//!
//! - [`any`] — `WireAny{Request, Response, StreamChunk}` enums used by
//!   adapter `patch_request` / `patch_response` (A-T0) and by Phase 2's
//!   N² wire→wire matrix.
//!
//! Phase 1d lands these enums as a scaffold. `WireAnyRequest` has real
//! OpenAi/Anthropic/Gemini variants because the wire request structs exist.
//! `WireAnyResponse` and `WireAnyStreamChunk` are uninhabited until Phase 2
//! (codec S2 / S4) introduces wire-level response and stream chunk structs.
//!
//! The runtime path still flows through [`crate::AnyCodec`] (a codec
//! dispatcher); Phase 2 migrates the proxy hot path onto `WireAny*`.

pub mod any;

pub use any::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
