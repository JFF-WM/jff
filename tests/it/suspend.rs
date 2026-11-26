//! Tests for `jff_wm::primitives::suspend`.

use jff_wm::primitives::suspend::{behind_cover, suspended};

#[test]
fn a_window_under_a_cover_is_behind_it() {
    assert!(behind_cover(2, [(true, Some(1))]));
}

#[test]
fn the_covering_window_itself_is_not_behind() {
    assert!(!behind_cover(1, [(true, Some(1))]));
}

#[test]
fn no_cover_means_not_behind() {
    assert!(!behind_cover(2, [(true, None)]));
}

#[test]
fn a_window_on_two_monitors_is_behind_only_when_both_are_covered() {
    assert!(behind_cover(3, [(true, Some(1)), (true, Some(2))]));
    assert!(!behind_cover(3, [(true, Some(1)), (true, None)]));
}

#[test]
fn monitors_the_window_is_not_on_do_not_count() {
    assert!(behind_cover(3, [(false, None), (true, Some(1))]));
    assert!(!behind_cover(3, [(false, Some(1))]));
    assert!(!behind_cover::<u32>(3, []));
}

#[test]
fn hidden_or_covered_windows_are_suspended_once_they_have_shown_something() {
    assert!(suspended(false, true, false));
    assert!(suspended(true, true, true));
    assert!(!suspended(true, true, false));
}

#[test]
fn a_window_that_has_not_shown_anything_is_never_suspended() {
    assert!(!suspended(false, false, false));
    assert!(!suspended(true, false, true));
}
