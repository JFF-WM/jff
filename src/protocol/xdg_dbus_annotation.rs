//! xdg-dbus-annotation-v1: an app tells jff which DBus object belongs to one of its windows
//! (or to the app itself), for example the `com.canonical.dbusmenu` object behind its menu bar.
//!
//! jff only keeps what it is told. It does not talk to DBus: a panel that wants the menu of
//! the focused window reads the address from here ([`surface_address`], [`surface_annotations`],
//! [`DbusAnnotationManager::client_address`]) and connects itself.
//!
//! An annotation is filed under its interface name, at most one per interface for a surface or
//! for a client (a second one is the `already_annotated` protocol error). It has no address
//! until the app sends `set_address`, and one that fails the checks of
//! `primitives::dbus_name` is dropped, so the previous address stays.
//!
//! Annotations of a surface live in its data map and go away with it. Annotations of a client
//! live in [`DbusAnnotationManager`] and go away with their resource, which also happens when
//! the client disconnects without destroying them.

use std::collections::HashMap;
use std::sync::Mutex;

use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::compositor::{get_role, with_states};
use smithay::wayland::shell::xdg::XDG_TOPLEVEL_ROLE;

use super::xdg_dbus_annotation_bindings::server::xdg_dbus_annotation_manager_v1::{
    self, Error, XdgDbusAnnotationManagerV1,
};
use super::xdg_dbus_annotation_bindings::server::xdg_dbus_annotation_v1::{
    self, XdgDbusAnnotationV1,
};
use crate::primitives::dbus_name::{valid_interface, valid_object_path, valid_service_name};
use crate::state::JffState;

/// Where an app's DBus object lives on the session bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbusAddress {
    /// Bus name of the service, unique (`:1.42`) or well-known (`org.example.App`).
    pub service_name: String,
    /// Object path on that service.
    pub object_path: String,
}

/// One annotation as other code sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbusAnnotation {
    /// Tells this annotation apart from every other one jff has seen, for as long as jff runs.
    pub id: u64,
    /// Interface the object is expected to implement.
    pub interface: String,
    /// Address of the object, `None` until the app has sent a valid one.
    pub address: Option<DbusAddress>,
}

/// An annotation and the protocol object that owns it.
struct Slot {
    owner: ObjectId,
    annotation: DbusAnnotation,
}

/// Annotations of one surface. One per surface, in its data map.
#[derive(Default)]
struct SurfaceSlots(Mutex<Vec<Slot>>);

/// What an `xdg_dbus_annotation_v1` object was created for.
pub struct AnnotationData {
    target: Target,
}

enum Target {
    /// Inert: the interface name was invalid, nothing is stored.
    None,
    Client(ClientId),
    Surface(Weak<WlSurface>),
}

/// The global, and the annotations that belong to clients rather than to a surface.
pub struct DbusAnnotationManager {
    _global: GlobalId,
    clients: HashMap<ClientId, Vec<Slot>>,
    last_id: u64,
}

impl DbusAnnotationManager {
    /// Create the `xdg_dbus_annotation_manager_v1` global.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, XdgDbusAnnotationManagerV1, ()>(1, ()),
            clients: HashMap::new(),
            last_id: 0,
        }
    }

    /// The next [`DbusAnnotation::id`].
    pub(crate) fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// Every client that has annotations, with them in creation order.
    pub fn all_client_annotations(&self) -> Vec<(ClientId, Vec<DbusAnnotation>)> {
        self.clients
            .iter()
            .map(|(client, slots)| {
                let annotations = slots.iter().map(|slot| slot.annotation.clone()).collect();
                (client.clone(), annotations)
            })
            .collect()
    }

    /// Every annotation `client` made for itself, in creation order.
    pub fn client_annotations(&self, client: &ClientId) -> Vec<DbusAnnotation> {
        self.clients
            .get(client)
            .map(|slots| slots.iter().map(|slot| slot.annotation.clone()).collect())
            .unwrap_or_default()
    }

    /// The address `client` gave for `interface`, `None` when it made no such annotation or has
    /// not set an address yet.
    pub fn client_address(&self, client: &ClientId, interface: &str) -> Option<DbusAddress> {
        self.clients
            .get(client)?
            .iter()
            .find(|slot| slot.annotation.interface == interface)?
            .annotation
            .address
            .clone()
    }
}

fn with_slots<T>(surface: &WlSurface, f: impl FnOnce(&mut Vec<Slot>) -> T) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(SurfaceSlots::default);
        let slots = states
            .data_map
            .get::<SurfaceSlots>()
            .expect("inserted just above");
        f(&mut slots.0.lock().unwrap())
    })
}

/// Every annotation `surface` carries, in creation order.
pub fn surface_annotations(surface: &WlSurface) -> Vec<DbusAnnotation> {
    with_slots(surface, |slots| {
        slots.iter().map(|slot| slot.annotation.clone()).collect()
    })
}

/// The address `surface` gave for `interface`, `None` when it has no such annotation or no
/// address was set yet.
pub fn surface_address(surface: &WlSurface, interface: &str) -> Option<DbusAddress> {
    with_slots(surface, |slots| {
        slots
            .iter()
            .find(|slot| slot.annotation.interface == interface)
            .and_then(|slot| slot.annotation.address.clone())
    })
}

/// File a new, addressless annotation of `interface` on `surface`, owned by the protocol object
/// `owner`. For other protocols that carry the same thing (`kde_appmenu.rs`). Returns `false`
/// and stores nothing when `surface` already has an annotation with this interface.
pub(crate) fn add_surface_slot(
    manager: &mut DbusAnnotationManager,
    surface: &WlSurface,
    owner: ObjectId,
    interface: &str,
) -> bool {
    let taken = with_slots(surface, |slots| {
        slots.iter().any(|s| s.annotation.interface == interface)
    });
    if taken {
        return false;
    }
    let id = manager.next_id();
    with_slots(surface, |slots| {
        slots.push(Slot {
            owner,
            annotation: DbusAnnotation {
                id,
                interface: interface.to_string(),
                address: None,
            },
        });
    });
    true
}

