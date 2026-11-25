use jff_wm::primitives::pixel_snap::{edge, grow, snap_rect, thickness};

const SCALES: [f64; 7] = [1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5];

#[test]
fn a_scale_of_one_changes_nothing() {
    assert_eq!(snap_rect((-7, 3, 100, 41), 1.0), (-7, 3, 100, 41));
    assert_eq!(edge(-7, 1.0), -7);
    assert_eq!(thickness(3, 1.0), 3);
}

#[test]
fn rectangles_that_touch_still_touch_in_pixels() {
    // The property `to_physical_precise_round` (position and size rounded apart) does not have:
    // the right edge of one rectangle is the left edge of the next.
    for scale in SCALES {
        for x in -40..40 {
            for w in 1..30 {
                for next in 1..12 {
                    let (left, _, left_w, _) = snap_rect((x, 0, w, 10), scale);
                    let (right, _, right_w, _) = snap_rect((x + w, 0, next, 10), scale);
                    assert_eq!(left + left_w, right, "x {x} w {w} scale {scale}");
                    assert!(left_w >= 0 && right_w >= 0);
                }
            }
        }
    }
}

#[test]
fn a_snapped_rectangle_is_within_a_pixel_of_the_exact_one() {
    for scale in SCALES {
        for x in -20..20 {
            for w in 0..25 {
                let (px, _, pw, _) = snap_rect((x, 0, w, 1), scale);
                assert!((f64::from(px) - f64::from(x) * scale).abs() <= 0.5 + 1e-9);
                assert!((f64::from(pw) - f64::from(w) * scale).abs() <= 1.0 + 1e-9);
            }
        }
    }
}

#[test]
fn a_ring_grown_from_a_snapped_rectangle_is_as_thick_on_every_side() {
    for scale in SCALES {
        for x in -20..20 {
            let inner = snap_rect((x, x * 2 + 1, 37, 23), scale);
            let ring = grow(inner, thickness(2, scale));
            let by = thickness(2, scale);
            assert_eq!(inner.0 - ring.0, by);
            assert_eq!(inner.1 - ring.1, by);
            assert_eq!(ring.0 + ring.2 - (inner.0 + inner.2), by);
            assert_eq!(ring.1 + ring.3 - (inner.1 + inner.3), by);
        }
    }
}

#[test]
fn grow_adds_to_every_side() {
    assert_eq!(grow((10, 20, 30, 40), 3), (7, 17, 36, 46));
    assert_eq!(grow((10, 20, 30, 40), 0), (10, 20, 30, 40));
}

#[test]
fn thickness_is_never_lost_below_one() {
    assert_eq!(thickness(1, 0.5), 1);
    assert_eq!(thickness(1, 0.25), 1);
    assert_eq!(thickness(2, 1.5), 3);
    assert_eq!(thickness(1, 1.5), 2);
    assert_eq!(thickness(0, 2.0), 0);
    assert_eq!(thickness(-4, 2.0), 0);
}

#[test]
fn a_negative_size_does_not_come_out() {
    assert_eq!(snap_rect((5, 5, -3, -3), 1.5).2, 0);
}
