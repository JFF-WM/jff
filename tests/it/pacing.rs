//! Tests for `jff_wm::app::pacing`.

use jff_wm::app::pacing::LastCallback;

#[test]
fn a_surface_gets_one_callback_per_cycle() {
    let last = LastCallback::default();
    assert!(last.first_in_cycle("DP-1", 7));
    assert!(!last.first_in_cycle("DP-1", 7));
    assert!(!last.first_in_cycle("DP-1", 7));
}

#[test]
fn the_next_cycle_sends_again() {
    let last = LastCallback::default();
    assert!(last.first_in_cycle("DP-1", 7));
    assert!(last.first_in_cycle("DP-1", 8));
    assert!(!last.first_in_cycle("DP-1", 8));
}

#[test]
fn another_monitor_has_its_own_cycle() {
    let last = LastCallback::default();
    assert!(last.first_in_cycle("DP-1", 7));
    assert!(last.first_in_cycle("HDMI-A-2", 7));
}

#[test]
fn a_wrapped_sequence_counts_as_a_new_cycle() {
    let last = LastCallback::default();
    assert!(last.first_in_cycle("DP-1", u32::MAX));
    assert!(last.first_in_cycle("DP-1", 0));
}
