//! gtk_shell1: GTK's private Wayland extension (`gdk/wayland/protocol/gtk-shell.xml` in GTK).
//!
//! GTK3 (and GTK4 for a few things) talks to the shell through it. jff advertises the global at
//! the version of the XML (7) and answers every request:
//!
//! * `gtk_surface1.set_dbus_properties` carries the app id, the `org.gtk.Menus` paths of the app
//!   menu and the menu bar, the `org.gtk.Actions` paths of the window and the app, and the bus
//!   name. That is a GMenuModel, not the `com.canonical.dbusmenu` that xdg-dbus-annotation and
//!   KDE appmenu carry (`xdg_dbus_annotation.rs`, `kde_appmenu.rs`). The addresses are filed as
//!   annotations of the surface, under the interface keys [`MENUBAR_INTERFACE`],
//!   [`APP_MENU_INTERFACE`], [`WINDOW_ACTIONS_INTERFACE`] and [`APP_ACTIONS_INTERFACE`], so
//!   `surface_address`, `surface_annotations` and the IPC `dbus_annotations` show them like
//!   any other. The app id and the a11y properties are kept in [`surface_properties`]. jff does
//!   not talk to DBus. A name or path that fails the checks of `primitives::dbus_name` is
//!   dropped.
//! * `set_modal` / `unset_modal` mark the window as a dialog, like `xdg_dialog_v1` does.
//! * `present` and `request_focus` ask for the window to be focused. They carry no input serial,
//!   so jff only believes them when their app got a key press, click or touch a moment ago
//!   (`InputSerials::client_acted_within`). Otherwise the window is marked urgent and the
//!   focus stays, which is what an xdg-activation token without a serial does too.
//! * `titlebar_gesture` (double click, right click, middle click on a client-drawn title bar)
//!   must carry the serial of the click it reacts to. A double click maximizes the window or
//!   puts it back. jff has no window menu and no lowering, so right and middle click do nothing.
//! * `gtk_shell1.system_bell` rings the bell like `xdg_system_bell_v1`.
//! * `gtk_surface1.configure` and `configure_edges` tell the window which of its edges are
//!   tiled and which can be resized, next to the xdg_toplevel states ([`send_configure`]).
//! * `set_startup_id`, `notify_launch` and `request_focus`'s startup id are startup
//!   notification. jff shows no launch feedback, so they are accepted and ignored.
//!
//! The `capabilities` event is sent with no bits set ([`CAPABILITIES`]). Setting
//! `global_app_menu` or `global_menu_bar` tells GTK to stop drawing that menu itself, which
//! only makes sense once something in jff's session shows it. Until then GTK apps keep their
//! own menu bar and the addresses above are there for whatever wants to read them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
};
use smithay::utils::Serial;
use smithay::wayland::compositor::{get_role, with_states};
use smithay::wayland::shell::xdg::{ToplevelSurface, XDG_TOPLEVEL_ROLE};

use super::gtk_shell_bindings::server::gtk_shell1::{self, GtkShell1};
use super::gtk_shell_bindings::server::gtk_surface1::{self, GtkSurface1};
use super::xdg_dbus_annotation::{remove_surface_slot, replace_surface_slots, DbusAddress};
use crate::desk::WindowMode;
use crate::primitives::dbus_name::{valid_interface, valid_object_path, valid_service_name};
use crate::state::JffState;

/// Highest version of the protocol, the one of `protocols/gtk-shell.xml`.
const VERSION: u32 = 7;

/// What `gtk_shell1.capabilities` says: no global app menu, no global menu bar, no desktop
/// icons. See the module documentation for why.
pub const CAPABILITIES: u32 = 0;

/// How long after a key press, click or touch an app may still ask for the focus with
/// `present` or `request_focus`.
const RECENT_INPUT: Duration = Duration::from_secs(5);

/// Annotation interface of the `org.gtk.Menus` object of the menu bar.
pub const MENUBAR_INTERFACE: &str = "org.gtk.Menus:menubar";
/// Annotation interface of the `org.gtk.Menus` object of the app menu.
pub const APP_MENU_INTERFACE: &str = "org.gtk.Menus:app-menu";
/// Annotation interface of the `org.gtk.Actions` object of the window.
pub const WINDOW_ACTIONS_INTERFACE: &str = "org.gtk.Actions:window";
/// Annotation interface of the `org.gtk.Actions` object of the application.
pub const APP_ACTIONS_INTERFACE: &str = "org.gtk.Actions:application";

