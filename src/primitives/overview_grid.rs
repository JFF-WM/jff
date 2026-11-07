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

/// Windows in one row, centered in `area`, `gap` px apart: the layout of the window switcher.
/// Every thumbnail is as tall as the row allows (or its real height, if that is less), no taller
/// than `max_height`, and the row fits in `area`. One entry per size, in order.
pub fn strip(sizes: &[Size], area: Rect, gap: i32, max_height: i32) -> Vec<Thumb> {
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }
    let area = (area.0, area.1, area.2.max(1), area.3.max(1));
    let sizes: Vec<Size> = sizes.iter().map(|s| (s.0.max(1), s.1.max(1))).collect();
    // Rounding can add up to a pixel per window, keep that much room.
    let slack = n as i32;
    let gap = if n > 1 {
        gap.clamp(0, (area.2 - 2 * slack).max(0) / (n as i32 - 1))
    } else {
        0
    };
    let scale_at = |height: f64, size: Size| (height / size.1 as f64).min(1.0);
    let row_w = |height: f64| {
        let windows: f64 = sizes
            .iter()
            .map(|s| s.0 as f64 * scale_at(height, *s))
            .sum();
        windows + (gap * (n as i32 - 1)) as f64
    };
    let budget = (area.2 - slack) as f64;
    let mut height = max_height.clamp(1, area.3) as f64;
    if row_w(height) > budget {
        let (mut lo, mut hi) = (0.0, height);
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if row_w(mid) <= budget {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        height = lo;
    }

    let sizes_out: Vec<(Size, f64)> = sizes
        .iter()
        .map(|&s| {
            let scale = scale_at(height, s);
            let tw = (s.0 as f64 * scale).round().max(1.0) as i32;
            let th = (s.1 as f64 * scale).round().max(1.0) as i32;
            ((tw, th), scale)
        })
        .collect();
    let total: i32 = sizes_out.iter().map(|(s, _)| s.0).sum::<i32>() + gap * (n as i32 - 1);
    let mut x = area.0 + (area.2 - total) / 2;
    sizes_out
        .into_iter()
        .map(|(size, scale)| {
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
