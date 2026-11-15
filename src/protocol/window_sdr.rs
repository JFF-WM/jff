//! How a `window-rule` (`sdr`, `sdr-white`) says the colors of a window are read.
//!
//! The setting lives in the data map of the window's root surface, which is what every drawing
//! path asks `render::effects::gamut::conversions` about (the window itself, its opening
//! picture, the overview and screenshots), so all of them agree. It goes away with the surface.

use std::sync::Mutex;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;

use crate::primitives::hdr_output::WindowSdr;

/// What the rules said about one surface.
#[derive(Default)]
struct WindowSdrData(Mutex<WindowSdr>);

/// The setting of `surface`, the default (nothing) when no rule gave it one.
pub fn of(surface: &WlSurface) -> WindowSdr {
    with_states(surface, |states| {
        states
            .data_map
            .get::<WindowSdrData>()
            .map_or_else(WindowSdr::default, |data| *data.0.lock().unwrap())
    })
}

/// Give `surface` this setting, the default one takes it away again.
pub fn set(surface: &WlSurface, setting: WindowSdr) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(WindowSdrData::default);
        let data = states
            .data_map
            .get::<WindowSdrData>()
            .expect("inserted just above");
        *data.0.lock().unwrap() = setting;
    });
}
