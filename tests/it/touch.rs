//! Fingers on a touchscreen, and which monitor the touchscreen covers.

use jff_wm::config::Config;
use jff_wm::primitives::swipe::Swipe;
use jff_wm::primitives::touch::{
    is_internal_panel, touch_output, touch_output_mapped, Fingers, TouchGesture,
};

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

#[test]
fn a_mapped_touchscreen_covers_the_monitor_it_names() {
    let names = ["eDP-1", "DP-1", "HDMI-A-1"];
    assert_eq!(touch_output_mapped(&names, Some("HDMI-A-1")), Some(2));
    assert_eq!(touch_output_mapped(&names, Some("eDP-1")), Some(0));
}

#[test]
fn a_mapping_to_a_monitor_that_is_gone_falls_back_to_the_default() {
    assert_eq!(
        touch_output_mapped(&["HDMI-A-1", "eDP-1"], Some("DP-9")),
        Some(1)
    );
    assert_eq!(
        touch_output_mapped(&["DP-1", "DP-2"], Some("DP-9")),
        Some(0)
    );
    assert_eq!(touch_output_mapped(&["DP-1"], None), Some(0));
    let none: [&str; 0] = [];
    assert_eq!(touch_output_mapped(&none, Some("DP-1")), None);
}

#[test]
fn touchscreen_blocks_map_devices_to_monitors() {
    let config = Config::parse(
        r#"input {
            touchscreen { map-to-output "eDP-1" }
            touchscreen "ELAN Touchscreen" { map-to-output "DP-2" }
        }"#,
    )
    .unwrap();
    let input = config.input();
    assert_eq!(input.touch_output_for("ELAN Touchscreen"), Some("DP-2"));
    assert_eq!(input.touch_output_for("Other Touchscreen"), Some("eDP-1"));
}

#[test]
fn without_a_touchscreen_block_nothing_is_mapped() {
    let config = Config::parse("").unwrap();
    assert_eq!(config.input().touch_output_for("ELAN Touchscreen"), None);
}

#[test]
fn a_bad_touchscreen_block_is_refused() {
    assert!(Config::parse(r#"input { touchscreen { map-to "DP-1" } }"#).is_err());
    assert!(Config::parse(r#"input { touchscreen "a" "b" { map-to-output "DP-1" } }"#).is_err());
}

type G = TouchGesture<u32>;

fn three(at: [(f64, f64); 3]) -> Vec<(u32, (f64, f64))> {
    at.iter().enumerate().map(|(i, p)| (i as u32, *p)).collect()
}

const FLAT: [(f64, f64); 3] = [(100.0, 100.0), (200.0, 100.0), (300.0, 100.0)];

#[test]
fn a_gesture_needs_exactly_three_fingers() {
    let mut g = G::default();
    assert!(!g.begin(vec![(0, (0.0, 0.0)), (1, (1.0, 1.0))], true));
    let four = vec![
        (0, (0.0, 0.0)),
        (1, (0.0, 0.0)),
        (2, (0.0, 0.0)),
        (3, (0.0, 0.0)),
    ];
    assert!(!g.begin(four, true));
    assert!(!g.is_active());
    assert!(g.begin(three(FLAT), true));
    assert!(g.is_active());
}

#[test]
fn no_gesture_while_something_else_owns_the_input() {
    let mut g = G::default();
    assert!(!g.begin(three(FLAT), false));
    assert!(!g.is_active());
}

#[test]
fn three_fingers_moving_sideways_swipe() {
    let mut g = G::default();
    g.begin(three(FLAT), true);
    let mut seen = None;
    for step in 1..=10 {
        let dx = -(step as f64) * 10.0;
        for (slot, (x, y)) in three(FLAT) {
            if let Some(swipe) = g.moved(slot, (x + dx, y)) {
                seen = seen.or(Some(swipe));
            }
        }
    }
    assert_eq!(seen, Some(Swipe::Left));
}

#[test]
fn a_swipe_counts_once() {
    let mut g = G::default();
    g.begin(three(FLAT), true);
    let mut count = 0;
    for step in 1..=40 {
        for (slot, (x, y)) in three(FLAT) {
            if g.moved(slot, (x, y - step as f64 * 10.0)).is_some() {
                count += 1;
            }
        }
    }
    assert_eq!(count, 1);
}

#[test]
fn a_short_drag_is_not_a_swipe() {
    let mut g = G::default();
    g.begin(three(FLAT), true);
    for (slot, (x, y)) in three(FLAT) {
        assert_eq!(g.moved(slot, (x + 20.0, y)), None);
    }
}

#[test]
fn lifting_a_finger_does_not_jump_the_middle() {
    let mut g = G::default();
    g.begin(three(FLAT), true);
    g.lift(0);
    // the middle moved from x=200 to x=250 by itself, that must not count
    assert_eq!(g.moved(1, (200.0, 100.0)), None);
    assert_eq!(g.moved(2, (300.0, 100.0)), None);
}

#[test]
fn the_gesture_forgets_everything_at_its_end() {
    let mut g = G::default();
    g.begin(three(FLAT), true);
    g.end();
    assert!(!g.is_active());
    assert_eq!(g.moved(0, (0.0, 0.0)), None);
    assert!(g.begin(three(FLAT), true));
}

#[test]
fn settling_every_finger_hands_back_what_each_was_owed() {
    let mut f = F::default();
    f.down(0);
    f.down(1);
    {
        let finger = f.get_mut(0).unwrap();
        finger.forwarded = true;
        finger.pressed = true;
        finger.target = Some("window");
    }
    f.get_mut(1).unwrap().emulated = true;
    let owed = f.settle_all();
    assert_eq!(owed.len(), 2);
    assert!(owed[0].forwarded && owed[0].pressed);
    assert!(owed[1].emulated);
    // nothing is owed any more, the fingers are still down
    assert_eq!(f.len(), 2);
    for slot in [0, 1] {
        let finger = f.get(slot).unwrap();
        assert!(!finger.forwarded && !finger.pressed && !finger.emulated);
        assert_eq!(finger.target, None);
    }
}

#[test]
fn fingers_remember_where_they_are() {
    let mut f = F::default();
    f.down(3);
    f.get_mut(3).unwrap().at = (12.0, 34.0);
    assert_eq!(f.positions(), vec![(3, (12.0, 34.0))]);
}