/// The `gtk_shell1` global.
pub struct GtkShellManager {
    _global: GlobalId,
}

impl GtkShellManager {
    /// Create the `gtk_shell1` global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, GtkShell1, ()>(VERSION, ()),
        }
    }
}

/// What a GTK window said about itself besides the DBus addresses, which are annotations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GtkSurfaceProperties {
    /// `application_id` of `set_dbus_properties`: the GApplication id.
    pub application_id: Option<String>,
    /// Bus name of the accessibility service, from `set_a11y_properties`.
    pub a11y_bus_name: Option<String>,
    /// Object path of the toplevel on the accessibility bus, from `set_a11y_properties`.
    pub a11y_toplevel_path: Option<String>,
}

/// [`GtkSurfaceProperties`] in the data map of the surface.
#[derive(Default)]
struct SurfaceProps(Mutex<GtkSurfaceProperties>);

/// The `gtk_surface1` objects of a surface, in the data map of the surface, to send them
/// `configure`.
#[derive(Default)]
struct SurfaceResources(Mutex<Vec<GtkSurface1>>);

/// What the GTK window said about itself with `set_dbus_properties` and
/// `set_a11y_properties`, empty when it said nothing (or nothing valid).
pub fn surface_properties(surface: &WlSurface) -> GtkSurfaceProperties {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceProps>()
            .map(|props| props.0.lock().unwrap().clone())
            .unwrap_or_default()
    })
}

fn update_properties(surface: &WlSurface, f: impl FnOnce(&mut GtkSurfaceProperties)) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(SurfaceProps::default);
        let props = states
            .data_map
            .get::<SurfaceProps>()
            .expect("inserted just above");
        f(&mut props.0.lock().unwrap());
    });
}

fn register_resource(surface: &WlSurface, resource: &GtkSurface1) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(SurfaceResources::default);
        let resources = states
            .data_map
            .get::<SurfaceResources>()
            .expect("inserted just above");
        resources.0.lock().unwrap().push(resource.clone());
    });
}

fn unregister_resource(surface: &WlSurface, resource: &GtkSurface1) {
    with_states(surface, |states| {
        if let Some(resources) = states.data_map.get::<SurfaceResources>() {
            resources
                .0
                .lock()
                .unwrap()
                .retain(|r| r.id() != resource.id());
        }
    });
}

/// What a `gtk_surface1` object was created for.
pub struct GtkSurfaceData {
    /// The `wl_surface` it belongs to.
    surface: Weak<WlSurface>,
    /// Whether `set_modal` put the surface in `dialog_toplevels`, as opposed to it being there
    /// already because of `xdg_dialog_v1`. `unset_modal` only takes out what it put in.
    modal_added: AtomicBool,
}

impl GlobalDispatch<GtkShell1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        client: &Client,
        resource: New<GtkShell1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        tracing::info!("gtk-shell: manager bound by {:?}", client.id());
        let shell = data_init.init(resource, ());
        shell.capabilities(CAPABILITIES);
    }
}

impl Dispatch<GtkShell1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &GtkShell1,
        request: gtk_shell1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            gtk_shell1::Request::GetGtkSurface {
                gtk_surface,
                surface,
            } => {
                let object = data_init.init(
                    gtk_surface,
                    GtkSurfaceData {
                        surface: surface.downgrade(),
                        modal_added: AtomicBool::new(false),
                    },
                );
                register_resource(&surface, &object);
            }
            gtk_shell1::Request::SetStartupId { startup_id } => {
                tracing::debug!("gtk-shell: set_startup_id {startup_id:?} ignored");
            }
            gtk_shell1::Request::NotifyLaunch { startup_id } => {
                tracing::debug!("gtk-shell: notify_launch {startup_id:?} ignored");
            }
            gtk_shell1::Request::SystemBell { surface } => {
                let surface = surface
                    .as_ref()
                    .and_then(|s| s.data::<GtkSurfaceData>())
                    .and_then(|data| data.surface.upgrade().ok());
                tracing::debug!("gtk-shell: system bell");
                state.show_bell(surface);
            }
        }
    }
}

