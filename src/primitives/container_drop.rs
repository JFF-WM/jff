//! Where a dragged window has to be let go to start a tiling container, and how a window that
//! joins one is split off, no Smithay/Wayland types, so it can be unit-tested on its own.
//!
//! A monitor with a window snapped to one side has an empty half: the other one. A window
//! dropped in it does not float there, it joins the snapped window in a new container, and the
//! two split the room along the axis the snap already used.
//!
//! A window that joins a container that exists already is split off one of its leaves, along
//! the longer side of that leaf's rectangle (see [`split_axis`]).
//!
//! A window of a container that is dragged onto a leaf of one lands by where the pointer is
//! on that leaf (see [`leaf_drop`]): the middle swaps the two, an edge puts it beside the leaf on
//! that side.

use crate::desk::Side;
use crate::primitives::tile_tree::{Axis, Rect};

/// The axis of the split for a window snapped to `side`: left and right sit next to each other,
/// top and bottom one above the other.
pub fn axis_for(side: Side) -> Axis {
    match side {
        Side::Left | Side::Right => Axis::Horizontal,
        Side::Top | Side::Bottom => Axis::Vertical,
    }
}

/// The other side of the same axis.
pub fn opposite(side: Side) -> Side {
    match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
        Side::Top => Side::Bottom,
        Side::Bottom => Side::Top,
    }
}

/// Whether the window snapped to `side` is the first half of the split, the left or the top
/// one. The tree puts a new window in the second half, so for the right and the bottom the
/// dropped window has to be the first leaf and the snapped one the second: nothing changes
/// sides.
pub fn snapped_is_first(side: Side) -> bool {
    matches!(side, Side::Left | Side::Top)
}

/// Whether `p` is in the half of `area` that a window snapped to `side` leaves empty. `area` is
/// what the snapped windows share (the monitor without its panels). The middle line belongs to
/// the right or bottom half, the same as for the snapped window itself, and a point outside
/// `area`, or in an `area` with no room, is in no half.
pub fn in_empty_half(area: Rect, side: Side, p: (f64, f64)) -> bool {
    let (x, y, w, h) = area;
    if w <= 0 || h <= 0 {
        return false;
    }
    let (left, top) = (f64::from(x), f64::from(y));
    let (right, bottom) = (f64::from(x) + f64::from(w), f64::from(y) + f64::from(h));
    if p.0 < left || p.0 >= right || p.1 < top || p.1 >= bottom {
        return false;
    }
    let (mid_x, mid_y) = (left + f64::from(w) / 2.0, top + f64::from(h) / 2.0);
    match side {
        Side::Left => p.0 >= mid_x,
        Side::Right => p.0 < mid_x,
        Side::Top => p.1 >= mid_y,
        Side::Bottom => p.1 < mid_y,
    }
}

/// The axis to split a leaf of `rect` along when a window joins it: along its longer side, so
/// the two halves come out closer to square. A wide leaf is cut into a left and a right half
/// (`Horizontal`), a tall one into a top and a bottom half (`Vertical`). A square leaf, and one
/// with no room, take `Horizontal`.
pub fn split_axis(rect: Rect) -> Axis {
    if rect.3 > rect.2 {
        Axis::Vertical
    } else {
        Axis::Horizontal
    }
}

/// Whether `p` is inside `rect`. The left and top edges belong to it, the right and bottom
/// ones do not, so two tiles that touch never both hold a point. A `rect` with no room holds
/// nothing.
pub fn in_rect(rect: Rect, p: (f64, f64)) -> bool {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return false;
    }
    let (left, top) = (f64::from(x), f64::from(y));
    p.0 >= left && p.0 < left + f64::from(w) && p.1 >= top && p.1 < top + f64::from(h)
}

/// Share of a leaf, from each edge, where a drop goes beside it and not onto it.
pub const DROP_EDGE_SHARE: f64 = 0.25;

/// What dropping a window of a container on a leaf of a container does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafDrop {
    /// The dragged window and the leaf exchange places.
    Swap,
    /// The dragged window goes beside the leaf, on this side of it, and the two share the
    /// leaf's room.
    Beside(Side),
}

/// What a drop at `p` on a leaf with rectangle `rect` does. The outer quarter of the leaf along
/// each side puts the window beside it on that side, the middle swaps. In a corner, where two
/// edge bands meet, the side `p` is nearer to (as a share of the leaf) wins, and a tie goes to
/// the left or right. A `rect` with no room, and a point outside it, swap.
pub fn leaf_drop(rect: Rect, p: (f64, f64)) -> LeafDrop {
    if !in_rect(rect, p) {
        return LeafDrop::Swap;
    }
    let (x, y, w, h) = rect;
    let across = (p.0 - f64::from(x)) / f64::from(w);
    let down = (p.1 - f64::from(y)) / f64::from(h);
    let candidates = [
        (across, Side::Left),
        (1.0 - across, Side::Right),
        (down, Side::Top),
        (1.0 - down, Side::Bottom),
    ];
    let mut best = (f64::INFINITY, Side::Left);
    for (share, side) in candidates {
        if share < best.0 {
            best = (share, side);
        }
    }
    if best.0 < DROP_EDGE_SHARE {
        LeafDrop::Beside(best.1)
    } else {
        LeafDrop::Swap
    }
}
