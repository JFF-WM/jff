//! `ext-foreign-toplevel-pidfd-v1`: a pidfd of the process behind each
//! `ext-foreign-toplevel-list-v1` window. Session restore and IPC use it to map
//! window → process without racing pid reuse (a pid number is recycled, a pidfd
//! is not).
//!
//! Xwayland windows get `no_fd`: the client is xwayland-satellite, not the X11
//! app, and a pidfd of the wrong process is worse than none. Hidden from
//! sandboxed clients, like `ext-foreign-toplevel-list-v1`.
//!
//! The pidfd is sent once, when the handle is created (the process never
//! changes), being double-buffered state, it is followed by a `done` on the
//! underlying handle, to the asking client only.
//!
//! The window list is [`JffState::publish_ext_foreign_toplevels`]'s,
//! `publish_ext_foreign_toplevel_pidfds` runs right after it.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::sync::Mutex;

use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::ext_foreign_toplevel_pidfd_bindings::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use super::ext_foreign_toplevel_pidfd_bindings::server::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1;
use super::ext_foreign_toplevel_pidfd_bindings::server::ext_foreign_toplevel_pidfd_handle_v1::{
    self, ExtForeignToplevelPidfdHandleV1,
};
use super::ext_foreign_toplevel_pidfd_bindings::server::ext_foreign_toplevel_pidfd_list_v1::{
    self, ExtForeignToplevelPidfdListV1,
};
use crate::ipc::WindowId;
use crate::state::{is_sandboxed, ClientState, JffState};

/// `SO_PEERPIDFD` from `<asm-generic/socket.h>`, Linux 6.5. Written out so an older `libc`
/// crate still builds.
const SO_PEERPIDFD: libc::c_int = 77;

/// The pidfd of the process on the other end of `stream`, `None` when the kernel is older than
/// 6.5 or the socket can't say. Called when a client connects.
pub fn peer_pidfd(stream: &std::os::unix::net::UnixStream) -> Option<OwnedFd> {
    let mut fd: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `getsockopt` writes at most `len` bytes into `fd`, which is a `c_int`, the
    // socket stays open for the call.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            SO_PEERPIDFD,
            (&mut fd as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc != 0 || fd < 0 {
        return None;
    }
    // SAFETY: on success the kernel made a new descriptor that nothing else owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A pidfd for `pid` from `pidfd_open(2)`, the fallback for a kernel without `SO_PEERPIDFD`.
fn open_pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: `pidfd_open` only reads its two integer arguments.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: on success the kernel made a new descriptor that nothing else owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// What jff keeps for `ext_foreign_toplevel_pidfd_list_v1` objects.
pub struct ExtForeignToplevelPidfd {
    _global: GlobalId,
    lists: Vec<PidfdList>,
}

/// One `ext_foreign_toplevel_pidfd_list_v1`.
struct PidfdList {
    object: ExtForeignToplevelPidfdListV1,
    /// The `ext_foreign_toplevel_list_v1` from `set_list`, `None` before it.
    list: Option<ExtForeignToplevelListV1>,
    /// The windows already announced on this object, by window id.
    /// With the `ext_foreign_toplevel_handle_v1` each one extends.
    announced: Vec<(
        u64,
        ExtForeignToplevelPidfdHandleV1,
        ExtForeignToplevelHandleV1,
    )>,
}

impl ExtForeignToplevelPidfd {
    /// Create the global. Hidden from sandboxed clients with the list it extends.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, ExtForeignToplevelPidfdListV1, ()>(1, ()),
            lists: Vec::new(),
        }
    }
}

/// What a `ext_foreign_toplevel_pidfd_list_v1` remembers about itself: its list, once set.
#[derive(Default)]
pub struct ListData {
    list: Mutex<Option<ExtForeignToplevelListV1>>,
}

impl JffState {
    /// Announce the windows of `ext-foreign-toplevel-list-v1` to every pidfd list that has a
    /// list set, and forget the ones that went away. Runs once per event-loop iteration, right
    /// after `publish_ext_foreign_toplevels`.
    pub fn publish_ext_foreign_toplevel_pidfds(&mut self) {
        self.ext_foreign_toplevel_pidfd
            .lists
            .retain(|l| l.object.is_alive());
        if self.ext_foreign_toplevel_pidfd.lists.is_empty() {
            return;
        }
        let satellite_or_protected = |pid: u32| self.close_watch.is_protected(pid);

        let mut lists = std::mem::take(&mut self.ext_foreign_toplevel_pidfd.lists);
        for i in 0..lists.len() {
            // Handles other pidfd lists already extend: a client that bound the toplevel list
            // twice has two handles per window, each extended once.
            let claimed: Vec<ExtForeignToplevelHandleV1> = lists
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, other)| other.announced.iter().map(|(_, _, parent)| parent.clone()))
                .collect();
            let entry = &mut lists[i];
            let Some(list) = entry.list.clone().filter(|list| list.is_alive()) else {
                continue;
            };
            // A window that closed: the client still owns the object, drop only our note.
            entry.announced.retain(|(id, object, _)| {
                object.is_alive()
                    && self
                        .ext_foreign_toplevel
                        .toplevels
                        .iter()
                        .any(|(w, _)| w == id)
            });

