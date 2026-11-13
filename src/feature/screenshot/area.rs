//! The arithmetic behind screenshots: no Smithay in here. Selection rectangle and how the
//! pointer edits it, logical-to-physical conversion, crop and row flip, and where the file goes.
//! Everything is unit-tested. `screenshot.rs` does the rendering and the input.

use std::path::{Path, PathBuf};

/// `(x, y, width, height)`, like `layout::Rect`.
pub type Rect = (i32, i32, i32, i32);

/// Pixels the pointer has to move with the button down before it counts as a drag, not a click.
pub const DRAG_THRESHOLD: f64 = 3.0;
/// How close to an edge or corner a press grabs it, in logical pixels.
pub const HANDLE_MARGIN: f64 = 8.0;

/// The rectangle with corners `a` and `b`, in any order, kept inside `bounds`. Always at least
/// 1x1 (when `bounds` allows), so dragging from any corner gives the same positive size.
pub fn normalize_rect(a: (f64, f64), b: (f64, f64), bounds: Rect) -> Rect {
    let (bx, by, bw, bh) = bounds;
    let (right_bound, bottom_bound) = (bx + bw.max(1), by + bh.max(1));

    let (left, right) = fix_span(a.0.min(b.0).floor(), a.0.max(b.0).ceil(), bx, right_bound);
    let (top, bottom) = fix_span(a.1.min(b.1).floor(), a.1.max(b.1).ceil(), by, bottom_bound);
    (left, top, right - left, bottom - top)
}

/// Clamp `lo..hi` into `min..max`, and make it at least 1 wide.
fn fix_span(lo: f64, hi: f64, min: i32, max: i32) -> (i32, i32) {
    let clamp = |v: f64| v.max(f64::from(min)).min(f64::from(max)) as i32;
    let (mut lo, mut hi) = (clamp(lo), clamp(hi));
    if hi <= lo {
        if lo < max {
            hi = lo + 1;
        } else {
            lo = max - 1;
            hi = max;
        }
    }
    (lo, hi)
}

/// Which edges a press holds. `h`: -1 left, 1 right, 0 neither. `v`: -1 top, 1 bottom, 0
/// neither. Both set means a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub h: i8,
    pub v: i8,
}

/// The edge or corner of `rect` that `p` is within `margin` of.
pub fn hit_handle(rect: Rect, p: (f64, f64), margin: f64) -> Option<Handle> {
    let (left, top) = (f64::from(rect.0), f64::from(rect.1));
    let (right, bottom) = (f64::from(rect.0 + rect.2), f64::from(rect.1 + rect.3));
    let within_x = p.0 >= left - margin && p.0 <= right + margin;
    let within_y = p.1 >= top - margin && p.1 <= bottom + margin;

    let h = if within_y && (p.0 - left).abs() <= margin {
        -1
    } else if within_y && (p.0 - right).abs() <= margin {
        1
    } else {
        0
    };
    let v = if within_x && (p.1 - top).abs() <= margin {
        -1
    } else if within_x && (p.1 - bottom).abs() <= margin {
        1
    } else {
        0
    };
    (h != 0 || v != 0).then_some(Handle { h, v })
}

/// `rect` with the edges named by `handle` moved to `p`. Dragging an edge past the opposite one
/// flips the rectangle, same as dragging a fresh one.
pub fn resize_rect(rect: Rect, handle: Handle, p: (f64, f64), bounds: Rect) -> Rect {
    let mut left = f64::from(rect.0);
    let mut top = f64::from(rect.1);
    let mut right = f64::from(rect.0 + rect.2);
    let mut bottom = f64::from(rect.1 + rect.3);
    match handle.h {
        -1 => left = p.0,
        1 => right = p.0,
        _ => {}
    }
    match handle.v {
        -1 => top = p.1,
        1 => bottom = p.1,
        _ => {}
    }
    normalize_rect((left, top), (right, bottom), bounds)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Pressed, not moved far enough to count as a selection yet.
    Pending { from: (f64, f64) },
    /// Drawing a new rectangle from this corner.
    New { from: (f64, f64) },
    /// Moving edges of the existing rectangle.
    Resize(Handle),
}

/// The area-screenshot selection: what the pointer has drawn so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    bounds: Rect,
    rect: Option<Rect>,
    drag: Option<Drag>,
}

impl Selector {
    /// A selector that keeps its rectangle inside `bounds`.
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            rect: None,
            drag: None,
        }
    }

    /// The selection so far, in the same coordinates as `bounds`.
    pub fn rect(&self) -> Option<Rect> {
        self.rect
    }

    /// Left button pressed.
    pub fn press(&mut self, p: (f64, f64)) {
        if self.drag.is_some() {
            return;
        }
        self.drag = Some(
            match self.rect.and_then(|r| hit_handle(r, p, HANDLE_MARGIN)) {
                Some(handle) => Drag::Resize(handle),
                None => Drag::Pending { from: p },
            },
        );
    }

    /// Pointer moved.
    pub fn motion(&mut self, p: (f64, f64)) {
        match self.drag {
            Some(Drag::Pending { from }) => {
                let moved = (p.0 - from.0).hypot(p.1 - from.1);
                if moved >= DRAG_THRESHOLD {
                    self.drag = Some(Drag::New { from });
                    self.rect = Some(normalize_rect(from, p, self.bounds));
                }
            }
            Some(Drag::New { from }) => self.rect = Some(normalize_rect(from, p, self.bounds)),
            Some(Drag::Resize(handle)) => {
                if let Some(rect) = self.rect {
                    self.rect = Some(resize_rect(rect, handle, p, self.bounds));
                }
            }
            None => {}
        }
    }

    /// Left button released. A click that never became a drag clears the selection.
    pub fn release(&mut self, p: (f64, f64)) {
        self.motion(p);
        if matches!(self.drag, Some(Drag::Pending { .. })) {
            self.rect = None;
        }
        self.drag = None;
    }
}

