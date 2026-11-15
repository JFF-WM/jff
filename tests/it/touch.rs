//! Fingers on a touchscreen, and which monitor the touchscreen covers.

use jff_wm::primitives::touch::{is_internal_panel, touch_output, Fingers};

type F = Fingers<u32, &'static str>;

#[test]
fn the_first_finger_is_the_primary_one() {
    let mut f = F::default();
    assert!(f.down(0));
    assert!(!f.down(1));
    assert!(f.is_primary(0));
    assert!(!f.is_primary(1));
    assert_eq!(f.len(), 2);
}

#[test]
fn a_second_finger_does_not_take_over_while_the_first_is_down() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    // a third and fourth, in any order, are not primary either
    assert!(!f.down(2));
    assert!(!f.down(3));
    assert!(f.is_primary(0));
}

#[test]
fn when_the_primary_lifts_the_next_finger_down_takes_over() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    assert!(f.up(0).is_some());
    // nobody drives the pointer until a finger touches down...
    assert!(!f.is_primary(1));
    // ...the next one does, even with the other still on the glass
    assert!(f.down(2));
    assert!(f.is_primary(2));
}

#[test]
fn the_first_touch_after_everything_lifted_is_primary_again() {
    let mut f = F::default();
    f.down(0);
    f.up(0);
    assert!(f.is_empty());
    assert!(f.down(0));
}

#[test]
fn lifting_a_finger_that_is_not_down_does_nothing() {
    let mut f = F::default();
    f.down(0);
    assert_eq!(f.up(7), None);
    assert_eq!(f.len(), 1);
    assert!(f.is_primary(0));
}

#[test]
fn lifting_a_finger_says_what_it_was_owed() {
    let mut f = F::default();
    f.down(4);
    {
        let finger = f.get_mut(4).unwrap();
        finger.forwarded = true;
        finger.pressed = true;
        finger.target = Some("window");
    }
    let finger = f.up(4).unwrap();
    assert_eq!(finger.slot, 4);
    assert!(finger.forwarded && finger.pressed);
    assert_eq!(finger.target, Some("window"));
    assert!(f.get(4).is_none());
}

#[test]
fn a_new_finger_owes_nothing() {
    let mut f = F::default();
    f.down(0);
    let finger = f.get(0).unwrap();
    assert!(!finger.forwarded);
    assert!(!finger.pressed);
    assert_eq!(finger.target, None);
}

#[test]
fn a_slot_that_touches_down_twice_starts_over() {
    // the release got lost: the slot must not be listed twice, or stay marked as owing things
    let mut f = F::default();
    f.down(0);
    f.get_mut(0).unwrap().pressed = true;
    assert!(f.down(0));
    assert_eq!(f.len(), 1);
    assert!(!f.get(0).unwrap().pressed);
    assert!(f.is_primary(0));
}

#[test]
fn a_stale_second_touch_of_a_secondary_slot_stays_secondary() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    assert!(!f.down(1));
    assert!(f.is_primary(0));
    assert_eq!(f.len(), 2);
}

#[test]
fn a_cancel_hands_back_every_finger_and_forgets_the_primary() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    f.get_mut(0).unwrap().pressed = true;
    let gone = f.clear();
    assert_eq!(gone.len(), 2);
    assert!(gone.iter().any(|g| g.slot == 0 && g.pressed));
    assert!(f.is_empty());
    assert!(!f.is_primary(0));
    assert!(f.down(5));
}

#[test]
fn a_grab_taking_the_touch_keeps_the_fingers_but_stops_forwarding() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    for slot in [0, 1] {
        let finger = f.get_mut(slot).unwrap();
        finger.forwarded = true;
        finger.target = Some("window");
    }
    f.get_mut(0).unwrap().pressed = true;
    f.detach_all();
    assert_eq!(f.len(), 2);
    for slot in [0, 1] {
        let finger = f.get(slot).unwrap();
        assert!(!finger.forwarded);
        assert_eq!(finger.target, None);
    }
    // the compositor's press is still held: its release is still owed
    assert!(f.get(0).unwrap().pressed);
    assert!(f.is_primary(0));
}

#[test]
fn builtin_panels_are_recognised_by_connector_name() {
    assert!(is_internal_panel("eDP-1"));
    assert!(is_internal_panel("eDP-2"));
    assert!(is_internal_panel("LVDS-1"));
    assert!(is_internal_panel("DSI-1"));
    assert!(is_internal_panel("edp-1"));
    assert!(!is_internal_panel("DP-1"));
    assert!(!is_internal_panel("HDMI-A-1"));
    assert!(!is_internal_panel("winit"));
    assert!(!is_internal_panel(""));
}

#[test]
fn a_touchscreen_covers_the_builtin_panel() {
    assert_eq!(touch_output(&["HDMI-A-1", "eDP-1", "DP-2"]), Some(1));
    assert_eq!(touch_output(&["eDP-1", "HDMI-A-1"]), Some(0));
}

#[test]
fn without_a_builtin_panel_a_touchscreen_covers_the_first_monitor() {
    assert_eq!(touch_output(&["DP-1", "HDMI-A-1"]), Some(0));
    assert_eq!(touch_output(&["winit"]), Some(0));
}

#[test]
fn without_a_monitor_there_is_nothing_to_cover() {
    let none: [&str; 0] = [];
    assert_eq!(touch_output(&none), None);
    let owned: Vec<String> = vec![];
    assert_eq!(touch_output(&owned), None);
}

#[test]
fn owned_names_work_too() {
    let names = vec!["DP-1".to_string(), "eDP-1".to_string()];
    assert_eq!(touch_output(&names), Some(1));
}
