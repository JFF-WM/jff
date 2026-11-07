//! xdg-toplevel-icon-v1: a client says which icon its window has, by theme name or as pictures.
//!
//! Smithay keeps what the client set as double-buffered surface state, so what we read here is
//! from the last commit. The overview uses it for the app icon on a window's thumbnail, ahead of
//! the icon of the desktop file the app id points at (see `crate::feature::overview_mode`): the
//! app knows best, and some apps have no desktop file at all.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use smithay::wayland::xdg_toplevel_icon::ToplevelIconCachedState;

use crate::feature::app_icons::{icon_for, scale, Icon, ICON_PX};

/// Sizes (square edge, pixels) clients are told to prefer.
pub const ICON_SIZES: [i32; 3] = [32, 64, 128];
/// Don't read pictures bigger than this on a side.
const MAX_SIDE: i32 = 1024;

/// Stands for what `surface` last set as its icon, `0` when it set none. Changes when the icon
/// does, so a painted badge can be told to be out of date without reading any pixels.
pub fn fingerprint(surface: &WlSurface) -> u64 {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ToplevelIconCachedState>();
        let state = cached.current();
        if state.icon_name().is_none() && state.buffers().is_empty() {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        state.icon_name().hash(&mut hasher);
        for (buffer, scale) in state.buffers() {
            buffer.id().hash(&mut hasher);
            scale.hash(&mut hasher);
        }
        hasher.finish().max(1)
    })
}

/// The icon `surface` set, ready to draw, and whether it is sharp: found by theme name, or a
/// picture at least as big as we draw. A smaller picture would be blown up and look blurry, so
/// the caller should prefer another source when it has one. A theme name that can be found wins
/// over pictures, as the protocol suggests. `None` when it set nothing usable.
pub fn client_icon(surface: &WlSurface) -> Option<(Arc<Icon>, bool)> {
    // Copy what's needed out of the surface state first: reading the pictures takes a while.
    let (name, buffers) = with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ToplevelIconCachedState>();
        let state = cached.current();
        (
            state.icon_name().map(str::to_owned),
            state
                .buffers()
                .iter()
                .map(|(buffer, _)| buffer.clone())
                .collect::<Vec<_>>(),
        )
    });
    if let Some(icon) = name.as_deref().and_then(icon_for) {
        return Some((icon, true));
    }
    // The smallest picture that is at least as big as we draw, else the biggest there is.
    let mut pictures: Vec<(Vec<u8>, u32, u32)> = buffers.iter().filter_map(read_picture).collect();
    pictures.sort_by_key(|(_, width, _)| *width);
    let (rgba, width, height) = match pictures
        .iter()
        .position(|(_, width, _)| *width >= ICON_PX as u32)
    {
        Some(i) => pictures.swap_remove(i),
        None => pictures.pop()?,
    };
    let mut hasher = DefaultHasher::new();
    rgba.hash(&mut hasher);
    let icon = Icon {
        source: PathBuf::from(format!("toplevel-icon:{:016x}", hasher.finish())),
        rgba: scale(&rgba, width, height, ICON_PX as u32),
    };
    Some((Arc::new(icon), width.max(height) >= ICON_PX as u32))
}

/// An shm buffer as straight-alpha 8-bit RGBA with its size. `None` for other kinds of buffer,
/// formats we don't read, or sizes that don't add up.
fn read_picture(buffer: &WlBuffer) -> Option<(Vec<u8>, u32, u32)> {
    with_buffer_contents(buffer, |ptr, len, data| {
        let (width, height) = (data.width, data.height);
        if width <= 0 || height <= 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        // Where red, green, blue and alpha are in a pixel, and whether there is any alpha. The
        // formats are little-endian words, so `Argb8888` is B, G, R, A in memory.
        let (r, g, b, a, has_alpha) = match data.format {
            wl_shm::Format::Argb8888 => (2, 1, 0, 3, true),
            wl_shm::Format::Xrgb8888 => (2, 1, 0, 3, false),
            wl_shm::Format::Abgr8888 => (0, 1, 2, 3, true),
            wl_shm::Format::Xbgr8888 => (0, 1, 2, 3, false),
            _ => return None,
        };
        let (width, height) = (width as usize, height as usize);
        let (offset, stride) = (data.offset.max(0) as usize, data.stride.max(0) as usize);
        if stride < width * 4 || offset + stride * (height - 1) + width * 4 > len {
            return None;
        }
        let pool = unsafe { std::slice::from_raw_parts(ptr, len) };
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let row = &pool[offset + y * stride..offset + y * stride + width * 4];
            for px in row.chunks_exact(4) {
                let alpha = if has_alpha { px[a] } else { 255 };
                // wl_shm colour is multiplied by alpha, the icons are not.
                let straight = |c: u8| match alpha {
                    0 => 0,
                    255 => c,
                    alpha => ((u32::from(c) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                        .min(255) as u8,
                };
                rgba.extend_from_slice(&[straight(px[r]), straight(px[g]), straight(px[b]), alpha]);
            }
        }
        Some((rgba, width as u32, height as u32))
    })
    .ok()
    .flatten()
}
