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
