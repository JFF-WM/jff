//! Pure geometry of the window overview, no Smithay/Wayland types, so it can be unit-tested on
//! its own (like `layout.rs` and `geom.rs`).
//!
//! The overview shows every window of one monitor as a thumbnail in a grid. A `Thumb` says
//! where a window's geometry rect goes and how much it is scaled down.

use std::time::Duration;

use crate::primitives::geom::{Pos, Size};
use crate::primitives::layout::{self, Rect};

/// Where a window is drawn in the overview, in the shared desktop space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    /// Top-left of the (scaled) window geometry.
    pub pos: Pos,
    /// Size of the (scaled) window geometry.
    pub size: Size,
    /// How much smaller than the real window it's drawn. 1.0 for real size.
    pub scale: f64,
}

impl Thumb {
    pub fn rect(&self) -> Rect {
        (self.pos.0, self.pos.1, self.size.0, self.size.1)
    }
}

/// A direction for keyboard navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Lay out windows of `sizes` in a grid inside `area`, `gap` px apart.
///
/// Tries every column count from 1 to N. Within a cell a window is scaled to fit, never above
/// real size, and centered. The column count that leaves the most window area on screen wins,
/// among equals, the grid whose shape is closest to the area's. Rows that aren't full (the last
/// one) are centered as a whole. One entry per size, in order.
pub fn arrange(sizes: &[Size], area: Rect, gap: i32) -> Vec<Thumb> {
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }
    let area = (area.0, area.1, area.2.max(1), area.3.max(1));
    let sizes: Vec<Size> = sizes.iter().map(|s| (s.0.max(1), s.1.max(1))).collect();

    let mut best: Option<(f64, f64, Vec<Thumb>)> = None;
    for cols in 1..=n {
        let thumbs = grid(&sizes, area, gap, cols);
        let covered: f64 = thumbs
            .iter()
            .map(|t| t.size.0 as f64 * t.size.1 as f64)
            .sum();
        let rows = n.div_ceil(cols);
        let shape = (cols as f64 / rows as f64) / (area.2 as f64 / area.3 as f64);
        let mismatch = shape.ln().abs();
        let better = match &best {
            None => true,
            Some((c, m, _)) => covered > c + 1.0 || ((covered - c).abs() <= 1.0 && mismatch < *m),
        };
        if better {
            best = Some((covered, mismatch, thumbs));
        }
    }
    best.map(|b| b.2).unwrap_or_default()
}

/// The grid with exactly `cols` columns.
fn grid(sizes: &[Size], area: Rect, gap: i32, cols: usize) -> Vec<Thumb> {
    let n = sizes.len();
    let rows = n.div_ceil(cols);
    let gap_f = gap as f64;
    let cell_w = ((area.2 as f64 - gap_f * (cols as f64 - 1.0)) / cols as f64).max(1.0);
    let cell_h = ((area.3 as f64 - gap_f * (rows as f64 - 1.0)) / rows as f64).max(1.0);

    sizes
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| {
            let (row, col) = (i / cols, i % cols);
            let in_row = if row == rows - 1 {
                n - row * cols
            } else {
                cols
            };
            // A row that isn't full sits in the middle of the area.
            let row_w = in_row as f64 * cell_w + (in_row as f64 - 1.0) * gap_f;
            let row_x = area.0 as f64 + (area.2 as f64 - row_w) / 2.0;
            let cell_x = row_x + col as f64 * (cell_w + gap_f);
            let cell_y = area.1 as f64 + row as f64 * (cell_h + gap_f);

            let scale = (cell_w / w as f64).min(cell_h / h as f64).min(1.0);
            let tw = (w as f64 * scale).round().max(1.0);
            let th = (h as f64 * scale).round().max(1.0);
            let x = cell_x + (cell_w - tw) / 2.0;
            let y = cell_y + (cell_h - th) / 2.0;
            Thumb {
                pos: (x.round() as i32, y.round() as i32),
                size: (tw as i32, th as i32),
                scale,
            }
        })
        .collect()
}

/// Windows in one row at a fixed size, `gap` px apart: the layout of the window switcher.
///
/// Every thumbnail is `height` tall (or its real height, if that is less) however many windows
/// there are, and no wider than `area`. The row starts at the left edge of `area` and is
/// centered vertically, it may be longer than `area`. `strip_offset` says how far to slide it.
/// One entry per size, in order.
pub fn strip(sizes: &[Size], area: Rect, gap: i32, height: i32) -> Vec<Thumb> {
    let area = (area.0, area.1, area.2.max(1), area.3.max(1));
    let height = height.clamp(1, area.3) as f64;
    let gap = gap.max(0);
    let mut x = area.0;
    sizes
        .iter()
        .map(|&(w, h)| {
            let (w, h) = (w.max(1), h.max(1));
            let scale = (height / h as f64).min(area.2 as f64 / w as f64).min(1.0);
            let size = (
                (w as f64 * scale).round().max(1.0) as i32,
                (h as f64 * scale).round().max(1.0) as i32,
            );
            let thumb = Thumb {
                pos: (x, area.1 + (area.3 - size.1) / 2),
                size,
                scale,
            };
            x += size.0 + gap;
            thumb
        })
        .collect()
}

