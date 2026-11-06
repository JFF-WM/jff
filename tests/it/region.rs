//! Tests for `jff_wm::primitives::region`.

use std::sync::Arc;

use smithay::utils::Logical;
use smithay::utils::Physical;
use smithay::utils::Point;
use smithay::utils::Rectangle;
use smithay::utils::Scale;
use smithay::wayland::compositor::RectangleKind;
use smithay::wayland::compositor::RegionAttributes;

use jff_wm::primitives::region::{region_to_non_overlapping_rects, TransformedRegion};

/// Run the recipe `steps` (kind and corners x1, y1, x2, y2) and list the result as
/// `(x1, y1, x2, y2)`.
fn flatten(steps: &[(RectangleKind, (i32, i32, i32, i32))]) -> Vec<(i32, i32, i32, i32)> {
    let region = RegionAttributes {
        rects: steps
            .iter()
            .map(|&(kind, (x1, y1, x2, y2))| {
                (kind, Rectangle::from_extremities((x1, y1), (x2, y2)))
            })
            .collect(),
    };
    let mut out = Vec::new();
    region_to_non_overlapping_rects(&region, &mut out);
    out.iter()
        .map(|r| (r.loc.x, r.loc.y, r.loc.x + r.size.w, r.loc.y + r.size.h))
        .collect()
}

/// Whether the pixel `(x, y)` is inside any of `rects`.
fn covers(rects: &[(i32, i32, i32, i32)], x: i32, y: i32) -> bool {
    rects
        .iter()
        .any(|&(x1, y1, x2, y2)| x1 <= x && x < x2 && y1 <= y && y < y2)
}

#[test]
fn empty_region_gives_nothing() {
    assert!(flatten(&[]).is_empty());
}

#[test]
fn empty_rectangles_are_ignored() {
    use RectangleKind::Add;
    assert!(flatten(&[(Add, (0, 0, 0, 5))]).is_empty());
    assert!(flatten(&[(Add, (0, 0, 5, 0))]).is_empty());
}

#[test]
fn one_rectangle_stays_one_rectangle() {
    use RectangleKind::Add;
    assert_eq!(flatten(&[(Add, (0, 0, 10, 10))]), [(0, 0, 10, 10)]);
}

#[test]
fn separate_rectangles_stay_separate() {
    use RectangleKind::Add;
    assert_eq!(
        flatten(&[(Add, (0, 0, 5, 10)), (Add, (7, 0, 12, 10))]),
        [(0, 0, 5, 10), (7, 0, 12, 10)]
    );
}

#[test]
fn touching_rectangles_are_merged() {
    use RectangleKind::Add;
    assert_eq!(
        flatten(&[(Add, (0, 0, 5, 10)), (Add, (5, 0, 12, 10))]),
        [(0, 0, 12, 10)]
    );
}

#[test]
fn overlapping_rectangles_are_cut_into_bands() {
    use RectangleKind::Add;
    assert_eq!(
        flatten(&[(Add, (0, 0, 10, 10)), (Add, (5, 5, 15, 15))]),
        [(0, 0, 10, 5), (0, 5, 15, 10), (5, 10, 15, 15)]
    );
}

#[test]
fn subtracting_the_middle_leaves_a_frame() {
    use RectangleKind::{Add, Subtract};
    assert_eq!(
        flatten(&[(Add, (0, 0, 20, 20)), (Subtract, (5, 5, 15, 15))]),
        // The two side pieces of the frame are one tall rectangle each.
        [
            (0, 0, 20, 5),
            (0, 5, 5, 15),
            (15, 5, 20, 15),
            (0, 15, 20, 20)
        ]
    );
}

#[test]
fn subtract_comes_after_add_only_in_recipe_order() {
    use RectangleKind::{Add, Subtract};
    // Subtracting first has nothing to remove, the later add wins.
    assert_eq!(
        flatten(&[(Subtract, (0, 0, 10, 10)), (Add, (0, 0, 10, 10))]),
        [(0, 0, 10, 10)]
    );
    // The other order removes it again.
    assert!(flatten(&[(Add, (0, 0, 10, 10)), (Subtract, (0, 0, 10, 10))]).is_empty());
}

#[test]
fn output_rectangles_never_overlap_and_cover_the_same_pixels() {
    use RectangleKind::{Add, Subtract};
    let steps = [
        (Add, (0, 0, 12, 9)),
        (Add, (4, 3, 20, 14)),
        (Subtract, (6, 5, 10, 11)),
        (Add, (8, 7, 9, 8)),
        (Subtract, (15, 0, 30, 4)),
    ];
    let rects = flatten(&steps);

    // Recompute each pixel straight from the recipe, then compare with the output.
    for y in -1..16 {
        for x in -1..32 {
            let mut inside = false;
            for &(kind, (x1, y1, x2, y2)) in &steps {
                if x1 <= x && x < x2 && y1 <= y && y < y2 {
                    inside = matches!(kind, RectangleKind::Add);
                }
            }
            let hits = rects
                .iter()
                .filter(|&&(x1, y1, x2, y2)| x1 <= x && x < x2 && y1 <= y && y < y2)
                .count();
            assert_eq!(hits, usize::from(inside), "pixel ({x}, {y})");
            assert_eq!(covers(&rects, x, y), inside);
        }
    }
}

#[test]
fn filter_damage_clips_to_the_region() {
    let region = TransformedRegion {
        rects: Arc::new(vec![Rectangle::new((2, 2).into(), (4, 4).into())]),
        scale: Scale::from(1.),
        offset: Point::from((0., 0.)),
    };
    let crop = Rectangle::<f64, Logical>::new((0., 0.).into(), (10., 10.).into());
    let dst = Rectangle::<i32, Physical>::new((0, 0).into(), (10, 10).into());
    let damage = [Rectangle::<i32, Physical>::new(
        (0, 0).into(),
        (10, 10).into(),
    )];

    let mut out = Vec::new();
    region.filter_damage(crop, dst, &damage, &mut out);
    assert_eq!(out, [Rectangle::new((2, 2).into(), (4, 4).into())]);
}
