//! Tests for `jff_wm::primitives::layout`.

use jff_wm::primitives::layout::{
    arrange, bounds, clamp_to_rects, fit_into, hot_corner_barrier, hot_corner_rect, inside_any_corner, intersects,
    nearest, reachable, Rect,
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

#[test]
fn hot_corner_is_a_square_at_the_top_left() {
    assert_eq!(hot_corner_rect(A, 4), (0, 0, 4, 4));
    assert_eq!(hot_corner_rect(B, 4), (1920, 0, 4, 4));
}

#[test]
fn inside_any_corner_checks_every_monitor() {
    let rects = [A, B];
    assert!(inside_any_corner((0.0, 0.0), &rects, 4));
    assert!(inside_any_corner((3.9, 3.9), &rects, 4));
    assert!(inside_any_corner((1920.0, 0.0), &rects, 4));
    assert!(inside_any_corner((1923.0, 3.0), &rects, 4));
    // The far edges of the square are outside, like everywhere in this module.
    assert!(!inside_any_corner((4.0, 0.0), &rects, 4));
    assert!(!inside_any_corner((0.0, 4.0), &rects, 4));
    // Other corners of a monitor are not hot.
    assert!(!inside_any_corner((1919.0, 0.0), &rects, 4));
    assert!(!inside_any_corner((0.0, 1079.0), &rects, 4));
    assert!(!inside_any_corner((960.0, 540.0), &rects, 4));
}

#[test]
fn inside_any_corner_is_off_without_size_or_monitors() {
    assert!(!inside_any_corner((0.0, 0.0), &[A], 0));
    assert!(!inside_any_corner((0.0, 0.0), &[A], -3));
    assert!(!inside_any_corner((0.0, 0.0), &[], 4));
}

const C: Rect = (0, 1080, 1920, 1080);

#[test]
fn barrier_stops_the_pointer_leaving_through_the_corner_edge() {
    let all = [A, B];
    // On B's top row, pushing left: held on B's left edge, still in its hot corner.
    assert_eq!(
        hot_corner_barrier((1930.0, 2.0), (1900.0, 2.0), &all, &all, 4),
        (1920.0, 2.0)
    );
    // A fast flick from far away is stopped too.
    assert_eq!(
        hot_corner_barrier((2500.0, 2.0), (1000.0, 2.0), &all, &all, 4),
        (1920.0, 2.0)
    );
    // Already on the edge pixel.
    assert_eq!(
        hot_corner_barrier((1920.0, 2.0), (1900.0, 2.0), &all, &all, 4),
        (1920.0, 2.0)
    );
    // A diagonal that crosses the edge inside the corner's height.
    assert_eq!(
        hot_corner_barrier((1925.0, 6.0), (1915.0, 0.0), &all, &all, 4),
        (1920.0, 0.0)
    );
}

#[test]
fn barrier_works_the_other_way_too() {
    let all = [A, B];
    // From A's top row into B's corner: held on A.
    assert_eq!(
        hot_corner_barrier((1900.0, 2.0), (1930.0, 2.0), &all, &all, 4),
        (1919.0, 2.0)
    );
}

#[test]
fn barrier_is_only_as_tall_as_the_corner() {
    let all = [A, B];
    assert_eq!(
        hot_corner_barrier((1930.0, 4.0), (1900.0, 4.0), &all, &all, 4),
        (1900.0, 4.0)
    );
    assert_eq!(
        hot_corner_barrier((1930.0, 100.0), (1900.0, 100.0), &all, &all, 4),
        (1900.0, 100.0)
    );
    // Crossing at exactly the bottom of the band is outside, like contains().
    assert_eq!(
        hot_corner_barrier((1925.0, 8.0), (1915.0, 0.0), &all, &all, 4),
        (1915.0, 0.0)
    );
}

#[test]
fn barrier_covers_the_top_edge_of_a_monitor_below() {
    let all = [A, C];
    assert_eq!(
        hot_corner_barrier((2.0, 1090.0), (2.0, 1070.0), &all, &all, 4),
        (2.0, 1080.0)
    );
    assert_eq!(
        hot_corner_barrier((500.0, 1090.0), (500.0, 1070.0), &all, &all, 4),
        (500.0, 1070.0)
    );
}

#[test]
fn barrier_leaves_other_moves_alone() {
    let all = [A, B];
    // Within one monitor, even in the corner.
    assert_eq!(
        hot_corner_barrier((10.0, 2.0), (0.0, 0.0), &all, &all, 4),
        (0.0, 0.0)
    );
    // No corners enabled, or a size of 0.
    assert_eq!(
        hot_corner_barrier((1930.0, 2.0), (1900.0, 2.0), &all, &[], 4),
        (1900.0, 2.0)
    );
    assert_eq!(
        hot_corner_barrier((1930.0, 2.0), (1900.0, 2.0), &all, &all, 0),
        (1900.0, 2.0)
    );
    // Only A has a corner: B's left edge is no barrier.
    assert_eq!(
        hot_corner_barrier((1930.0, 2.0), (1900.0, 2.0), &all, &[A], 4),
        (1900.0, 2.0)
    );
}