/// How far to slide a row made by `strip` sideways so that `selected` is in the middle of
/// `area`. A row that fits in `area` doesn't scroll, it is centered as a whole. An index past
/// the end counts as the first thumbnail.
pub fn strip_offset(thumbs: &[Thumb], selected: usize, area: Rect) -> i32 {
    let (Some(first), Some(last)) = (thumbs.first(), thumbs.last()) else {
        return 0;
    };
    let middle = area.0 + area.2 / 2;
    let (left, right) = (first.pos.0, last.pos.0 + last.size.0);
    if right - left <= area.2 {
        return middle - (left + right) / 2;
    }
    let t = thumbs.get(selected).unwrap_or(first);
    middle - (t.pos.0 + t.size.0 / 2)
}

/// `thumb`, `dx` px further right.
pub fn shifted(thumb: Thumb, dx: i32) -> Thumb {
    Thumb {
        pos: (thumb.pos.0 + dx, thumb.pos.1),
        ..thumb
    }
}

/// Where a slide from `from` to `to` is `elapsed` into its `total` time: fast start, gentle stop.
pub fn slide_at(from: f64, to: f64, elapsed: Duration, total: Duration) -> f64 {
    lerp(from, to, ease_out_cubic(progress(elapsed, total)))
}

/// Which of alt, ctrl and logo are held (in that order).
pub type Held = (bool, bool, bool);

/// Whether a modifier that was in `held` is no longer in `now`.
pub fn modifier_released(held: Held, now: Held) -> bool {
    (held.0 && !now.0) || (held.1 && !now.1) || (held.2 && !now.2)
}

/// The thumbnail under `p`. Later entries are drawn on top, so they're checked first.
pub fn hit_test(thumbs: &[Thumb], p: (f64, f64)) -> Option<usize> {
    thumbs.iter().rposition(|t| layout::contains(t.rect(), p))
}

/// The thumbnail nearest to `from` in direction `dir`, if there's one.
///
/// Thumbnails inside a 90 degree cone around the direction win, so "right" stays in the same
/// row. When the cone is empty any thumbnail on that side will do.
pub fn neighbor(thumbs: &[Thumb], from: usize, dir: Dir) -> Option<usize> {
    let origin = thumbs.get(from)?;
    let (fx, fy) = layout::center(origin.pos, origin.size);

    // (index, distance along the direction, distance across it)
    let scored: Vec<(usize, f64, f64)> = thumbs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, t)| {
            let (cx, cy) = layout::center(t.pos, t.size);
            let (dx, dy) = (cx - fx, cy - fy);
            let (along, across) = match dir {
                Dir::Right => (dx, dy.abs()),
                Dir::Left => (-dx, dy.abs()),
                Dir::Down => (dy, dx.abs()),
                Dir::Up => (-dy, dx.abs()),
            };
            (along > 0.0).then_some((i, along, across))
        })
        .collect();

    cheapest(scored.iter().copied().filter(|c| c.2 <= c.1))
        .or_else(|| cheapest(scored.iter().copied()))
}

fn cheapest(candidates: impl Iterator<Item = (usize, f64, f64)>) -> Option<usize> {
    candidates
        .min_by(|a, b| (a.1 + 2.0 * a.2).total_cmp(&(b.1 + 2.0 * b.2)))
        .map(|c| c.0)
}

/// Where `p` lies on `thumb`, as fractions of its width and height, clamped to the thumbnail.
pub fn anchor_in(thumb: &Thumb, p: (f64, f64)) -> (f64, f64) {
    let fraction =
        |at: f64, start: i32, len: i32| ((at - start as f64) / len.max(1) as f64).clamp(0.0, 1.0);
    (
        fraction(p.0, thumb.pos.0, thumb.size.0),
        fraction(p.1, thumb.pos.1, thumb.size.1),
    )
}

/// Whether `to` is at least `threshold` px away from `from`.
pub fn moved_past(from: (f64, f64), to: (f64, f64), threshold: f64) -> bool {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    dx * dx + dy * dy >= threshold * threshold
}

/// `thumb` moved so its `anchor` (see `anchor_in`) is under `p`.
pub fn dragged_thumb(thumb: Thumb, anchor: (f64, f64), p: (f64, f64)) -> Thumb {
    Thumb {
        pos: (
            (p.0 - anchor.0 * thumb.size.0 as f64).round() as i32,
            (p.1 - anchor.1 * thumb.size.1 as f64).round() as i32,
        ),
        ..thumb
    }
}

