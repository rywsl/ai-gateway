use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io::Cursor;

pub const HEADER_SIZE: usize = 10;
const MAX_PAYLOAD_LEN: usize = 64 * 1024 * 1024;

pub mod flags {
    pub const NONE: u8 = 0x00;
    pub const END_STREAM: u8 = 0x01;
    pub const GZIP_COMPRESSED: u8 = 0x02;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    RequestHeaders = 0x01,
    RequestBody = 0x02,
    ResponseHeaders = 0x03,
    ResponseBody = 0x04,
    StreamEnd = 0x05,
    StreamError = 0x06,
    Ping = 0x10,
    Pong = 0x11,
    GoAway = 0x12,
    HeartbeatData = 0x13,
    HeartbeatAck = 0x14,
}

impl MsgType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::RequestHeaders,
            0x02 => Self::RequestBody,
            0x03 => Self::ResponseHeaders,
            0x04 => Self::ResponseBody,
            0x05 => Self::StreamEnd,
            0x06 => Self::StreamError,
            0x10 => Self::Ping,
            0x11 => Self::Pong,
            0x12 => Self::GoAway,
            0x13 => Self::HeartbeatData,
            0x14 => Self::HeartbeatAck,
            _ => return None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("unknown Aether message type: 0x{0:02x}")]
    UnknownMsgType(u8),
    #[error("Aether frame payload too large: {0} bytes")]
    PayloadTooLarge(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub stream_id: u32,
    pub msg_type: MsgType,
    pub flags: u8,
    pub payload: Bytes,
}

impl Frame {
    pub fn new(stream_id: u32, msg_type: MsgType, flags: u8, payload: impl Into<Bytes>) -> Self {
        Self {
            stream_id,
            msg_type,
            flags,
            payload: payload.into(),
        }
    }

    pub fn control(msg_type: MsgType, payload: impl Into<Bytes>) -> Self {
        Self::new(0, msg_type, flags::NONE, payload)
    }

    pub fn is_end_stream(&self) -> bool {
        self.flags & flags::END_STREAM != 0
    }

    pub fn is_gzip(&self) -> bool {
        self.flags & flags::GZIP_COMPRESSED != 0
    }

    pub fn encode(&self, dst: &mut BytesMut) {
        dst.reserve(HEADER_SIZE + self.payload.len());
        dst.put_u32(self.stream_id);
        dst.put_u8(self.msg_type as u8);
        dst.put_u8(self.flags);
        dst.put_u32(self.payload.len() as u32);
        dst.extend_from_slice(&self.payload);
    }

    pub fn decode(src: &mut Cursor<&[u8]>) -> Result<Option<Self>, FrameError> {
        let start = src.position();
        if src.remaining() < HEADER_SIZE {
            return Ok(None);
        }

        let stream_id = src.get_u32();
        let raw_msg_type = src.get_u8();
        let flags = src.get_u8();
        let payload_len = src.get_u32() as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(FrameError::PayloadTooLarge(payload_len));
        }
        if src.remaining() < payload_len {
            src.set_position(start);
            return Ok(None);
        }

        let msg_type =
            MsgType::from_u8(raw_msg_type).ok_or(FrameError::UnknownMsgType(raw_msg_type))?;
        let mut payload = vec![0; payload_len];
        src.copy_to_slice(&mut payload);

        Ok(Some(Self {
            stream_id,
            msg_type,
            flags,
            payload: Bytes::from(payload),
        }))
    }
}
