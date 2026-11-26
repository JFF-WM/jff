//! Tests for `jff_wm::primitives::preferred_buffer`.

use jff_wm::primitives::preferred_buffer::follow_monitor;

#[test]
fn the_only_monitor_it_is_shown_on_is_followed() {
    assert_eq!(
        follow_monitor([(false, 2.0), (true, 1.0)], Some(0)),
        Some(1)
    );
}

#[test]
fn the_highest_scale_wins_between_two_monitors() {
    assert_eq!(follow_monitor([(true, 1.0), (true, 2.0)], None), Some(1));
}

#[test]
fn equal_scales_keep_the_first() {
    assert_eq!(follow_monitor([(true, 1.5), (true, 1.5)], None), Some(0));
}

#[test]
fn a_window_shown_nowhere_follows_its_home_monitor() {
    assert_eq!(
        follow_monitor([(false, 1.0), (false, 2.0)], Some(1)),
        Some(1)
    );
    assert_eq!(follow_monitor([(false, 1.0)], None), None);
    assert_eq!(follow_monitor([], Some(0)), Some(0));
}