/// Where a window of real `size` goes when its thumbnail is dropped with the pointer at `p` on
/// `target`: the grabbed point stays under the pointer, and the window is pulled inside the
/// monitor as far as it fits.
pub fn drop_pos(anchor: (f64, f64), p: (f64, f64), size: Size, target: Rect) -> Pos {
    let pos = (
        (p.0 - anchor.0 * size.0 as f64).round() as i32,
        (p.1 - anchor.1 * size.1 as f64).round() as i32,
    );
    layout::fit_into(pos, size, target)
}

/// The squares of the workspace wall: one per workspace, in a row centered along the bottom of
/// `area`, `bottom_gap` px above its lower edge. Every square has the shape of a monitor of
/// `monitor` size and is `height` px tall, shrunk (all together) if the row would be wider than
/// 90% of `area`. Empty for no workspaces.
pub fn wall(
    count: usize,
    area: Rect,
    monitor: Size,
    height: i32,
    gap: i32,
    bottom_gap: i32,
) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let (mw, mh) = (monitor.0.max(1) as f64, monitor.1.max(1) as f64);
    let n = count as f64;
    let gaps = (count as i32 - 1) * gap;
    let mut h = height.max(1) as f64;
    let mut w = h * mw / mh;
    let room = area.2 as f64 * 0.9 - gaps as f64;
    if n * w > room {
        let shrink = (room / (n * w)).max(0.05);
        w *= shrink;
        h *= shrink;
    }
    let (w, h) = ((w.round() as i32).max(1), (h.round() as i32).max(1));
    let total = count as i32 * w + gaps;
    let x0 = area.0 + (area.2 - total) / 2;
    let y = area.1 + area.3 - bottom_gap - h;
    (0..count as i32)
        .map(|i| (x0 + i * (w + gap), y, w, h))
        .collect()
}

/// Where a window at `pos` with geometry `size` goes inside the wall square `cell`, which shows
/// the whole of the monitor at `monitor`: the monitor shrunk to the square's width.
pub fn wall_thumb(cell: Rect, monitor: Rect, pos: Pos, size: Size) -> Thumb {
    let scale = f64::from(cell.2) / f64::from(monitor.2.max(1));
    let at =
        |v: i32, origin: i32, start: i32| start + (f64::from(v - origin) * scale).round() as i32;
    Thumb {
        pos: (at(pos.0, monitor.0, cell.0), at(pos.1, monitor.1, cell.1)),
        size: (
            ((f64::from(size.0) * scale).round() as i32).max(1),
            ((f64::from(size.1) * scale).round() as i32).max(1),
        ),
        scale,
    }
}

/// The square of the wall `p` is in, if any.
pub fn wall_hit(cells: &[Rect], p: (f64, f64)) -> Option<usize> {
    cells.iter().position(|cell| layout::contains(*cell, p))
}

/// The next (or previous) index out of `len`, wrapping around.
pub fn cycle(len: usize, current: usize, backwards: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let current = current % len;
    if backwards {
        (current + len - 1) % len
    } else {
        (current + 1) % len
    }
}

/// How far along an animation of length `total` is after `elapsed`, 0.0 to 1.0.
pub fn progress(elapsed: Duration, total: Duration) -> f64 {
    if total.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
}

/// Fast start, gentle stop.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Between where a window really is (`from`, normally scale 1.0) and its thumbnail (`to`).
/// `t` is 0.0 at the real window, 1.0 at the thumbnail.
pub fn lerp_thumb(from: Thumb, to: Thumb, t: f64) -> Thumb {
    let mix = |a: i32, b: i32| lerp(a as f64, b as f64, t).round() as i32;
    Thumb {
        pos: (mix(from.pos.0, to.pos.0), mix(from.pos.1, to.pos.1)),
        size: (mix(from.size.0, to.size.0), mix(from.size.1, to.size.1)),
        scale: lerp(from.scale, to.scale, t),
    }
}

/// Thumbnails whose short side is under this get no app icon: it would cover them.
pub const BADGE_MIN_THUMB: i32 = 72;
/// Smallest and biggest side of an app icon badge, in pixels.
pub const BADGE_MIN: i32 = 32;
pub const BADGE_MAX: i32 = 80;

/// The square an app icon badge covers on a thumbnail at `thumb`: centred along the bottom
/// edge, a little above it. `None` when the thumbnail is too small to carry one.
pub fn badge_rect(thumb: Rect) -> Option<Rect> {
    let short = thumb.2.min(thumb.3);
    if short < BADGE_MIN_THUMB {
        return None;
    }
    let side = ((f64::from(short) * 0.28).round() as i32).clamp(BADGE_MIN, BADGE_MAX);
    let inset = (side / 4).max(6);
    Some((
        thumb.0 + (thumb.2 - side) / 2,
        thumb.1 + thumb.3 - side - inset,
        side,
        side,
    ))
}
