//! Tests for `jff_wm::protocol::input_timestamps`: how a nanosecond time becomes the
//! protocol's three words.

use jff_wm::protocol::input_timestamps::split_timestamp;

#[test]
fn zero_is_all_zero() {
    assert_eq!(split_timestamp(0), (0, 0, 0));
}

#[test]
fn seconds_and_nanoseconds_are_split() {
    // 12 s and 345_678_901 ns
    assert_eq!(split_timestamp(12_345_678_901), (0, 12, 345_678_901));
    // one nanosecond short of a second stays in the nanosecond word
    assert_eq!(split_timestamp(999_999_999), (0, 0, 999_999_999));
    assert_eq!(split_timestamp(1_000_000_000), (0, 1, 0));
}

#[test]
fn seconds_past_32_bits_go_in_the_high_word() {
    // 2^32 s: the low word wraps to 0 and the high word becomes 1
    let ns = (1u64 << 32) * 1_000_000_000;
    assert_eq!(split_timestamp(ns), (1, 0, 0));
    assert_eq!(split_timestamp(ns + 7), (1, 0, 7));
}

#[test]
fn the_words_put_back_together_give_the_input() {
    for ns in [1u64, 42, 1_999_999_999, 86_400_000_000_000, u64::MAX / 3] {
        let (hi, lo, rest) = split_timestamp(ns);
        let secs = ((hi as u64) << 32) | lo as u64;
        assert_eq!(secs * 1_000_000_000 + rest as u64, ns);
    }
}
