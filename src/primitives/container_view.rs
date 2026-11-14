//! How a tiling container looks and what a click on it hits, no Smithay/Wayland types, so it
//! can be unit-tested on its own.
//!
//! A container is drawn with a border around its rectangle. In the overview it is one item: a
//! cell of the grid that holds the container's rectangle at a smaller scale, with the name
//! above, and its windows laid out inside the cell where the tree put them. Every number here
//! is in real pixels at scale 1.0 and is scaled down with the cell.

use crate::primitives::geom::Size;
use crate::primitives::layout::{self, Rect};
use crate::primitives::overview_grid::Thumb;

/// Room around the windows of a cell, at both sides and at the bottom, in real pixels.
pub const CELL_PAD: i32 = 24;
/// Room above the windows of a cell, for the name, in real pixels.
pub const CELL_LABEL: i32 = 96;
/// How far outside the container's rectangle a click still counts as on its border, at least.
/// A border is a few pixels wide, too thin to aim at.
pub const BORDER_GRAB: i32 = 6;

/// The smallest rectangle that holds all of `rects`, `None` for none.
pub fn bounds(rects: &[Rect]) -> Option<Rect> {
    let (first, rest) = rects.split_first()?;
    let (mut left, mut top) = (first.0, first.1);
    let (mut right, mut bottom) = (first.0 + first.2, first.1 + first.3);
    for rect in rest {
        left = left.min(rect.0);
        top = top.min(rect.1);
        right = right.max(rect.0 + rect.2);
        bottom = bottom.max(rect.1 + rect.3);
    }
    Some((left, top, right - left, bottom - top))
}

/// The size a container takes in the overview grid, before it is scaled to fit: its rectangle
/// and the room around it.
pub fn cell_size(area: Size) -> Size {
    (
        area.0.max(1) + 2 * CELL_PAD,
        area.1.max(1) + CELL_LABEL + CELL_PAD,
    )
}

/// `value` real pixels at the scale of `cell`.
fn scaled(value: i32, cell: &Thumb) -> i32 {
    (f64::from(value) * cell.scale).round() as i32
}

/// Where the windows of a container go inside its `cell`: each of `leaves` (where the window is
/// in real space) keeps its place and size relative to `area`, the container's rectangle,
/// scaled like the cell. One thumbnail per leaf, in order, none under 1 px.
pub fn leaf_thumbs(cell: Thumb, area: Rect, leaves: &[Rect]) -> Vec<Thumb> {
    leaves
        .iter()
        .map(|leaf| Thumb {
            pos: (
                cell.pos.0 + scaled(CELL_PAD + leaf.0 - area.0, &cell),
                cell.pos.1 + scaled(CELL_LABEL + leaf.1 - area.1, &cell),
            ),
            size: (scaled(leaf.2, &cell).max(1), scaled(leaf.3, &cell).max(1)),
            scale: cell.scale,
        })
        .collect()
}

/// The strip across the top of `cell` the name goes in, inside its side padding.
pub fn label_rect(cell: Thumb) -> Rect {
    let pad = scaled(CELL_PAD, &cell);
    (
        cell.pos.0 + pad,
        cell.pos.1,
        (cell.size.0 - 2 * pad).max(1),
        scaled(CELL_LABEL, &cell).max(1),
    )
}

/// What a container is called in the overview: its name, or when it has none the app id of its
/// first window. Empty when it has neither.
pub fn label(name: Option<&str>, first_app_id: &str) -> String {
    match name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => name.to_owned(),
        None => first_app_id.trim().to_owned(),
    }
}

/// Whether `p` is on the border of the container whose rectangle is `area`: outside it, and
/// within `reach` px of it (at least [`BORDER_GRAB`]), corners included. Inside the rectangle is
/// the windows', not the border's.
pub fn on_border(area: Rect, reach: i32, p: (f64, f64)) -> bool {
    if area.2 <= 0 || area.3 <= 0 {
        return false;
    }
    let reach = reach.max(BORDER_GRAB);
    let outer = (
        area.0 - reach,
        area.1 - reach,
        area.2 + 2 * reach,
        area.3 + 2 * reach,
    );
    layout::contains(outer, p) && !layout::contains(area, p)
}

/// The size a container of real size `old` has once it is dropped on `target`: the same, unless
/// the monitor is smaller.
pub fn fit_size(old: Size, target: Rect) -> Size {
    (old.0.min(target.2).max(1), old.1.min(target.3).max(1))
}
