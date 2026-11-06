//! Tests for `jff_wm::primitives::geom`.

use jff_wm::primitives::geom::{
    anchored_pos, fit_size, inset_rect, nearest_corner, overflows, resize, Edges, Size,
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
