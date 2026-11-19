//! wp_content_type_v1: a client says what a surface shows (photo, video, or game).
//!
//! Smithay keeps the value as double-buffered surface state, so what we read here is from the
//! last commit. jff only uses it for fullscreen windows (see
//! `crate::protocol::layer_shell::fullscreen_content`): with `content-type-hint` in the
//! output's config, a covering fullscreen window that says `photo`, `video` or `game` makes jff
//! set the connector's `content type` property to the matching value, so the monitor can switch
//! to its photo, cinema or game mode.
//!
//! A `window-rule` with `content-type` can say it instead of the client, or over it. X11 windows
//! (xwayland-satellite) cannot bind the protocol at all, and not every Wayland client does, so a
//! rule is how such a window gets to be told as a game or a video. The rule is stored in the
//! surface's data map by [`set_override`] and wins over the client in [`content`], so the
//! connector logic and the stats overlay both see it.

use std::sync::Mutex;

use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::content_type::ContentTypeSurfaceCachedState;

/// What a surface shows, as far as jff acts on it: the three values of wp_content_type_v1 that
/// have a connector `content type` of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Photo,
    Video,
    Game,
}

impl Content {
    /// The name the config, the logs and the stats overlay use.
    pub fn name(self) -> &'static str {
        match self {
            Content::Photo => "photo",
            Content::Video => "video",
            Content::Game => "game",
        }
    }
}

/// What a `window-rule` said about one surface: `Some(Some(c))` is that content, `Some(None)`
/// is none whatever the client says, `None` leaves it to the client.
#[derive(Default)]
struct RuleOverride(Mutex<Option<Option<Content>>>);

/// Make `surface` show `content` (`Some(Some(..))`) or nothing (`Some(None)`) whatever its client
/// says, or give it back to the client with `None`.
pub fn set_override(surface: &WlSurface, content: Option<Option<Content>>) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(RuleOverride::default);
        let data = states
            .data_map
            .get::<RuleOverride>()
            .expect("inserted just above");
        *data.0.lock().unwrap() = content;
    });
}

/// What `surface` shows: what a `window-rule` says if one did, else what the last commit of the
/// client said. `None` for no content type, or one jff does not act on.
pub fn content(surface: &WlSurface) -> Option<Content> {
    with_states(surface, |states| {
        if let Some(forced) = states
            .data_map
            .get::<RuleOverride>()
            .and_then(|data| *data.0.lock().unwrap())
        {
            return forced;
        }
        let mut cached = states.cached_state.get::<ContentTypeSurfaceCachedState>();
        match cached.current().content_type() {
            Type::Photo => Some(Content::Photo),
            Type::Video => Some(Content::Video),
            Type::Game => Some(Content::Game),
            _ => None,
        }
    })
}
