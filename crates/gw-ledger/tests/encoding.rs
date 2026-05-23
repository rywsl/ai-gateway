//! Tier-selection + round-trip unit tests for `gw_ledger::encoding`.

use gw_ledger::encoding::{pick_encoding, Encoding, TierThresholds};

const T: TierThresholds = TierThresholds {
    small_threshold_bytes: 1024,
    medium_threshold_bytes: 65_536,
};

#[test]
fn picks_raw_below_small_threshold() {
    assert_eq!(pick_encoding(0, &T), Encoding::Raw);
    assert_eq!(pick_encoding(1023, &T), Encoding::Raw);
}

#[test]
fn picks_lz4_at_small_threshold_inclusive() {
    assert_eq!(pick_encoding(1024, &T), Encoding::Lz4);
    assert_eq!(pick_encoding(65_535, &T), Encoding::Lz4);
}

#[test]
fn picks_zstd_at_medium_threshold_inclusive() {
    assert_eq!(pick_encoding(65_536, &T), Encoding::Zstd);
    assert_eq!(pick_encoding(8_388_608, &T), Encoding::Zstd);
}

#[test]
fn encoding_str_matches_spec() {
    assert_eq!(Encoding::Raw.as_str(), "raw+json");
    assert_eq!(Encoding::Lz4.as_str(), "lz4+json");
    assert_eq!(Encoding::Zstd.as_str(), "zstd+json");
}

use gw_ledger::encoding::{encode, CompressionCfg};

fn cfg() -> CompressionCfg {
    CompressionCfg {
        thresholds: T,
        zstd_level: 3,
    }
}

#[test]
fn encode_raw_passthrough() {
    let input = b"hello".to_vec();
    let (enc, out) = encode(input.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Raw);
    assert_eq!(out, input);
}

#[test]
fn encode_lz4_shrinks_repetitive() {
    let input = "abc".repeat(2_000); // 6 KiB, falls in lz4 tier; highly compressible
    let bytes = input.into_bytes();
    let (enc, out) = encode(bytes.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Lz4);
    assert!(
        out.len() < bytes.len() / 4,
        "lz4 should shrink repetitive bytes meaningfully"
    );
}

#[test]
fn encode_zstd_shrinks_large_repetitive() {
    let input = "xyz".repeat(50_000); // 150 KiB, zstd tier
    let bytes = input.into_bytes();
    let (enc, out) = encode(bytes.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Zstd);
    assert!(
        out.len() < bytes.len() / 10,
        "zstd should shrink very repetitive bytes hard"
    );
}

use gw_ledger::encoding::decode;

#[test]
fn round_trip_raw() {
    let input = b"under-one-kib".to_vec();
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Raw);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn round_trip_lz4() {
    let input = "lz4-tier-".repeat(200).into_bytes(); // ~ 1.8 KiB
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Lz4);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn round_trip_zstd() {
    let input = "zstd-tier-".repeat(20_000).into_bytes(); // ~ 200 KiB
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Zstd);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}
