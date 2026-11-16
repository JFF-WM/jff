//! Touchpad swipes jff keeps for itself.

use jff_wm::primitives::swipe::{Swipe, SwipeTracker};

#[test]
fn only_three_fingers_are_kept() {
    let mut s = SwipeTracker::default();
    assert!(s.begin(3, true));
    assert!(!s.begin(2, true));
    assert!(!s.begin(4, true));
}

#[test]
fn nothing_is_kept_while_something_else_owns_the_input() {
    let mut s = SwipeTracker::default();
    assert!(!s.begin(3, false));
    assert_eq!(s.update(-500.0, 0.0), None);
}

#[test]
fn a_short_swipe_is_not_counted() {
    let mut s = SwipeTracker::default();
    s.begin(3, true);
    assert_eq!(s.update(-30.0, 5.0), None);
    assert_eq!(s.update(-30.0, 5.0), None);
}

#[test]
fn small_moves_add_up_to_a_swipe() {
    let mut s = SwipeTracker::default();
    s.begin(3, true);
    assert_eq!(s.update(-30.0, 0.0), None);
    assert_eq!(s.update(-30.0, 0.0), None);
    assert_eq!(s.update(-30.0, 0.0), Some(Swipe::Left));
}

#[test]
fn the_main_direction_wins() {
    let mut s = SwipeTracker::default();
    s.begin(3, true);
    assert_eq!(s.update(60.0, -90.0), Some(Swipe::Up));
    s.end();
    s.begin(3, true);
    assert_eq!(s.update(-20.0, 100.0), Some(Swipe::Down));
    s.end();
    s.begin(3, true);
    assert_eq!(s.update(100.0, 20.0), Some(Swipe::Right));
}

#[test]
fn a_swipe_counts_once() {
    let mut s = SwipeTracker::default();
    s.begin(3, true);
    assert_eq!(s.update(-100.0, 0.0), Some(Swipe::Left));
    assert_eq!(s.update(-100.0, 0.0), None);
}

#[test]
fn the_next_swipe_starts_from_nothing() {
    let mut s = SwipeTracker::default();
    s.begin(3, true);
    assert_eq!(s.update(-100.0, 0.0), Some(Swipe::Left));
    assert!(s.end());
    s.begin(3, true);
    assert_eq!(s.update(-50.0, 0.0), None);
}

#[test]
fn ending_tells_whether_the_swipe_was_kept() {
    let mut s = SwipeTracker::default();
    s.begin(4, true);
    assert!(!s.end());
    s.begin(3, true);
    assert!(s.end());
}
