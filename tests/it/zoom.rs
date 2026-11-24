//! The screen magnifier: levels, animation and the way the shown part follows the pointer.

use std::time::{Duration, Instant};

use jff_wm::primitives::zoom::{Pan, Tracking, View, Zoom, MAX_LEVEL};

const SIZE: (f64, f64) = (1920.0, 1080.0);
const NO_TIME: Duration = Duration::ZERO;

fn zoomed(level: f64) -> (Zoom, Instant) {
    let now = Instant::now();
    let mut zoom = Zoom::new(now);
    zoom.set_target(now, level, NO_TIME);
    (zoom, now)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn a_new_zoom_is_not_active() {
    let now = Instant::now();
    let zoom = Zoom::new(now);
    assert!(!zoom.active(now));
    assert!(!zoom.animating(now));
    assert_eq!(zoom.level(now), 1.0);
}

#[test]
fn steps_multiply_the_level_and_stop_at_the_ends() {
    let now = Instant::now();
    let mut zoom = Zoom::new(now);
    zoom.zoom_in(now, 2.0, 8.0, NO_TIME);
    zoom.zoom_in(now, 2.0, 8.0, NO_TIME);
    assert_eq!(zoom.target(), 4.0);
    zoom.zoom_in(now, 2.0, 8.0, NO_TIME);
    zoom.zoom_in(now, 2.0, 8.0, NO_TIME);
    assert_eq!(zoom.target(), 8.0, "stops at max");
    for _ in 0..10 {
        zoom.zoom_out(now, 2.0, NO_TIME);
    }
    assert_eq!(zoom.target(), 1.0, "never below no zoom");
    assert!(!zoom.active(now));
}

#[test]
fn max_cannot_go_past_the_hard_limit() {
    let now = Instant::now();
    let mut zoom = Zoom::new(now);
    for _ in 0..100 {
        zoom.zoom_in(now, 2.0, 1000.0, NO_TIME);
    }
    assert_eq!(zoom.target(), MAX_LEVEL);
}

#[test]
fn reset_goes_back_to_no_zoom() {
    let (mut zoom, now) = zoomed(5.0);
    zoom.reset(now, NO_TIME);
    assert_eq!(zoom.level(now), 1.0);
    assert!(!zoom.active(now));
}

#[test]
fn a_change_runs_over_its_duration() {
    let now = Instant::now();
    let mut zoom = Zoom::new(now);
    let duration = Duration::from_millis(200);
    zoom.set_target(now, 4.0, duration);
    assert!(zoom.animating(now));
    assert!(close(zoom.level(now), 1.0));

    let middle = zoom.level(now + duration / 2);
    assert!(middle > 1.0 && middle < 4.0);
    // slows down towards the end: more than half of the way in half of the time
    assert!(middle > 2.0);

    let end = now + duration;
    assert!(!zoom.animating(end));
    assert!(close(zoom.level(end), 4.0));
}

#[test]
fn a_change_started_midway_starts_from_where_the_level_is() {
    let now = Instant::now();
    let mut zoom = Zoom::new(now);
    let duration = Duration::from_millis(200);
    zoom.set_target(now, 4.0, duration);
    let later = now + duration / 2;
    let here = zoom.level(later);
    zoom.set_target(later, 1.0, duration);
    assert!(close(zoom.level(later), here));
    assert!(zoom.active(later));
    assert!(!zoom.active(later + duration));
}

#[test]
fn proportional_keeps_the_pointer_over_what_it_points_at() {
    let (mut zoom, now) = zoomed(2.0);
    for pointer in [(0.0, 0.0), (960.0, 540.0), (1919.0, 1079.0), (300.0, 800.0)] {
        let view = zoom.view(now, Tracking::Proportional, pointer, SIZE);
        let (x, y) = view.to_screen(pointer);
        assert!(close(x, pointer.0) && close(y, pointer.1), "{pointer:?}");
    }
}

#[test]
fn proportional_shows_the_corners_at_the_corners() {
    let (mut zoom, now) = zoomed(4.0);
    let top_left = zoom.view(now, Tracking::Proportional, (0.0, 0.0), SIZE);
    assert_eq!(top_left.origin, (0.0, 0.0));
    let bottom_right = zoom.view(now, Tracking::Proportional, SIZE, SIZE);
    let (x, y, w, h) = bottom_right.rect();
    assert!(close(x + w, SIZE.0) && close(y + h, SIZE.1));
}

#[test]
fn centered_puts_the_pointer_in_the_middle() {
    let (mut zoom, now) = zoomed(2.0);
    let view = zoom.view(now, Tracking::Centered, (1000.0, 500.0), SIZE);
    let (x, y) = view.to_screen((1000.0, 500.0));
    assert!(close(x, SIZE.0 / 2.0) && close(y, SIZE.1 / 2.0));
}

#[test]
fn centered_stops_at_the_edges_of_the_screen() {
    let (mut zoom, now) = zoomed(2.0);
    let view = zoom.view(now, Tracking::Centered, (10.0, 10.0), SIZE);
    assert_eq!(view.origin, (0.0, 0.0));
    let view = zoom.view(now, Tracking::Centered, (1915.0, 1075.0), SIZE);
    let (x, y, w, h) = view.rect();
    assert!(close(x + w, SIZE.0) && close(y + h, SIZE.1));
}

#[test]
fn push_leaves_the_view_alone_until_the_pointer_reaches_its_edge() {
    let (mut zoom, now) = zoomed(2.0);
    let first = zoom.view(now, Tracking::Push, (480.0, 270.0), SIZE);
    let inside = zoom.view(now, Tracking::Push, (500.0, 300.0), SIZE);
    assert_eq!(first.origin, inside.origin);

    // past the right edge of what is shown: the view goes along, the pointer stays on screen
    let (x, _, w, _) = inside.rect();
    let beyond = x + w + 100.0;
    let pushed = zoom.view(now, Tracking::Push, (beyond, 300.0), SIZE);
    assert!(pushed.origin.0 > inside.origin.0);
    let (shown_x, _) = pushed.to_screen((beyond, 300.0));
    assert!(shown_x > 0.0 && shown_x < SIZE.0);
}

#[test]
fn push_goes_back_the_same_way() {
    let (mut zoom, now) = zoomed(2.0);
    zoom.view(now, Tracking::Proportional, (1900.0, 1000.0), SIZE);
    let view = zoom.view(now, Tracking::Push, (900.0, 500.0), SIZE);
    let pushed = zoom.view(now, Tracking::Push, (view.origin.0 - 50.0, 500.0), SIZE);
    assert!(pushed.origin.0 < view.origin.0);
}

#[test]
fn disabled_does_not_follow_the_pointer() {
    let (mut zoom, now) = zoomed(2.0);
    let a = zoom.view(now, Tracking::Disabled, (100.0, 100.0), SIZE);
    let b = zoom.view(now, Tracking::Disabled, (1800.0, 900.0), SIZE);
    assert_eq!(a.origin, b.origin);
}

#[test]
fn a_pan_moves_the_view_a_tenth_of_what_is_shown() {
    let (mut zoom, now) = zoomed(2.0);
    // from the middle, so there is room to go either way
    zoom.view(now, Tracking::Proportional, (960.0, 540.0), SIZE);
    let before = zoom.view(now, Tracking::Disabled, (960.0, 540.0), SIZE);
    zoom.pan(now, Pan::Right, SIZE);
    let after = zoom.view(now, Tracking::Disabled, (960.0, 540.0), SIZE);
    assert!(close(after.origin.0 - before.origin.0, 96.0));
    assert_eq!(after.origin.1, before.origin.1);

    zoom.pan(now, Pan::Down, SIZE);
    zoom.pan(now, Pan::Left, SIZE);
    zoom.pan(now, Pan::Up, SIZE);
    let back = zoom.view(now, Tracking::Disabled, (960.0, 540.0), SIZE);
    assert!(close(back.origin.0, before.origin.0));
    assert!(close(back.origin.1, before.origin.1));
}

#[test]
fn a_pan_stops_at_the_edge_of_the_screen() {
    let (mut zoom, now) = zoomed(2.0);
    zoom.view(now, Tracking::Proportional, (0.0, 0.0), SIZE);
    for _ in 0..50 {
        zoom.pan(now, Pan::Left, SIZE);
        zoom.pan(now, Pan::Up, SIZE);
    }
    let view = zoom.view(now, Tracking::Disabled, (0.0, 0.0), SIZE);
    assert_eq!(view.origin, (0.0, 0.0));
    for _ in 0..50 {
        zoom.pan(now, Pan::Right, SIZE);
        zoom.pan(now, Pan::Down, SIZE);
    }
    let view = zoom.view(now, Tracking::Disabled, (0.0, 0.0), SIZE);
    let (x, y, w, h) = view.rect();
    assert!(close(x + w, SIZE.0) && close(y + h, SIZE.1));
}

#[test]
fn a_new_zoom_starts_from_the_corner_not_from_the_last_one() {
    let (mut zoom, now) = zoomed(2.0);
    zoom.view(now, Tracking::Proportional, (1900.0, 1000.0), SIZE);
    zoom.reset(now, NO_TIME);
    zoom.set_target(now, 2.0, NO_TIME);
    let view = zoom.view(now, Tracking::Disabled, (0.0, 0.0), SIZE);
    assert_eq!(view.origin, (0.0, 0.0));
}

#[test]
fn changing_the_level_keeps_what_is_under_the_pointer_in_place() {
    let (mut zoom, now) = zoomed(2.0);
    let pointer = (700.0, 300.0);
    zoom.view(now, Tracking::Push, pointer, SIZE);
    let before: View = zoom.view(now, Tracking::Push, pointer, SIZE);
    let at = before.to_screen(pointer);
    zoom.set_target(now, 3.0, NO_TIME);
    let after = zoom.view(now, Tracking::Disabled, pointer, SIZE);
    let now_at = after.to_screen(pointer);
    assert!(close(at.0, now_at.0) && close(at.1, now_at.1));
    assert!(close(after.level, 3.0));
}

#[test]
fn a_view_maps_points_both_ways() {
    let view = View {
        level: 2.0,
        origin: (100.0, 50.0),
        size: SIZE,
    };
    let p = (400.0, 300.0);
    let there = view.to_screen(p);
    let back = view.from_screen(there);
    assert!(close(back.0, p.0) && close(back.1, p.1));
    assert_eq!(view.shown(), (960.0, 540.0));
}
