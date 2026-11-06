//! Tests for `jff_wm::primitives::layout`.

use jff_wm::primitives::layout::{
    arrange, bounds, clamp_to_rects, fit_into, intersects, nearest, reachable, Rect,
};

const A: Rect = (0, 0, 1920, 1080);
const B: Rect = (1920, 0, 1280, 1024);

#[test]
fn unplaced_monitors_go_left_to_right() {
    let sizes = [(1920, 1080), (1280, 1024), (800, 600)];
    assert_eq!(
        arrange(&sizes, &[None, None, None]),
        [(0, 0), (1920, 0), (3200, 0)]
    );
}

#[test]
fn fixed_positions_are_kept_and_others_go_after_them() {
    let sizes = [(1920, 1080), (1280, 1024)];
    assert_eq!(arrange(&sizes, &[Some((0, 0)), None]), [(0, 0), (1920, 0)]);
    assert_eq!(arrange(&sizes, &[None, Some((0, 0))]), [(1280, 0), (0, 0)]);
}

#[test]
fn bounds_cover_all_monitors() {
    assert_eq!(bounds(&[A, B]), (0, 0, 3200, 1080));
    assert_eq!(bounds(&[]), (0, 0, 0, 0));
}

#[test]
fn pointer_crosses_between_monitors() {
    assert_eq!(clamp_to_rects((1925.0, 500.0), &[A, B]), (1925.0, 500.0));
}

#[test]
fn pointer_is_pulled_out_of_dead_space() {
    assert_eq!(clamp_to_rects((2500.0, 1070.0), &[A, B]), (2500.0, 1023.0));
    assert_eq!(clamp_to_rects((-50.0, -5.0), &[A, B]), (0.0, 0.0));
    assert_eq!(clamp_to_rects((10.0, 10.0), &[]), (10.0, 10.0));
}

#[test]
fn nearest_prefers_the_containing_monitor() {
    assert_eq!(nearest((2000.0, 10.0), &[A, B]), Some(1));
    assert_eq!(nearest((5000.0, 10.0), &[A, B]), Some(1));
    assert_eq!(nearest((0.0, 0.0), &[]), None);
}

#[test]
fn windows_off_every_monitor_are_unreachable() {
    assert!(reachable((100, 100), (400, 300), &[A, B]));
    assert!(!reachable((4000, 100), (400, 300), &[A, B]));
    assert!(!reachable((100, 2000), (400, 300), &[A, B]));
}

#[test]
fn fit_into_moves_only_as_much_as_needed() {
    assert_eq!(fit_into((2500, 900), (400, 300), B), (2500, 724));
    assert_eq!(fit_into((4000, 100), (400, 300), A), (1520, 100));
    assert_eq!(fit_into((10, 10), (5000, 5000), A), (0, 0));
}

#[test]
fn rects_intersect_only_with_shared_pixels() {
    assert!(intersects(A, (1900, 0, 100, 100)));
    assert!(!intersects(A, B));
}
