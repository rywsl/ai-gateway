//! Kiro / S5 EventStream support.
//!
//! Kiro streams use AWS EventStream binary frames. This module keeps the
//! binary framer and the embedded `<thinking>` payload parser inside
//! `gw-codec`; Kiro is not added to `gw-core::protocol` until the gateway
//! cross-process protocol contract grows a Kiro shape.

use bytes::{BufMut, BytesMut};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{
    FinishReasonIR, MessageIR, RequestIR, ResponseIR, ResponseThinkingIR, StreamEventIR,
    ThinkingBlockIR,
};
use std::borrow::Cow;
use std::fmt;

use crate::common::{bad_request_err, classify, content_to_text, map_code};
use crate::trait_def::{EmptyStreamState, Headers, ProtocolCodec};

const EVENTSTREAM_PREAMBLE_LEN: usize = 12;
const EVENTSTREAM_TRAILER_LEN: usize = 4;
const EVENTSTREAM_MIN_LEN: usize = EVENTSTREAM_PREAMBLE_LEN + EVENTSTREAM_TRAILER_LEN;
const EVENTSTREAM_MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventStreamFrame {
    pub headers: Vec<u8>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KiroPayloadPart<'a> {
    Text(&'a str),
    Thinking(&'a str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KiroCodecError {
    FrameTooShort,
    FrameLength { declared: usize, actual: usize },
    HeaderLength { headers: usize, frame: usize },
    PreludeCrc { expected: u32, actual: u32 },
    MessageCrc { expected: u32, actual: u32 },
    UnclosedThinkingTag,
    UnexpectedThinkingClose,
}

impl fmt::Display for KiroCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooShort => write!(f, "eventstream frame too short"),
            Self::FrameLength { declared, actual } => {
                write!(
                    f,
                    "eventstream length mismatch: declared {declared}, actual {actual}"
                )
            }
            Self::HeaderLength { headers, frame } => {
                write!(
                    f,
                    "eventstream header length {headers} exceeds frame length {frame}"
                )
            }
            Self::PreludeCrc { expected, actual } => {
                write!(
                    f,
                    "eventstream prelude crc mismatch: expected {expected:#010x}, actual {actual:#010x}"
                )
            }
            Self::MessageCrc { expected, actual } => {
                write!(
                    f,
                    "eventstream message crc mismatch: expected {expected:#010x}, actual {actual:#010x}"
                )
            }
            Self::UnclosedThinkingTag => write!(f, "unclosed thinking tag"),
            Self::UnexpectedThinkingClose => write!(f, "unexpected closing thinking tag"),
        }
    }
}

impl std::error::Error for KiroCodecError {}

