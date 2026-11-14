//! Tests for `jff_wm::primitives::container_drop`.

use jff_wm::desk::Side;
use jff_wm::primitives::container_drop::{
    axis_for, in_empty_half, in_rect, leaf_drop, opposite, snapped_is_first, split_axis, LeafDrop,
    DROP_EDGE_SHARE,
};
use jff_wm::primitives::tile_tree::Axis;

const SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];
const AREA: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

#[test]
fn the_split_follows_the_axis_of_the_snap() {
    assert_eq!(axis_for(Side::Left), Axis::Horizontal);
    assert_eq!(axis_for(Side::Right), Axis::Horizontal);
    assert_eq!(axis_for(Side::Top), Axis::Vertical);
    assert_eq!(axis_for(Side::Bottom), Axis::Vertical);
}

#[test]
fn the_opposite_side_is_on_the_same_axis_and_comes_back() {
    for side in SIDES {
        assert_ne!(opposite(side), side);
        assert_eq!(opposite(opposite(side)), side);
        assert_eq!(axis_for(opposite(side)), axis_for(side));
    }
}

#[test]
fn the_snapped_window_is_first_on_the_left_and_the_top() {
    assert!(snapped_is_first(Side::Left));
    assert!(snapped_is_first(Side::Top));
    assert!(!snapped_is_first(Side::Right));
    assert!(!snapped_is_first(Side::Bottom));
}

#[test]
fn the_empty_half_is_the_one_the_snapped_window_is_not_in() {
    assert!(in_empty_half(AREA, Side::Left, (1500.0, 500.0)));
    assert!(!in_empty_half(AREA, Side::Left, (400.0, 500.0)));
    assert!(in_empty_half(AREA, Side::Right, (400.0, 500.0)));
    assert!(!in_empty_half(AREA, Side::Right, (1500.0, 500.0)));
    assert!(in_empty_half(AREA, Side::Top, (900.0, 900.0)));
    assert!(!in_empty_half(AREA, Side::Top, (900.0, 100.0)));
    assert!(in_empty_half(AREA, Side::Bottom, (900.0, 100.0)));
    assert!(!in_empty_half(AREA, Side::Bottom, (900.0, 900.0)));
}

#[test]
fn the_middle_line_belongs_to_the_right_or_bottom_half() {
    assert!(in_empty_half(AREA, Side::Left, (960.0, 500.0)));
    assert!(!in_empty_half(AREA, Side::Left, (959.9, 500.0)));
    assert!(!in_empty_half(AREA, Side::Right, (960.0, 500.0)));
    assert!(in_empty_half(AREA, Side::Right, (959.9, 500.0)));
    assert!(in_empty_half(AREA, Side::Top, (500.0, 540.0)));
    assert!(!in_empty_half(AREA, Side::Bottom, (500.0, 540.0)));
}

#[test]
fn a_point_belongs_to_exactly_one_of_the_two_halves_of_an_axis() {
    for side in SIDES {
        for x in (0..1920).step_by(60) {
            for y in (0..1080).step_by(45) {
                let p = (f64::from(x), f64::from(y));
                assert_ne!(
                    in_empty_half(AREA, side, p),
                    in_empty_half(AREA, opposite(side), p),
                    "{side:?} at {p:?}"
                );
            }
        }
    }
}

#[test]
fn a_point_outside_the_area_is_in_no_half() {
    // Panels take the room around the area on a real monitor.
    let area = (0, 30, 1920, 1020);
    for side in SIDES {
        assert!(!in_empty_half(area, side, (1500.0, 10.0)), "{side:?} above");
        assert!(
            !in_empty_half(area, side, (400.0, 1050.0)),
            "{side:?} below"
        );
        assert!(!in_empty_half(area, side, (-1.0, 500.0)), "{side:?} left");
        assert!(
            !in_empty_half(area, side, (1920.0, 500.0)),
            "{side:?} right"
        );
    }
    assert!(in_empty_half(area, Side::Left, (1919.9, 1049.9)));
}

#[test]
fn the_halves_are_those_of_the_area_on_a_monitor_that_is_not_at_the_origin() {
    // The second monitor of a pair, with a 24 px bar on top.
    let area = (1920, 24, 1280, 696);
    assert!(in_empty_half(area, Side::Left, (2560.0, 300.0)));
    assert!(!in_empty_half(area, Side::Left, (2559.9, 300.0)));
    assert!(in_empty_half(area, Side::Right, (2559.9, 300.0)));
    assert!(in_empty_half(area, Side::Top, (2000.0, 372.0)));
    assert!(!in_empty_half(area, Side::Top, (2000.0, 371.9)));
    // The first monitor's half is not this monitor's.
    assert!(!in_empty_half(area, Side::Right, (400.0, 300.0)));
}

#[test]
fn an_area_with_no_room_has_no_half() {
    for side in SIDES {
        assert!(!in_empty_half((0, 0, 0, 0), side, (0.0, 0.0)));
        assert!(!in_empty_half((0, 0, 0, 100), side, (0.0, 50.0)));
        assert!(!in_empty_half((0, 0, 100, -5), side, (50.0, 0.0)));
    }
}

#[test]
fn a_one_pixel_area_still_has_a_defined_answer() {
    // Nothing panics, and the point in it is in exactly one half.
    for side in SIDES {
        assert_ne!(
            in_empty_half((5, 5, 1, 1), side, (5.0, 5.0)),
            in_empty_half((5, 5, 1, 1), opposite(side), (5.0, 5.0))
        );
    }
}

