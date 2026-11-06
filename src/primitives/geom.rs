//! Pure resize geometry, no Smithay/Wayland types, so it can be unit-tested on its own.

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

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Size = (0, 0);
    const BR: Edges = Edges {
        left: false,
        right: true,
        top: false,
        bottom: true,
    };
    const TL: Edges = Edges {
        left: true,
        right: false,
        top: true,
        bottom: false,
    };

    #[test]
    fn exact_fit_is_not_an_overflow() {
        assert!(!overflows((1900, 1016), (1900, 1016)));
        assert!(overflows((1901, 1016), (1900, 1016)));
        assert!(overflows((100, 2000), (1900, 1016)));
    }

    #[test]
    fn fit_shrinks_only_what_overflows() {
        assert_eq!(fit_size((1200, 700), (1000, 800), 40), (920, 700));
        assert_eq!(fit_size((300, 200), (1000, 800), 40), (300, 200));
        assert_eq!(fit_size((300, 200), (50, 50), 40), (1, 1));
    }

    #[test]
    fn inset_shrinks_every_side() {
        assert_eq!(inset_rect((0, 0, 1920, 1080), 0), (0, 0, 1920, 1080));
        assert_eq!(inset_rect((0, 30, 1920, 1050), 10), (10, 40, 1900, 1030));
        assert_eq!(inset_rect((1920, 0, 800, 600), 8), (1928, 8, 784, 584));
    }

    #[test]
    fn inset_never_collapses() {
        assert_eq!(inset_rect((0, 0, 100, 40), 500), (19, 19, 62, 2));
        assert_eq!(inset_rect((0, 0, 1, 1), 5), (0, 0, 1, 1));
        assert_eq!(inset_rect((5, 5, 100, 100), -3), (5, 5, 100, 100));
    }

    #[test]
    fn nearest_corner_picks_quadrant() {
        assert_eq!(nearest_corner((10.0, 10.0), (200, 100)), TL);
        assert_eq!(nearest_corner((190.0, 90.0), (200, 100)), BR);
        let tr = nearest_corner((190.0, 10.0), (200, 100));
        assert!(tr.right && tr.top && !tr.left && !tr.bottom);
    }

    #[test]
    fn bottom_right_grows_without_moving() {
        assert_eq!(
            resize((10, 20), (100, 80), (30, 15), BR, NONE, NONE),
            ((10, 20), (130, 95))
        );
    }

    #[test]
    fn top_left_keeps_far_corner_fixed() {
        let (pos, size) = resize((50, 50), (100, 80), (-10, -10), TL, NONE, NONE);
        assert_eq!((pos, size), ((40, 40), (110, 90)));
        assert_eq!((pos.0 + size.0, pos.1 + size.1), (150, 130));
    }

    #[test]
    fn respects_min_and_anchors_on_clamp() {
        let (pos, size) = resize((50, 50), (100, 80), (90, 0), TL, (40, 40), NONE);
        assert_eq!(size, (40, 80));
        assert_eq!(pos.0 + size.0, 150);
    }

    #[test]
    fn anchoring_follows_the_real_size() {
        assert_eq!(anchored_pos((50, 50), (100, 80), (110, 90), TL), (40, 40));
        assert_eq!(anchored_pos((50, 50), (100, 80), (97, 90), TL), (53, 40));
        assert_eq!(anchored_pos((50, 50), (100, 80), (130, 95), BR), (50, 50));
    }

    #[test]
    fn respects_max() {
        let (_, size) = resize((0, 0), (100, 80), (500, 500), BR, NONE, (300, 0));
        assert_eq!(size, (300, 580));
    }

    #[test]
    fn never_collapses_below_one() {
        let (_, size) = resize((0, 0), (100, 80), (-500, -500), BR, NONE, NONE);
        assert_eq!(size, (1, 1));
    }
}
