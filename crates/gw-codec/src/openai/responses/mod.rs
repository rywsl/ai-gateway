//! OpenAI Responses API wire types (T3-B).
//!
//! Conversion to/from chat / anthropic / gemini lives in T5-F.

pub mod wire_request;
pub mod wire_response;
pub mod wire_stream_chunk;

pub use wire_request::OpenAiResponsesRequest;
pub use wire_response::OpenAiResponsesResponse;
pub use wire_stream_chunk::OpenAiResponsesStreamChunk;
