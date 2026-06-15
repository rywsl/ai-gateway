use gw_tunnel_client::aether::frame::{Frame, MsgType};
use gw_tunnel_client::aether::heartbeat::{HeartbeatAck, HeartbeatData};
use std::io::Cursor;

fn heartbeat_payload() -> Vec<u8> {
    let bytes = std::fs::read("tests/fixtures/aether/heartbeat_data.bin").unwrap();
    let mut cur = Cursor::new(bytes.as_slice());
    let frame = Frame::decode(&mut cur).unwrap().unwrap();
    assert_eq!(frame.msg_type, MsgType::HeartbeatData);
    frame.payload.to_vec()
}

#[test]
fn heartbeat_data_deserializes_flat_proxy_metrics() {
    let data: HeartbeatData = serde_json::from_slice(&heartbeat_payload()).unwrap();
    assert_eq!(data.node_id, "capture-node-1");
    assert_eq!(data.heartbeat_id, 1);
    assert_eq!(data.active_connections, 1);
    assert_eq!(data.total_requests, 0);
    assert_eq!(data.failed_requests, 0);
    assert_eq!(data.dns_failures, 0);
    assert_eq!(data.stream_errors, 0);
    assert_eq!(data.proxy_metadata.version, "0.2.5");
}

#[test]
fn heartbeat_ack_serializes_only_heartbeat_id() {
    let json = serde_json::to_value(HeartbeatAck { heartbeat_id: 7 }).unwrap();
    assert_eq!(json, serde_json::json!({ "heartbeat_id": 7 }));
}
