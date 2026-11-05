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

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = (0, 0, 1920, 1080);

    fn thumb(x: i32, y: i32, w: i32, h: i32) -> Thumb {
        Thumb {
            pos: (x, y),
            size: (w, h),
            scale: 1.0,
        }
    }

    #[test]
    fn nothing_to_arrange() {
        assert!(arrange(&[], SCREEN, 16).is_empty());
    }

    #[test]
    fn a_small_window_is_centered_at_real_size() {
        let t = arrange(&[(400, 300)], SCREEN, 16);
        assert_eq!(t, vec![thumb(760, 390, 400, 300)]);
    }

    #[test]
    fn a_big_window_shrinks_to_fit() {
        let t = arrange(&[(3840, 2160)], SCREEN, 16);
        assert_eq!(t[0].pos, (0, 0));
        assert_eq!(t[0].size, (1920, 1080));
        assert_eq!(t[0].scale, 0.5);
    }

    #[test]
    fn windows_are_never_scaled_up() {
        let sizes: Vec<Size> = (0..7).map(|i| (100 + i * 50, 80 + i * 20)).collect();
        for t in arrange(&sizes, SCREEN, 16) {
            assert!(t.scale <= 1.0);
        }
    }

    #[test]
    fn three_windows_make_two_columns_with_a_centered_last_row() {
        let t = arrange(&[(800, 600); 3], SCREEN, 16);
        assert_eq!(t[0].pos.1, t[1].pos.1);
        assert!(t[2].pos.1 > t[0].pos.1);
        let center_x = t[2].pos.0 as f64 + t[2].size.0 as f64 / 2.0;
        assert!((center_x - 960.0).abs() <= 1.0, "center was {center_x}");
    }

    #[test]
    fn thumbnails_stay_inside_the_area_and_apart() {
        let area = (100, 50, 1600, 900);
        for n in 1..=12usize {
            let sizes: Vec<Size> = (0..n as i32)
                .map(|i| (300 + (i * 97) % 700, 200 + (i * 61) % 500))
                .collect();
            let thumbs = arrange(&sizes, area, 16);
            assert_eq!(thumbs.len(), n);
            for (i, t) in thumbs.iter().enumerate() {
                assert!(t.pos.0 >= area.0 && t.pos.1 >= area.1, "n={n} i={i}");
                assert!(t.pos.0 + t.size.0 <= area.0 + area.2 + 1, "n={n} i={i}");
                assert!(t.pos.1 + t.size.1 <= area.1 + area.3 + 1, "n={n} i={i}");
                for (j, u) in thumbs.iter().enumerate().skip(i + 1) {
                    assert!(
                        !layout::intersects(t.rect(), u.rect()),
                        "n={n}: {i} overlaps {j}"
                    );
                }
            }
        }
    }

    #[test]
    fn degenerate_input_does_not_panic() {
        let t = arrange(&[(0, 0), (-5, 10)], (0, 0, 0, 0), 16);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn hit_test_prefers_the_topmost() {
        let thumbs = [thumb(0, 0, 100, 100), thumb(50, 50, 100, 100)];
        assert_eq!(hit_test(&thumbs, (75.0, 75.0)), Some(1));
        assert_eq!(hit_test(&thumbs, (10.0, 10.0)), Some(0));
        assert_eq!(hit_test(&thumbs, (500.0, 500.0)), None);
        assert_eq!(hit_test(&thumbs[..1], (100.0, 50.0)), None);
    }

    #[test]
    fn neighbor_walks_a_grid() {
        let g = [
            thumb(0, 0, 100, 100),
            thumb(200, 0, 100, 100),
            thumb(0, 200, 100, 100),
            thumb(200, 200, 100, 100),
        ];
        assert_eq!(neighbor(&g, 0, Dir::Right), Some(1));
        assert_eq!(neighbor(&g, 0, Dir::Down), Some(2));
        assert_eq!(neighbor(&g, 3, Dir::Left), Some(2));
        assert_eq!(neighbor(&g, 3, Dir::Up), Some(1));
        assert_eq!(neighbor(&g, 0, Dir::Left), None);
        assert_eq!(neighbor(&g, 0, Dir::Up), None);
        assert_eq!(neighbor(&g, 9, Dir::Up), None);
    }

    #[test]
    fn neighbor_does_not_jump_rows_sideways() {
        let g = [
            thumb(0, 0, 100, 100),
            thumb(200, 0, 100, 100),
            thumb(100, 200, 100, 100),
        ];
        assert_eq!(neighbor(&g, 1, Dir::Right), None);
        assert_eq!(neighbor(&g, 1, Dir::Down), Some(2));
        assert!(neighbor(&g, 2, Dir::Up).is_some());
    }

    #[test]
    fn neighbor_falls_back_outside_the_cone() {
        let g = [thumb(0, 0, 100, 100), thumb(100, 1000, 100, 100)];
        assert_eq!(neighbor(&g, 0, Dir::Right), Some(1));
    }

    #[test]
    fn anchor_is_a_fraction_of_the_thumbnail() {
        let t = thumb(100, 200, 400, 300);
        assert_eq!(anchor_in(&t, (300.0, 350.0)), (0.5, 0.5));
        assert_eq!(anchor_in(&t, (0.0, 9999.0)), (0.0, 1.0));
    }

    #[test]
    fn drag_starts_only_past_the_threshold() {
        assert!(!moved_past((10.0, 10.0), (13.0, 14.0), 8.0));
        assert!(moved_past((10.0, 10.0), (10.0, 18.0), 8.0));
        assert!(moved_past((10.0, 10.0), (0.0, 10.0), 8.0));
    }

    #[test]
    fn dragged_thumbnail_keeps_the_grabbed_point_under_the_pointer() {
        let t = thumb(100, 200, 400, 300);
        let moved = dragged_thumb(t, (0.25, 0.5), (1000.0, 700.0));
        assert_eq!(moved.pos, (900, 550));
        assert_eq!(moved.size, t.size);
        assert_eq!(moved.scale, t.scale);
    }

    #[test]
    fn dropped_window_keeps_the_grab_ratio_at_real_size() {
        let target: Rect = (1920, 0, 1280, 1024);
        assert_eq!(
            drop_pos((0.5, 0.25), (2560.0, 500.0), (800, 400), target),
            (2160, 400)
        );
    }

    #[test]
    fn dropped_window_is_pulled_inside_the_monitor() {
        let target: Rect = (1920, 0, 1280, 1024);
        assert_eq!(
            drop_pos((0.5, 0.5), (1925.0, 5.0), (800, 400), target),
            (1920, 0)
        );
        assert_eq!(
            drop_pos((0.5, 0.5), (3199.0, 1020.0), (800, 400), target),
            (2400, 624)
        );
        assert_eq!(
            drop_pos((0.5, 0.5), (2500.0, 500.0), (5000, 5000), target),
            (1920, 0)
        );
    }

    #[test]
    fn cycling_wraps() {
        assert_eq!(cycle(3, 0, false), 1);
        assert_eq!(cycle(3, 2, false), 0);
        assert_eq!(cycle(3, 0, true), 2);
        assert_eq!(cycle(1, 0, false), 0);
        assert_eq!(cycle(0, 0, true), 0);
    }

    #[test]
    fn easing_runs_from_zero_to_one() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(-3.0), 0.0);
        assert_eq!(ease_out_cubic(7.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn progress_is_clamped() {
        let total = Duration::from_millis(200);
        assert_eq!(progress(Duration::from_millis(100), total), 0.5);
        assert_eq!(progress(Duration::from_secs(5), total), 1.0);
        assert_eq!(progress(Duration::ZERO, Duration::ZERO), 1.0);
    }

    #[test]
    fn lerp_thumb_goes_from_real_window_to_thumbnail() {
        let from = thumb(100, 100, 800, 600);
        let to = Thumb {
            pos: (0, 0),
            size: (400, 300),
            scale: 0.5,
        };
        assert_eq!(lerp_thumb(from, to, 0.0), from);
        assert_eq!(lerp_thumb(from, to, 1.0), to);
        let mid = lerp_thumb(from, to, 0.5);
        assert_eq!(mid.pos, (50, 50));
        assert_eq!(mid.size, (600, 450));
        assert_eq!(mid.scale, 0.75);
    }
}
