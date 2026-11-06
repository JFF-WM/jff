//! Drag-and-drop between clients, and the drag icon that follows the pointer.
//!
//! Smithay hands us the drag through `WaylandDndGrabHandler::dnd_requested`. We take the
//! pointer with a `DnDGrab`, draw the icon the client attached, drop it when the drag ends.

use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, Focus};
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Physical, Point, Scale, Serial};
use smithay::wayland::selection::data_device::WaylandDndGrabHandler;

use crate::render::elements::JffElement;
use crate::state::JffState;

impl WaylandDndGrabHandler for JffState {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else {
                    source.cancel();
                    return;
                };
                // Needs a held button, or it's not a real drag.
                let Some(start_data) = pointer.grab_start_data() else {
                    source.cancel();
                    return;
                };
                self.dnd_icon = icon;
                // Nobody else sets the cursor during a drag, so this sticks.
                self.cursor
                    .set_status(CursorImageStatus::Named(CursorIcon::Grabbing));

                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                // Keep: the grab does the data-device focus itself.
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => source.cancel(), // no touch support
        }
    }
}

impl DndGrabHandler for JffState {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
        self.cursor.set_status(CursorImageStatus::default_named());
    }
}

/// The icon the client attached to the drag, drawn with its corner at `loc`. Empty when there
/// is no icon or it's gone.
///
/// Free function: the renderers borrow the backend inside `JffState`, so callers pass the one
/// field they need.
pub fn icon_elements(
    icon: Option<&WlSurface>,
    renderer: &mut GlesRenderer,
    loc: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Vec<JffElement> {
    match icon.filter(|icon| icon.alive()) {
        Some(icon) => {
            render_elements_from_surface_tree(renderer, icon, loc, scale, 1.0, Kind::Unspecified)
        }
        None => Vec::new(),
    }
}
