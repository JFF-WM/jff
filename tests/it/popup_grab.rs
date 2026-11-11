//! When a menu's grab has to end because the keyboard went somewhere else.

use jff_wm::focus::{popup_grab_outlived, seat_grab_is_foreign, KeyboardFocus};

#[test]
fn panel_menu_opened_while_the_overview_is_open_keeps_its_grab() {
    // the overview already had the keyboard: nothing moved, the menu stays
    // even for a panel that takes keys: the overview has no surface, so the keyboard is
    // always "elsewhere" and that must not close the menu
    assert!(!popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Overview,
        false,
        || true
    ));
    assert!(!popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Screenshot,
        false,
        || true
    ));
    assert!(!popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Overview,
        false,
        || false
    ));
}

#[test]
fn panel_menu_ends_when_the_keyboard_goes_to_the_overview() {
    assert!(popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Overview,
        true,
        || false
    ));
    assert!(popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Screenshot,
        true,
        || false
    ));
}

#[test]
fn panel_menu_follows_the_keyboard_only_for_a_panel_that_takes_keys() {
    assert!(popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Nobody,
        false,
        || true
    ));
    assert!(!popup_grab_outlived(
        false,
        true,
        &KeyboardFocus::Nobody,
        false,
        || false
    ));
    // the keyboard is on the menu's own panel
    assert!(!popup_grab_outlived(
        false,
        false,
        &KeyboardFocus::Nobody,
        false,
        || true
    ));
}

#[test]
fn window_menu_ends_as_soon_as_its_window_loses_the_keyboard() {
    assert!(popup_grab_outlived(
        true,
        true,
        &KeyboardFocus::Overview,
        false,
        || false
    ));
    assert!(!popup_grab_outlived(
        true,
        false,
        &KeyboardFocus::Nobody,
        false,
        || false
    ));
}

#[test]
fn a_menu_may_replace_the_overviews_pointer_grab() {
    // the request's serial is the click's, not the one the overview grabbed with
    assert!(!seat_grab_is_foreign(true, false, false, true));
    // another grab (a move, a resize) is still a stranger
    assert!(seat_grab_is_foreign(true, false, false, false));
}

#[test]
fn a_menu_grab_request_matching_the_current_grab_is_not_foreign() {
    assert!(!seat_grab_is_foreign(false, false, false, false));
    assert!(!seat_grab_is_foreign(true, true, false, false));
    assert!(!seat_grab_is_foreign(true, false, true, false));
}
