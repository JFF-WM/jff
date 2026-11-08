//! xdg-toplevel-drag-v1: a drag-and-drop that also carries a window along.
//!
//! A client (a browser dragging a tab out, a terminal dragging a tab) creates a drag object for
//! the `wl_data_source` of its drag, and attaches a toplevel to it. While the drag runs, the
//! window follows the pointer, held at the offset the client gave, and does not count when the
//! drop target is picked. When the drag ends the window stays where it is, and like the end of
//! an interactive move it snaps if it was let go at the edge of a monitor.
//!
//! Smithay does not carry this protocol. The drag itself is the ordinary data-device drag
//! (`feature/dnd.rs`), which tells us here when it starts and ends, the window is moved from the
//! pointer motion code (`update_toplevel_drag`).

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_protocols::xdg::toplevel_drag::v1::server::{
    xdg_toplevel_drag_manager_v1::{self, XdgToplevelDragManagerV1},
    xdg_toplevel_drag_v1::{self, XdgToplevelDragV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Logical, Point};

use crate::protocol::layer_shell::is_mapped;
use crate::state::JffState;

pub struct ToplevelDrags {
    _global: GlobalId,
    drags: Vec<Drag>,
}

/// One `xdg_toplevel_drag_v1` object.
struct Drag {
    resource: XdgToplevelDragV1,
    source: WlDataSource,
    attached: Option<Attached>,
    /// The drag of `source` is running.
    active: bool,
}

struct Attached {
    window: Window,
    surface: WlSurface,
    /// Where the pointer is in the window, from the window geometry's corner.
    offset: (i32, i32),
    /// The window has shown content since it was attached. It is let go when that stops.
    mapped: bool,
}

impl ToplevelDrags {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, XdgToplevelDragManagerV1, ()>(1, ()),
            drags: Vec::new(),
        }
    }

    /// The window a running drag carries, if any: it is not a drop target.
    pub fn dragged_window(&self) -> Option<&Window> {
        self.drags
            .iter()
            .find(|drag| drag.active && drag.source.is_alive())
            .and_then(|drag| drag.attached.as_ref())
            .map(|attached| &attached.window)
    }

    /// Whether `root` is a window some drag object has taken: it is placed by the drag, not by
    /// the rules for new windows.
    pub fn is_attached(&self, root: &WlSurface) -> bool {
        self.drags
            .iter()
            .filter_map(|drag| drag.attached.as_ref())
            .any(|attached| attached.surface == *root)
    }

    fn drag_mut(&mut self, resource: &XdgToplevelDragV1) -> Option<&mut Drag> {
        self.drags
            .iter_mut()
            .find(|drag| drag.resource == *resource)
    }
}

impl GlobalDispatch<XdgToplevelDragManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelDragManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("xdg-toplevel-drag: a client bound the manager");
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelDragManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XdgToplevelDragManagerV1,
        request: xdg_toplevel_drag_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        if let xdg_toplevel_drag_manager_v1::Request::GetXdgToplevelDrag { id, data_source } =
            request
        {
            let taken = state
                .toplevel_drags
                .drags
                .iter()
                .any(|drag| drag.source == data_source);
            if taken {
                resource.post_error(
                    xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                    "data source already has a toplevel drag",
                );
                return;
            }
            tracing::info!("xdg-toplevel-drag: drag object created for a data source");
            let resource = data_init.init(id, data_source.clone());
            state.toplevel_drags.drags.push(Drag {
                resource,
                source: data_source,
                attached: None,
                active: false,
            });
        }
    }
}

impl Dispatch<XdgToplevelDragV1, WlDataSource> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XdgToplevelDragV1,
        request: xdg_toplevel_drag_v1::Request,
        _data: &WlDataSource,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        if let xdg_toplevel_drag_v1::Request::Attach {
            toplevel,
            x_offset,
            y_offset,
        } = request
        {
            state.toplevel_drag_attach(resource, &toplevel, (x_offset, y_offset));
        }
        // `Destroy` needs nothing here: `destroyed` forgets the drag. A client that destroys
        // it while the drag still runs gets no error (the protocol asks for one), the drag just
        // goes on without the window being moved by it.
    }

    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &XdgToplevelDragV1,
        _data: &WlDataSource,
    ) {
        let id: ObjectId = resource.id();
        state
            .toplevel_drags
            .drags
            .retain(|drag| drag.resource.id() != id);
    }
}

impl JffState {
    /// `attach`: the window follows the pointer for the rest of the drag (or from now on, when
    /// the drag already runs).
    fn toplevel_drag_attach(
        &mut self,
        resource: &XdgToplevelDragV1,
        toplevel: &XdgToplevel,
        offset: (i32, i32),
    ) {
        let window = self
            .desk
            .find_by(|w| w.toplevel().is_some_and(|t| t.xdg_toplevel() == toplevel))
            .map(|entry| entry.window.clone());
        let Some(window) = window else {
            return;
        };
        let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) else {
            return;
        };