impl Dispatch<GtkSurface1, GtkSurfaceData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &GtkSurface1,
        request: gtk_surface1::Request,
        data: &GtkSurfaceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // `release` is handled in `destroyed`.
        let Some(surface) = data.surface.upgrade().ok() else {
            return;
        };
        match request {
            gtk_surface1::Request::SetDbusProperties {
                application_id,
                app_menu_path,
                menubar_path,
                window_object_path,
                application_object_path,
                unique_bus_name,
            } => {
                tracing::info!(
                    "gtk-shell: set_dbus_properties {application_id:?} {app_menu_path:?} \
                     {menubar_path:?} {window_object_path:?} {application_object_path:?} \
                     {unique_bus_name:?}"
                );
                // No role yet is fine, an app may send this before its xdg_toplevel.
                if get_role(&surface).is_some_and(|role| role != XDG_TOPLEVEL_ROLE) {
                    tracing::info!("gtk-shell: set_dbus_properties ignored, not a toplevel");
                    return;
                }
                let entries = dbus_entries(
                    unique_bus_name.as_deref(),
                    [
                        (MENUBAR_INTERFACE, menubar_path.as_deref()),
                        (APP_MENU_INTERFACE, app_menu_path.as_deref()),
                        (WINDOW_ACTIONS_INTERFACE, window_object_path.as_deref()),
                        (APP_ACTIONS_INTERFACE, application_object_path.as_deref()),
                    ],
                );
                replace_surface_slots(
                    &mut state.dbus_annotation,
                    &surface,
                    &resource.id(),
                    entries,
                );
                let application_id = application_id.filter(|id| valid_interface(id));
                update_properties(&surface, |props| props.application_id = application_id);
            }
            gtk_surface1::Request::SetModal => {
                // True when it was not there: then it is ours to take out again.
                if state.dialog_toplevels.insert(surface) {
                    data.modal_added.store(true, Ordering::SeqCst);
                }
            }
            gtk_surface1::Request::UnsetModal => {
                if data.modal_added.swap(false, Ordering::SeqCst) {
                    state.dialog_toplevels.remove(&surface);
                }
            }
            gtk_surface1::Request::Present { time } => {
                tracing::debug!("gtk-shell: present {time}");
                state.gtk_focus_request(&surface);
            }
            gtk_surface1::Request::RequestFocus { startup_id } => {
                tracing::debug!("gtk-shell: request_focus {startup_id:?}");
                state.gtk_focus_request(&surface);
            }
            gtk_surface1::Request::TitlebarGesture {
                serial,
                seat: _,
                gesture,
            } => match gesture {
                WEnum::Value(gesture) => {
                    state.gtk_titlebar_gesture(&surface, Serial::from(serial), gesture);
                }
                WEnum::Unknown(value) => {
                    resource.post_error(
                        gtk_surface1::Error::InvalidGesture,
                        format!("{value} is not a gesture"),
                    );
                }
            },
            gtk_surface1::Request::SetA11yProperties {
                a11y_dbus_name,
                toplevel_object_path,
            } => {
                let valid =
                    valid_service_name(&a11y_dbus_name) && valid_object_path(&toplevel_object_path);
                tracing::debug!(
                    "gtk-shell: set_a11y_properties {a11y_dbus_name:?} \
                     {toplevel_object_path:?}, valid: {valid}"
                );
                if valid {
                    update_properties(&surface, |props| {
                        props.a11y_bus_name = Some(a11y_dbus_name);
                        props.a11y_toplevel_path = Some(toplevel_object_path);
                    });
                }
            }
            gtk_surface1::Request::Release => {}
        }
    }

    // Also runs when the client goes away without releasing it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &GtkSurface1,
        data: &GtkSurfaceData,
    ) {
        let Some(surface) = data.surface.upgrade().ok() else {
            return;
        };
        remove_surface_slot(&surface, &resource.id());
        unregister_resource(&surface, resource);
        update_properties(&surface, |props| *props = GtkSurfaceProperties::default());
        if data.modal_added.swap(false, Ordering::SeqCst) {
            state.dialog_toplevels.remove(&surface);
        }
    }
}

