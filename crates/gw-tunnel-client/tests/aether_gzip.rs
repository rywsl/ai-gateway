use bytes::Bytes;
use gw_tunnel_client::aether::gzip::{decompress, maybe_compress};

#[test]
fn leaves_small_payload_uncompressed() {
    let payload = Bytes::from_static(b"small");
    let (encoded, compressed) = maybe_compress(payload.clone()).unwrap();
    assert!(!compressed);
    assert_eq!(encoded, payload);
}

#[test]
fn compresses_repetitive_large_payload_and_roundtrips() {
    let payload = Bytes::from(vec![b'a'; 2048]);
    let (encoded, compressed) = maybe_compress(payload.clone()).unwrap();
    assert!(compressed);
    assert!(encoded.len() < payload.len());
    assert_eq!(decompress(&encoded).unwrap(), payload);
}
