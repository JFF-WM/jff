//! KDE appmenu (`org_kde_kwin_appmenu_manager`): the older protocol Qt and some browsers use to
//! tell the compositor where the DBus menu of a window lives. Apps that speak
//! xdg-dbus-annotation (`xdg_dbus_annotation.rs`) use that one instead.
//!
//! The two protocols carry the same thing, so jff keeps both in one place. An appmenu object is
//! stored as an annotation of its surface with the interface `com.canonical.dbusmenu`, next to
//! the annotations of xdg-dbus-annotation: `surface_address`, `surface_annotations` and the IPC
//! `dbus_annotations` show it without knowing which protocol it came from.
//!
//! jff only keeps what it is told, it does not talk to DBus. A surface has at most one
//! `com.canonical.dbusmenu` annotation, from either protocol. A second appmenu object for a
//! surface that already has one is inert: the protocol has no error for it, so jff keeps the
//! first and ignores the new one. An address that fails the checks of `primitives::dbus_name`
//! is dropped, so the previous address stays. The annotation goes away with its appmenu object,
//! which also happens when the client disconnects without releasing it, or with its surface.

use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::compositor::get_role;
use smithay::wayland::shell::xdg::XDG_TOPLEVEL_ROLE;

use super::kde_appmenu_bindings::server::org_kde_kwin_appmenu::{self, OrgKdeKwinAppmenu};
use super::kde_appmenu_bindings::server::org_kde_kwin_appmenu_manager::{
    self, OrgKdeKwinAppmenuManager,
};
use super::xdg_dbus_annotation::{
    add_surface_slot, remove_surface_slot, set_surface_slot_address, DbusAddress,
};
use crate::primitives::dbus_name::{valid_object_path, valid_service_name};
use crate::state::JffState;

/// The interface every appmenu object is filed under.
pub const DBUSMENU_INTERFACE: &str = "com.canonical.dbusmenu";

/// The `org_kde_kwin_appmenu_manager` global.
pub struct KdeAppmenuManager {
    _global: GlobalId,
}

impl KdeAppmenuManager {
    /// Create the `org_kde_kwin_appmenu_manager` global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, OrgKdeKwinAppmenuManager, ()>(1, ()),
        }
    }
}

/// What an `org_kde_kwin_appmenu` object was created for.
pub struct AppmenuData {
    /// `None` when the object is inert: its surface was no toplevel or already had a menu.
    surface: Option<Weak<WlSurface>>,
}

impl GlobalDispatch<OrgKdeKwinAppmenuManager, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        client: &Client,
        resource: New<OrgKdeKwinAppmenuManager>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("kde-appmenu: manager bound by {:?}", client.id());
        data_init.init(resource, ());
    }
}

impl Dispatch<OrgKdeKwinAppmenuManager, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &OrgKdeKwinAppmenuManager,
        request: org_kde_kwin_appmenu_manager::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            org_kde_kwin_appmenu_manager::Request::Create { id, surface } => {
                let role = get_role(&surface);
                tracing::info!("kde-appmenu: create for a surface with role {role:?}");
                // No role yet is fine, an app may create this before its xdg_toplevel.
                if role.is_some_and(|role| role != XDG_TOPLEVEL_ROLE) {
                    tracing::info!("kde-appmenu: create ignored, the surface is not a toplevel");
                    data_init.init(id, AppmenuData { surface: None });
                    return;
                }
                // The object id is needed to file the slot, so create the object first.
                let object = data_init.init(
                    id,
                    AppmenuData {
                        surface: Some(surface.downgrade()),
                    },
                );
                // False when the surface already has a menu. The object stays inert then: it
                // owns no slot, so `set_address` and `destroyed` below find nothing to touch.
                let added = add_surface_slot(
                    &mut state.dbus_annotation,
                    &surface,
                    object.id(),
                    DBUSMENU_INTERFACE,
                );
                tracing::info!(
                    "kde-appmenu: create {}",
                    if added {
                        "stored"
                    } else {
                        "ignored, the surface already has a dbusmenu annotation"
                    }
                );
            }
            org_kde_kwin_appmenu_manager::Request::Release => {}
        }
    }
}

impl Dispatch<OrgKdeKwinAppmenu, AppmenuData> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        resource: &OrgKdeKwinAppmenu,
        request: org_kde_kwin_appmenu::Request,
        data: &AppmenuData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let org_kde_kwin_appmenu::Request::SetAddress {
            service_name,
            object_path,
        } = request
        else {
            // `release` is handled in `destroyed`.
            return;
        };
        tracing::info!("kde-appmenu: set_address {service_name:?} {object_path:?}");
        if !valid_service_name(&service_name) || !valid_object_path(&object_path) {
            tracing::info!("kde-appmenu: set_address rejected, not a valid DBus name or path");
            return;
        }
        let Some(surface) = data.surface.as_ref().and_then(|s| s.upgrade().ok()) else {
            tracing::info!(
                "kde-appmenu: set_address ignored, the object is inert or its surface is gone"
            );
            return;
        };
        set_surface_slot_address(
            &surface,
            &resource.id(),
            DbusAddress {
                service_name,
                object_path,
            },
        );
    }

    // Also runs when the client goes away without releasing it.
    fn destroyed(
        _state: &mut JffState,
        _client: ClientId,
        resource: &OrgKdeKwinAppmenu,
        data: &AppmenuData,
    ) {
        tracing::info!("kde-appmenu: appmenu object released");
        if let Some(surface) = data.surface.as_ref().and_then(|s| s.upgrade().ok()) {
            remove_surface_slot(&surface, &resource.id());
        }
    }
}
