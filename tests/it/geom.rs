//! Tests for `jff_wm::primitives::geom`.

use jff_wm::desk::Side;
use jff_wm::primitives::geom::{
    anchored_pos, edge_at, fit_size, inset_rect, nearest_corner, overflows, pointer_anchored_pos,
    resize, snapped_rect, Edges, Size,
};

const NONE: Size = (0, 0);
const BR: Edges = Edges {
    left: false,
    right: true,
    top: false,
    bottom: true,
};
const TL: Edges = Edges {
    left: true,
    right: false,
    top: true,
    bottom: false,
};

#[test]
fn exact_fit_is_not_an_overflow() {
    assert!(!overflows((1900, 1016), (1900, 1016)));
    assert!(overflows((1901, 1016), (1900, 1016)));
    assert!(overflows((100, 2000), (1900, 1016)));
}

#[test]
fn fit_shrinks_only_what_overflows() {
    assert_eq!(fit_size((1200, 700), (1000, 800), 40), (920, 700));
    assert_eq!(fit_size((300, 200), (1000, 800), 40), (300, 200));
    assert_eq!(fit_size((300, 200), (50, 50), 40), (1, 1));
}

#[test]
fn inset_shrinks_every_side() {
    assert_eq!(inset_rect((0, 0, 1920, 1080), 0), (0, 0, 1920, 1080));
    assert_eq!(inset_rect((0, 30, 1920, 1050), 10), (10, 40, 1900, 1030));
    assert_eq!(inset_rect((1920, 0, 800, 600), 8), (1928, 8, 784, 584));
}

#[test]
fn inset_never_collapses() {
    assert_eq!(inset_rect((0, 0, 100, 40), 500), (19, 19, 62, 2));
    assert_eq!(inset_rect((0, 0, 1, 1), 5), (0, 0, 1, 1));
    assert_eq!(inset_rect((5, 5, 100, 100), -3), (5, 5, 100, 100));
}

#[test]
fn nearest_corner_picks_quadrant() {
    assert_eq!(nearest_corner((10.0, 10.0), (200, 100)), TL);
    assert_eq!(nearest_corner((190.0, 90.0), (200, 100)), BR);
    let tr = nearest_corner((190.0, 10.0), (200, 100));
    assert!(tr.right && tr.top && !tr.left && !tr.bottom);
}

#[test]
fn bottom_right_grows_without_moving() {
    assert_eq!(
        resize((10, 20), (100, 80), (30, 15), BR, NONE, NONE),
        ((10, 20), (130, 95))
    );
}

#[test]
fn top_left_keeps_far_corner_fixed() {
    let (pos, size) = resize((50, 50), (100, 80), (-10, -10), TL, NONE, NONE);
    assert_eq!((pos, size), ((40, 40), (110, 90)));
    assert_eq!((pos.0 + size.0, pos.1 + size.1), (150, 130));
}

#[test]
fn respects_min_and_anchors_on_clamp() {
    let (pos, size) = resize((50, 50), (100, 80), (90, 0), TL, (40, 40), NONE);
    assert_eq!(size, (40, 80));
    assert_eq!(pos.0 + size.0, 150);
}

#[test]
fn anchoring_follows_the_real_size() {
    assert_eq!(anchored_pos((50, 50), (100, 80), (110, 90), TL), (40, 40));
    assert_eq!(anchored_pos((50, 50), (100, 80), (97, 90), TL), (53, 40));
    assert_eq!(anchored_pos((50, 50), (100, 80), (130, 95), BR), (50, 50));
}

#[test]
fn respects_max() {
    let (_, size) = resize((0, 0), (100, 80), (500, 500), BR, NONE, (300, 0));
    assert_eq!(size, (300, 580));
}

#[test]
fn never_collapses_below_one() {
    let (_, size) = resize((0, 0), (100, 80), (-500, -500), BR, NONE, NONE);
    assert_eq!(size, (1, 1));
}

#[test]
fn no_gap_and_no_border_gives_exact_halves() {
    let r = (10, 20, 101, 51);
    assert_eq!(snapped_rect(r, 0, 0, Side::Left), (10, 20, 50, 51));
    assert_eq!(snapped_rect(r, 0, 0, Side::Right), (60, 20, 51, 51));
    assert_eq!(snapped_rect(r, 0, 0, Side::Top), (10, 20, 101, 25));
    assert_eq!(snapped_rect(r, 0, 0, Side::Bottom), (10, 45, 101, 26));
}

#[test]
fn gap_only_splits_the_middle_gap_in_two() {
    let r = (0, 0, 1000, 600);
    assert_eq!(snapped_rect(r, 10, 10, Side::Left), (10, 10, 485, 580));
    assert_eq!(snapped_rect(r, 10, 10, Side::Right), (505, 10, 485, 580));
}

#[test]
fn border_only_keeps_the_two_rings_apart() {
    // inset 4 (one border), middle gap 8 (two borders)
    let r = (0, 0, 1000, 600);
    assert_eq!(snapped_rect(r, 4, 8, Side::Left), (4, 4, 492, 592));
    assert_eq!(snapped_rect(r, 4, 8, Side::Right), (504, 4, 492, 592));
}

