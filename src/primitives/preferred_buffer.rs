//! Which monitor a window takes its preferred buffer scale and transform from
//! (`wl_surface.preferred_buffer_scale`, `wl_surface.preferred_buffer_transform`).

/// The monitor a window follows. `monitors` has one item for each monitor: whether the window is
/// shown on it, and its scale. Of the monitors the window is shown on, the one with the highest
/// scale wins (the first of equals), so the window is sharp on all of them. A window that is
/// shown on none (hidden, or off screen) follows `home`, the monitor it belongs to, so it draws
/// at the right scale from the start. `None` when there is nothing to follow.
pub fn follow_monitor(
    monitors: impl IntoIterator<Item = (bool, f64)>,
    home: Option<usize>,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, (shown, scale)) in monitors.into_iter().enumerate() {
        if !shown {
            continue;
        }
        if best.map_or(true, |(_, top)| scale > top) {
            best = Some((i, scale));
        }
    }
    best.map(|(i, _)| i).or(home)
}
