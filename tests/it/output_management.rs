//! Tests for the adaptive sync rules of `jff_wm::protocol::output_management`.

use jff_wm::config::Vrr;
use jff_wm::protocol::output_management::{adaptive_sync_enabled, vrr_for_request};

#[test]
fn fullscreen_and_always_show_as_enabled() {
    assert!(adaptive_sync_enabled(Vrr::Fullscreen, Some(true)));
    assert!(adaptive_sync_enabled(Vrr::Always, Some(true)));
    assert!(!adaptive_sync_enabled(Vrr::Off, Some(true)));
}

#[test]
fn a_monitor_that_cannot_do_it_shows_as_disabled() {
    assert!(!adaptive_sync_enabled(Vrr::Always, Some(false)));
    // A dark monitor says nothing either way, the setting counts.
    assert!(adaptive_sync_enabled(Vrr::Always, None));
}

#[test]
fn sending_back_what_the_head_showed_changes_nothing() {
    // The default is `fullscreen`, which shows as enabled: a client that returns "enabled" must
    // not turn it into `always`.
    assert_eq!(
        vrr_for_request(Vrr::Fullscreen, Some(true), true),
        Some(Vrr::Fullscreen)
    );
    assert_eq!(vrr_for_request(Vrr::Off, Some(true), false), Some(Vrr::Off));
}

#[test]
fn asking_for_a_change_sets_always_or_off() {
    assert_eq!(
        vrr_for_request(Vrr::Off, Some(true), true),
        Some(Vrr::Always)
    );
    assert_eq!(
        vrr_for_request(Vrr::Fullscreen, Some(true), false),
        Some(Vrr::Off)
    );
    assert_eq!(vrr_for_request(Vrr::Always, None, false), Some(Vrr::Off));
}

#[test]
fn enabling_it_on_a_monitor_that_cannot_do_it_is_refused() {
    assert_eq!(vrr_for_request(Vrr::Off, Some(false), true), None);
    // Asking for it off on such a monitor is fine, it already is.
    assert_eq!(
        vrr_for_request(Vrr::Off, Some(false), false),
        Some(Vrr::Off)
    );
}