        // Is another window still attached? Then it must go first.
        let attached_window = self
            .toplevel_drags
            .drags
            .iter()
            .find(|drag| drag.resource == *resource)
            .and_then(|drag| drag.attached.as_ref())
            .map(|attached| attached.window.clone());
        if let Some(old) = attached_window {
            if self.desk.find_by(|w| *w == old).is_some() {
                resource.post_error(
                    xdg_toplevel_drag_v1::Error::ToplevelAttached,
                    "a toplevel is already attached",
                );
                return;
            }
        }

        tracing::info!(
            "xdg-toplevel-drag: attach, offset {offset:?}, drag running: {}",
            self.toplevel_drags
                .drags
                .iter()
                .any(|drag| drag.resource == *resource && drag.active)
        );
        let mapped = is_mapped(&surface);
        let Some(drag) = self.toplevel_drags.drag_mut(resource) else {
            return;
        };
        drag.attached = Some(Attached {
            window: window.clone(),
            surface,
            offset,
            mapped,
        });
        // It is in front of the windows it is dragged over.
        self.desk.raise_where(|w| *w == window);
        self.update_toplevel_drag();
    }

    /// A data-device drag started by `client`: if one of its toplevel drag objects belongs to
    /// the source, the drag now carries its window.
    pub fn toplevel_drag_started(&mut self, client: Option<&Client>) {
        let client = client.map(|c| c.id());
        let found = self.toplevel_drags.drags.iter_mut().rev().find(|drag| {
            !drag.active
                && drag.source.is_alive()
                && client.as_ref().map_or(true, |id| {
                    drag.source.client().map(|c| c.id()).as_ref() == Some(id)
                })
        });
        if let Some(drag) = found {
            drag.active = true;
            tracing::info!(
                "xdg-toplevel-drag: drag started, window attached: {}",
                drag.attached.is_some()
            );
        }
        self.update_toplevel_drag();
    }

    /// The drag ended, dropped or cancelled. The window stays where it is, and is let go: at the
    /// edge of a monitor it snaps, like a window dragged by its titlebar.
    pub fn toplevel_drag_ended(&mut self, at: Point<f64, Logical>) {
        let mut carried = Vec::new();
        for drag in self.toplevel_drags.drags.iter_mut().filter(|d| d.active) {
            drag.active = false;
            if let Some(attached) = drag.attached.take() {
                carried.push(attached.window);
            }
        }
        tracing::info!(
            "xdg-toplevel-drag: drag ended at {at:?}, windows carried: {}",
            carried.len()
        );
        for window in carried {
            let Some(toplevel) = window.toplevel().cloned() else {
                continue;
            };
            // Unmapped or gone, or somebody changed its mode meanwhile.
            let size = window.geometry().size;
            let floating = self.desk.find_by(|w| *w == window).is_some()
                && self.desk.is_floating(|w| *w == window);
            if size.w <= 0 || size.h <= 0 || !floating {
                continue;
            }
            if let Some(mode) = self.edge_drop_mode(at) {
                self.set_mode(&toplevel, mode);
            }
        }
    }

    /// Move the window a running drag carries to the pointer. Called after the pointer moved.
    pub fn update_toplevel_drag(&mut self) {
        let Some(drag) = self
            .toplevel_drags
            .drags
            .iter()
            .find(|drag| drag.active && drag.source.is_alive())
        else {
            return;
        };
        let Some(attached) = drag.attached.as_ref() else {
            return;
        };
        let window = attached.window.clone();
        let offset = attached.offset;
        // A window that is maximized or snapped has no place of its own to move.
        if !self.desk.is_floating(|w| *w == window) {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let at = pointer.current_location();
        let pos = (
            at.x.round() as i32 - offset.0,
            at.y.round() as i32 - offset.1,
        );
        self.desk.set_pos_where(|w| *w == window, pos);
    }

    /// A commit of `root`. A window that was shown and is not any more is let go.
    pub fn toplevel_drag_commit(&mut self, root: &WlSurface) {
        let mapped = is_mapped(root);
        for drag in self.toplevel_drags.drags.iter_mut() {
            let Some(attached) = drag.attached.as_mut() else {
                continue;
            };
            if attached.surface != *root {
                continue;
            }
            if mapped {
                attached.mapped = true;
            } else if attached.mapped {
                drag.attached = None;
            }
        }
    }

    /// A toplevel is gone: no drag carries it.
    pub fn toplevel_drag_gone(&mut self, surface: &WlSurface) {
        for drag in self.toplevel_drags.drags.iter_mut() {
            if drag
                .attached
                .as_ref()
                .is_some_and(|attached| attached.surface == *surface)
            {
                drag.attached = None;
            }
        }
    }
}
