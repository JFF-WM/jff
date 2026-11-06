//! Pure multi-monitor geometry, no Smithay/Wayland types, so it can be unit-tested on its own.
//!
//! Every monitor lives in one shared coordinate space. A `Rect` is `(x, y, width, height)` in
//! that space.

use crate::primitives::geom::{Pos, Size};

pub type Rect = (i32, i32, i32, i32);

/// Whether `p` is inside `rect`. The right and bottom edges count as outside.
pub fn contains(rect: Rect, p: (f64, f64)) -> bool {
    p.0 >= rect.0 as f64
        && p.0 < (rect.0 + rect.2) as f64
        && p.1 >= rect.1 as f64
        && p.1 < (rect.1 + rect.3) as f64
}

/// `rect` grown by `margin` px on every side.
pub fn grow(rect: Rect, margin: i32) -> Rect {
    (
        rect.0 - margin,
        rect.1 - margin,
        rect.2 + 2 * margin,
        rect.3 + 2 * margin,
    )
}

/// Whether two rects share at least one pixel.
pub fn intersects(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// Smallest rect holding every one of `rects`, `(0, 0, 0, 0)` if empty.
pub fn bounds(rects: &[Rect]) -> Rect {
    if rects.is_empty() {
        return (0, 0, 0, 0);
    }
    let x0 = rects.iter().map(|r| r.0).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r.1).min().unwrap_or(0);
    let x1 = rects.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
    (x0, y0, x1 - x0, y1 - y0)
}

/// Where each monitor goes. Positions in `fixed` are kept, the rest go left to right, after
/// everything already placed, along the top edge.
pub fn arrange(sizes: &[Size], fixed: &[Option<Pos>]) -> Vec<Pos> {
    let mut edge = fixed
        .iter()
        .zip(sizes)
        .filter_map(|(pos, size)| pos.map(|p| p.0 + size.0))
        .max()
        .unwrap_or(0);

    let mut placed = Vec::with_capacity(sizes.len());
    for (size, pos) in sizes.iter().zip(fixed) {
        match pos {
            Some(pos) => placed.push(*pos),
            None => {
                placed.push((edge, 0));
                edge += size.0;
            }
        }
    }
    placed
}

/// Squared distance from `p` to `rect`. 0 when inside.
fn distance_sq(rect: Rect, p: (f64, f64)) -> f64 {
    let dx = (rect.0 as f64 - p.0)
        .max(0.0)
        .max(p.0 - (rect.0 + rect.2) as f64);
    let dy = (rect.1 as f64 - p.1)
        .max(0.0)
        .max(p.1 - (rect.1 + rect.3) as f64);
    dx * dx + dy * dy
}

/// Index of the rect containing `p`, else the closest one.
pub fn nearest(p: (f64, f64), rects: &[Rect]) -> Option<usize> {
    rects
        .iter()
        .enumerate()
        .map(|(i, r)| (i, distance_sq(*r, p)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Keep a pointer on some monitor: unchanged when already on one, otherwise pulled to the
/// closest point of the closest monitor.
pub fn clamp_to_rects(p: (f64, f64), rects: &[Rect]) -> (f64, f64) {
    let Some(i) = nearest(p, rects) else {
        return p;
    };
    let r = rects[i];
    if contains(r, p) {
        return p;
    }
    (
        p.0.clamp(r.0 as f64, (r.0 + (r.2 - 1).max(0)) as f64),
        p.1.clamp(r.1 as f64, (r.1 + (r.3 - 1).max(0)) as f64),
    )
}

/// The middle of a window.
pub fn center(pos: Pos, size: Size) -> (f64, f64) {
    (
        pos.0 as f64 + size.0 as f64 / 2.0,
        pos.1 as f64 + size.1 as f64 / 2.0,
    )
}

/// A point on the window's titlebar: what the user has to reach to drag it.
pub fn grab_point(pos: Pos, size: Size) -> (f64, f64) {
    (pos.0 as f64 + size.0 as f64 / 2.0, pos.1 as f64 + 10.0)
}

/// Whether the user can still grab the window by its titlebar.
pub fn reachable(pos: Pos, size: Size, rects: &[Rect]) -> bool {
    let p = grab_point(pos, size);
    rects.iter().any(|r| contains(*r, p))
}

/// Move the window the least needed to fit inside `rect`.
pub fn fit_into(pos: Pos, size: Size, rect: Rect) -> Pos {
    (
        pos.0.clamp(rect.0, (rect.0 + rect.2 - size.0).max(rect.0)),
        pos.1.clamp(rect.1, (rect.1 + rect.3 - size.1).max(rect.1)),
    )
}
