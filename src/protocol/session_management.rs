//! xdg_session_management_v1: apps ask jff to remember where their windows were,
//! and put them back next time they start.
//!
//! The experimental xx_session_management_v1 (same idea, a few requests and events apart) is
//! offered next to it, because some builds still ask for that one. Both globals share one store
//! and one set of sessions, [`SessionRes`] and [`ToplevelRes`] stand for either kind of object.
//!
//! A session is just an id the app keeps around. Every window it adds has a name,
//! and jff saves that window's state under the name (see `desk::session_store`) in
//! a file, so it survives the app and jff restarting. Saved on change, in
//! [`JffState::session_flush`], and when the window closes.
//!
//! Windows get restored before they're first shown, so the first configure has to
//! wait for the app to commit once: `new_toplevel` doesn't send it anymore,
//! [`JffState::send_initial_configure`] does, from the commit handler.

use std::collections::HashMap;

use super::xdg_session_bindings::server::{
    xdg_session_manager_v1::{self, XdgSessionManagerV1},
    xdg_session_v1::{self, XdgSessionV1},
    xdg_toplevel_session_v1::XdgToplevelSessionV1,
};
use super::xx_session_bindings::server::{
    xx_session_manager_v1::{self, XxSessionManagerV1},
    xx_session_v1::{self, XxSessionV1},
    xx_toplevel_session_v1::{self, XxToplevelSessionV1},
};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource,
};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::desk::session_store::{self, SavedWindow, Store};
use crate::desk::WindowMode;
use crate::primitives::layout;
use crate::state::JffState;

/// A window the app put in a session.
struct Member {
    name: String,
    surface: WlSurface,
}

/// A session an app has open right now.
struct Live {
    /// Tells this session apart from an older one with the same id (i.e. one that got replaced).
    serial: u64,
    resource: SessionRes,
    members: Vec<Member>,
}

/// A window whose state is waiting on the app's first commit.
struct Pending {
    saved: SavedWindow,
    resource: ToplevelRes,
    /// What `restored` names, for the sessions that send it along.
    toplevel: XdgToplevel,
}

const NAME_IN_USE: &str = "name or toplevel already in the session";
const ALREADY_MAPPED: &str = "restore_toplevel after the first commit";

/// A session object of either protocol.
#[derive(Clone)]
enum SessionRes {
    Xdg(XdgSessionV1),
    Xx(XxSessionV1),
}

impl SessionRes {
    fn is_alive(&self) -> bool {
        match self {
            Self::Xdg(s) => s.is_alive(),
            Self::Xx(s) => s.is_alive(),
        }
    }

    fn client_id(&self) -> Option<ClientId> {
        match self {
            Self::Xdg(s) => s.client().map(|c| c.id()),
            Self::Xx(s) => s.client().map(|c| c.id()),
        }
    }

    fn replaced(&self) {
        match self {
            Self::Xdg(s) => s.replaced(),
            Self::Xx(s) => s.replaced(),
        }
    }

    fn created(&self, id: String) {
        match self {
            Self::Xdg(s) => s.created(id),
            Self::Xx(s) => s.created(id),
        }
    }

    fn restored(&self) {
        match self {
            Self::Xdg(s) => s.restored(),
            Self::Xx(s) => s.restored(),
        }
    }

    fn name_in_use(&self) {
        match self {
            Self::Xdg(s) => s.post_error(xdg_session_v1::Error::NameInUse, NAME_IN_USE),
            Self::Xx(s) => s.post_error(xx_session_v1::Error::NameInUse, NAME_IN_USE),
        }
    }

    fn already_mapped(&self) {
        match self {
            Self::Xdg(s) => s.post_error(xdg_session_v1::Error::AlreadyMapped, ALREADY_MAPPED),
            Self::Xx(s) => s.post_error(xx_session_v1::Error::AlreadyMapped, ALREADY_MAPPED),
        }
    }
}

