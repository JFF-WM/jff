//! wp_content_type_v1: a client says what a surface shows (photo, video, or game).
//!
//! Smithay keeps the value as double-buffered surface state, so what we read here is from the
//! last commit. jff only uses it for fullscreen windows (see
//! `crate::protocol::layer_shell::fullscreen_says_game`): with `content-type-hint` in the
//! output's config, a covering fullscreen window that says `game` makes jff set the connector's
//! `content type` property, so the monitor can switch to its game mode.

use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::content_type::ContentTypeSurfaceCachedState;

/// Whether the last commit of `surface` said its content is a game.
pub fn says_game(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ContentTypeSurfaceCachedState>();
        matches!(cached.current().content_type(), Type::Game)
    })
}
