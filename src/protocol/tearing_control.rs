//! wp_tearing_control_v1: a client says whether a surface may be shown with tearing (as soon as
//! possible, not at the next vblank), which games use to cut input lag.
//!
//! The protocol is only a hint. The TTY backend flips in sync with the display, except with
//! `allow-tearing` set and a fullscreen window covering the monitor that asked for tearing:
//! that frame is queued as an async page flip (`PresentationMode::Async`), and falls back to
//! a normal flip when the hardware or the frame can't do it. This module advertises the global, enforces the rules (one object per
//! surface), and remembers the hint (see `wants_tearing`). The hint is stored the moment it's
//! set, not on the next commit, that only matters once something uses it.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_manager_v1::{
    self, WpTearingControlManagerV1,
};
use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_v1::{
    self, PresentationHint, WpTearingControlV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::wayland::compositor::with_states;

use crate::state::JffState;

/// What's remembered of a surface, in its user data.
#[derive(Default)]
struct SurfaceTearing {
    /// The client asked for async presentation.
    hint_async: AtomicBool,
    /// A `wp_tearing_control_v1` object exists for the surface.
    has_object: AtomicBool,
}

/// User data of a `wp_tearing_control_v1` object.
pub struct TearingObject {
    surface: WlSurface,
    /// False for the extra object of a surface that already had one (a protocol error): it
    /// mustn't touch what the first one set.
    owner: bool,
}

/// The `wp_tearing_control_manager_v1` global.
pub struct TearingControlState {
    #[allow(dead_code)]
    global: GlobalId,
}

impl TearingControlState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            global: display.create_global::<JffState, WpTearingControlManagerV1, ()>(1, ()),
        }
    }
}

/// Whether the client asked for `surface` to be shown with tearing. The TTY backend acts on it
/// for a fullscreen window covering the monitor, with `allow-tearing` set.
pub fn wants_tearing(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states
            .data_map
            .get::<SurfaceTearing>()
            .is_some_and(|data| data.hint_async.load(Ordering::SeqCst))
    })
}

fn set_hint(surface: &WlSurface, hint_async: bool) {
    if !surface.is_alive() {
        return;
    }
    with_states(surface, |states| {
        if let Some(data) = states.data_map.get::<SurfaceTearing>() {
            data.hint_async.store(hint_async, Ordering::SeqCst);
        }
    });
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for JffState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for JffState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } = request {
            let already = with_states(&surface, |states| {
                states
                    .data_map
                    .insert_if_missing_threadsafe(SurfaceTearing::default);
                states
                    .data_map
                    .get::<SurfaceTearing>()
                    .map_or(false, |data| data.has_object.swap(true, Ordering::SeqCst))
            });
            // The new id has to be initialised even when the request is an error.
            data_init.init(
                id,
                TearingObject {
                    surface,
                    owner: !already,
                },
            );
            if already {
                manager.post_error(
                    wp_tearing_control_manager_v1::Error::TearingControlExists,
                    "the surface already has a tearing control object",
                );
            }
        }
    }
}

impl Dispatch<WpTearingControlV1, TearingObject> for JffState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &TearingObject,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_tearing_control_v1::Request::SetPresentationHint { hint } = request {
            if data.owner {
                set_hint(
                    &data.surface,
                    matches!(hint, WEnum::Value(PresentationHint::Async)),
                );
            }
        }
    }

    // The object is gone (destroyed, or its client left): the surface goes back to vsync and
    // can get a new object.
    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _resource: &WpTearingControlV1,
        data: &TearingObject,
    ) {
        if !data.owner || !data.surface.is_alive() {
            return;
        }
        with_states(&data.surface, |states| {
            if let Some(surface) = states.data_map.get::<SurfaceTearing>() {
                surface.hint_async.store(false, Ordering::SeqCst);
                surface.has_object.store(false, Ordering::SeqCst);
            }
        });
    }
}
