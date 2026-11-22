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

#[test]
fn resting_under_a_fullscreen_window_hides_it_only_there() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.set_fullscreen_after(Some(ms(5000)));
    assert_eq!(hide.after(), Some(ms(5000)));
    assert_eq!(hide.remaining(t0 + ms(2000)), Some(ms(3000)));
    assert!(!hide.hidden_in(true));
    assert_eq!(hide.remaining(t0 + ms(5000)), None);
    // not fullscreen (or the compositor needs the cursor): still drawn
    assert!(!hide.hidden_in(false));
    assert!(!hide.hidden());
    assert!(hide.hidden_in(true));
}

#[test]
fn any_pointer_activity_shows_it_and_restarts_the_time() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.set_fullscreen_after(Some(ms(1000)));
    hide.remaining(t0 + ms(1500));
    assert!(hide.hidden_in(true));
    hide.pointer_activity(t0 + ms(1600));
    assert!(!hide.hidden_in(true));
    assert_eq!(hide.remaining(t0 + ms(2000)), Some(ms(600)));
}

#[test]
fn both_rest_times_fire_in_order() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.configure(false, Some(ms(3000)));
    hide.set_fullscreen_after(Some(ms(1000)));
    // the shorter one arms the timer, the longer one is found out when it fires
    assert_eq!(hide.after(), Some(ms(1000)));
    assert_eq!(hide.remaining(t0 + ms(1000)), Some(ms(2000)));
    assert!(hide.hidden_in(true));
    assert!(!hide.hidden_in(false));
    assert_eq!(hide.remaining(t0 + ms(3000)), None);
    assert!(hide.hidden_in(false));
}

#[test]
fn turning_the_fullscreen_time_off_shows_the_cursor() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    hide.set_fullscreen_after(Some(ms(100)));
    hide.remaining(t0 + ms(200));
    assert!(hide.hidden_in(true));
    hide.set_fullscreen_after(None);
    assert!(!hide.hidden_in(true));
    assert_eq!(hide.fullscreen_after(), None);
}

#[test]
fn the_fullscreen_time_is_off_by_default() {
    let t0 = Instant::now();
    let mut hide = CursorHide::new(t0);
    assert_eq!(hide.after(), None);
    assert_eq!(hide.remaining(t0 + ms(60_000)), None);
    assert!(!hide.hidden_in(true));
}