/// A toplevel session object of either protocol.
enum ToplevelRes {
    Xdg(XdgToplevelSessionV1),
    Xx(XxToplevelSessionV1),
}

impl ToplevelRes {
    fn is_alive(&self) -> bool {
        match self {
            Self::Xdg(t) => t.is_alive(),
            Self::Xx(t) => t.is_alive(),
        }
    }

    /// The xx version names the toplevel in the event, the xdg one does not.
    fn restored(&self, toplevel: &XdgToplevel) {
        match self {
            Self::Xdg(t) => t.restored(),
            Self::Xx(t) => t.restored(toplevel),
        }
    }
}

pub struct SessionManagement {
    _global: GlobalId,
    _xx_global: GlobalId,
    /// What we remember, by session id, including sessions nobody has open.
    store: Store,
    live: HashMap<String, Live>,
    pending: HashMap<WlSurface, Pending>,
    /// Restored windows that haven't shown a buffer yet. Their mode gets applied once
    /// the size is known.
    applied: HashMap<WlSurface, SavedWindow>,
    next_serial: u64,
    /// Store has changes not yet written to the file.
    dirty: bool,
}

impl SessionManagement {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, XdgSessionManagerV1, ()>(1, ()),
            _xx_global: display.create_global::<JffState, XxSessionManagerV1, ()>(1, ()),
            store: session_store::load_store(),
            live: HashMap::new(),
            pending: HashMap::new(),
            applied: HashMap::new(),
            next_serial: 0,
            dirty: false,
        }
    }

    fn write(&mut self) {
        tracing::info!(
            "xdg-session: writing store ({} sessions) to {:?}",
            self.store.len(),
            session_store::store_path()
        );
        session_store::save_store(&self.store);
        self.dirty = false;
    }

    /// The id a `get_session` for `requested` gets, whether something was saved under it, and
    /// the serial of the new session.
    fn pick_id(&mut self, requested: Option<String>) -> (String, bool, u64) {
        // An id nothing was ever saved under is a new session, per the protocol.
        let known = requested.clone().filter(|id| self.store.contains_key(id));
        tracing::info!(
            "xdg-session: get_session known={} stored_sessions={}",
            known.is_some(),
            self.store.len()
        );
        // Same as KWin: the id the app asks for is the session id, known or not. Handing
        // back a different id can make the app think it failed and skip adding its windows
        // entirely (Firefox sends nothing after `created` when that happens).
        // Only an id some other app currently has open gets a fresh one.
        let id = match (&known, &requested) {
            (Some(id), _) => id.clone(),
            (None, Some(id)) if !id.is_empty() && !self.live.contains_key(id) => id.clone(),
            _ => session_store::new_session_id(),
        };
        self.next_serial += 1;
        (id, known.is_some(), self.next_serial)
    }

    /// Make `session` (already created, with `id` and `serial` from [`Self::pick_id`]) the live
    /// session for `id`. Returns false, changing nothing, when the same client has that
    /// session open already: the caller raises `in_use`.
    fn open(
        &mut self,
        client: &Client,
        session: SessionRes,
        id: String,
        serial: u64,
        known: bool,
    ) -> bool {
        if let Some(old) = self.live.get(&id) {
            if old.resource.is_alive() && old.resource.client_id() == Some(client.id()) {
                return false;
            }
            // Another app is taking the session over: the old one's objects go inert.
            old.resource.replaced();
        }
        if let Some(old) = self.live.remove(&id) {
            for member in old.members {
                self.pending.remove(&member.surface);
                self.applied.remove(&member.surface);
            }
        }
        self.live.insert(
            id.clone(),
            Live {
                serial,
                resource: session.clone(),
                members: Vec::new(),
            },
        );
        if known {
            session.restored();
        } else {
            session.created(id);
        }
        true
    }

    /// The session `data` belongs to, unless it was replaced or ended.
    fn current(&mut self, data: &SessionData) -> Option<&mut Live> {
        self.live
            .get_mut(&data.id)
            .filter(|live| live.serial == data.serial)
    }

    /// Forget window `name` of a session, saved state and all.
    fn remove_member(&mut self, id: &str, name: &str) {
        if let Some(live) = self.live.get_mut(id) {
            if let Some(index) = live.members.iter().position(|m| m.name == name) {
                let member = live.members.remove(index);
                self.pending.remove(&member.surface);
                self.applied.remove(&member.surface);
            }
        }
        if let Some(windows) = self.store.get_mut(id) {
            if windows.remove(name).is_some() {
                self.dirty = true;
            }
            if windows.is_empty() {
                self.store.remove(id);
            }
        }
    }

    /// App is done with the session. State stays unless `forget`.
    fn end(&mut self, data: &SessionData, forget: bool) {
        if self.current(data).is_none() {
            return;
        }
        if let Some(live) = self.live.remove(&data.id) {
            for member in live.members {
                self.pending.remove(&member.surface);
                self.applied.remove(&member.surface);
            }
        }
        if forget && self.store.remove(&data.id).is_some() {
            self.dirty = true;
        }
    }
}

