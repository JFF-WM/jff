//! wl_fixes: small corrections to core Wayland that could not go in `wl_display` itself.
//! `destroy_registry` lets a client free a `wl_registry` it is done with, instead of the
//! object (and its ID) living until the client disconnects. Newer libwayland clients call it
//! right after their initial round of binding.
//!
//! Version 2 adds `ack_global_remove`: a client answers every `wl_registry.global_remove` with
//! it, which tells the compositor it will not bind that global any more. A global that goes
//! away (an unplugged monitor) can then be destroyed as soon as every client has answered,
//! instead of after a guess of how long the news takes to arrive, see
//! `feature/outputs/mod.rs`. Clients that bound version 1 never answer, the guess stays for them.

use std::collections::HashSet;

use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_fixes::{self, WlFixes};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::JffState;

/// The first version with `ack_global_remove`.
const ACK_GLOBAL_REMOVE_SINCE: u32 = 2;

pub struct Fixes {
    _global: GlobalId,
    /// Clients that bound wl_fixes at version 2 or later: they answer every
    /// `wl_registry.global_remove` with `ack_global_remove`. Clients that left are dropped
    /// from it whenever a global is removed (`feature/outputs/mod.rs`).
    pub acking: HashSet<ClientId>,
}

impl Fixes {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, WlFixes, ()>(2, ()),
            acking: HashSet::new(),
        }
    }
}

impl GlobalDispatch<WlFixes, ()> for JffState {
    fn bind(
        state: &mut JffState,
        _display: &DisplayHandle,
        client: &Client,
        resource: New<WlFixes>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let fixes = data_init.init(resource, ());
        if fixes.version() >= ACK_GLOBAL_REMOVE_SINCE {
            state.fixes.acking.insert(client.id());
        }
    }
}

impl Dispatch<WlFixes, ()> for JffState {
    fn request(
        state: &mut JffState,
        client: &Client,
        _resource: &WlFixes,
        request: wl_fixes::Request,
        _data: &(),
        display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            wl_fixes::Request::DestroyRegistry { registry } => {
                // libwayland owns the registry object, so it is freed through the backend.
                // A registry that is already gone is not an error worth reporting.
                let _ = display
                    .backend_handle()
                    .destroy_object::<JffState>(&registry.id());
            }
            wl_fixes::Request::AckGlobalRemove { name, .. } => {
                // An ack for a global already destroyed (the others were quicker, or the wait
                // ran out) is not an error either, so `invalid_ack_remove` is never raised.
                state.ack_global_remove(client, name);
            }
            // `destroy` needs nothing: the resource goes with the request.
            _ => {}
        }
    }
}
