//! When the cursor hides itself.

use std::time::{Duration, Instant};

use jff_wm::primitives::cursor_hide::CursorHide;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn nothing_hides_the_cursor_by_default() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.key_pressed();
    assert!(!hide.hidden());
    assert_eq!(hide.remaining(t0 + ms(60_000)), None);
    assert!(!hide.hidden());
}

#[test]
fn a_key_hides_it_until_the_pointer_moves() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(true, None);
    hide.key_pressed();
    assert!(hide.hidden());
    hide.key_pressed();
    assert!(hide.hidden());
    hide.pointer_activity(t0 + ms(5));
    assert!(!hide.hidden());
}

#[test]
fn resting_hides_it_after_the_time() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(false, Some(ms(1000)));
    assert_eq!(hide.remaining(t0 + ms(400)), Some(ms(600)));
    assert!(!hide.hidden());
    assert_eq!(hide.remaining(t0 + ms(1000)), None);
    assert!(hide.hidden());
    hide.pointer_activity(t0 + ms(1500));
    assert!(!hide.hidden());
    assert_eq!(hide.remaining(t0 + ms(1700)), Some(ms(800)));
}

#[test]
fn moving_the_pointer_pushes_the_time_back() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(false, Some(ms(1000)));
    hide.pointer_activity(t0 + ms(900));
    assert_eq!(hide.remaining(t0 + ms(1000)), Some(ms(900)));
    assert!(!hide.hidden());
}

#[test]
fn typing_does_not_count_as_the_pointer_resting_or_moving() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(true, Some(ms(1000)));
    hide.key_pressed();
    assert_eq!(hide.remaining(t0 + ms(500)), Some(ms(500)));
}

#[test]
fn turning_a_setting_off_shows_what_it_hid() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(true, Some(ms(100)));
    hide.key_pressed();
    assert!(hide.hidden());
    hide.configure(false, Some(ms(100)));
    assert!(!hide.hidden());
    assert_eq!(hide.remaining(t0 + ms(200)), None);
    assert!(hide.hidden());
    hide.configure(false, None);
    assert!(!hide.hidden());
}

#[test]
fn a_clock_that_runs_backwards_does_not_panic() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0 + ms(500));
    hide.configure(false, Some(ms(100)));
    assert_eq!(hide.remaining(t0), Some(ms(100)));
}