pub struct SessionData {
    id: String,
    serial: u64,
}

pub struct ToplevelSessionData {
    id: String,
    serial: u64,
    name: String,
}

impl GlobalDispatch<XdgSessionManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgSessionManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl GlobalDispatch<XxSessionManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XxSessionManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgSessionManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        client: &Client,
        resource: &XdgSessionManagerV1,
        request: xdg_session_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("xdg-session: manager request {request:?}");
        let xdg_session_manager_v1::Request::GetSession {
            id: new_session,
            session_id,
            ..
        } = request
        else {
            return; // destroy: sessions stick around
        };
        // `reason` just says how much to restore. jff restores the same either way.
        let sm = &mut state.session_management;
        let (id, known, serial) = sm.pick_id(session_id);
        let session = data_init.init(
            new_session,
            SessionData {
                id: id.clone(),
                serial,
            },
        );
        if !sm.open(client, SessionRes::Xdg(session), id, serial, known) {
            resource.post_error(
                xdg_session_manager_v1::Error::InUse,
                "session already in use",
            );
        }
    }
}

impl Dispatch<XxSessionManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        client: &Client,
        resource: &XxSessionManagerV1,
        request: xx_session_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("xx-session: manager request {request:?}");
        let xx_session_manager_v1::Request::GetSession {
            id: new_session,
            session: session_id,
            ..
        } = request
        else {
            return; // destroy: sessions stick around
        };
        let sm = &mut state.session_management;
        let (id, known, serial) = sm.pick_id(session_id);
        let session = data_init.init(
            new_session,
            SessionData {
                id: id.clone(),
                serial,
            },
        );
        if !sm.open(client, SessionRes::Xx(session), id, serial, known) {
            resource.post_error(
                xx_session_manager_v1::Error::InUse,
                "session already in use",
            );
        }
    }
}

impl ToplevelSessionData {
    fn of(data: &SessionData, name: &str) -> Self {
        Self {
            id: data.id.clone(),
            serial: data.serial,
            name: name.to_owned(),
        }
    }
}

impl Dispatch<XdgSessionV1, SessionData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XdgSessionV1,
        request: xdg_session_v1::Request,
        data: &SessionData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("xdg-session: session {} request {request:?}", data.id);
        let session = SessionRes::Xdg(resource.clone());
        match request {
            xdg_session_v1::Request::Destroy => state.session_management.end(data, false),
            xdg_session_v1::Request::Remove => {
                state.session_management.end(data, true);
                state.session_save_if_dirty();
            }
            xdg_session_v1::Request::AddToplevel { id, toplevel, name } => {
                let member = data_init.init(id, ToplevelSessionData::of(data, &name));
                let member = ToplevelRes::Xdg(member);
                state.session_add_toplevel(&session, data, member, &toplevel, name, false);
            }
            xdg_session_v1::Request::RestoreToplevel { id, toplevel, name } => {
                let member = data_init.init(id, ToplevelSessionData::of(data, &name));
                let member = ToplevelRes::Xdg(member);
                state.session_add_toplevel(&session, data, member, &toplevel, name, true);
            }
            xdg_session_v1::Request::RemoveToplevel { name } => {
                if state.session_management.current(data).is_some() {
                    state.session_management.remove_member(&data.id, &name);
                    state.session_save_if_dirty();
                }
            }
        }
    }
}

