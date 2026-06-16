//! Frame codec — matches `aether-hub/src/protocol.rs` byte layout.
//!
//! Layout: magic(2) | version(1) | msg_type(1) | flags(2) | stream_id(4) |
//! length(4) | payload(length)
//!
//! Total header is `HEADER_LEN = 14` bytes. Payloads above `MAX_PAYLOAD_LEN`
//! (4 MiB) are rejected on decode to bound memory.

use bytes::{BufMut, Bytes, BytesMut};

pub const MAGIC: [u8; 2] = [0xAE, 0x21];
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 14;
pub const MAX_PAYLOAD_LEN: usize = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Open = 0x01,
    Data = 0x02,
    Close = 0x03,
    Ping = 0x04,
    Pong = 0x05,
    Error = 0x06,
    ControlAuth = 0x10,
    ControlNodeInfo = 0x11,
}

impl MsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => Self::Open,
            0x02 => Self::Data,
            0x03 => Self::Close,
            0x04 => Self::Ping,
            0x05 => Self::Pong,
            0x06 => Self::Error,
            0x10 => Self::ControlAuth,
            0x11 => Self::ControlNodeInfo,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub msg_type: MsgType,
    pub flags: u16,
    pub stream_id: u32,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("unknown msg type {0:#x}")]
    UnknownMsgType(u8),
    #[error("payload exceeds max ({0} bytes)")]
    PayloadTooLarge(usize),
}

pub fn encode_frame(f: &Frame, buf: &mut BytesMut) {
    buf.reserve(HEADER_LEN + f.payload.len());
    buf.put_slice(&MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(f.msg_type as u8);
    buf.put_u16(f.flags);
    buf.put_u32(f.stream_id);
    buf.put_u32(f.payload.len() as u32);
    buf.put_slice(&f.payload);
}

/// Decode the next frame.
///
/// Returns:
/// - `Ok(Some((frame, consumed)))` on success — `consumed` bytes can be discarded.
/// - `Ok(None)` if the buffer holds an incomplete frame; caller should read more.
/// - `Err(_)` on a hard protocol error (bad magic / version / msg type / oversized).
pub fn decode_frame(input: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
    if input.len() < HEADER_LEN {
        return Ok(None);
    }
    if input[0..2] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    if input[2] != VERSION {
        return Err(FrameError::BadVersion(input[2]));
    }
    let mt = MsgType::from_u8(input[3]).ok_or(FrameError::UnknownMsgType(input[3]))?;
    let flags = u16::from_be_bytes([input[4], input[5]]);
    let stream_id = u32::from_be_bytes([input[6], input[7], input[8], input[9]]);
    let length = u32::from_be_bytes([input[10], input[11], input[12], input[13]]) as usize;
    if length > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge(MAX_PAYLOAD_LEN));
    }
    let total = HEADER_LEN + length;
    if input.len() < total {
        return Ok(None);
    }
    let payload = Bytes::copy_from_slice(&input[HEADER_LEN..total]);
    Ok(Some((
        Frame {
            msg_type: mt,
            flags,
            stream_id,
            payload,
        },
        total,
    )))
}
