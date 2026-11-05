//! `ext-foreign-toplevel-list-v1`: the standard, read-only list of open windows (title, app id,
//! stable identifier) that taskbars, docks, and screen-capture pickers use. It can't control
//! windows, that's `wlr-foreign-toplevel-management`.
//!
//! Smithay does the protocol, this file says which windows are on the list. Like `jff msg
//! watch`, changes come from comparing, not from calling a function from every place that
//! changes a window: once per event-loop iteration, `JffState::publish_ext_foreign_toplevels`
//! takes the view list (the one `jff msg views` shows), compares it with what clients were
//! told, and sends the difference.
//!
//! A window is on the list from the moment it has a size (same rule as `jff msg views`) until
//! it's closed, on every workspace. The identifier is `jff-<view id>`: never changes while the
//! window lives, never reused. Only title and app id are sent, the protocol has nothing else.

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};

use crate::state::JffState;

/// Everything the protocol needs to keep between requests.
pub struct ExtForeignToplevel {
    list: ForeignToplevelListState,
    /// The windows on the list, by view id.
    toplevels: Vec<(u64, ForeignToplevelHandle)>,
}

impl ExtForeignToplevel {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            list: ForeignToplevelListState::new::<JffState>(display),
            toplevels: Vec::new(),
        }
    }
}

impl ForeignToplevelListHandler for JffState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.ext_foreign_toplevel.list
    }
}

impl JffState {
    /// Tell every client that bound the protocol what changed since the last call. Runs once per
    /// event-loop iteration.
    pub fn publish_ext_foreign_toplevels(&mut self) {
        let views = self.view_list();
        let ft = &mut self.ext_foreign_toplevel;

        // Windows that went away: `closed` goes to clients, then the handle is dropped.
        ft.toplevels.retain(|(id, handle)| {
            let alive = views.iter().any(|v| v.id == *id);
            if !alive {
                ft.list.remove_toplevel(handle);
            }
            alive
        });

        for view in &views {
            let title = view.title.as_deref().unwrap_or_default();
            let app_id = view.app_id.as_deref().unwrap_or_default();
            match ft.toplevels.iter().find(|(id, _)| *id == view.id) {
                // Not new: send what changed.
                Some((_, handle)) => {
                    let mut changed = false;
                    if handle.title() != title {
                        handle.send_title(title);
                        changed = true;
                    }
                    if handle.app_id() != app_id {
                        handle.send_app_id(app_id);
                        changed = true;
                    }
                    if changed {
                        handle.send_done();
                    }
                }
                // New.
                None => {
                    let handle = ft.list.new_toplevel_with_identifier::<JffState>(
                        title,
                        app_id,
                        format!("jff-{}", view.id),
                    );
                    ft.toplevels.push((view.id, handle));
                }
            }
        }
    }
}
