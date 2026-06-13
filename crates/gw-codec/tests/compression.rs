use bytes::Bytes;
use gw_codec::compression::{decode, encode, negotiate_outbound, parse_content_encoding, Encoding};

// Generous cap for round-trip tests where input data is small and the cap
// only needs to be larger than the plaintext payload.
const CAP: usize = 64 * 1024;

#[test]
fn gzip_roundtrip() {
    let data = b"hello compress world".repeat(64);
    let enc = encode(Encoding::Gzip, &data).unwrap();
    let dec = decode(Encoding::Gzip, &enc, CAP).unwrap();
    assert_eq!(&dec[..], &data[..]);
}

#[test]
fn brotli_roundtrip() {
    let data = b"hello compress world".repeat(64);
    let enc = encode(Encoding::Br, &data).unwrap();
    let dec = decode(Encoding::Br, &enc, CAP).unwrap();
    assert_eq!(&dec[..], &data[..]);
}

#[test]
fn identity_passthrough() {
    let data = Bytes::from_static(b"raw");
    let enc = encode(Encoding::Identity, &data).unwrap();
    assert_eq!(&enc[..], b"raw");
    let dec = decode(Encoding::Identity, &enc, CAP).unwrap();
    assert_eq!(&dec[..], b"raw");
}

#[test]
fn negotiate_prefers_br_over_gzip_when_both_listed() {
    assert_eq!(negotiate_outbound("gzip, br"), Encoding::Br);
    assert_eq!(negotiate_outbound("gzip"), Encoding::Gzip);
    assert_eq!(negotiate_outbound("identity"), Encoding::Identity);
    assert_eq!(negotiate_outbound("zstd"), Encoding::Identity); // v1 不接受
    assert_eq!(negotiate_outbound(""), Encoding::Identity);
}

#[test]
fn parse_content_encoding_cases() {
    assert_eq!(parse_content_encoding(Some("gzip")), Encoding::Gzip);
    assert_eq!(parse_content_encoding(Some("br")), Encoding::Br);
    assert_eq!(parse_content_encoding(Some("identity")), Encoding::Identity);
    assert_eq!(parse_content_encoding(Some("GZIP")), Encoding::Gzip); // case-insensitive
    assert_eq!(parse_content_encoding(None), Encoding::Identity);
}

#[test]
fn empty_body_roundtrip() {
    for e in [Encoding::Identity, Encoding::Gzip, Encoding::Br] {
        let enc = encode(e, b"").unwrap();
        let dec = decode(e, &enc, CAP).unwrap();
        assert!(dec.is_empty());
    }
}

#[test]
fn malformed_gzip_returns_error() {
    let r = decode(Encoding::Gzip, b"not gzip data", CAP);
    assert!(r.is_err());
}
