//! Logical rectangles on the physical pixel grid.
//!
//! jff places windows and draws everything of its own in logical integers, and the output's scale
//! (1.25, 1.5, ...) turns those into frame pixels. Rounding a rectangle's position and its size
//! separately, which is what `to_physical_precise_round` does, does not keep two rectangles
//! that touch in the logical space touching in frame pixels: the right edge of one is
//! `round(x * s) + round(w * s)`, the left edge of the next is `round((x + w) * s)`, and those
//! differ by a pixel often enough to show as a seam or a thicker or thinner border.
//!
//! The functions here round **edges** instead. The same logical coordinate gives the same frame
//! pixel wherever it appears, so rectangles that share an edge in the logical space share it in
//! frame pixels, and a ring grown from a snapped rectangle has the same thickness on every side.
//!
//! A rectangle is `(x, y, width, height)`. At a scale of 1 every function returns its input.

/// `(x, y, width, height)`.
pub type Rect = (i32, i32, i32, i32);

/// The frame pixel a logical coordinate falls on: the nearest one, halves away from zero.
pub fn edge(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

/// A logical rectangle as frame pixels: each of its four edges is snapped on its own, the size
/// is what is left between them. Never negative.
pub fn snap_rect(rect: Rect, scale: f64) -> Rect {
    let (x, y, w, h) = rect;
    let left = edge(x, scale);
    let top = edge(y, scale);
    let right = edge(x + w, scale);
    let bottom = edge(y + h, scale);
    (left, top, (right - left).max(0), (bottom - top).max(0))
}

/// A rectangle in frame pixels with `by` more pixels on every side.
pub fn grow(rect: Rect, by: i32) -> Rect {
    let (x, y, w, h) = rect;
    (x - by, y - by, w + 2 * by, h + 2 * by)
}

/// A logical length in frame pixels, for something drawn with a constant thickness: the nearest
/// whole number, and at least one for anything above zero, so a thin line is never lost at a
/// scale below one. Zero and below give zero.
pub fn thickness(value: i32, scale: f64) -> i32 {
    if value <= 0 {
        0
    } else {
        edge(value, scale).max(1)
    }
}