// ---- pixels --------------------------------------------------------------------------------

/// `rect` (logical, in the shared desktop space) as pixels of the monitor at `origin` with
/// `scale`. Edges are rounded, not the size, so neighbouring rectangles still touch.
pub fn physical_rect(rect: Rect, origin: (i32, i32), scale: f64) -> Rect {
    let edge = |v: i32, o: i32| (f64::from(v - o) * scale).round() as i32;
    let left = edge(rect.0, origin.0);
    let top = edge(rect.1, origin.1);
    let right = edge(rect.0 + rect.2, origin.0);
    let bottom = edge(rect.1 + rect.3, origin.1);
    (left, top, (right - left).max(0), (bottom - top).max(0))
}

/// The pixels of `rect` out of an RGBA picture `width` by `height`, as `(pixels, width, height)`.
/// Clipped to the picture. `None` when nothing is left.
pub fn crop_rgba(src: &[u8], width: i32, height: i32, rect: Rect) -> Option<(Vec<u8>, i32, i32)> {
    crop_channels(src, width, height, rect, 4)
}

/// [`crop_rgba`] for any kind of value: `per_pixel` of them make a pixel (4 bytes of RGBA, or 4
/// half floats of an HDR picture).
pub fn crop_channels<T: Copy>(
    src: &[T],
    width: i32,
    height: i32,
    rect: Rect,
    per_pixel: usize,
) -> Option<(Vec<T>, i32, i32)> {
    let left = rect.0.clamp(0, width);
    let top = rect.1.clamp(0, height);
    let right = (rect.0 + rect.2).clamp(0, width);
    let bottom = (rect.1 + rect.3).clamp(0, height);
    if right <= left
        || bottom <= top
        || src.len() < (width as usize) * (height as usize) * per_pixel
    {
        return None;
    }
    let stride = width as usize * per_pixel;
    let row_len = (right - left) as usize * per_pixel;
    let mut out = Vec::with_capacity(row_len * (bottom - top) as usize);
    for y in top..bottom {
        let start = y as usize * stride + left as usize * per_pixel;
        out.extend_from_slice(&src[start..start + row_len]);
    }
    Some((out, right - left, bottom - top))
}

/// Flip a picture of rows of `stride` bytes upside down, in place. Reading back from the GPU can
/// give the bottom row first.
pub fn flip_rows(buf: &mut [u8], stride: usize) {
    if stride == 0 {
        return;
    }
    let rows = buf.len() / stride;
    for y in 0..rows / 2 {
        let (top, bottom) = buf.split_at_mut((rows - 1 - y) * stride);
        top[y * stride..(y + 1) * stride].swap_with_slice(&mut bottom[..stride]);
    }
}

/// Convert premultiplied RGBA (GPU output) to the straight alpha PNG wants, in place.
pub fn unpremultiply(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        let alpha = u32::from(px[3]);
        match alpha {
            255 => {}
            0 => px[..3].fill(0),
            _ => {
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
}

// ---- where the file goes ---------------------------------------------------------------------

/// `path` with a leading `~` or `$HOME` taken as the home directory.
pub fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    for prefix in ["~", "$HOME"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                if let Some(home) = home {
                    return home.join(rest.trim_start_matches('/'));
                }
            }
        }
    }
    PathBuf::from(path)
}

/// Where screenshots go: the config's `path`, else `$XDG_PICTURES_DIR/Screenshots`, else
/// `~/Pictures/Screenshots`. `None` when there's no home directory to build one from.
pub fn screenshot_dir(
    config_path: Option<&str>,
    xdg_pictures: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let non_empty = |s: &&str| !s.trim().is_empty();
    if let Some(path) = config_path.filter(non_empty) {
        return Some(expand_home(path, home));
    }
    if let Some(dir) = xdg_pictures.filter(non_empty) {
        let dir = expand_home(dir, home);
        if dir.is_absolute() {
            return Some(dir.join("Screenshots"));
        }
    }
    home.map(|home| home.join("Pictures").join("Screenshots"))
}

/// `Screenshot from <stamp>.png`. With `n > 0`, `Screenshot from <stamp> (n).png`.
pub fn file_name(stamp: &str, n: u32) -> String {
    if n == 0 {
        format!("Screenshot from {stamp}.png")
    } else {
        format!("Screenshot from {stamp} ({n}).png")
    }
}
