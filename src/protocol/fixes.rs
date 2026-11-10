//! wl_fixes: small corrections to core Wayland that could not go in `wl_display` itself.
//! `destroy_registry` lets a client free a `wl_registry` it is done with, instead of the
//! object (and its ID) living until the client disconnects. Newer libwayland clients call it
//! right after their initial round of binding.

use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_fixes::{self, WlFixes};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::JffState;

pub struct Fixes {
    _global: GlobalId,
}

impl Fixes {
    pub fn new(display: &DisplayHandle) -> Self {
        // Version 1 is `destroy` and `destroy_registry`, which is all a client needs from it.
        Self {
            _global: display.create_global::<JffState, WlFixes, ()>(1, ()),
        }
    }
}

impl GlobalDispatch<WlFixes, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WlFixes>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlFixes, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &WlFixes,
        request: wl_fixes::Request,
        _data: &(),
        display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        if let wl_fixes::Request::DestroyRegistry { registry } = request {
            // libwayland owns the registry object, so it is freed through the backend.
            // A registry that is already gone is not an error worth reporting.
            let _ = display
                .backend_handle()
                .destroy_object::<JffState>(&registry.id());
        }
        // `destroy` needs nothing: the resource goes with the request.
    }
}
