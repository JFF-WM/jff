//! When a window is told it is suspended (`xdg_toplevel` state `suspended`): nobody can see it,
//! so its client may stop drawing, pause video and drop animations.

/// Whether a window that is shown is hidden by the fullscreen windows that cover monitors.
/// `monitors` has one item for each monitor: whether the window is on it, and the window that
/// covers it (`None`: nothing covers it). The window is behind a cover when it is on at least one
/// monitor and on every one it is on, another window covers that monitor. A window that is on
/// no monitor is not behind anything.
pub fn behind_cover<T: PartialEq>(
    me: T,
    monitors: impl IntoIterator<Item = (bool, Option<T>)>,
) -> bool {
    let mut on_any = false;
    for (on, cover) in monitors {
        if !on {
            continue;
        }
        on_any = true;
        match cover {
            Some(cover) if cover != me => {}
            _ => return false,
        }
    }
    on_any
}

/// Whether a window is suspended. `shown`: it is on the workspace that is shown and not put
/// away. `fitted`: it has shown something already, before that it is never suspended, a client
/// that is told it is hidden before its first frame may never draw one.
pub fn suspended(shown: bool, fitted: bool, behind: bool) -> bool {
    fitted && (!shown || behind)
}
