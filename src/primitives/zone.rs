//! Pure geometry of xx-zones: where a window is inside a zone, and how a zone moves as a unit.
//!
//! A zone has an `origin` in the shared desktop space, the client only ever sees coordinates
//! relative to it. Windows keep their desktop position on the desk, so everything that already
//! reads it (drawing, hit-testing, snap, scanout, the session) is untouched.

use crate::primitives::geom::{Pos, Size};
use crate::primitives::layout::Rect;

/// Where `pos` (desktop space) is, seen from a zone at `origin`.
pub fn to_zone(origin: Pos, pos: Pos) -> Pos {
    (pos.0 - origin.0, pos.1 - origin.1)
}

/// The desktop position of the zone coordinates `rel`, for a zone at `origin`.
pub fn to_desktop(origin: Pos, rel: Pos) -> Pos {
    (origin.0 + rel.0, origin.1 + rel.1)
}

/// `pos` moved by `delta`.
pub fn shifted(pos: Pos, delta: Pos) -> Pos {
    (pos.0 + delta.0, pos.1 + delta.1)
}

/// How far `to` is from `from`.
pub fn delta(from: Pos, to: Pos) -> Pos {
    (to.0 - from.0, to.1 - from.1)
}

/// The zone coordinates jff accepts for a request for `rel` by a window `size` big in a zone
/// `zone` big: the window stays inside the zone. A zone side of 0 is infinite, nothing
/// bounds it there, except that a window may not start before the zone does.
pub fn sanitize(rel: Pos, size: Size, zone: Size) -> Pos {
    let clamp = |at: i32, window: i32, room: i32| {
        let at = at.max(0);
        if room <= 0 {
            at
        } else {
            at.min((room - window).max(0))
        }
    };
    (clamp(rel.0, size.0, zone.0), clamp(rel.1, size.1, zone.1))
}

/// The box around the rectangles `(pos, size)`. `None` for no rectangles.
pub fn bounds_of(windows: &[(Pos, Size)]) -> Option<(Pos, Size)> {
    let first = windows.first()?;
    let mut b = (
        first.0 .0,
        first.0 .1,
        first.0 .0 + first.1 .0,
        first.0 .1 + first.1 .1,
    );
    for &(pos, size) in &windows[1..] {
        b.0 = b.0.min(pos.0);
        b.1 = b.1.min(pos.1);
        b.2 = b.2.max(pos.0 + size.0);
        b.3 = b.3.max(pos.1 + size.1);
    }
    Some(((b.0, b.1), (b.2 - b.0, b.3 - b.1)))
}

/// The middle of the box around the rectangles, as a point. `None` for no rectangles.
pub fn center_of(windows: &[(Pos, Size)]) -> Option<(f64, f64)> {
    let (pos, size) = bounds_of(windows)?;
    Some((
        pos.0 as f64 + size.0 as f64 / 2.0,
        pos.1 as f64 + size.1 as f64 / 2.0,
    ))
}

/// The origin a zone gets on a monitor when it has nothing to go by: the top-left of `area`
/// (the part of the monitor panels left free).
pub fn default_origin(area: Rect) -> Pos {
    (area.0, area.1)
}

/// The offset of `origin` from the monitor at `monitor`, to put the zone back at the same place
/// on that monitor when the monitor moves.
pub fn offset_in(monitor: Rect, origin: Pos) -> Pos {
    (origin.0 - monitor.0, origin.1 - monitor.1)
}

/// The origin for a zone that sits at `offset` on the monitor at `monitor`.
pub fn origin_in(monitor: Rect, offset: Pos) -> Pos {
    (monitor.0 + offset.0, monitor.1 + offset.1)
}
