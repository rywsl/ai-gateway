//! Same-protocol passthrough fast path (ADR-019).
//!
//! When the upstream and downstream speak the same protocol and the gateway
//! has not mutated the IR, we can hand the original request body to the
//! upstream verbatim — zero parse, zero serialize, zero copy.
//!
//! When mutations are present we must re-render the IR through the codec, at
//! which point the body is necessarily owned.

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::Mutations;
use std::borrow::Cow;

use crate::trait_def::ProtocolCodec;

#[inline]
pub fn same_protocol_passthrough<'a, C: ProtocolCodec>(
    codec: &C,
    body: &'a [u8],
    mutations: &Mutations,
) -> Result<Cow<'a, [u8]>, ErrorIR<'static>> {
    if mutations.is_empty() {
        return Ok(Cow::Borrowed(body));
    }
    let ir = codec.parse_request(body)?;
    let mut buf = BytesMut::with_capacity(body.len());
    codec.render_request(&ir, &mut buf)?;
    Ok(Cow::Owned(buf.to_vec()))
}
