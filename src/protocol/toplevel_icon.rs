//! xdg-toplevel-icon-v1: a client says which icon its window has, by theme name or as pictures.
//!
//! Smithay keeps what the client set as double-buffered surface state, so what we read here is
//! from the last commit. The overview uses it for the app icon on a window's thumbnail, ahead of
//! the icon of the desktop file the app id points at (see `crate::feature::overview_mode`): the
//! app knows best, and some apps have no desktop file at all.
//!
//! Scripts and panels get it too: the `icon` member and event of `jff msg` (see [`ipc_icon`]) give
//! the theme name and an image file, so a bar can show the icon of an app that has no desktop file.
//! The protocol has no part in `wlr-foreign-toplevel`, which has no way to carry an icon.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use smithay::wayland::xdg_toplevel_icon::ToplevelIconCachedState;

use crate::feature::app_icons::{icon_for, scale, Icon, ICON_PX};
use crate::feature::screenshot::save::encode_png;
use crate::ipc::events::IconRef;

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

/// How many answers of [`ipc_icon`] are kept. A window that changes its icon all the time makes
/// a new one each time, so the cache starts over when it has this many.
const IPC_CACHE_MAX: usize = 512;

fn ipc_cache() -> &'static Mutex<HashMap<u64, Option<IconRef>>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Option<IconRef>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What `jff msg` says about the icon `surface` set: its theme name and an image file. `None`
/// when it set no icon, or nothing that can be told. Cheap to ask again and again: the answer is
/// kept until the icon changes, and the PNG of a picture is written once.
pub fn ipc_icon(surface: &WlSurface) -> Option<IconRef> {
    let key = fingerprint(surface);
    if key == 0 {
        return None;
    }
    if let Some(found) = ipc_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
    {
        return found.clone();
    }
    let icon = describe(surface);
    let mut cache = ipc_cache().lock().unwrap_or_else(|p| p.into_inner());
    if cache.len() >= IPC_CACHE_MAX {
        cache.clear();
    }
    cache.insert(key, icon.clone());
    icon
}

/// Work out [`ipc_icon`]'s answer. A theme name that can be found wins over pictures, as it does
/// for [`client_icon`].
fn describe(surface: &WlSurface) -> Option<IconRef> {
    let (name, buffers) = with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ToplevelIconCachedState>();
        let state = cached.current();
        (
            state
                .icon_name()
                .map(str::to_owned)
                .filter(|name| !name.is_empty()),
            state
                .buffers()
                .iter()
                .map(|(buffer, _)| buffer.clone())
                .collect::<Vec<_>>(),
        )
    });
    if let Some(icon) = name.as_deref().and_then(icon_for) {
        let path = icon.source.to_string_lossy().into_owned();
        return Some(IconRef {
            name,
            path: Some(path),
        });
    }
    // The biggest picture there is: the reader of the file can scale it down, not up.
    let path = buffers
        .iter()
        .filter_map(read_picture)
        .max_by_key(|(_, width, _)| *width)
        .and_then(|(rgba, width, height)| {
            let dir = icon_dir()?;
            write_png(&dir, &rgba, width, height)
        })
        .map(|path| path.to_string_lossy().into_owned());
    if name.is_none() && path.is_none() {
        return None;
    }
    Some(IconRef { name, path })
}

/// Where the PNGs of icons go: `$XDG_RUNTIME_DIR/jff-wm/icons`. `None` without a runtime dir,
/// the files are not worth putting where others can read them.
fn icon_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("jff-wm").join("icons"))
}

/// Write straight-alpha RGBA as a PNG in `dir` (made when missing, for the user alone), named
/// after what it shows, so the same picture is one file however often it is set. Returns the
/// file, `None` when it cannot be written.
pub fn write_png(dir: &Path, rgba: &[u8], width: u32, height: u32) -> Option<PathBuf> {
    // FNV-1a: the name has to be the same in every run of jff.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in width
        .to_le_bytes()
        .iter()
        .chain(height.to_le_bytes().iter())
        .chain(rgba)
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let path = dir.join(format!("{hash:016x}.png"));
    if path.is_file() {
        return Some(path);
    }
    let bytes = encode_png(rgba, width, height).ok()?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .ok()?;
    // Never a half written file under the name.
    let partial = dir.join(format!("{hash:016x}.png.part"));
    std::fs::write(&partial, bytes).ok()?;
    std::fs::rename(&partial, &path).ok()?;
    Some(path)
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
