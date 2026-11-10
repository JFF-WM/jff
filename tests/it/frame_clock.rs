//! Tests for `jff_wm::primitives::frame_clock`.

use std::time::Duration;

use jff_wm::primitives::frame_clock::{next_presentation_time, wait_until, MIN_WAIT};

const REFRESH: Duration = Duration::from_millis(16);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn no_vblank_seen_is_one_refresh_away() {
    assert_eq!(next_presentation_time(None, REFRESH, ms(100)), ms(116));
}

#[test]
fn right_after_a_vblank_is_the_next_one() {
    assert_eq!(
        next_presentation_time(Some(ms(1000)), REFRESH, ms(1000)),
        ms(1016)
    );
    assert_eq!(
        next_presentation_time(Some(ms(1000)), REFRESH, ms(1001)),
        ms(1016)
    );
}

#[test]
fn stays_on_the_grid_after_missed_vblanks() {
    // 1000 + 3 × 16 = 1048 is behind now, so the next one is 1064
    assert_eq!(
        next_presentation_time(Some(ms(1000)), REFRESH, ms(1050)),
        ms(1064)
    );
}

#[test]
fn exactly_on_a_grid_line_waits_for_the_following_one() {
    assert_eq!(
        next_presentation_time(Some(ms(1000)), REFRESH, ms(1032)),
        ms(1048)
    );
}

#[test]
fn a_clock_that_went_back_still_gives_a_future_time() {
    assert_eq!(
        next_presentation_time(Some(ms(1000)), REFRESH, ms(990)),
        ms(1016)
    );
}

#[test]
fn zero_refresh_does_not_divide_by_zero() {
    let next = next_presentation_time(Some(ms(1000)), Duration::ZERO, ms(1000));
    assert!(next >= ms(1000));
}

#[test]
fn wait_is_never_shorter_than_the_minimum() {
    assert_eq!(wait_until(ms(100), ms(90)), ms(10));
    assert_eq!(wait_until(ms(100), ms(100)), MIN_WAIT);
    assert_eq!(wait_until(ms(100), ms(200)), MIN_WAIT);
}