#[test]
fn border_wider_than_gaps_wins() {
    // gaps 2, border 6: inset 6, middle gap 12
    let r = (0, 0, 1920, 1080);
    assert_eq!(snapped_rect(r, 6, 12, Side::Left), (6, 6, 948, 1068));
    assert_eq!(snapped_rect(r, 6, 12, Side::Right), (966, 6, 948, 1068));
}

#[test]
fn odd_sizes_keep_the_total_exact() {
    let r = (0, 0, 101, 51);
    assert_eq!(snapped_rect(r, 0, 5, Side::Left), (0, 0, 48, 51));
    assert_eq!(snapped_rect(r, 0, 5, Side::Right), (53, 0, 48, 51));
    assert_eq!(snapped_rect(r, 0, 4, Side::Left), (0, 0, 48, 51));
    assert_eq!(snapped_rect(r, 0, 4, Side::Right), (52, 0, 49, 51));
}

#[test]
fn all_four_sides_use_the_same_rule() {
    let r = (100, 50, 800, 600);
    assert_eq!(snapped_rect(r, 10, 20, Side::Left), (110, 60, 380, 580));
    assert_eq!(snapped_rect(r, 10, 20, Side::Right), (510, 60, 380, 580));
    assert_eq!(snapped_rect(r, 10, 20, Side::Top), (110, 60, 780, 280));
    assert_eq!(snapped_rect(r, 10, 20, Side::Bottom), (110, 360, 780, 280));
}

#[test]
fn tiny_output_keeps_every_window_at_least_one_pixel() {
    let r = (0, 0, 100, 40);
    assert_eq!(snapped_rect(r, 500, 500, Side::Left), (19, 19, 1, 2));
    assert_eq!(snapped_rect(r, 500, 500, Side::Right), (80, 19, 1, 2));
    assert_eq!(snapped_rect(r, 500, 500, Side::Top), (19, 19, 62, 1));
    assert_eq!(snapped_rect(r, 500, 500, Side::Bottom), (19, 20, 62, 1));
    assert_eq!(snapped_rect((0, 0, 1, 1), 5, 5, Side::Left), (0, 0, 1, 1));
    assert_eq!(snapped_rect((0, 0, 1, 1), 5, 5, Side::Right), (0, 0, 1, 1));
}

#[test]
fn snapped_windows_stay_inside_and_never_overlap() {
    for len in 1..40 {
        for inset in [-3, 0, 1, 5, 100] {
            for gap in [-3, 0, 1, 2, 7, 60] {
                let area = (7, 3, len, len + 5);
                let (ox, oy, ow, oh) = inset_rect(area, inset);
                for (a, b, horizontal) in [
                    (Side::Left, Side::Right, true),
                    (Side::Top, Side::Bottom, false),
                ] {
                    let (a, b) = (
                        snapped_rect(area, inset, gap, a),
                        snapped_rect(area, inset, gap, b),
                    );
                    for r in [a, b] {
                        assert!(r.2 >= 1 && r.3 >= 1, "{r:?} for {area:?} {inset} {gap}");
                        assert!(r.0 >= ox && r.1 >= oy, "{r:?} for {area:?} {inset} {gap}");
                        assert!(r.0 + r.2 <= ox + ow && r.1 + r.3 <= oy + oh);
                    }
                    let (start, end, size) = if horizontal {
                        (a.0 + a.2, b.0, ow)
                    } else {
                        (a.1 + a.3, b.1, oh)
                    };
                    if size >= 2 {
                        let want = gap.clamp(0, size - 2);
                        assert_eq!(end - start, want, "{area:?} {inset} {gap}");
                    }
                }
            }
        }
    }
}

#[test]
fn edges_are_found_within_the_margin() {
    let r = (0, 0, 1920, 1080);
    assert_eq!(edge_at(r, (0.0, 500.0), 2.0), Some(Side::Left));
    assert_eq!(edge_at(r, (1919.0, 500.0), 2.0), Some(Side::Right));
    assert_eq!(edge_at(r, (900.0, 1.0), 2.0), Some(Side::Top));
    assert_eq!(edge_at(r, (900.0, 1079.0), 2.0), None);
    assert_eq!(edge_at(r, (900.0, 500.0), 2.0), None);
    assert_eq!(edge_at(r, (0.0, 0.0), 2.0), Some(Side::Left));
    assert_eq!(
        edge_at((1920, 0, 1280, 720), (1920.0, 300.0), 2.0),
        Some(Side::Left)
    );
}

#[test]
fn pointer_keeps_its_relative_spot() {
    let from = (0, 0, 1000, 500);
    assert_eq!(
        pointer_anchored_pos(from, (500, 250), (500.0, 250.0)),
        (250, 125)
    );
    assert_eq!(pointer_anchored_pos(from, (500, 250), (0.0, 0.0)), (0, 0));
    assert_eq!(
        pointer_anchored_pos(from, (500, 250), (1000.0, 500.0)),
        (500, 250)
    );
}
