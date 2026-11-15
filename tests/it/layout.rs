//! Tests for `jff_wm::primitives::layout`.

use jff_wm::primitives::layout::{
    arrange, bounds, carry_pos, center_in, clamp_to_rects, cycle, dialog_pos, fit_into,
    hot_corner_barrier, hot_corner_rect, inside_any_corner, intersects, nearest, neighbor,
    reachable, Direction, MonitorTarget, Rect,
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

#[test]
fn a_dialog_is_centered_over_its_parent() {
    let parent = (300, 200, 800, 600);
    assert_eq!(dialog_pos((400, 200), Some(parent), A, 40), (500, 400));
}

#[test]
fn a_dialog_without_a_parent_is_centered_on_the_monitor() {
    assert_eq!(dialog_pos((400, 200), None, A, 40), (760, 440));
    // on the second monitor
    assert_eq!(dialog_pos((400, 200), None, B, 40), (1920 + 440, 412));
}

#[test]
fn a_dialog_stays_inside_the_monitor() {
    // the parent hangs off the left edge: the dialog is pulled back in, with the margin
    let parent = (-500, 100, 800, 600);
    let (x, y) = dialog_pos((400, 200), Some(parent), A, 40);
    assert_eq!((x, y), (40, 300));
    // and off the bottom right
    let parent = (1700, 900, 600, 400);
    let (x, y) = dialog_pos((400, 200), Some(parent), A, 40);
    assert_eq!((x, y), (1920 - 400 - 40, 1080 - 200 - 40));
}

#[test]
fn a_parent_on_another_monitor_is_not_followed() {
    // the parent is on A, the dialog is placed for B
    assert_eq!(
        dialog_pos((400, 200), Some((100, 100, 600, 400)), B, 40),
        dialog_pos((400, 200), None, B, 40)
    );
}

#[test]
fn a_dialog_too_big_for_the_monitor_keeps_its_corner_reachable() {
    assert_eq!(
        dialog_pos((3000, 2000), Some((0, 0, 800, 600)), A, 40),
        (40, 40)
    );
}

/// Three monitors in a row, plugged in in a different order than they sit.
const ROW: [Rect; 3] = [
    (1920, 0, 1920, 1080),
    (0, 0, 1920, 1080),
    (3840, 0, 1280, 1024),
];

#[test]
fn neighbors_in_a_row() {
    // ROW[1] is on the left, ROW[0] in the middle, ROW[2] on the right.
    assert_eq!(neighbor(&ROW, 0, Direction::Left), Some(1));
    assert_eq!(neighbor(&ROW, 0, Direction::Right), Some(2));
    assert_eq!(neighbor(&ROW, 1, Direction::Right), Some(0));
    assert_eq!(neighbor(&ROW, 2, Direction::Left), Some(0));
    // The edges of the desktop, and nothing above or below a row.
    assert_eq!(neighbor(&ROW, 1, Direction::Left), None);
    assert_eq!(neighbor(&ROW, 2, Direction::Right), None);
    assert_eq!(neighbor(&ROW, 0, Direction::Up), None);
    assert_eq!(neighbor(&ROW, 0, Direction::Down), None);
}

#[test]
fn neighbors_above_and_below() {
    let rects = [A, (0, 1080, 1920, 1080)];
    assert_eq!(neighbor(&rects, 0, Direction::Down), Some(1));
    assert_eq!(neighbor(&rects, 1, Direction::Up), Some(0));
    assert_eq!(neighbor(&rects, 0, Direction::Right), None);
}

#[test]
fn the_closest_facing_monitor_wins() {
    // Two to the right of A: a near one that faces it and a far one, the near one is next.
    let rects = [A, (3200, 0, 1280, 1024), (1920, 0, 1280, 1024)];
    assert_eq!(neighbor(&rects, 0, Direction::Right), Some(2));
    // A monitor that faces A beats a nearer one that only lies diagonally.
    let rects = [A, (1920, 1500, 800, 600), (2720, 0, 1280, 1024)];
    assert_eq!(neighbor(&rects, 0, Direction::Right), Some(2));
    // Without one that faces it, the diagonal one is still to the right.
    let rects = [A, (1920, 1500, 800, 600)];
    assert_eq!(neighbor(&rects, 0, Direction::Right), Some(1));
}

#[test]
fn a_monitor_that_is_off_is_nobodys_neighbor() {
    let rects = [A, (1920, 0, 0, 0), B];
    assert_eq!(neighbor(&rects, 0, Direction::Right), Some(2));
    assert_eq!(cycle(&rects, 0, false), Some(2));
}

#[test]
fn neighbor_of_nothing_is_nothing() {
    assert_eq!(neighbor(&[], 0, Direction::Left), None);
    assert_eq!(neighbor(&ROW, 7, Direction::Left), None);
}

#[test]
fn cycling_goes_by_position_and_wraps() {
    // Left to right: ROW[1], ROW[0], ROW[2].
    assert_eq!(cycle(&ROW, 1, false), Some(0));
    assert_eq!(cycle(&ROW, 0, false), Some(2));
    assert_eq!(cycle(&ROW, 2, false), Some(1));
    assert_eq!(cycle(&ROW, 1, true), Some(2));
    assert_eq!(cycle(&ROW, 0, true), Some(1));
}

#[test]
fn cycling_needs_another_monitor() {
    assert_eq!(cycle(&[A], 0, false), None);
    assert_eq!(cycle(&[], 0, false), None);
    assert_eq!(cycle(&ROW, 9, false), None);
    // Two on top of each other: top to bottom.
    let stacked = [(0, 1080, 1920, 1080), A];
    assert_eq!(cycle(&stacked, 1, false), Some(0));
    assert_eq!(cycle(&stacked, 0, false), Some(1));
}

#[test]
fn monitor_targets_are_parsed() {
    assert_eq!(MonitorTarget::parse("left"), Some(MonitorTarget::Left));
    assert_eq!(MonitorTarget::parse("RIGHT"), Some(MonitorTarget::Right));
    assert_eq!(MonitorTarget::parse("Up"), Some(MonitorTarget::Up));
    assert_eq!(MonitorTarget::parse("down"), Some(MonitorTarget::Down));
    assert_eq!(MonitorTarget::parse("next"), Some(MonitorTarget::Next));
    assert_eq!(MonitorTarget::parse("prev"), Some(MonitorTarget::Prev));
    // Anything else is a name, kept as it is.
    assert_eq!(
        MonitorTarget::parse("DP-2"),
        Some(MonitorTarget::Named("DP-2".to_string()))
    );
    assert_eq!(MonitorTarget::parse(""), None);
    assert_eq!(MonitorTarget::parse("   "), None);
}

#[test]
fn monitor_targets_are_resolved() {
    let names: Vec<String> = ["DP-1", "HDMI-1", "DP-2"].map(String::from).to_vec();
    // DP-1 is in the middle (ROW[0]).
    assert_eq!(MonitorTarget::Left.resolve(&ROW, &names, 0), Ok(1));
    assert_eq!(MonitorTarget::Right.resolve(&ROW, &names, 0), Ok(2));
    assert_eq!(MonitorTarget::Next.resolve(&ROW, &names, 0), Ok(2));
    assert_eq!(MonitorTarget::Prev.resolve(&ROW, &names, 0), Ok(1));
    assert_eq!(
        MonitorTarget::Named("DP-2".to_string()).resolve(&ROW, &names, 0),
        Ok(2)
    );
    // A name can be the monitor in use, a way never is.
    assert_eq!(
        MonitorTarget::Named("DP-1".to_string()).resolve(&ROW, &names, 0),
        Ok(0)
    );
    assert!(MonitorTarget::Up.resolve(&ROW, &names, 0).is_err());
    assert!(MonitorTarget::Left.resolve(&ROW, &names, 1).is_err());
    assert!(MonitorTarget::Next.resolve(&[A], &names, 0).is_err());
    let missing = MonitorTarget::Named("DP-9".to_string()).resolve(&ROW, &names, 0);
    assert!(missing.unwrap_err().contains("DP-9"));
}

#[test]
fn a_window_keeps_its_offset_when_it_goes_to_another_monitor() {
    // 100 px in and 50 px down from the corner of A: the same on B.
    assert_eq!(carry_pos((100, 50), (400, 300), A, B), (1920 + 100, 50));
    // And back.
    assert_eq!(carry_pos((2020, 50), (400, 300), B, A), (100, 50));
}

#[test]
fn a_window_that_does_not_fit_the_new_monitor_is_moved_inside() {
    let small: Rect = (1920, 0, 800, 600);
    // 1500 px in on a monitor that is 800 wide: pushed back until it fits.
    assert_eq!(
        carry_pos((1500, 700), (400, 300), A, small),
        (1920 + 800 - 400, 600 - 300)
    );
}

#[test]
fn a_window_is_centered_in_a_monitor() {
    assert_eq!(
        center_in((400, 200), B),
        (1920 + (1280 - 400) / 2, (1024 - 200) / 2)
    );
    // One bigger than the monitor hangs over evenly.
    assert_eq!(center_in((2000, 1200), A), (-40, -60));
}