impl Dispatch<XxSessionV1, SessionData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XxSessionV1,
        request: xx_session_v1::Request,
        data: &SessionData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("xx-session: session {} request {request:?}", data.id);
        let session = SessionRes::Xx(resource.clone());
        match request {
            xx_session_v1::Request::Destroy => state.session_management.end(data, false),
            xx_session_v1::Request::Remove => {
                state.session_management.end(data, true);
                state.session_save_if_dirty();
            }
            xx_session_v1::Request::AddToplevel { id, toplevel, name } => {
                let member = data_init.init(id, ToplevelSessionData::of(data, &name));
                let member = ToplevelRes::Xx(member);
                state.session_add_toplevel(&session, data, member, &toplevel, name, false);
            }
            xx_session_v1::Request::RestoreToplevel { id, toplevel, name } => {
                let member = data_init.init(id, ToplevelSessionData::of(data, &name));
                let member = ToplevelRes::Xx(member);
                state.session_add_toplevel(&session, data, member, &toplevel, name, true);
            }
        }
    }
}

impl Dispatch<XdgToplevelSessionV1, ToplevelSessionData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &XdgToplevelSessionV1,
        request: <XdgToplevelSessionV1 as Resource>::Request,
        data: &ToplevelSessionData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!(
            "xdg-session: toplevel session {:?} request {request:?}",
            data.name
        );
        // `destroy` just drops this object, the window stays in the session.
        // Removing a window is `remove_toplevel` on the session in this version. If the
        // toplevel session object ever gets a `remove` of its own, it's told apart by name.
        if format!("{request:?}") != "Remove" {
            return;
        }
        state.session_remove_member(data);
    }
}

impl Dispatch<XxToplevelSessionV1, ToplevelSessionData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &XxToplevelSessionV1,
        request: xx_toplevel_session_v1::Request,
        data: &ToplevelSessionData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!(
            "xx-session: toplevel session {:?} request {request:?}",
            data.name
        );
        // `destroy` just drops this object, the window stays in the session.
        if let xx_toplevel_session_v1::Request::Remove = request {
            state.session_remove_member(data);
        }
    }
}

impl JffState {
    /// A toplevel session object asked to remove its window from the session.
    fn session_remove_member(&mut self, data: &ToplevelSessionData) {
        let sm = &mut self.session_management;
        let current = sm
            .live
            .get(&data.id)
            .is_some_and(|live| live.serial == data.serial);
        if current {
            sm.remove_member(&data.id, &data.name);
            self.session_save_if_dirty();
        }
    }

    fn session_add_toplevel(
        &mut self,
        session: &SessionRes,
        data: &SessionData,
        member: ToplevelRes,
        toplevel: &XdgToplevel,
        name: String,
        restore: bool,
    ) {
        let Some(surface) = self
            .xdg_shell_state
            .get_toplevel(toplevel)
            .map(|t| (t.wl_surface().clone(), t.is_initial_configure_sent()))
        else {
            return;
        };
        let (surface, mapped) = surface;
        tracing::info!("xdg-session: toplevel name={name:?} restore={restore} mapped={mapped}");
        let sm = &mut self.session_management;
        let Some(live) = sm.current(data) else {
            return; // replaced or ended: inert
        };
        if live
            .members
            .iter()
            .any(|m| m.name == name || m.surface == surface)
        {
            session.name_in_use();
            return;
        }
        live.members.push(Member {
            name: name.clone(),
            surface: surface.clone(),
        });
        if !restore {
            return;
        }
        if mapped {
            session.already_mapped();
            return;
        }
        let saved = sm.store.get(&data.id).and_then(|w| w.get(&name)).cloned();
        if let Some(saved) = saved {
            sm.pending.insert(
                surface,
                Pending {
                    saved,
                    resource: member,
                    toplevel: toplevel.clone(),
                },
            );
        }
    }