            let Some(client) = list.client() else {
                continue;
            };
            for (window_id, handle) in &self.ext_foreign_toplevel.toplevels {
                if entry.announced.iter().any(|(id, _, _)| id == window_id) {
                    continue;
                }
                // The client's handle for the window, one no other pidfd list extends. Smithay does
                // not say which list made a handle, so this goes by client. A window the client
                // has no handle for yet is for next time.
                let client_id = client.id();
                let Some(parent) = handle.resources().into_iter().find(|r| {
                    r.client().is_some_and(|c| c.id() == client_id) && !claimed.contains(r)
                }) else {
                    continue;
                };
                let Ok(object) = client
                    .create_resource::<ExtForeignToplevelPidfdHandleV1, (), JffState>(
                        &self.display_handle,
                        entry.object.version(),
                        (),
                    )
                else {
                    continue;
                };
                entry.object.toplevel(&object, &parent);

                match self.window_client_pidfd(*window_id, &satellite_or_protected) {
                    Some(fd) => object.pidfd(fd.as_fd()),
                    None => object.no_fd(),
                }
                // The pidfd is double-buffered state, applied by `done` of the handle.
                parent.done();
                entry.announced.push((*window_id, object, parent));
            }
        }
        self.ext_foreign_toplevel_pidfd.lists = lists;
    }

    /// The pidfd of the process that owns window `id`, `None` when there is no window, no
    /// pidfd to be had, or the window belongs to a process that is not its app
    /// (xwayland-satellite).
    fn window_client_pidfd(&self, id: u64, is_protected: &dyn Fn(u32) -> bool) -> Option<OwnedFd> {
        let window = self
            .desk
            .all_top_down()
            .map(|entry| &entry.window)
            .find(|w| w.user_data().get::<WindowId>().is_some_and(|w| w.0 == id))?;
        let client = window.toplevel()?.wl_surface().client()?;
        let pid = client
            .get_credentials(&self.display_handle)
            .ok()
            .and_then(|creds| u32::try_from(creds.pid).ok());
        if pid.is_some_and(is_protected) {
            return None;
        }
        let state = client.get_data::<ClientState>()?;
        match state.pidfd.as_ref() {
            Some(fd) => fd.try_clone().ok(),
            None => open_pidfd(pid?),
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelPidfdListV1, ()> for JffState {
    fn bind(
        state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelPidfdListV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let object = data_init.init(resource, ListData::default());
        state.ext_foreign_toplevel_pidfd.lists.push(PidfdList {
            object,
            list: None,
            announced: Vec::new(),
        });
    }

    // Same audience as ext-foreign-toplevel-list-v1.
    fn can_view(client: Client, _global_data: &()) -> bool {
        !is_sandboxed(&client)
    }
}

impl Dispatch<ExtForeignToplevelPidfdListV1, ListData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ExtForeignToplevelPidfdListV1,
        request: ext_foreign_toplevel_pidfd_list_v1::Request,
        data: &ListData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use ext_foreign_toplevel_pidfd_list_v1::{Error, Request};

        match request {
            Request::Destroy => {}
            Request::SetList { list } => {
                let mut set = data.list.lock().unwrap_or_else(|p| p.into_inner());
                if set.is_some() {
                    resource.post_error(Error::ListSet, "set_list was called more than once");
                    return;
                }
                let used = state
                    .ext_foreign_toplevel_pidfd
                    .lists
                    .iter()
                    .any(|other| other.list.as_ref() == Some(&list));
                if used {
                    resource.post_error(
                        Error::HasExtension,
                        "the toplevel list already has a pidfd extension",
                    );
                    return;
                }
                *set = Some(list.clone());
                if let Some(entry) = state
                    .ext_foreign_toplevel_pidfd
                    .lists
                    .iter_mut()
                    .find(|entry| &entry.object == resource)
                {
                    entry.list = Some(list);
                }
                // The windows already open are announced by the next
                // `publish_ext_foreign_toplevel_pidfds`, right after this round of requests.
            }
        }
    }

    fn destroyed(
        state: &mut JffState,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ExtForeignToplevelPidfdListV1,
        _data: &ListData,
    ) {
        state
            .ext_foreign_toplevel_pidfd
            .lists
            .retain(|entry| &entry.object != resource);
    }
}

impl Dispatch<ExtForeignToplevelPidfdHandleV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ExtForeignToplevelPidfdHandleV1,
        request: ext_foreign_toplevel_pidfd_handle_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            ext_foreign_toplevel_pidfd_handle_v1::Request::Destroy => {}
        }
    }
}
