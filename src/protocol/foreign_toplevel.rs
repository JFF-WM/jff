//! `wlr-foreign-toplevel-management-unstable-v1`: taskbars and docks (waybar's `wlr/taskbar`,
//! nwg-panel, yambar, ...) list open windows and ask jff to focus, maximize, fullscreen, or
//! close them.
//!
//! Like `jff msg watch`, changes come from comparing, not from calling an `emit` from every
//! place that changes a window: once per event-loop iteration, `JffState::publish_foreign_toplevels`
//! takes the view list (the one `jff msg views` shows), compares it with what clients were told,
//! and sends the difference. Nothing is computed while no client has bound the protocol.
//!
//! Not supported:
//! - minimize: jff has no minimized windows, so `set_minimized` is ignored and the `minimized`
//!   state is never sent.
//! - `set_rectangle` (where the taskbar button is, for a minimize animation): ignored.
//! - the `parent` event: never sent.
//!
//! Windows on every workspace are announced, not just the ones on the shown workspace. A window
//! is announced once it has a size (same rule as `jff msg views`).

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::ipc::events::View;
use crate::ipc::ViewId;
use crate::state::JffState;

/// Highest version jff speaks. 3 only adds the `parent` event, which we never send.
const VERSION: u32 = 3;

/// `zwlr_foreign_toplevel_handle_v1::state` values.
const STATE_MAXIMIZED: u32 = 0;
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
    fullscreen: bool,
    /// The window has the keyboard.
    activated: bool,
}

impl Info {
    fn from_view(view: &View) -> Self {
        Self {
            title: view.title.clone().unwrap_or_default(),
            app_id: view.app_id.clone().unwrap_or_default(),
            output: view.output.clone(),
            maximized: view.maximized,
            fullscreen: view.fullscreen,
            activated: view.focused,
        }
    }

    /// The `state` array: native-endian `u32`s.
    fn state_bytes(&self, version: u32) -> Vec<u8> {
        let mut states = Vec::new();
        if self.maximized {
            states.push(STATE_MAXIMIZED);
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
    /// The manager a handle was created for, and the handle.
    handles: Vec<(ZwlrForeignToplevelManagerV1, ZwlrForeignToplevelHandleV1)>,
}

/// Everything the protocol needs to keep between requests.
pub struct ForeignToplevel {
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    toplevels: Vec<Toplevel>,
    _global: GlobalId,
}

impl ForeignToplevel {
    /// Create the protocol global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            managers: Vec::new(),
            toplevels: Vec::new(),
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

    if changed(|i| i.maximized) || changed(|i| i.fullscreen) || changed(|i| i.activated) {
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
            return;
        }

        let views = self.view_list();
        let outputs = self.outputs.clone();
        let display = self.display_handle.clone();
        let ft = &mut self.foreign_toplevel;

        // Windows that went away.
        ft.toplevels.retain(|toplevel| {
            let alive = views.iter().any(|v| v.id == toplevel.id);
            if !alive {
                for (_, handle) in &toplevel.handles {
                    handle.closed();
                }
            }
            alive
        });

        for view in &views {
            let info = Info::from_view(view);
            match ft.toplevels.iter_mut().find(|t| t.id == view.id) {
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
                        id: view.id,
                        last: info,
                        handles: Vec::new(),
                    };
                    for manager in &ft.managers {
                        announce(&display, manager, &mut toplevel, &outputs);
                    }
                    ft.toplevels.push(toplevel);
                }
            }
        }
    }

    /// The window with the view id `id`.
    fn window_by_view_id(&self, id: u64) -> Option<Window> {
        self.desk
            .find_by(|w| w.user_data().get::<ViewId>() == Some(&ViewId(id)))
            .map(|entry| entry.window.clone())
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
        let Some(window) = state.window_by_view_id(*id) else {
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
            Request::Close => toplevel.send_close(),
            // jff has no minimized windows, and no animation that needs the button's place.
            Request::SetMinimized | Request::UnsetMinimized | Request::SetRectangle { .. } => {}
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
