//! Tests for `jff_wm::primitives::container_view`.

use jff_wm::primitives::container_view::{
    bounds, cell_size, fit_size, label, label_rect, leaf_thumbs, on_border, BORDER_GRAB,
    CELL_LABEL, CELL_PAD,
};
use jff_wm::primitives::overview_grid::Thumb;

fn cell(pos: (i32, i32), size: (i32, i32), scale: f64) -> Thumb {
    Thumb { pos, size, scale }
}

#[test]
fn bounds_is_the_smallest_rectangle_around_all() {
    assert_eq!(bounds(&[]), None);
    assert_eq!(bounds(&[(10, 20, 30, 40)]), Some((10, 20, 30, 40)));
    assert_eq!(
        bounds(&[(0, 0, 50, 100), (60, 0, 40, 100)]),
        Some((0, 0, 100, 100))
    );
    assert_eq!(
        bounds(&[(100, 50, 10, 10), (0, 200, 10, 10), (300, 0, 5, 5)]),
        Some((0, 0, 305, 210))
    );
}

#[test]
fn a_cell_has_room_for_the_name_above_and_padding_around() {
    assert_eq!(
        cell_size((1000, 500)),
        (1000 + 2 * CELL_PAD, 500 + CELL_LABEL + CELL_PAD)
    );
    // never a size the grid cannot divide
    let (w, h) = cell_size((0, -5));
    assert!(w > 0 && h > 0);
}

#[test]
fn leaves_keep_their_place_in_the_cell_at_scale_one() {
    let area = (100, 200, 1000, 500);
    let leaves = [(100, 200, 498, 500), (602, 200, 498, 500)];
    let size = cell_size((area.2, area.3));
    let thumbs = leaf_thumbs(cell((10, 20), size, 1.0), area, &leaves);
    assert_eq!(thumbs.len(), 2);
    assert_eq!(thumbs[0].pos, (10 + CELL_PAD, 20 + CELL_LABEL));
    assert_eq!(thumbs[0].size, (498, 500));
    assert_eq!(thumbs[1].pos, (10 + CELL_PAD + 502, 20 + CELL_LABEL));
    assert!(thumbs.iter().all(|t| t.scale == 1.0));
}

#[test]
fn leaves_keep_the_ratios_of_the_tree_when_the_cell_is_smaller() {
    // The tree gave the left window 3/4 of the width and the right one 1/4.
    let area = (0, 0, 1200, 800);
    let leaves = [(0, 0, 900, 800), (900, 0, 300, 800)];
    let size = cell_size((area.2, area.3));
    for scale in [1.0, 0.5, 0.25] {
        let scaled = (
            (f64::from(size.0) * scale).round() as i32,
            (f64::from(size.1) * scale).round() as i32,
        );
        let thumbs = leaf_thumbs(cell((0, 0), scaled, scale), area, &leaves);
        let ratio = f64::from(thumbs[0].size.0) / f64::from(thumbs[1].size.0);
        assert!((ratio - 3.0).abs() < 0.1, "{scale}: {ratio}");
        assert_eq!(thumbs[0].size.1, thumbs[1].size.1, "{scale}");
        assert_eq!(thumbs[0].scale, scale);
    }
}

#[test]
fn leaves_stay_inside_the_cell_and_below_the_name() {
    let area = (500, 300, 1920, 1080);
    let leaves = [(500, 300, 950, 1080), (1470, 300, 950, 1080)];
    let size = cell_size((area.2, area.3));
    for scale in [1.0, 0.4, 0.17] {
        let scaled = (
            (f64::from(size.0) * scale).round() as i32,
            (f64::from(size.1) * scale).round() as i32,
        );
        let at = cell((77, 33), scaled, scale);
        let label = label_rect(at);
        for t in leaf_thumbs(at, area, &leaves) {
            assert!(t.pos.0 >= at.pos.0 && t.pos.1 >= at.pos.1, "{scale}");
            // one pixel of rounding is allowed
            assert!(t.pos.0 + t.size.0 <= at.pos.0 + at.size.0 + 1, "{scale}");
            assert!(t.pos.1 + t.size.1 <= at.pos.1 + at.size.1 + 1, "{scale}");
            assert!(t.pos.1 + 1 >= label.1 + label.3, "{scale}: under the name");
        }
    }
}

