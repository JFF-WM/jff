//! Working with `wl_region`s: flatten them into rectangles and place those on screen.

use std::collections::BTreeSet;
use std::sync::Arc;

use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{RectangleKind, RegionAttributes};

/// Flatten a `wl_region` into a list of non-overlapping rectangles.
///
/// A `wl_region` is a *recipe*: "add this rectangle", "subtract this rectangle", applied in
/// order, with added rectangles allowed to overlap. A renderer wants the *result* as plain
/// rectangles, each pixel covered exactly once.
///
/// Every rectangle edge is a horizontal line. Between two neighbouring lines (a "band") the
/// region is uniform, so within a band it is just a set of disjoint horizontal spans `[x1,
/// x2)`. Run the recipe once per band on spans, turn each span into a rectangle as tall as
/// the band, then join vertically adjacent bands that came out identical.
///
/// `output` is cleared first. Rectangles come out sorted top to bottom, then left to right.
pub fn region_to_non_overlapping_rects(
    region: &RegionAttributes,
    output: &mut Vec<Rectangle<i32, Logical>>,
) {
    output.clear();

    // Usable rectangles as (kind, x1, y1, x2, y2). Empty and inverted ones do nothing.
    let rects: Vec<(RectangleKind, i32, i32, i32, i32)> = region
        .rects
        .iter()
        .filter(|(_, r)| r.size.w > 0 && r.size.h > 0)
        .map(|(kind, r)| {
            (
                *kind,
                r.loc.x,
                r.loc.y,
                r.loc.x.saturating_add(r.size.w),
                r.loc.y.saturating_add(r.size.h),
            )
        })
        .collect();

    let lines: BTreeSet<i32> = rects
        .iter()
        .flat_map(|&(_, _, y1, _, y2)| [y1, y2])
        .collect();

    let mut spans: Vec<(i32, i32)> = Vec::new();
    // Spans of the previous band and where its rectangles start in `output`, to join
    // identical bands.
    let mut previous: Vec<(i32, i32)> = Vec::new();
    let mut previous_start = 0;

    let mut lines = lines.into_iter();
    let Some(mut top) = lines.next() else {
        return; // No usable rectangles: the region is empty.
    };

    for bottom in lines {
        spans.clear();

        // `top` and `bottom` are consecutive edges, so a rectangle either covers the whole
        // band or none of it.
        for &(kind, x1, y1, x2, y2) in &rects {
            if y1 > top || y2 < bottom {
                continue;
            }
            match kind {
                RectangleKind::Add => add_span(&mut spans, x1, x2),
                RectangleKind::Subtract => subtract_span(&mut spans, x1, x2),
            }
        }

        if !spans.is_empty() && spans == previous {
            // Same shape as the band above, directly below it: make its rectangles taller.
            let extra = bottom - top;
            for rect in &mut output[previous_start..] {
                rect.size.h += extra;
            }
        } else {
            previous_start = output.len();
            for &(x1, x2) in &spans {
                output.push(Rectangle::from_extremities((x1, top), (x2, bottom)));
            }
            previous.clone_from(&spans);
        }

        top = bottom;
    }
}

/// Add `[x1, x2)` to a sorted list of disjoint spans, merging overlaps and touches.
fn add_span(spans: &mut Vec<(i32, i32)>, mut x1: i32, mut x2: i32) {
    spans.retain(|&(start, end)| {
        if end < x1 || start > x2 {
            true // Separate: keep.
        } else {
            // Overlapping or touching: absorb into the new span.
            x1 = x1.min(start);
            x2 = x2.max(end);
            false
        }
    });

    let at = spans.partition_point(|&(start, _)| start < x1);
    spans.insert(at, (x1, x2));
}

/// Remove `[x1, x2)` from a sorted list of disjoint spans, splitting any span it lands inside.
fn subtract_span(spans: &mut Vec<(i32, i32)>, x1: i32, x2: i32) {
    let mut result = Vec::with_capacity(spans.len() + 1);
    for &(start, end) in spans.iter() {
        if end <= x1 || start >= x2 {
            result.push((start, end)); // Untouched.
            continue;
        }
        if start < x1 {
            result.push((start, x1)); // Left of the cut.
        }
        if end > x2 {
            result.push((x2, end)); // Right of the cut.
        }
    }
    *spans = result;
}

/// A region of non-overlapping rectangles, scaled and moved before use.
///
/// Clients send regions in whole logical pixels, but windows can be drawn at fractional
/// scales and animated. Moving each rectangle by a fractional amount and rounding it
/// independently would let neighbours drift (one-pixel cracks or overlaps). So the transform
/// applies to rectangle *corners*: two rectangles sharing an edge share the exact corner
/// value, go through the exact same arithmetic, and round to the same pixel.
#[derive(Debug, Clone)]
pub struct TransformedRegion {
    /// Non-overlapping rectangles, usually relative to the surface.
    pub rects: Arc<Vec<Rectangle<i32, Logical>>>,
    /// Applied to every corner first.
    pub scale: Scale<f64>,
    /// Added to every corner after scaling.
    pub offset: Point<f64, Logical>,
}

impl TransformedRegion {
    /// Transformed top-left and bottom-right corner of every rectangle.
    pub fn iter(&self) -> impl Iterator<Item = (Point<f64, Logical>, Point<f64, Logical>)> + '_ {
        self.rects.iter().map(|r| {
            // Corners, not loc+size, for the reason in the type docs. The `i32` sums are
            // exact in `f64`.
            let r = r.to_f64();
            let top_left = r.loc.upscale(self.scale) + self.offset;
            let bottom_right = (r.loc + r.size.to_point()).upscale(self.scale) + self.offset;
            (top_left, bottom_right)
        })
    }

    /// Keep only the parts of `damage` inside the region, appending them to `out`.
    ///
    /// * `crop`: the part of the effect being drawn, in [`iter`](Self::iter)'s coordinate
    ///   space.
    /// * `dst`: where `crop` lands on screen, in physical pixels.
    /// * `damage`: damaged rectangles, relative to the top-left of `dst`.
    ///
    /// Output rectangles are relative to `dst` too.
    pub fn filter_damage(
        &self,
        crop: Rectangle<f64, Logical>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        out: &mut Vec<Rectangle<i32, Physical>>,
    ) {
        // Logical -> physical pixels, for the part being shown.
        let to_physical = dst.size.to_f64() / crop.size;
        let crop_size = crop.size.to_point();

        for (mut top_left, mut bottom_right) in self.iter() {
            top_left -= crop.loc;
            bottom_right -= crop.loc;

            let top_left = Point::<f64, Logical>::from((top_left.x.max(0.), top_left.y.max(0.)));
            let bottom_right = Point::<f64, Logical>::from((
                bottom_right.x.min(crop_size.x),
                bottom_right.y.min(crop_size.y),
            ));
            if bottom_right.x <= top_left.x || bottom_right.y <= top_left.y {
                continue;
            }

            // Round corners to pixels, so touching rectangles still touch.
            let rect = Rectangle::from_extremities(
                top_left.to_physical_precise_round(to_physical),
                bottom_right.to_physical_precise_round(to_physical),
            );

            out.extend(damage.iter().filter_map(|d| rect.intersection(*d)));
        }
    }
}
