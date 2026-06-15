use bytes::BytesMut;
use gw_tunnel_client::aether::frame::{flags, Frame, MsgType, HEADER_SIZE};
use std::io::Cursor;

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/aether/{name}")).expect("fixture missing")
}

#[test]
fn frame_decode_request_headers_fixture() {
    let bytes = read_fixture("request_headers.bin");
    let mut cur = Cursor::new(bytes.as_slice());
    let frame = Frame::decode(&mut cur)
        .expect("decode failed")
        .expect("partial frame in fixture");
    assert_eq!(frame.msg_type, MsgType::RequestHeaders);
    assert_eq!(frame.flags & flags::END_STREAM, 0);
    assert!(frame.payload.len() > 0);
    assert_eq!(frame.stream_id, 2);
}

#[test]
fn frame_decode_heartbeat_data_fixture() {
    let bytes = read_fixture("heartbeat_data.bin");
    let mut cur = Cursor::new(bytes.as_slice());
    let frame = Frame::decode(&mut cur).unwrap().unwrap();
    assert_eq!(frame.msg_type, MsgType::HeartbeatData);
    assert_eq!(frame.stream_id, 0);
}

#[test]
fn frame_encode_roundtrip_via_fixtures() {
    for name in [
        "request_headers.bin",
        "request_body.bin",
        "response_headers.bin",
        "response_body.bin",
        "stream_end.bin",
        "heartbeat_data.bin",
    ] {
        let bytes = read_fixture(name);
        let mut cur = Cursor::new(bytes.as_slice());
        let frame = Frame::decode(&mut cur).unwrap().unwrap();
        let mut out = BytesMut::new();
        frame.encode(&mut out);
        assert_eq!(&out[..], &bytes[..], "encode of {name} changed wire bytes");
    }
}

#[test]
fn frame_decode_partial_returns_none_without_advancing() {
    let bytes = vec![0u8; 5];
    let mut cur = Cursor::new(bytes.as_slice());
    let res = Frame::decode(&mut cur).unwrap();
    assert!(res.is_none());
    assert_eq!(cur.position(), 0);
}

#[test]
fn frame_constants_match_upstream_spec() {
    assert_eq!(HEADER_SIZE, 10);
    assert_eq!(flags::NONE, 0x00);
    assert_eq!(flags::END_STREAM, 0x01);
    assert_eq!(flags::GZIP_COMPRESSED, 0x02);
    assert_eq!(MsgType::HeartbeatData as u8, 0x13);
    assert_eq!(MsgType::HeartbeatAck as u8, 0x14);
}