/// Give the annotation `owner` made on `surface` an address. Does nothing when `owner` has no
/// annotation there.
pub(crate) fn set_surface_slot_address(
    surface: &WlSurface,
    owner: &ObjectId,
    address: DbusAddress,
) {
    with_slots(surface, |slots| {
        if let Some(slot) = slots.iter_mut().find(|slot| &slot.owner == owner) {
            slot.annotation.address = Some(address);
        }
    });
}

/// Drop the annotation `owner` made on `surface`, if it has one.
pub(crate) fn remove_surface_slot(surface: &WlSurface, owner: &ObjectId) {
    with_slots(surface, |slots| slots.retain(|slot| &slot.owner != owner));
}

impl GlobalDispatch<XdgDbusAnnotationManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgDbusAnnotationManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgDbusAnnotationManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        client: &Client,
        resource: &XdgDbusAnnotationManagerV1,
        request: xdg_dbus_annotation_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            xdg_dbus_annotation_manager_v1::Request::CreateClient { interface, id } => {
                let client_id = client.id();
                if !valid_interface(&interface) {
                    data_init.init(
                        id,
                        AnnotationData {
                            target: Target::None,
                        },
                    );
                    return;
                }
                let taken = state
                    .dbus_annotation
                    .clients
                    .get(&client_id)
                    .is_some_and(|slots| slots.iter().any(|s| s.annotation.interface == interface));
                let object = data_init.init(
                    id,
                    AnnotationData {
                        target: if taken {
                            Target::None
                        } else {
                            Target::Client(client_id.clone())
                        },
                    },
                );
                if taken {
                    resource.post_error(
                        Error::AlreadyAnnotated,
                        "the client already has an annotation with this interface",
                    );
                    return;
                }
                let id = state.dbus_annotation.next_id();
                state
                    .dbus_annotation
                    .clients
                    .entry(client_id)
                    .or_default()
                    .push(Slot {
                        owner: object.id(),
                        annotation: DbusAnnotation {
                            id,
                            interface,
                            address: None,
                        },
                    });
            }
            xdg_dbus_annotation_manager_v1::Request::CreateSurface {
                interface,
                id,
                toplevel,
            } => {
                if !valid_interface(&interface) {
                    data_init.init(
                        id,
                        AnnotationData {
                            target: Target::None,
                        },
                    );
                    return;
                }
                // No role yet is fine, an app may annotate before it creates its xdg_toplevel.
                if get_role(&toplevel).is_some_and(|role| role != XDG_TOPLEVEL_ROLE) {
                    data_init.init(
                        id,
                        AnnotationData {
                            target: Target::None,
                        },
                    );
                    resource.post_error(Error::BadTarget, "the wl_surface is not a toplevel");
                    return;
                }
                let taken = with_slots(&toplevel, |slots| {
                    slots.iter().any(|s| s.annotation.interface == interface)
                });
                let object = data_init.init(
                    id,
                    AnnotationData {
                        target: if taken {
                            Target::None
                        } else {
                            Target::Surface(toplevel.downgrade())
                        },
                    },
                );
                if taken {
                    resource.post_error(
                        Error::AlreadyAnnotated,
                        "the wl_surface already has an annotation with this interface",
                    );
                    return;
                }
                let id = state.dbus_annotation.next_id();
                with_slots(&toplevel, |slots| {
                    slots.push(Slot {
                        owner: object.id(),
                        annotation: DbusAnnotation {
                            id,
                            interface,
                            address: None,
                        },
                    });
                });
            }
            xdg_dbus_annotation_manager_v1::Request::Destroy => {}
        }
    }
}

impl Dispatch<XdgDbusAnnotationV1, AnnotationData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XdgDbusAnnotationV1,
        request: xdg_dbus_annotation_v1::Request,
        data: &AnnotationData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let xdg_dbus_annotation_v1::Request::SetAddress {
            service_name,
            object_path,
        } = request
        else {
            // `destroy` is handled in `destroyed`.
            return;
        };
        if !valid_service_name(&service_name) || !valid_object_path(&object_path) {
            return;
        }
        let address = Some(DbusAddress {
            service_name,
            object_path,
        });
        let id = resource.id();
        let set = |slots: &mut Vec<Slot>| {
            if let Some(slot) = slots.iter_mut().find(|slot| slot.owner == id) {
                slot.annotation.address = address.clone();
            }
        };
        match &data.target {
            Target::None => {}
            Target::Client(client) => {
                if let Some(slots) = state.dbus_annotation.clients.get_mut(client) {
                    set(slots);
                }
            }
            Target::Surface(surface) => {
                if let Ok(surface) = surface.upgrade() {
                    with_slots(&surface, set);
                }
            }
        }
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &XdgDbusAnnotationV1,
        data: &AnnotationData,
    ) {
        let id = resource.id();
        match &data.target {
            Target::None => {}
            Target::Client(client) => {
                if let Some(slots) = state.dbus_annotation.clients.get_mut(client) {
                    slots.retain(|slot| slot.owner != id);
                    if slots.is_empty() {
                        state.dbus_annotation.clients.remove(client);
                    }
                }
            }
            Target::Surface(surface) => {
                if let Ok(surface) = surface.upgrade() {
                    with_slots(&surface, |slots| slots.retain(|slot| slot.owner != id));
                }
            }
        }
    }
}