    /// First configure of a window, on its first commit. Puts the window back first if the
    /// app asked for that.
    pub fn send_initial_configure(&mut self, toplevel: &ToplevelSurface) {
        let surface = toplevel.wl_surface().clone();
        if let Some(pending) = self.session_management.pending.remove(&surface) {
            if pending.resource.is_alive() {
                self.session_restore(toplevel, &surface, pending);
            }
        }
        toplevel.send_configure();
    }

    fn session_restore(
        &mut self,
        toplevel: &ToplevelSurface,
        surface: &WlSurface,
        pending: Pending,
    ) {
        let saved = pending.saved;
        // Where it was, as long as that place still exists, the monitor may be gone.
        let rects = self.output_rects();
        let target = self
            .outputs
            .iter()
            .position(|output| output.name() == saved.output)
            .and_then(|i| rects.get(i).copied());
        let inside = |rect: layout::Rect, p: (i32, i32)| {
            p.0 >= rect.0 && p.0 < rect.0 + rect.2 && p.1 >= rect.1 && p.1 < rect.1 + rect.3
        };
        let mut pos = saved.pos;
        // The saved monitor wins: a maximized or snapped window is saved with the place it
        // floats back to, which can be on a different monitor than the one it was shown on.
        if let Some(rect) = target {
            if !inside(rect, (pos.0 + 16, pos.1 + 16)) {
                pos = layout::fit_into(pos, saved.size, rect);
            }
        }
        let on_screen = rects
            .iter()
            .any(|&rect| inside(rect, (pos.0 + 16, pos.1 + 16)));
        let output_known = saved.output.is_empty() || target.is_some();
        tracing::info!(
            "xdg-session: restoring at {pos:?} output={:?} on_screen={on_screen} known={output_known}",
            saved.output
        );
        if on_screen && output_known {
            let workspace = saved
                .workspace
                .min(self.workspace_count().saturating_sub(1));
            for entry in self.desk.iter_mut() {
                if entry
                    .window
                    .toplevel()
                    .is_some_and(|t| t.wl_surface() == surface)
                {
                    entry.pos = pos;
                    entry.workspace = workspace;
                    if !saved.output.is_empty() {
                        entry.output = saved.output.clone();
                    }
                }
            }
        }
        if saved.size.0 > 0 && saved.size.1 > 0 {
            toplevel.with_pending_state(|state| state.size = Some(saved.size.into()));
        }
        // Before the first configure, per the protocol.
        pending.resource.restored(&pending.toplevel);
        self.session_management
            .applied
            .insert(surface.clone(), saved);
    }

    /// Saved state of a restored window, once it has a size. `None` for a window that
    /// wasn't restored, or whose state was already taken.
    pub fn session_take_restored(&mut self, root: &WlSurface) -> Option<SavedWindow> {
        self.session_management.applied.remove(root)
    }

    /// Whether `root` was put back by its session and has not shown its first buffer yet.
    pub fn session_is_restored(&self, root: &WlSurface) -> bool {
        self.session_management.applied.contains_key(root)
    }

    /// Maximized, fullscreen or snapped as it was. Floating needs nothing, size and place
    /// are already set.
    pub fn session_finish_restore(&mut self, toplevel: &ToplevelSurface, saved: &SavedWindow) {
        if saved.mode == WindowMode::Floating {
            return;
        }
        let surface = toplevel.wl_surface().clone();
        self.set_mode(toplevel, saved.mode);
        // What it floats back to is what was saved, not what it had just now.
        self.desk.set_restore_where(
            |w: &Window| w.toplevel().is_some_and(|t| t.wl_surface() == &surface),
            (saved.pos, saved.size),
        );
    }

