//! wp_content_type_v1: a client says what a surface shows (photo, video, or game).
//!
//! Smithay keeps the value as double-buffered surface state, so what we read here is from the
//! last commit. jff only uses it for fullscreen windows (see
//! `crate::protocol::layer_shell::fullscreen_says_game`): with `content-type-hint` in the
//! output's config, a covering fullscreen window that says `game` makes jff set the connector's
//! `content type` property, so the monitor can switch to its game mode.
//!
//! A `window-rule` with `content-type` can say it instead of the client, or over it. X11 windows
//! (xwayland-satellite) cannot bind the protocol at all, and not every Wayland client does, so a
//! rule is how such a window gets to be told as a game. The rule is stored in the surface's data map by
//! [`set_override`] and wins over the client in [`says_game`], so the connector logic and the
//! stats overlay both see it.

use std::sync::Mutex;

use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::content_type::ContentTypeSurfaceCachedState;

/// What a `window-rule` said about one surface: `Some(true)` is a game, `Some(false)` is not,
/// `None` leaves it to the client.
#[derive(Default)]
struct RuleOverride(Mutex<Option<bool>>);

/// Make `surface` a game (`Some(true)`) or not (`Some(false)`) whatever its client says, or
/// give it back to the client with `None`.
pub fn set_override(surface: &WlSurface, game: Option<bool>) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(RuleOverride::default);
        let data = states
            .data_map
            .get::<RuleOverride>()
            .expect("inserted just above");
        *data.0.lock().unwrap() = game;
    });
}

/// Whether `surface` shows a game: what a `window-rule` says if one did, else whether the last
/// commit of the client said so.
pub fn says_game(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        if let Some(forced) = states
            .data_map
            .get::<RuleOverride>()
            .and_then(|data| *data.0.lock().unwrap())
        {
            return forced;
        }
        let mut cached = states.cached_state.get::<ContentTypeSurfaceCachedState>();
        matches!(cached.current().content_type(), Type::Game)
    })
}
