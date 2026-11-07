//! The opacity a client set on a surface (wp_alpha_modifier_v1).
//!
//! Smithay's surface elements already apply it when they draw (and report it in `alpha()`, so
//! the corner mask's `alpha` uniform and the opaque regions follow). The effects drawn around a
//! surface, blur, border - are not part of the surface, so they read it here and fade with it.
//! Only the surface it was set on is affected, never its subsurfaces.

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::alpha_modifier::AlphaModifierSurfaceCachedState;
use smithay::wayland::compositor::{with_states, SurfaceData};
use smithay::wayland::seat::WaylandFocus;

/// Multiplier of the surface owning `states`: 0.0 transparent, 1.0 opaque (also when the client
/// never set one). The committed value, the client's set is double-buffered.
pub fn multiplier_of(states: &SurfaceData) -> f32 {
    states
        .cached_state
        .get::<AlphaModifierSurfaceCachedState>()
        .current()
        .multiplier_f32()
        .unwrap_or(1.0)
}

/// [`multiplier_of`] for a surface.
pub fn surface_multiplier(surface: &WlSurface) -> f32 {
    with_states(surface, multiplier_of)
}

/// Multiplier of the root surface of `window`: what fades the window, its subsurfaces and
/// popups keep their own.
pub fn window_multiplier(window: &Window) -> f32 {
    window
        .wl_surface()
        .map_or(1.0, |surface| surface_multiplier(&surface))
}