#[test]
fn a_wide_leaf_is_cut_into_a_left_and_a_right_half() {
    assert_eq!(split_axis((0, 0, 1920, 1080)), Axis::Horizontal);
    assert_eq!(split_axis((300, 40, 101, 100)), Axis::Horizontal);
}

#[test]
fn a_tall_leaf_is_cut_into_a_top_and_a_bottom_half() {
    assert_eq!(split_axis((0, 0, 960, 1080)), Axis::Vertical);
    assert_eq!(split_axis((300, 40, 100, 101)), Axis::Vertical);
}

#[test]
fn a_square_leaf_and_one_with_no_room_are_cut_side_by_side() {
    assert_eq!(split_axis((0, 0, 500, 500)), Axis::Horizontal);
    assert_eq!(split_axis((0, 0, 0, 0)), Axis::Horizontal);
    assert_eq!(split_axis((10, 10, -4, -9)), Axis::Horizontal);
}

#[test]
fn the_axis_does_not_depend_on_where_the_leaf_is() {
    for (x, y) in [(0, 0), (1920, 24), (-1280, -300)] {
        assert_eq!(split_axis((x, y, 800, 600)), Axis::Horizontal);
        assert_eq!(split_axis((x, y, 600, 800)), Axis::Vertical);
    }
}

#[test]
fn a_rect_holds_its_left_and_top_edges_and_not_its_right_and_bottom_ones() {
    let rect = (100, 50, 200, 100);
    assert!(in_rect(rect, (100.0, 50.0)));
    assert!(in_rect(rect, (299.9, 149.9)));
    assert!(!in_rect(rect, (300.0, 100.0)));
    assert!(!in_rect(rect, (200.0, 150.0)));
    assert!(!in_rect(rect, (99.9, 100.0)));
    assert!(!in_rect(rect, (200.0, 49.9)));
}

#[test]
fn touching_rects_never_both_hold_a_point() {
    let (left, right) = ((0, 0, 960, 1080), (960, 0, 960, 1080));
    for x in (0..1920).step_by(40) {
        let p = (f64::from(x), 500.0);
        assert_ne!(in_rect(left, p), in_rect(right, p), "{p:?}");
    }
}

#[test]
fn a_rect_with_no_room_holds_nothing() {
    assert!(!in_rect((0, 0, 0, 0), (0.0, 0.0)));
    assert!(!in_rect((0, 0, 0, 100), (0.0, 50.0)));
    assert!(!in_rect((0, 0, 100, -5), (50.0, -2.0)));
}

const LEAF: (i32, i32, i32, i32) = (100, 50, 800, 400);

#[test]
fn the_middle_of_a_leaf_swaps() {
    assert_eq!(leaf_drop(LEAF, (500.0, 250.0)), LeafDrop::Swap);
    assert_eq!(leaf_drop(LEAF, (300.0, 150.0)), LeafDrop::Swap);
}

#[test]
fn an_edge_of_a_leaf_puts_the_window_beside_it_on_that_side() {
    assert_eq!(
        leaf_drop(LEAF, (110.0, 250.0)),
        LeafDrop::Beside(Side::Left)
    );
    assert_eq!(
        leaf_drop(LEAF, (890.0, 250.0)),
        LeafDrop::Beside(Side::Right)
    );
    assert_eq!(leaf_drop(LEAF, (500.0, 60.0)), LeafDrop::Beside(Side::Top));
    assert_eq!(
        leaf_drop(LEAF, (500.0, 440.0)),
        LeafDrop::Beside(Side::Bottom)
    );
}

#[test]
fn the_edge_band_is_a_share_of_the_leaf_on_each_axis() {
    // 25% of 800 px is 200 px across, 25% of 400 px is 100 px down.
    assert!((DROP_EDGE_SHARE - 0.25).abs() < f64::EPSILON);
    assert_eq!(
        leaf_drop(LEAF, (299.0, 250.0)),
        LeafDrop::Beside(Side::Left)
    );
    assert_eq!(leaf_drop(LEAF, (300.0, 250.0)), LeafDrop::Swap);
    assert_eq!(leaf_drop(LEAF, (500.0, 149.0)), LeafDrop::Beside(Side::Top));
    assert_eq!(leaf_drop(LEAF, (500.0, 150.0)), LeafDrop::Swap);
}

#[test]
fn in_a_corner_the_nearer_edge_wins() {
    // Close to the left edge and a little off the top: left. The other way round: top.
    assert_eq!(leaf_drop(LEAF, (105.0, 90.0)), LeafDrop::Beside(Side::Left));
    assert_eq!(leaf_drop(LEAF, (180.0, 52.0)), LeafDrop::Beside(Side::Top));
    assert_eq!(
        leaf_drop(LEAF, (895.0, 445.0)),
        LeafDrop::Beside(Side::Right)
    );
    assert_eq!(
        leaf_drop(LEAF, (850.0, 449.0)),
        LeafDrop::Beside(Side::Bottom)
    );
}

#[test]
fn a_point_outside_a_leaf_and_a_leaf_with_no_room_swap() {
    assert_eq!(leaf_drop(LEAF, (50.0, 250.0)), LeafDrop::Swap);
    assert_eq!(leaf_drop(LEAF, (900.0, 250.0)), LeafDrop::Swap);
    assert_eq!(leaf_drop((0, 0, 0, 0), (0.0, 0.0)), LeafDrop::Swap);
    assert_eq!(leaf_drop((0, 0, 100, 0), (50.0, 0.0)), LeafDrop::Swap);
}