#[test]
fn a_leaf_never_shrinks_to_nothing() {
    let thumbs = leaf_thumbs(
        cell((0, 0), (10, 10), 0.001),
        (0, 0, 100, 100),
        &[(0, 0, 1, 1)],
    );
    assert!(thumbs[0].size.0 >= 1 && thumbs[0].size.1 >= 1);
    assert!(leaf_thumbs(cell((0, 0), (10, 10), 1.0), (0, 0, 1, 1), &[]).is_empty());
}

#[test]
fn the_name_goes_in_the_strip_across_the_top() {
    let at = cell((100, 200), (500, 400), 0.5);
    let (x, y, w, h) = label_rect(at);
    assert_eq!(x, 100 + CELL_PAD / 2);
    assert_eq!(y, 200);
    assert_eq!(w, 500 - CELL_PAD);
    assert_eq!(h, CELL_LABEL / 2);
    // a cell too narrow for its padding still gives a strip
    assert!(label_rect(cell((0, 0), (2, 2), 0.5)).2 >= 1);
}

#[test]
fn a_container_is_called_by_its_name_or_else_by_the_app_id_of_its_first_window() {
    assert_eq!(label(Some("Terminals"), "foot"), "Terminals");
    assert_eq!(label(None, "foot"), "foot");
    assert_eq!(label(Some(""), "foot"), "foot");
    assert_eq!(label(Some("   "), "foot"), "foot");
    assert_eq!(label(Some("  mail  "), "foot"), "mail");
    assert_eq!(label(None, " org.gnome.Nautilus "), "org.gnome.Nautilus");
    assert_eq!(label(None, ""), "");
    assert_eq!(label(Some(""), ""), "");
}

#[test]
fn the_border_is_the_band_just_outside_the_rectangle() {
    let area = (100, 100, 400, 300);
    // outside, within reach, on every side
    assert!(on_border(area, 8, (96.0, 200.0)));
    assert!(on_border(area, 8, (503.0, 200.0)));
    assert!(on_border(area, 8, (300.0, 95.0)));
    assert!(on_border(area, 8, (300.0, 405.0)));
    // the corners
    assert!(on_border(area, 8, (95.0, 95.0)));
    assert!(on_border(area, 8, (505.0, 405.0)));
    // inside is the windows'
    assert!(!on_border(area, 8, (100.0, 100.0)));
    assert!(!on_border(area, 8, (300.0, 250.0)));
    assert!(!on_border(area, 8, (499.9, 399.9)));
    // too far out
    assert!(!on_border(area, 8, (91.0, 200.0)));
    assert!(!on_border(area, 8, (300.0, 409.0)));
    assert!(!on_border(area, 8, (0.0, 0.0)));
}

#[test]
fn a_thin_border_can_still_be_hit() {
    let area = (100, 100, 400, 300);
    assert!(on_border(
        area,
        1,
        (100.0 - f64::from(BORDER_GRAB) + 0.5, 200.0)
    ));
    assert!(on_border(area, 0, (99.0, 200.0)));
    assert!(!on_border(
        area,
        0,
        (100.0 - f64::from(BORDER_GRAB) - 1.0, 200.0)
    ));
}

#[test]
fn a_rectangle_with_no_room_has_no_border() {
    assert!(!on_border((0, 0, 0, 0), 8, (0.0, 0.0)));
    assert!(!on_border((10, 10, 0, 50), 8, (5.0, 20.0)));
    assert!(!on_border((10, 10, 50, -1), 8, (30.0, 5.0)));
}

#[test]
fn a_container_dropped_on_a_smaller_monitor_shrinks_to_it() {
    let monitor = (1920, 0, 1280, 720);
    assert_eq!(fit_size((800, 600), monitor), (800, 600));
    assert_eq!(fit_size((1900, 600), monitor), (1280, 600));
    assert_eq!(fit_size((1900, 1000), monitor), (1280, 720));
    assert_eq!(fit_size((0, 0), monitor), (1, 1));
}
