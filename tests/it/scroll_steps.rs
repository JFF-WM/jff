//! Scroll events becoming workspace steps.

use jff_wm::primitives::scroll_steps::ScrollSteps;

#[test]
fn a_wheel_notch_is_one_step_either_way() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.wheel(120.0), 1);
    assert_eq!(s.wheel(-120.0), -1);
}

#[test]
fn fine_wheel_events_add_up_to_a_notch() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.wheel(60.0), 0);
    assert_eq!(s.wheel(60.0), 1);
    assert_eq!(s.wheel(30.0), 0);
}

#[test]
fn a_fast_wheel_gives_several_steps() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.wheel(360.0), 3);
    assert_eq!(s.wheel(-240.0), -2);
}

#[test]
fn turning_the_wheel_around_drops_the_half_notch() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.wheel(90.0), 0);
    // the 90 down is dropped, so 90 up and 30 more make exactly one notch up
    assert_eq!(s.wheel(-90.0), 0);
    assert_eq!(s.wheel(-30.0), -1);
}

#[test]
fn a_touchpad_gesture_is_one_step_however_long() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.touch(30.0, false), 0);
    assert_eq!(s.touch(30.0, false), 1);
    assert_eq!(s.touch(200.0, false), 0);
    assert_eq!(s.touch(0.0, true), 0);
    assert_eq!(s.touch(-50.0, false), -1);
}

#[test]
fn reset_arms_the_touchpad_again() {
    let mut s = ScrollSteps::default();
    assert_eq!(s.touch(50.0, false), 1);
    s.reset();
    assert_eq!(s.touch(50.0, false), 1);
}
