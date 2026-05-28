//! Observability primitives shared across crates.
//!
//! - [`sink`] — `observe()` unified error event sink + `ErrorObservation` type.
pub mod sink;

pub use sink::{observe, ErrorKind, ErrorObservation, Stage};
