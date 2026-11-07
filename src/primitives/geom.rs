//! Pure resize geometry, no Smithay/Wayland types, so it can be unit-tested on its own.

use crate::desk::Side;

pub type Pos = (i32, i32);
pub type Size = (i32, i32);

/// Which window edges the user is dragging.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Where a window snapped to `side` of `area` goes. The outer edges keep `inset` px free, the
/// edge shared with the window on the other side leaves `gap` px between the two, half of it
/// on each. Like `inset_rect`, it never returns less than 1px per window.
pub fn snapped_rect(
    area: (i32, i32, i32, i32),
    inset: i32,
    gap: i32,
    side: Side,
) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = inset_rect(area, inset);
    // Offset and length of one half along an axis of length `len`.
    let half = |len: i32, second: bool| {
        if len <= 1 {
            return (0, len);
        }
        let gap = gap.clamp(0, len - 2);
        let mid = len / 2;
        if second {
            let start = mid + (gap - gap / 2);
            (start, len - start)
        } else {
            (0, mid - gap / 2)
        }
    };
    match side {
        Side::Left | Side::Right => {
            let (dx, w) = half(w, side == Side::Right);
            (x + dx, y, w, h)
        }
        Side::Top | Side::Bottom => {
            let (dy, h) = half(h, side == Side::Bottom);
            (x, y + dy, w, h)
        }
    }
}

/// Left and right win in the corners, the bottom edge doesn't count.
pub fn edge_at(rect: (i32, i32, i32, i32), p: (f64, f64), margin: f64) -> Option<Side> {
    let (x, y, w, _) = rect;
    if p.0 <= x as f64 + margin {
        Some(Side::Left)
    } else if p.0 >= (x + w) as f64 - margin {
        Some(Side::Right)
    } else if p.1 <= y as f64 + margin {
        Some(Side::Top)
    } else {
        None
    }
}

pub fn pointer_anchored_pos(from: (i32, i32, i32, i32), size: Size, p: (f64, f64)) -> Pos {
    let (x, y, w, h) = from;
    let ratio = |at: f64, start: i32, len: i32| {
        if len > 0 {
            ((at - start as f64) / len as f64).clamp(0.0, 1.0)
        } else {
            0.5
        }
    };
    (
        (p.0 - ratio(p.0, x, w) * size.0 as f64).round() as i32,
        (p.1 - ratio(p.1, y, h) * size.1 as f64).round() as i32,
    )
}

/// Whether the window is bigger than `area` on either axis.
pub fn overflows(size: Size, area: Size) -> bool {
    size.0 > area.0 || size.1 > area.1
}

/// Shrink `size` so the window fits inside `area` with `margin` px on every side. Never grows,
/// never returns less than 1px.
pub fn fit_size(size: Size, area: Size, margin: i32) -> Size {
    (
        size.0.min((area.0 - 2 * margin).max(1)),
        size.1.min((area.1 - 2 * margin).max(1)),
    )
}

/// Shrink the rectangle `(x, y, w, h)` by `gap` px on every side. The gap is capped so the
/// result is never smaller than 1x1, and a negative gap counts as 0.
pub fn inset_rect(rect: (i32, i32, i32, i32), gap: i32) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = rect;
    // Largest gap that still leaves one pixel on the narrower axis.
    let gap = gap.clamp(0, (w.min(h) - 1).max(0) / 2);
    (x + gap, y + gap, w - 2 * gap, h - 2 * gap)
}

/// The corner nearest to `p` (window-local). Used when the user starts a resize from the middle
/// of a window, not from a specific edge.
pub fn nearest_corner(p: (f64, f64), size: Size) -> Edges {
    let left = p.0 < size.0 as f64 / 2.0;
    let top = p.1 < size.1 as f64 / 2.0;
    Edges {
        left,
        right: !left,
        top,
        bottom: !top,
    }
}

/// New size and position after dragging `edges` by `delta`.
///
/// The opposite edge stays put, so dragging the left or top edge also moves the window.
/// `min`/`max` come from the client, a `max` component of 0 means no limit.
pub fn resize(
    initial_pos: Pos,
    initial_size: Size,
    delta: (i32, i32),
    edges: Edges,
    min: Size,
    max: Size,
) -> (Pos, Size) {
    let axis = |start: i32, d: i32, grow_after: bool, grow_before: bool, min: i32, max: i32| {
        let wanted = if grow_after {
            start + d
        } else if grow_before {
            start - d
        } else {
            start
        };
        let wanted = wanted.max(min.max(1));
        if max > 0 {
            wanted.min(max.max(min))
        } else {
            wanted
        }
    };

    let w = axis(
        initial_size.0,
        delta.0,
        edges.right,
        edges.left,
        min.0,
        max.0,
    );
    let h = axis(
        initial_size.1,
        delta.1,
        edges.bottom,
        edges.top,
        min.1,
        max.1,
    );

    let x = if edges.left {
        initial_pos.0 + initial_size.0 - w
    } else {
        initial_pos.0
    };
    let y = if edges.top {
        initial_pos.1 + initial_size.1 - h
    } else {
        initial_pos.1
    };
    ((x, y), (w, h))
}

/// Where the window sits at `size` when the opposite edges must stay put. Used with the size
/// the client really has, which can lag behind (or differ from) the size we asked for.
pub fn anchored_pos(initial_pos: Pos, initial_size: Size, size: Size, edges: Edges) -> Pos {
    (
        if edges.left {
            initial_pos.0 + initial_size.0 - size.0
        } else {
            initial_pos.0
        },
        if edges.top {
            initial_pos.1 + initial_size.1 - size.1
        } else {
            initial_pos.1
        },
    )
}
