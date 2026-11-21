//! `wlr-foreign-toplevel-management-unstable-v1`: taskbars and docks (waybar's `wlr/taskbar`,
//! nwg-panel, yambar, ...) list open windows and ask jff to focus, maximize, fullscreen, or
//! close them.
//!
//! Like `jff msg watch`, changes come from comparing, not from calling an `emit` from every
//! place that changes a window: once per event-loop iteration, `JffState::publish_foreign_toplevels`
//! takes the window list (the one `jff msg windows` shows), compares it with what clients were told,
//! and sends the difference. Nothing is computed while no client has bound the protocol.
//!
//! `set_rectangle` says where the taskbar's button for a window is (in the taskbar's own surface).
//! It is kept per window and read when the window is minimized or restored: the minimize then
//! flies the window's picture into the button, and the restore flies it back out (see
//! `feature::window_anim`). The rectangle is turned into the shared space when it is used, so a
//! taskbar that has moved since still gets it right. Without one there is no animation, the
//! window just goes or comes. A zero width and height takes the rectangle away again.
//!
//! The `parent` event (version 3) says which window a dialog belongs to, so a taskbar can group
//! it under that window. It follows the transient parent of the toplevel, and is sent once both
//! windows have been announced.
//!
//! Not supported:
//! - urgency: the protocol has no state for "asks for attention" (only maximized, minimized,
//!   activated and fullscreen), so a window that is urgent looks like any other here. The
//!   `urgent` member and event of `jff msg` and the `urgent` state of `ext-workspace-v1` carry it.
//!
//! Windows on every workspace are announced, not just the ones on the shown workspace. A window
//! is announced once it has a size (same rule as `jff msg windows`).

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::ipc::events::WindowInfo;
use crate::ipc::WindowId;
use crate::primitives::layout::Rect;
use crate::protocol::text_input::surface_rect;
use crate::state::JffState;

/// Highest version jff speaks. 3 adds the `parent` event.
const VERSION: u32 = 3;

/// `zwlr_foreign_toplevel_handle_v1::state` values.
const STATE_MAXIMIZED: u32 = 0;
const STATE_MINIMIZED: u32 = 1;
const STATE_ACTIVATED: u32 = 2;
const STATE_FULLSCREEN: u32 = 3;

/// What a client is told about one window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Info {
    title: String,
    app_id: String,
    /// Name of the monitor the window is on.
    output: Option<String>,
    maximized: bool,
    /// The window is out of sight on purpose (minimized, or in the scratchpad).
    minimized: bool,
    fullscreen: bool,
    /// The window has the keyboard.
    activated: bool,
}

impl Info {
    fn from_window(window: &WindowInfo, minimized: bool) -> Self {
        Self {
            title: window.title.clone().unwrap_or_default(),
            app_id: window.app_id.clone().unwrap_or_default(),
            output: window.output.clone(),
            maximized: window.maximized,
            minimized,
            fullscreen: window.fullscreen,
            activated: window.focused,
        }
    }

    /// The `state` array: native-endian `u32`s.
    fn state_bytes(&self, version: u32) -> Vec<u8> {
        let mut states = Vec::new();
        if self.maximized {
            states.push(STATE_MAXIMIZED);
        }
        if self.minimized {
            states.push(STATE_MINIMIZED);
        }
        if self.activated {
            states.push(STATE_ACTIVATED);
        }
        // `fullscreen` exists since version 2: an older client wouldn't know the value.
        if self.fullscreen && version >= 2 {
            states.push(STATE_FULLSCREEN);
        }
        states.iter().flat_map(|s| s.to_ne_bytes()).collect()
    }
}

/// One window, and the handle every client that bound the protocol has for it.
struct Toplevel {
    id: u64,
    /// What the clients were told last.
    last: Info,
    /// The window id of the window clients were told is its parent.
    parent: Option<u64>,
    /// The manager a handle was created for, and the handle.
    handles: Vec<(ZwlrForeignToplevelManagerV1, ZwlrForeignToplevelHandleV1)>,
}

