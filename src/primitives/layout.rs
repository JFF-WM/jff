//! Pure multi-monitor geometry, no Smithay/Wayland types, so it can be unit-tested on its own.
//!
//! Every monitor lives in one shared coordinate space. A `Rect` is `(x, y, width, height)` in
//! that space.

use crate::primitives::geom::{Pos, Size};

pub type Rect = (i32, i32, i32, i32);

/// Where a dialog `size` big goes: centred on `parent`, the rect of the window it belongs to,
/// or on `area` (the monitor) when it has none or the parent isn't on that monitor. Then moved
/// to lie inside `area` with `margin` px to spare, a dialog too big for that keeps its
/// top-left corner reachable instead.
pub fn dialog_pos(size: Size, parent: Option<Rect>, area: Rect, margin: i32) -> Pos {
    let anchor = parent
        .filter(|p| p.2 > 0 && p.3 > 0 && intersects(*p, area))
        .unwrap_or(area);
    let inside = |want: i32, start: i32, length: i32, extent: i32| {
        want.min(start + length - extent - margin)
            .max(start + margin)
    };
    (
        inside(anchor.0 + (anchor.2 - size.0) / 2, area.0, area.2, size.0),
        inside(anchor.1 + (anchor.3 - size.1) / 2, area.1, area.3, size.1),
    )
}

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

/// The hot corner of the monitor at `rect`: a `size` px square at its top-left.
pub fn hot_corner_rect(rect: Rect, size: i32) -> Rect {
    (rect.0, rect.1, size, size)
}

/// Whether `p` is in the top-left `size` px square of any of `rects`. Always false for a
/// `size` of 0 or less.
pub fn inside_any_corner(p: (f64, f64), rects: &[Rect], size: i32) -> bool {
    size > 0
        && rects
            .iter()
            .any(|rect| contains(hot_corner_rect(*rect, size), p))
}

/// Whether the straight move `from` -> `to` crosses the left edge of `rect` within its top
/// `size` px, or the top edge within its left `size` px: the two edges of a hot corner.
fn crosses_corner_edges(rect: Rect, from: (f64, f64), to: (f64, f64), size: i32) -> bool {
    let (x0, y0) = (rect.0 as f64, rect.1 as f64);
    let size = size as f64;
    // Left edge: where the move crosses the line x = x0.
    let left = (from.0 - x0) * (to.0 - x0) <= 0.0 && from.0 != to.0 && {
        let y = from.1 + (x0 - from.0) / (to.0 - from.0) * (to.1 - from.1);
        y >= y0 && y < y0 + size
    };
    // Top edge: where it crosses the line y = y0.
    let top = (from.1 - y0) * (to.1 - y0) <= 0.0 && from.1 != to.1 && {
        let x = from.0 + (y0 - from.1) / (to.1 - from.1) * (to.0 - from.0);
        x >= x0 && x < x0 + size
    };
    left || top
}

/// A barrier around hot corners: the pointer moving `from` -> `to` (already clamped to `all`,
/// the monitors) may not change monitor through the `size` px of a hot corner's two edges, in
/// either direction. Only the monitors in `enabled` have a corner. A blocked move ends where
/// the pointer is held on the monitor it was on, so it can slide into the corner without
/// overshooting onto the neighbour. Moves within one monitor are never touched.
pub fn hot_corner_barrier(
    from: (f64, f64),
    to: (f64, f64),
    all: &[Rect],
    enabled: &[Rect],
    size: i32,
) -> (f64, f64) {
    if size <= 0 {
        return to;
    }
    let blocked = enabled.iter().any(|rect| {
        contains(*rect, from) != contains(*rect, to) && crosses_corner_edges(*rect, from, to, size)
    });
    if !blocked {
        return to;
    }
    let home = all
        .iter()
        .copied()
        .find(|rect| contains(*rect, from))
        .or_else(|| nearest(from, all).map(|i| all[i]));
    match home {
        Some(home) => clamp_to_rects(to, &[home]),
        None => to,
    }
}
