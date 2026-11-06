//! wp_pointer_warp_v1: a client moves the pointer inside its own surface.

use smithay::backend::input::InputTime;
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::input::pointer::MotionEvent;
use smithay::reexports::wayland_protocols::wp::pointer_warp::v1::server::wp_pointer_warp_v1::{
    self, WpPointerWarpV1,
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use crate::primitives::layout;
use crate::state::JffState;

pub struct PointerWarp {
    _global: GlobalId,
}

impl PointerWarp {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, WpPointerWarpV1, ()>(1, ()),
        }
    }
}

impl GlobalDispatch<WpPointerWarpV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpPointerWarpV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpPointerWarpV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &WpPointerWarpV1,
        request: wp_pointer_warp_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        if let wp_pointer_warp_v1::Request::WarpPointer {
            surface,
            pointer,
            x,
            y,
            serial,
        } = request
        {
            // a bad request is ignored, the protocol lets us
            if surface.client() == pointer.client() {
                state.warp_pointer(&surface, (x, y).into(), serial);
            }
        }
    }
}

impl JffState {
    fn warp_pointer(&mut self, surface: &WlSurface, local: Point<f64, Logical>, serial: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.current_focus().as_ref() != Some(surface)
            || pointer.last_enter().map(u32::from) != Some(serial)
            || !bbox_from_surface_tree(surface, (0, 0))
                .to_f64()
                .contains(local)
        {
            return;
        }
        let Some((under, origin)) = self.pointer_focus_under(pointer.current_location()) else {
            return;
        };
        if under != *surface {
            return;
        }
        let (x, y) = layout::clamp_to_rects(
            (origin.x + local.x, origin.y + local.y),
            &self.pointer_bounds(),
        );
        let pos: Point<f64, Logical> = (x, y).into();
        let under = self.pointer_focus_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }
}