/// Everything the protocol needs to keep between requests.
pub struct ForeignToplevel {
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    toplevels: Vec<Toplevel>,
    /// Where a taskbar said its button for a window is, by window id: the taskbar's surface and
    /// the rectangle in it (x, y, width, height).
    rectangles: HashMap<u64, (WlSurface, Rect)>,
    _global: GlobalId,
}

impl ForeignToplevel {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            managers: Vec::new(),
            toplevels: Vec::new(),
            rectangles: HashMap::new(),
            _global: display
                .create_global::<JffState, ZwlrForeignToplevelManagerV1, ()>(VERSION, ()),
        }
    }
}

/// Send `new` on `handle`: everything when `old` is `None`, else what differs. Ends with `done`.
fn send_changes(
    handle: &ZwlrForeignToplevelHandleV1,
    old: Option<&Info>,
    new: &Info,
    outputs: &[smithay::output::Output],
) {
    let changed = |f: fn(&Info) -> bool| old.map_or(true, |old| f(old) != f(new));

    if old.map_or(true, |old| old.title != new.title) {
        handle.title(new.title.clone());
    }
    if old.map_or(true, |old| old.app_id != new.app_id) {
        handle.app_id(new.app_id.clone());
    }

    let old_output = old.and_then(|old| old.output.as_deref());
    if old.is_none() || old_output != new.output.as_deref() {
        // Only the `wl_output` objects of the client that owns `handle` can be named to it.
        if let Some(client) = handle.client() {
            let objects = |name: Option<&str>| {
                outputs
                    .iter()
                    .find(|o| Some(o.name().as_str()) == name)
                    .map(|o| o.client_outputs(&client).collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            for output in objects(old_output) {
                handle.output_leave(&output);
            }
            for output in objects(new.output.as_deref()) {
                handle.output_enter(&output);
            }
        }
    }

    if changed(|i| i.maximized)
        || changed(|i| i.minimized)
        || changed(|i| i.fullscreen)
        || changed(|i| i.activated)
    {
        handle.state(new.state_bytes(handle.version()));
    }
    handle.done();
}

impl JffState {
    /// Tell every client that bound the protocol what changed since the last call. Runs once
    /// per event-loop iteration, does nothing but a few checks while nobody uses the protocol.
    pub fn publish_foreign_toplevels(&mut self) {
        self.foreign_toplevel.managers.retain(|m| m.is_alive());
        if self.foreign_toplevel.managers.is_empty() {
            self.foreign_toplevel.toplevels.clear();
            self.foreign_toplevel.rectangles.clear();
            return;
        }

        let windows = self.window_list();
        let parents = self.window_parents();
        let minimized: Vec<u64> = self
            .desk
            .all_top_down()
            .filter(|entry| entry.hidden.is_some())
            .filter_map(|entry| entry.window.user_data().get::<WindowId>().map(|id| id.0))
            .collect();
        let outputs = self.outputs.clone();
        let display = self.display_handle.clone();
        let ft = &mut self.foreign_toplevel;

        // Windows that went away.
        ft.toplevels.retain(|toplevel| {
            let alive = windows.iter().any(|v| v.id == toplevel.id);
            if !alive {
                for (_, handle) in &toplevel.handles {
                    handle.closed();
                }
            }
            alive
        });
        ft.rectangles
            .retain(|id, _| windows.iter().any(|v| v.id == *id));

        for window in &windows {
            let info = Info::from_window(window, minimized.contains(&window.id));
            match ft.toplevels.iter_mut().find(|t| t.id == window.id) {
                // Not new: send what changed.
                Some(toplevel) => {
                    toplevel.handles.retain(|(_, handle)| handle.is_alive());
                    if toplevel.last == info {
                        continue;
                    }
                    for (_, handle) in &toplevel.handles {
                        send_changes(handle, Some(&toplevel.last), &info, &outputs);
                    }
                    toplevel.last = info;
                }
                // New: every manager gets a handle for it.
                None => {
                    let mut toplevel = Toplevel {
                        id: window.id,
                        last: info,
                        parent: None,
                        handles: Vec::new(),
                    };
                    for manager in &ft.managers {
                        announce(&display, manager, &mut toplevel, &outputs);
                    }
                    ft.toplevels.push(toplevel);
                }
            }
        }

        // Parents go last, when every handle exists: a dialog is above its parent, so it is
        // announced first. A parent that clients have not heard of yet is waited for.
        let known: Vec<u64> = ft.toplevels.iter().map(|t| t.id).collect();
        let changes: Vec<(usize, Option<u64>)> = ft
            .toplevels
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let want = parents.get(&t.id).copied().filter(|p| known.contains(p));
                (want != t.parent).then_some((i, want))
            })
            .collect();
        for (i, want) in changes {
            let parent_handles = want
                .and_then(|pid| ft.toplevels.iter().find(|t| t.id == pid))
                .map(|t| t.handles.clone())
                .unwrap_or_default();
            let toplevel = &mut ft.toplevels[i];
            send_parent(&toplevel.handles, &parent_handles);
            toplevel.parent = want;
        }
    }

    /// For every window that is a dialog of another, the window id of the one it belongs to.
    fn window_parents(&self) -> HashMap<u64, u64> {
        let id_of = |window: &Window| window.user_data().get::<WindowId>().map(|id| id.0);
        self.desk
            .all_top_down()
            .filter_map(|entry| {
                let parent = entry.window.toplevel()?.parent()?;
                let parent_entry = self.desk.find_by(|w| {
                    w.toplevel()
                        .is_some_and(|toplevel| *toplevel.wl_surface() == parent)
                })?;
                Some((id_of(&entry.window)?, id_of(&parent_entry.window)?))
            })
            .collect()
    }

    /// Where a taskbar said its button for `window` is, in the shared space. `None` when none
    /// did, or its taskbar is gone or not placed.
    pub fn foreign_toplevel_rectangle(&self, window: &Window) -> Option<Rect> {
        let id = window.user_data().get::<WindowId>()?.0;
        let (surface, (x, y, width, height)) = self.foreign_toplevel.rectangles.get(&id)?;
        if !surface.is_alive() {
            return None;
        }
        let origin = surface_rect(&self.desk, &self.outputs, surface)?.loc;
        Some((origin.x + x, origin.y + y, *width, *height))
    }

    /// The window with the window id `id`.
    fn window_by_window_id(&self, id: u64) -> Option<Window> {
        self.desk
            .find_by(|w| w.user_data().get::<WindowId>() == Some(&WindowId(id)))
            .map(|entry| entry.window.clone())
    }
}