    /// The state of `surface` as it should be saved. `None` until the window has shown
    /// something: before that its place and size are just a guess.
    fn session_snapshot(&self, surface: &WlSurface) -> Option<SavedWindow> {
        let entry = self.desk.all_top_down().find(|e| {
            e.window
                .toplevel()
                .is_some_and(|t| t.wl_surface() == surface)
        })?;
        if !entry.fitted {
            return None;
        }
        let size = entry.window.geometry().size;
        // The monitor the window is on right now. `entry.output` only stays up to date with
        // per-output workspaces, so it can still name the monitor the window opened on.
        let rects = self.output_rects();
        let output = layout::nearest(layout::center(entry.pos, (size.w, size.h)), &rects)
            .and_then(|i| self.outputs.get(i))
            .map_or_else(|| entry.output.clone(), |output| output.name());
        let (pos, size) = match entry.restore {
            Some(restore) if entry.mode != WindowMode::Floating => restore,
            _ => (entry.pos, (size.w, size.h)),
        };
        if size.0 <= 0 || size.1 <= 0 {
            return None;
        }
        Some(SavedWindow {
            mode: entry.mode,
            pos,
            size,
            workspace: entry.workspace,
            output,
        })
    }

    /// Save whatever changed in the open sessions' windows. Cheap when nothing did. Runs
    /// before every frame. Nothing gets written mid-drag or mid-resize, it's the end of
    /// that which is worth saving.
    pub fn session_flush(&mut self) {
        self.session_management
            .live
            .retain(|_, live| live.resource.is_alive());
        let windows: Vec<(String, String, WlSurface)> = self
            .session_management
            .live
            .iter()
            .flat_map(|(id, live)| {
                live.members
                    .iter()
                    .filter(|m| m.surface.is_alive())
                    .map(move |m| (id.clone(), m.name.clone(), m.surface.clone()))
            })
            .collect();
        for (id, name, surface) in windows {
            let Some(snapshot) = self.session_snapshot(&surface) else {
                continue;
            };
            let sm = &mut self.session_management;
            let saved = sm.store.entry(id).or_default();
            if saved.get(&name) != Some(&snapshot) {
                saved.insert(name, snapshot);
                sm.dirty = true;
            }
        }
        if self.dragging_view.is_none() && self.resize.is_none() {
            self.session_save_if_dirty();
        }
    }

    /// Write the store if anything changed since the last save.
    fn session_save_if_dirty(&mut self) {
        if self.session_management.dirty {
            tracing::debug!(
                "xdg-session: saving, {} live sessions, store has {}",
                self.session_management.live.len(),
                self.session_management.store.len()
            );
            self.session_management.write();
        }
    }

    /// A window is closing: save it as it was, while the desk still has it. The window
    /// leaves its sessions, but what was saved stays for next start.
    pub fn session_toplevel_gone(&mut self, surface: &WlSurface) {
        let sm = &mut self.session_management;
        sm.pending.remove(surface);
        sm.applied.remove(surface);
        let memberships: Vec<(String, String)> = sm
            .live
            .iter()
            .flat_map(|(id, live)| {
                live.members
                    .iter()
                    .filter(|m| &m.surface == surface)
                    .map(move |m| (id.clone(), m.name.clone()))
            })
            .collect();
        if memberships.is_empty() {
            return;
        }
        let snapshot = self.session_snapshot(surface);
        let sm = &mut self.session_management;
        for (id, name) in memberships {
            if let Some(snapshot) = &snapshot {
                let saved = sm.store.entry(id.clone()).or_default();
                if saved.get(&name) != Some(snapshot) {
                    saved.insert(name.clone(), snapshot.clone());
                    sm.dirty = true;
                }
            }
            if let Some(live) = sm.live.get_mut(&id) {
                live.members.retain(|m| &m.surface != surface);
            }
        }
        self.session_save_if_dirty();
    }
}