pub fn encode_frame(frame: &EventStreamFrame) -> Vec<u8> {
    let total_len = EVENTSTREAM_MIN_LEN + frame.headers.len() + frame.payload.len();
    assert!(
        total_len <= u32::MAX as usize,
        "eventstream frame too large"
    );
    assert!(
        frame.headers.len() <= u32::MAX as usize,
        "eventstream headers too large"
    );

    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&(total_len as u32).to_be_bytes());
    out.extend_from_slice(&(frame.headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32fast::hash(&out[..8]);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&frame.headers);
    out.extend_from_slice(&frame.payload);
    let message_crc = crc32fast::hash(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

pub fn decode_frame(frame: &[u8]) -> Result<EventStreamFrame, KiroCodecError> {
    if frame.len() < EVENTSTREAM_MIN_LEN {
        return Err(KiroCodecError::FrameTooShort);
    }

    let declared = u32::from_be_bytes(frame[0..4].try_into().expect("length bytes")) as usize;
    let headers_len = u32::from_be_bytes(frame[4..8].try_into().expect("headers bytes")) as usize;
    if declared != frame.len() {
        return Err(KiroCodecError::FrameLength {
            declared,
            actual: frame.len(),
        });
    }
    if declared > EVENTSTREAM_MAX_FRAME_LEN {
        return Err(KiroCodecError::FrameLength {
            declared,
            actual: frame.len(),
        });
    }

    let expected_prelude_crc =
        u32::from_be_bytes(frame[8..12].try_into().expect("prelude crc bytes"));
    let actual_prelude_crc = crc32fast::hash(&frame[..8]);
    if expected_prelude_crc != actual_prelude_crc {
        return Err(KiroCodecError::PreludeCrc {
            expected: expected_prelude_crc,
            actual: actual_prelude_crc,
        });
    }

    let header_end = EVENTSTREAM_PREAMBLE_LEN + headers_len;
    let payload_end = declared - EVENTSTREAM_TRAILER_LEN;
    if header_end > payload_end {
        return Err(KiroCodecError::HeaderLength {
            headers: headers_len,
            frame: declared,
        });
    }

    let expected_message_crc = u32::from_be_bytes(
        frame[payload_end..declared]
            .try_into()
            .expect("message crc bytes"),
    );
    let actual_message_crc = crc32fast::hash(&frame[..payload_end]);
    if expected_message_crc != actual_message_crc {
        return Err(KiroCodecError::MessageCrc {
            expected: expected_message_crc,
            actual: actual_message_crc,
        });
    }

    Ok(EventStreamFrame {
        headers: frame[EVENTSTREAM_PREAMBLE_LEN..header_end].to_vec(),
        payload: frame[header_end..payload_end].to_vec(),
    })
}

pub fn split_thinking_tags(input: &str) -> Result<Vec<KiroPayloadPart<'_>>, KiroCodecError> {
    const OPEN: &str = "<thinking>";
    const CLOSE: &str = "</thinking>";

    let mut parts = Vec::new();
    let mut rest = input;
    loop {
        let Some(open_idx) = rest.find(OPEN) else {
            if rest.contains(CLOSE) {
                return Err(KiroCodecError::UnexpectedThinkingClose);
            }
            if !rest.is_empty() {
                parts.push(KiroPayloadPart::Text(rest));
            }
            return Ok(parts);
        };

        let before = &rest[..open_idx];
        if before.contains(CLOSE) {
            return Err(KiroCodecError::UnexpectedThinkingClose);
        }
        if !before.is_empty() {
            parts.push(KiroPayloadPart::Text(before));
        }

        let after_open = &rest[open_idx + OPEN.len()..];
        let Some(close_idx) = after_open.find(CLOSE) else {
            return Err(KiroCodecError::UnclosedThinkingTag);
        };
        parts.push(KiroPayloadPart::Thinking(&after_open[..close_idx]));
        rest = &after_open[close_idx + CLOSE.len()..];
    }
}

pub fn render_thinking_tags(parts: &[KiroPayloadPart<'_>]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            KiroPayloadPart::Text(text) => out.push_str(text),
            KiroPayloadPart::Thinking(text) => {
                out.push_str("<thinking>");
                out.push_str(text);
                out.push_str("</thinking>");
            }
        }
    }
    out
}

pub struct Kiro;

impl ProtocolCodec for Kiro {
    fn detect_request(path: &str, headers: &Headers, body_head: &[u8]) -> Option<Self> {
        let kiro_path = path.contains("/kiro") || path.contains("/s5");
        let eventstream_header = headers.iter().any(|(k, v)| {
            let key = k.as_str();
            key.eq_ignore_ascii_case("content-type")
                && v.to_ascii_lowercase()
                    .contains("application/vnd.amazon.eventstream")
        });
        let eventstream_body =
            body_head.len() >= EVENTSTREAM_MIN_LEN && decode_frame_prefix(body_head).is_some();

        if eventstream_header && (kiro_path || eventstream_body) {
            Some(Kiro)
        } else {
            None
        }
    }

    fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, ErrorIR<'static>> {
        let frame = decode_frame(body).map_err(|e| bad_request_err(format!("kiro parse: {e}")))?;
        let payload = std::str::from_utf8(&frame.payload)
            .map_err(|e| bad_request_err(format!("kiro payload utf8: {e}")))?;
        Ok(RequestIR {
            messages: vec![MessageIR::User {
                content: gw_core::ir::ContentIR::Text(Cow::Owned(payload.to_owned())),
            }],
            stream: true,
            ..RequestIR::default()
        })
    }

    fn render_request(
        &self,
        ir: &RequestIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let payload = ir
            .messages
            .iter()
            .map(|m| match m {
                MessageIR::System { content }
                | MessageIR::Developer { content }
                | MessageIR::User { content }
                | MessageIR::Assistant { content, .. }
                | MessageIR::Tool { content, .. } => content_to_text(content),
            })
            .collect::<Vec<_>>()
            .join("");
        buf.extend_from_slice(&encode_frame(&EventStreamFrame {
            headers: Vec::new(),
            payload: payload.into_bytes(),
        }));
        Ok(())
    }

    fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
        Box::new(EmptyStreamState)
    }

    fn parse_stream_chunk<'a>(
        &self,
        _state: &mut dyn gw_core::ir::StreamState,
        chunk: &'a [u8],
    ) -> Result<Vec<StreamEventIR<'a>>, ErrorIR<'static>> {
        let frame =
            decode_frame(chunk).map_err(|e| bad_request_err(format!("kiro stream parse: {e}")))?;
        let payload = std::str::from_utf8(&frame.payload)
            .map_err(|e| bad_request_err(format!("kiro stream payload utf8: {e}")))?;
        let mut events = Vec::new();
        for part in split_thinking_tags(payload)
            .map_err(|e| bad_request_err(format!("kiro thinking parse: {e}")))?
        {
            match part {
                KiroPayloadPart::Text(text) if !text.is_empty() => {
                    events.push(StreamEventIR::Delta {
                        text: Some(Cow::Owned(text.to_owned())),
                        tool_call: None,
                        thinking: None,
                    });
                }
                KiroPayloadPart::Thinking(text) if !text.is_empty() => {
                    events.push(StreamEventIR::ThinkingDelta {
                        text: Cow::Owned(text.to_owned()),
                        signature_chunk: None,
                    });
                }
                _ => {}
            }
        }
        Ok(events)
    }

    fn render_stream_chunk(
        &self,
        ir: &StreamEventIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let payload = match ir {
            StreamEventIR::Delta { text, .. } => text.as_deref().unwrap_or("").to_owned(),
            StreamEventIR::ThinkingDelta { text, .. } => {
                render_thinking_tags(&[KiroPayloadPart::Thinking(text.as_ref())])
            }
            StreamEventIR::End { finish_reason } => format!(
                r#"{{"type":"message_stop","finish_reason":"{}"}}"#,
                finish_reason_label(finish_reason)
            ),
            StreamEventIR::Error(err) => serde_json::json!({
                "type": "error",
                "code": err.code,
                "message": err.message,
            })
            .to_string(),
            StreamEventIR::Start { .. }
            | StreamEventIR::Usage(_)
            | StreamEventIR::ToolCallStart { .. }
            | StreamEventIR::ToolCallDelta { .. }
            | StreamEventIR::RefusalDelta { .. }
            | StreamEventIR::CitationsDelta { .. }
            | StreamEventIR::ServerToolStart { .. }
            | StreamEventIR::ServerToolDelta { .. } => String::new(),
        };

        if !payload.is_empty() {
            buf.extend_from_slice(&encode_frame(&EventStreamFrame {
                headers: Vec::new(),
                payload: payload.into_bytes(),
            }));
        }
        Ok(())
    }

    fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
        let frame = decode_frame(body).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("kiro parse_response: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;
        let payload = std::str::from_utf8(&frame.payload).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("kiro parse_response utf8: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })?;

        let mut text = String::new();
        let mut thinking = Vec::new();
        for part in split_thinking_tags(payload).map_err(|e| ErrorIR {
            category: ErrorCategory::UpstreamError,
            status: 502,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
            message: Cow::Owned(format!("kiro parse_response thinking: {e}")),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        })? {
            match part {
                KiroPayloadPart::Text(part) => text.push_str(part),
                KiroPayloadPart::Thinking(part) => thinking.push(ThinkingBlockIR {
                    text: Cow::Owned(part.to_owned()),
                    signature: None,
                }),
            }
        }

        Ok(ResponseIR {
            messages: vec![MessageIR::Assistant {
                content: gw_core::ir::ContentIR::Text(Cow::Owned(text)),
                tool_calls: None,
            }],
            thinking: (!thinking.is_empty()).then_some(ResponseThinkingIR { blocks: thinking }),
            finish_reason: Some(FinishReasonIR::Stop),
            ..ResponseIR::default()
        })
    }

    fn render_response(
        &self,
        ir: &ResponseIR<'_>,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let mut payload = String::new();
        if let Some(thinking) = &ir.thinking {
            for block in &thinking.blocks {
                payload.push_str("<thinking>");
                payload.push_str(block.text.as_ref());
                payload.push_str("</thinking>");
            }
        }
        for message in &ir.messages {
            if let MessageIR::Assistant { content, .. } = message {
                let text = content_to_text(content);
                if !text.is_empty() {
                    payload.push_str(text.as_ref());
                }
            }
        }
        buf.put_slice(&encode_frame(&EventStreamFrame {
            headers: Vec::new(),
            payload: payload.into_bytes(),
        }));
        Ok(())
    }

    fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a> {
        ErrorIR {
            category: classify(status),
            status,
            upstream_status: Some(status),
            code: Cow::Borrowed(map_code(status)),
            message: Cow::Owned(String::from_utf8_lossy(body).into_owned()),
            retryable: matches!(status, 500..=599 | 429),
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        }
    }

    fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut) {
        let body = serde_json::json!({
            "type": "error",
            "code": err.code,
            "message": err.message,
        });
        let payload = serde_json::to_vec(&body).unwrap_or_default();
        buf.put_slice(&encode_frame(&EventStreamFrame {
            headers: Vec::new(),
            payload,
        }));
    }

    fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32 {
        ir.messages
            .iter()
            .map(|m| match m {
                MessageIR::System { content }
                | MessageIR::Developer { content }
                | MessageIR::User { content }
                | MessageIR::Tool { content, .. }
                | MessageIR::Assistant { content, .. } => crate::common::approx_tokens(content),
            })
            .sum()
    }
}

fn decode_frame_prefix(frame: &[u8]) -> Option<()> {
    if frame.len() < EVENTSTREAM_MIN_LEN {
        return None;
    }
    let declared = u32::from_be_bytes(frame[0..4].try_into().ok()?) as usize;
    let headers_len = u32::from_be_bytes(frame[4..8].try_into().ok()?) as usize;
    let prelude_crc = u32::from_be_bytes(frame[8..12].try_into().ok()?);
    let plausible = declared >= EVENTSTREAM_MIN_LEN
        && declared <= EVENTSTREAM_MAX_FRAME_LEN
        && headers_len <= declared.saturating_sub(EVENTSTREAM_MIN_LEN);
    (plausible && crc32fast::hash(&frame[..8]) == prelude_crc).then_some(())
}

fn finish_reason_label(reason: &FinishReasonIR) -> &str {
    match reason {
        FinishReasonIR::Stop => "stop",
        FinishReasonIR::Length => "length",
        FinishReasonIR::ToolCalls => "tool_calls",
        FinishReasonIR::ContentFilter => "content_filter",
        FinishReasonIR::Refusal => "refusal",
        FinishReasonIR::Other(other) => other.as_ref(),
    }
}