/// Tell every handle of `handles` (of a version that has the event) its parent: the handle of
/// the parent that belongs to the same manager, none when `parent_handles` has none.
fn send_parent(
    handles: &[(ZwlrForeignToplevelManagerV1, ZwlrForeignToplevelHandleV1)],
    parent_handles: &[(ZwlrForeignToplevelManagerV1, ZwlrForeignToplevelHandleV1)],
) {
    for (manager, handle) in handles {
        if handle.version() < 3 {
            continue;
        }
        let parent = parent_handles
            .iter()
            .find(|(m, _)| m == manager)
            .map(|(_, h)| h);
        handle.parent(parent);
        handle.done();
    }
}

/// Create `toplevel`'s handle for `manager` and tell the client everything about it.
fn announce(
    display: &DisplayHandle,
    manager: &ZwlrForeignToplevelManagerV1,
    toplevel: &mut Toplevel,
    outputs: &[smithay::output::Output],
) {
    let Some(client) = manager.client() else {
        return;
    };
    let Ok(handle) = client.create_resource::<ZwlrForeignToplevelHandleV1, u64, JffState>(
        display,
        manager.version(),
        toplevel.id,
    ) else {
        return;
    };
    manager.toplevel(&handle);
    send_changes(&handle, None, &toplevel.last, outputs);
    toplevel.handles.push((manager.clone(), handle));
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for JffState {
    // Sandboxed apps must not see this.
    fn can_view(client: smithay::reexports::wayland_server::Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        state: &mut JffState,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        // Catch the new client up on the windows the others already know. Windows that appeared
        // since the last loop iteration are announced by `publish_foreign_toplevels`.
        let outputs = state.outputs.clone();
        let ft = &mut state.foreign_toplevel;
        for toplevel in &mut ft.toplevels {
            announce(display, &manager, toplevel, &outputs);
        }
        // ... and which of them are dialogs.
        let handle_of = |toplevels: &[Toplevel], id: u64| {
            toplevels
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| t.handles.iter().find(|(m, _)| *m == manager))
                .map(|(_, handle)| handle.clone())
        };
        for (id, parent) in ft
            .toplevels
            .iter()
            .filter_map(|t| t.parent.map(|parent| (t.id, parent)))
        {
            if let Some(child) = handle_of(&ft.toplevels, id) {
                if child.version() >= 3 {
                    child.parent(handle_of(&ft.toplevels, parent).as_ref());
                    child.done();
                }
            }
        }
        ft.managers.push(manager);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            let ft = &mut state.foreign_toplevel;
            ft.managers.retain(|m| m != resource);
            // The handles stay valid for the client to destroy, but get no more events.
            for toplevel in &mut ft.toplevels {
                toplevel.handles.retain(|(m, _)| m != resource);
            }
            resource.finished();
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, u64> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        id: &u64,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request;

        if let Request::Destroy = request {
            forget_handle(state, resource);
            return;
        }
        // The window may be gone already (the client hasn't heard of it yet).
        let Some(window) = state.window_by_window_id(*id) else {
            return;
        };
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };

        match request {
            Request::SetMaximized | Request::UnsetMaximized => {
                let want = matches!(request, Request::SetMaximized);
                if state.desk.is_maximized(|w| *w == window) != want {
                    state.set_maximized(&toplevel, want);
                }
            }
            Request::SetFullscreen { .. } | Request::UnsetFullscreen => {
                let want = matches!(request, Request::SetFullscreen { .. });
                let now = toplevel
                    .with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen));
                if want != now {
                    state.set_filled(&toplevel, xdg_toplevel::State::Fullscreen, want);
                }
            }
            Request::Activate { .. } => {
                // Same as `jff msg focus`: show the window's workspace, then take the overview
                // down with the pick (a plain focus would be taken back).
                state.show_workspace_of(&window);
                if state.overview_active() {
                    state.end_overview(Some(window));
                } else {
                    state.focus_window(&window);
                }
            }
            Request::Close => state.request_close(&toplevel),
            Request::SetMinimized => state.minimize_window(&window),
            Request::UnsetMinimized => state.unminimize_window(&window),
            Request::SetRectangle {
                surface,
                x,
                y,
                width,
                height,
            } => {
                let rectangles = &mut state.foreign_toplevel.rectangles;
                if width == 0 && height == 0 {
                    rectangles.remove(id);
                } else if width > 0 && height > 0 {
                    rectangles.insert(*id, (surface, (x, y, width, height)));
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut JffState,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        _data: &u64,
    ) {
        forget_handle(state, resource);
    }
}

/// Stop sending events to `handle`.
fn forget_handle(state: &mut JffState, handle: &ZwlrForeignToplevelHandleV1) {
    for toplevel in &mut state.foreign_toplevel.toplevels {
        toplevel.handles.retain(|(_, h)| h != handle);
    }
}