/// The annotations `set_dbus_properties` asks for: one per path that is valid, all on the bus
/// name `bus`. None at all when the bus name is missing or not valid.
fn dbus_entries<const N: usize>(
    bus: Option<&str>,
    paths: [(&'static str, Option<&str>); N],
) -> Vec<(&'static str, DbusAddress)> {
    let Some(bus) = bus.filter(|bus| valid_service_name(bus)) else {
        return Vec::new();
    };
    paths
        .into_iter()
        .filter_map(|(interface, path)| {
            let path = path.filter(|path| valid_object_path(path))?;
            Some((
                interface,
                DbusAddress {
                    service_name: bus.to_string(),
                    object_path: path.to_string(),
                },
            ))
        })
        .collect()
}

impl JffState {
    /// `present` or `request_focus` of a GTK window: focus it when its app was acted on a
    /// moment ago, mark it urgent otherwise.
    fn gtk_focus_request(&mut self, surface: &WlSurface) {
        let Some(window) = self
            .desk
            .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .map(|entry| entry.window.clone())
        else {
            return;
        };
        let acted = surface.client().is_some_and(|client| {
            self.input_serials
                .client_acted_within(&client.id(), RECENT_INPUT)
        });
        if acted {
            self.show_workspace_of(&window);
            self.focus_window(&window);
        } else if self.focused_window().as_ref() != Some(&window) {
            tracing::debug!("gtk-shell: focus request without recent input, urgent only");
            self.mark_urgent(&window);
        }
    }

    /// `titlebar_gesture`: only a gesture that carries the serial of a recent click of the app
    /// does anything.
    fn gtk_titlebar_gesture(
        &mut self,
        surface: &WlSurface,
        serial: Serial,
        gesture: gtk_surface1::Gesture,
    ) {
        let accepted = surface
            .client()
            .is_some_and(|client| self.input_serials.accepts(serial, Some(&client.id())));
        if !accepted {
            tracing::debug!("gtk-shell: titlebar_gesture with a serial of no recent click");
            return;
        }
        match gesture {
            gtk_surface1::Gesture::DoubleClick => {
                let Some(window) = self
                    .desk
                    .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                    .map(|entry| entry.window.clone())
                else {
                    return;
                };
                let Some(toplevel) = window.toplevel().cloned() else {
                    return;
                };
                match self.desk.mode_of(|w| *w == window) {
                    WindowMode::Maximized => self.set_maximized(&toplevel, false),
                    WindowMode::Floating | WindowMode::Snapped(_) => {
                        self.set_maximized(&toplevel, true);
                    }
                    WindowMode::Fullscreen => {}
                }
            }
            // jff has no window menu and no lowering.
            gtk_surface1::Gesture::RightClick | gtk_surface1::Gesture::MiddleClick => {
                tracing::debug!("gtk-shell: titlebar_gesture {gesture:?} has no action");
            }
        }
    }
}

/// Which edges of a window are tiled, and whether it is maximized or fullscreen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Edges {
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
    /// Maximized or fullscreen: nothing to resize.
    filled: bool,
}

impl Edges {
    fn any_tiled(self) -> bool {
        self.top || self.right || self.bottom || self.left
    }

    /// The values of `gtk_surface1.state` for `configure`. A version 1 object only knows
    /// `tiled`.
    fn states(self, version: u32) -> Vec<u32> {
        let mut states = Vec::new();
        if self.any_tiled() {
            states.push(gtk_surface1::State::Tiled as u32);
            if version >= 2 {
                for (on, state) in [
                    (self.top, gtk_surface1::State::TiledTop),
                    (self.right, gtk_surface1::State::TiledRight),
                    (self.bottom, gtk_surface1::State::TiledBottom),
                    (self.left, gtk_surface1::State::TiledLeft),
                ] {
                    if on {
                        states.push(state as u32);
                    }
                }
            }
        }
        states
    }

    /// The values of `gtk_surface1.edge_constraint` for `configure_edges`: the edges a resize
    /// can start from. A window that is maximized, fullscreen or tiled against an edge of the
    /// screen has no handle there. A member of a container is tiled on all four sides, and its
    /// edges are the dividers of the tree, so all four stay.
    fn resizable(self) -> Vec<u32> {
        if self.filled {
            return Vec::new();
        }
        let member = self.top && self.right && self.bottom && self.left;
        [
            (self.top, gtk_surface1::EdgeConstraint::ResizableTop),
            (self.right, gtk_surface1::EdgeConstraint::ResizableRight),
            (self.bottom, gtk_surface1::EdgeConstraint::ResizableBottom),
            (self.left, gtk_surface1::EdgeConstraint::ResizableLeft),
        ]
        .into_iter()
        .filter(|(tiled, _)| member || !tiled)
        .map(|(_, constraint)| constraint as u32)
        .collect()
    }
}

/// An array argument: the values in native byte order.
fn encode_array(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// Send `gtk_surface1.configure` and `configure_edges` to the GTK objects of `toplevel`, with
/// the tiled edges of its pending state. Call it before the xdg configure that goes with it:
/// GTK applies both at the `xdg_surface.configure`. Does nothing for a window that never
/// bound `gtk_shell1`.
pub fn send_configure(toplevel: &ToplevelSurface) {
    let resources = with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<SurfaceResources>()
            .map(|resources| resources.0.lock().unwrap().clone())
            .unwrap_or_default()
    });
    if resources.is_empty() {
        return;
    }
    let edges = toplevel.with_pending_state(|state| {
        use xdg_toplevel::State as S;
        Edges {
            top: state.states.contains(S::TiledTop),
            right: state.states.contains(S::TiledRight),
            bottom: state.states.contains(S::TiledBottom),
            left: state.states.contains(S::TiledLeft),
            filled: state.states.contains(S::Maximized) || state.states.contains(S::Fullscreen),
        }
    });
    for resource in resources.iter().filter(|r| r.is_alive()) {
        let version = resource.version();
        resource.configure(encode_array(&edges.states(version)));
        if version >= 2 {
            resource.configure_edges(encode_array(&edges.resizable()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_is_native_endian_words() {
        assert_eq!(encode_array(&[1, 5]).len(), 8);
        assert_eq!(encode_array(&[1])[..], 1u32.to_ne_bytes());
        assert!(encode_array(&[]).is_empty());
    }

    #[test]
    fn untiled_window_has_no_states_and_four_edges() {
        let edges = Edges::default();
        assert!(edges.states(7).is_empty());
        assert_eq!(edges.resizable().len(), 4);
    }

    #[test]
    fn snapped_left_is_tiled_left_and_cannot_resize_there() {
        let edges = Edges {
            left: true,
            ..Edges::default()
        };
        assert_eq!(edges.states(7), vec![1, 5]);
        assert_eq!(edges.states(1), vec![1]);
        assert_eq!(edges.resizable(), vec![1, 2, 3]);
    }

    #[test]
    fn container_member_keeps_all_edges() {
        let edges = Edges {
            top: true,
            right: true,
            bottom: true,
            left: true,
            filled: false,
        };
        assert_eq!(edges.states(7), vec![1, 2, 3, 4, 5]);
        assert_eq!(edges.resizable(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn filled_window_has_no_edges() {
        let edges = Edges {
            filled: true,
            ..Edges::default()
        };
        assert!(edges.resizable().is_empty());
    }

    #[test]
    fn dbus_entries_need_a_valid_bus_and_valid_paths() {
        let paths = [
            (MENUBAR_INTERFACE, Some("/org/gtk/menus/menubar")),
            (APP_MENU_INTERFACE, None),
            (WINDOW_ACTIONS_INTERFACE, Some("not a path")),
            (APP_ACTIONS_INTERFACE, Some("/org/example/App")),
        ];
        let entries = dbus_entries(Some(":1.42"), paths);
        let keys: Vec<_> = entries.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![MENUBAR_INTERFACE, APP_ACTIONS_INTERFACE]);
        assert_eq!(entries[0].1.service_name, ":1.42");
        assert!(dbus_entries(None, paths).is_empty());
        assert!(dbus_entries(Some("nodots"), paths).is_empty());
    }
}
