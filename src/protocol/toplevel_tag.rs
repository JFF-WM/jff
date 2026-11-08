//! xdg-toplevel-tag-v1: a client gives a window a stable tag ("browser", "picture-in-picture")
//! and a description.
//!
//! The tag is what `window-rule { match tag="…" }` reads (see `crate::config::WindowRule`).
//! Apps are asked to set it before their first commit, which is when rules are matched, but it
//! may change later. A later change does not move a window that has been placed already.
//!
//! The tag and description live in the surface's data map, so they go away with the surface.

use std::sync::Mutex;

use smithay::reexports::wayland_protocols::xdg::toplevel_tag::v1::server::xdg_toplevel_tag_manager_v1::{
    self, XdgToplevelTagManagerV1,
};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};
use smithay::wayland::compositor::with_states;

use crate::state::JffState;

/// Longest tag or description kept, in bytes. The protocol sets no limit, and the strings come
/// from clients.
const MAX_LEN: usize = 256;

pub struct ToplevelTagManager {
    _global: GlobalId,
}

impl ToplevelTagManager {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, XdgToplevelTagManagerV1, ()>(1, ()),
        }
    }
}

/// What a window said about itself. One per surface, in its data map.
#[derive(Default)]
struct ToplevelTagData {
    tag: Mutex<Option<String>>,
    description: Mutex<Option<String>>,
}

/// Cut `text` to [`MAX_LEN`] bytes, on a character boundary.
fn clamp(text: String) -> String {
    if text.len() <= MAX_LEN {
        return text;
    }
    let mut end = MAX_LEN;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn with_data<T>(surface: &WlSurface, f: impl FnOnce(&ToplevelTagData) -> T) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(ToplevelTagData::default);
        let data = states
            .data_map
            .get::<ToplevelTagData>()
            .expect("inserted just above");
        f(data)
    })
}

/// The tag `surface` set, `None` when it set none (or an empty one).
pub fn tag_of(surface: &WlSurface) -> Option<String> {
    with_data(surface, |data| data.tag.lock().unwrap().clone())
}

/// The description `surface` set, `None` when it set none (or an empty one).
pub fn description_of(surface: &WlSurface) -> Option<String> {
    with_data(surface, |data| data.description.lock().unwrap().clone())
}

impl GlobalDispatch<XdgToplevelTagManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelTagManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelTagManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &XdgToplevelTagManagerV1,
        request: xdg_toplevel_tag_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            xdg_toplevel_tag_manager_v1::Request::SetToplevelTag { toplevel, tag } => {
                let Some(surface) = state.xdg_shell_state.get_toplevel(&toplevel) else {
                    return;
                };
                let tag = Some(clamp(tag)).filter(|tag| !tag.is_empty());
                with_data(surface.wl_surface(), |data| {
                    *data.tag.lock().unwrap() = tag;
                });
            }
            xdg_toplevel_tag_manager_v1::Request::SetToplevelDescription {
                toplevel,
                description,
            } => {
                let Some(surface) = state.xdg_shell_state.get_toplevel(&toplevel) else {
                    return;
                };
                let description = Some(clamp(description)).filter(|text| !text.is_empty());
                with_data(surface.wl_surface(), |data| {
                    *data.description.lock().unwrap() = description;
                });
            }
            xdg_toplevel_tag_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
