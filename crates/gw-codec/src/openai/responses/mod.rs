//! OpenAI Responses API: wire types, IR conversions, and ProtocolCodec impl.
//!
//! T4-B landed the non-stream `render_response` / `parse_response` round-trip
//! plus a Responses-streaming SSE accumulator (`OpenAiResponsesStreamState`)
//! for the reasoning-summary + function-call event taxonomy.

pub mod codec;
pub mod conversion;
pub mod state;
pub mod via;
pub mod wire_request;
pub mod wire_response;
pub mod wire_stream_chunk;

pub use codec::OpenAiResponsesCodec;
pub use state::OpenAiResponsesStreamState;
pub use wire_request::OpenAiResponsesRequest;
pub use wire_response::OpenAiResponsesResponse;
pub use wire_stream_chunk::OpenAiResponsesStreamChunk;
