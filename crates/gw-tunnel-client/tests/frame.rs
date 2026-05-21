use bytes::{Bytes, BytesMut};
use gw_tunnel_client::frame::{decode_frame, encode_frame, Frame, MsgType};

#[test]
fn open_data_close_ping_roundtrip() {
    for (mt, payload) in [
        (MsgType::Open, Bytes::from_static(b"target=https://api.x")),
        (MsgType::Data, Bytes::from_static(b"hello body")),
        (MsgType::Close, Bytes::new()),
        (MsgType::Ping, Bytes::from_static(b"pingpayload")),
        (MsgType::Pong, Bytes::from_static(b"hb")),
        (MsgType::Error, Bytes::from_static(b"oops")),
        (MsgType::ControlAuth, Bytes::from_static(b"token=abc")),
        (
            MsgType::ControlNodeInfo,
            Bytes::from_static(b"node=us-east"),
        ),
    ] {
        let f = Frame {
            msg_type: mt,
            flags: 0,
            stream_id: 12345,
            payload: payload.clone(),
        };
        let mut buf = BytesMut::new();
        encode_frame(&f, &mut buf);
        let (decoded, consumed) = decode_frame(&buf).expect("decode").expect("frame");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.msg_type, mt);
        assert_eq!(decoded.stream_id, 12345);
        assert_eq!(decoded.flags, 0);
        assert_eq!(&decoded.payload[..], &payload[..]);
    }
}

#[test]
fn truncated_returns_need_more() {
    // Magic only — not enough for header
    let buf = vec![0xAEu8, 0x21];
    let r = decode_frame(&buf).expect("decode err shouldn't fire");
    assert!(r.is_none());
}

#[test]
fn truncated_partial_header_returns_need_more() {
    let mut buf = BytesMut::new();
    encode_frame(
        &Frame {
            msg_type: MsgType::Data,
            flags: 0,
            stream_id: 1,
            payload: Bytes::from_static(b"hello"),
        },
        &mut buf,
    );
    // Drop trailing bytes
    let truncated = &buf[..buf.len() - 2];
    let r = decode_frame(truncated).unwrap();
    assert!(r.is_none(), "partial body should return None");
}

#[test]
fn bad_magic_errors() {
    let buf = vec![0xFFu8; 32];
    let r = decode_frame(&buf);
    assert!(r.is_err());
}

#[test]
fn unknown_msg_type_errors() {
    let mut buf = BytesMut::new();
    encode_frame(
        &Frame {
            msg_type: MsgType::Data,
            flags: 0,
            stream_id: 1,
            payload: Bytes::from_static(b"a"),
        },
        &mut buf,
    );
    buf[3] = 0xEE; // replace msg_type byte
    let r = decode_frame(&buf);
    assert!(r.is_err());
}

#[test]
fn empty_payload_roundtrip() {
    let f = Frame {
        msg_type: MsgType::Close,
        flags: 0xBEEF,
        stream_id: 0,
        payload: Bytes::new(),
    };
    let mut buf = BytesMut::new();
    encode_frame(&f, &mut buf);
    let (d, c) = decode_frame(&buf).unwrap().unwrap();
    assert_eq!(c, buf.len());
    assert_eq!(d.flags, 0xBEEF);
    assert_eq!(d.msg_type, MsgType::Close);
}

#[test]
fn payload_length_check() {
    use gw_tunnel_client::frame::MAX_PAYLOAD_LEN;
    // We don't actually allocate MAX+1 bytes; we craft a header claiming oversized length.
    let mut buf = vec![0xAEu8, 0x21, 1, MsgType::Data as u8]; // magic + version + msg_type
    buf.extend_from_slice(&0u16.to_be_bytes()); // flags
    buf.extend_from_slice(&1u32.to_be_bytes()); // stream_id
    let oversized = (MAX_PAYLOAD_LEN as u32 + 1).to_be_bytes();
    buf.extend_from_slice(&oversized); // length
    let r = decode_frame(&buf);
    assert!(r.is_err(), "oversized payload should error");
}
