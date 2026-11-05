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
    let left = rect.0.clamp(0, width);
    let top = rect.1.clamp(0, height);
    let right = (rect.0 + rect.2).clamp(0, width);
    let bottom = (rect.1 + rect.3).clamp(0, height);
    if right <= left || bottom <= top || src.len() < (width as usize) * (height as usize) * 4 {
        return None;
    }
    let stride = width as usize * 4;
    let row_bytes = (right - left) as usize * 4;
    let mut out = Vec::with_capacity(row_bytes * (bottom - top) as usize);
    for y in top..bottom {
        let start = y as usize * stride + left as usize * 4;
        out.extend_from_slice(&src[start..start + row_bytes]);
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

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = (1920, 0, 1280, 720);

    #[test]
    fn a_drag_from_any_corner_gives_the_same_positive_rectangle() {
        let (a, b) = ((2000.0, 100.0), (2300.0, 400.0));
        let expected = (2000, 100, 300, 300);
        assert_eq!(normalize_rect(a, b, MONITOR), expected);
        assert_eq!(normalize_rect(b, a, MONITOR), expected);
        assert_eq!(normalize_rect((a.0, b.1), (b.0, a.1), MONITOR), expected);
        assert_eq!(normalize_rect((b.0, a.1), (a.0, b.1), MONITOR), expected);
    }

    #[test]
    fn a_drag_is_clamped_to_the_monitor() {
        assert_eq!(
            normalize_rect((1000.0, -50.0), (4000.0, 900.0), MONITOR),
            MONITOR
        );
        assert_eq!(
            normalize_rect((3100.0, 650.0), (5000.0, 5000.0), MONITOR),
            (3100, 650, 100, 70)
        );
        let r = normalize_rect((0.0, 0.0), (1950.0, 30.0), MONITOR);
        assert_eq!(r, (1920, 0, 30, 30));
    }

    #[test]
    fn a_rectangle_is_never_empty() {
        for (a, b) in [
            ((2000.0, 100.0), (2000.0, 100.0)),
            ((2000.0, 100.0), (2000.0, 200.0)),
            ((2000.0, 100.0), (2100.0, 100.0)),
        ] {
            let (_, _, w, h) = normalize_rect(a, b, MONITOR);
            assert!(w >= 1 && h >= 1, "{a:?} {b:?} gave {w}x{h}");
        }
        assert_eq!(
            normalize_rect((9999.0, 9999.0), (9999.0, 9999.0), MONITOR),
            (3199, 719, 1, 1)
        );
        assert_eq!(
            normalize_rect((10.4, 10.6), (20.2, 20.2), (0, 0, 100, 100)),
            (10, 10, 11, 11)
        );
    }

    #[test]
    fn handles_are_edges_and_corners() {
        let rect = (100, 100, 200, 100);
        let m = 8.0;
        assert_eq!(
            hit_handle(rect, (100.0, 150.0), m),
            Some(Handle { h: -1, v: 0 })
        );
        assert_eq!(
            hit_handle(rect, (300.0, 150.0), m),
            Some(Handle { h: 1, v: 0 })
        );
        assert_eq!(
            hit_handle(rect, (200.0, 100.0), m),
            Some(Handle { h: 0, v: -1 })
        );
        assert_eq!(
            hit_handle(rect, (200.0, 200.0), m),
            Some(Handle { h: 0, v: 1 })
        );
        assert_eq!(
            hit_handle(rect, (100.0, 100.0), m),
            Some(Handle { h: -1, v: -1 })
        );
        assert_eq!(
            hit_handle(rect, (300.0, 200.0), m),
            Some(Handle { h: 1, v: 1 })
        );
        assert_eq!(
            hit_handle(rect, (305.0, 95.0), m),
            Some(Handle { h: 1, v: -1 })
        );
        assert_eq!(hit_handle(rect, (200.0, 150.0), m), None);
        assert_eq!(hit_handle(rect, (50.0, 150.0), m), None);
        assert_eq!(hit_handle(rect, (200.0, 300.0), m), None);
    }

    #[test]
    fn resizing_moves_only_the_held_edges_and_can_flip() {
        let bounds = (0, 0, 1000, 800);
        let rect = (100, 100, 200, 100);
        let right = Handle { h: 1, v: 0 };
        let corner = Handle { h: -1, v: -1 };
        assert_eq!(
            resize_rect(rect, right, (400.0, 999.0), bounds),
            (100, 100, 300, 100)
        );
        assert_eq!(
            resize_rect(rect, corner, (50.0, 20.0), bounds),
            (50, 20, 250, 180)
        );
        assert_eq!(
            resize_rect(rect, Handle { h: -1, v: 0 }, (500.0, 150.0), bounds),
            (300, 100, 200, 100)
        );
        assert_eq!(
            resize_rect(rect, right, (5000.0, 0.0), bounds),
            (100, 100, 900, 100)
        );
    }

    #[test]
    fn the_selector_draws_resizes_and_forgets() {
        let mut s = Selector::new(MONITOR);
        assert_eq!(s.rect(), None);

        s.press((2000.0, 100.0));
        assert_eq!(s.rect(), None, "a press alone selects nothing");
        s.motion((2001.0, 101.0));
        assert_eq!(s.rect(), None, "below the threshold it is still a click");
        s.motion((2300.0, 400.0));
        assert_eq!(s.rect(), Some((2000, 100, 300, 300)));
        s.release((1000.0, 400.0));
        assert_eq!(s.rect(), Some((1920, 100, 80, 300)));

        s.motion((2500.0, 500.0));
        assert_eq!(s.rect(), Some((1920, 100, 80, 300)));

        s.press((2000.0, 200.0));
        s.motion((2400.0, 900.0));
        s.release((2400.0, 900.0));
        assert_eq!(s.rect(), Some((1920, 100, 480, 300)));

        s.press((3000.0, 600.0));
        s.release((3000.0, 600.0));
        assert_eq!(s.rect(), None);

        s.press((2100.0, 100.0));
        s.press((2900.0, 700.0));
        s.motion((2200.0, 200.0));
        assert_eq!(s.rect(), Some((2100, 100, 100, 100)));
    }

    #[test]
    fn a_new_drag_replaces_the_old_selection() {
        let mut s = Selector::new(MONITOR);
        s.press((2000.0, 100.0));
        s.motion((2200.0, 300.0));
        s.release((2200.0, 300.0));
        s.press((3000.0, 600.0));
        s.motion((3100.0, 650.0));
        s.release((3100.0, 650.0));
        assert_eq!(s.rect(), Some((3000, 600, 100, 50)));
    }

    #[test]
    fn logical_rectangles_become_pixels_of_their_monitor() {
        let rect = (2000, 100, 300, 200);
        assert_eq!(physical_rect(rect, (1920, 0), 1.0), (80, 100, 300, 200));
        assert_eq!(physical_rect(rect, (1920, 0), 2.0), (160, 200, 600, 400));
        let a = physical_rect((1921, 1, 3, 3), (1920, 0), 1.5);
        let b = physical_rect((1924, 1, 3, 3), (1920, 0), 1.5);
        assert_eq!(a, (2, 2, 4, 4));
        assert_eq!(b, (6, 2, 5, 4));
        assert_eq!(a.0 + a.2, b.0, "edge at 4 * 1.5 = 6 is shared");
        assert_eq!(
            physical_rect((1920, 0, 1280, 720), (1920, 0), 1.25),
            (0, 0, 1600, 900)
        );
    }

    fn picture(w: usize, h: usize) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 7, 255]))
            .collect()
    }

    #[test]
    fn crop_takes_the_right_pixels_and_clips() {
        let src = picture(6, 4);
        let (out, w, h) = crop_rgba(&src, 6, 4, (2, 1, 3, 2)).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(&out[..4], &[2, 1, 7, 255]);
        assert_eq!(&out[out.len() - 4..], &[4, 2, 7, 255]);
        assert_eq!(out.len(), 3 * 2 * 4);

        let (_, w, h) = crop_rgba(&src, 6, 4, (4, 3, 10, 10)).unwrap();
        assert_eq!((w, h), (2, 1));
        let (_, w, h) = crop_rgba(&src, 6, 4, (-5, -5, 7, 6)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert!(crop_rgba(&src, 6, 4, (6, 0, 2, 2)).is_none());
        assert!(crop_rgba(&src, 6, 4, (1, 1, 0, 2)).is_none());
        assert!(crop_rgba(&src[..10], 6, 4, (0, 0, 2, 2)).is_none());
        // The whole picture comes back unchanged.
        assert_eq!(crop_rgba(&src, 6, 4, (0, 0, 6, 4)).unwrap().0, src);
    }

    #[test]
    fn flipping_rows_twice_gives_the_picture_back() {
        let src = picture(3, 5);
        let mut buf = src.clone();
        flip_rows(&mut buf, 3 * 4);
        assert_eq!(&buf[..4], &[0, 4, 7, 255], "first row is the old last row");
        assert_eq!(
            &buf[2 * 3 * 4..2 * 3 * 4 + 4],
            &[0, 2, 7, 255],
            "middle row stays"
        );
        flip_rows(&mut buf, 3 * 4);
        assert_eq!(buf, src);
        let mut two = picture(1, 2);
        flip_rows(&mut two, 4);
        assert_eq!(two, vec![0, 1, 7, 255, 0, 0, 7, 255]);
        flip_rows(&mut [], 4);
        flip_rows(&mut [1, 2, 3, 4], 0);
    }

    #[test]
    fn unpremultiply_restores_straight_alpha() {
        let mut px = [
            255, 255, 255, 255, 64, 32, 0, 128, 10, 10, 10, 0, 100, 0, 0, 100,
        ];
        unpremultiply(&mut px);
        assert_eq!(
            &px[0..4],
            &[255, 255, 255, 255],
            "opaque pixels are untouched"
        );
        assert_eq!(&px[4..8], &[128, 64, 0, 128]);
        assert_eq!(
            &px[8..12],
            &[0, 0, 0, 0],
            "transparent pixels lose their colour"
        );
        assert_eq!(&px[12..16], &[255, 0, 0, 100]);
        let mut bad = [200, 0, 0, 100];
        unpremultiply(&mut bad);
        assert_eq!(bad[0], 255);
    }

    #[test]
    fn the_folder_follows_config_then_xdg_then_home() {
        let home = Path::new("/home/neo");
        assert_eq!(
            screenshot_dir(Some("/tmp/shots"), Some("/home/neo/Pics"), Some(home)),
            Some(PathBuf::from("/tmp/shots"))
        );
        assert_eq!(
            screenshot_dir(Some("~/Shots"), None, Some(home)),
            Some(PathBuf::from("/home/neo/Shots"))
        );
        assert_eq!(
            screenshot_dir(None, Some("/home/neo/Pics"), Some(home)),
            Some(PathBuf::from("/home/neo/Pics/Screenshots"))
        );
        assert_eq!(
            screenshot_dir(None, Some("$HOME/Imagens"), Some(home)),
            Some(PathBuf::from("/home/neo/Imagens/Screenshots"))
        );
        assert_eq!(
            screenshot_dir(None, None, Some(home)),
            Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
        );
        assert_eq!(
            screenshot_dir(Some(" "), Some(""), Some(home)),
            Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
        );
        assert_eq!(
            screenshot_dir(None, Some("Pictures"), Some(home)),
            Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
        );
        assert_eq!(screenshot_dir(None, None, None), None);
        assert_eq!(
            screenshot_dir(Some("/tmp/x"), None, None),
            Some(PathBuf::from("/tmp/x"))
        );
    }

    #[test]
    fn home_is_only_expanded_at_the_start_of_a_whole_component() {
        let home = Path::new("/home/neo");
        assert_eq!(expand_home("~", Some(home)), PathBuf::from("/home/neo"));
        assert_eq!(
            expand_home("~/a/b", Some(home)),
            PathBuf::from("/home/neo/a/b")
        );
        assert_eq!(expand_home("~root/a", Some(home)), PathBuf::from("~root/a"));
        assert_eq!(expand_home("/x/~/a", Some(home)), PathBuf::from("/x/~/a"));
        assert_eq!(expand_home("~/a", None), PathBuf::from("~/a"));
    }

    #[test]
    fn file_names_carry_the_time_and_avoid_clashes() {
        assert_eq!(
            file_name("2026-11-04 21-30-05", 0),
            "Screenshot from 2026-11-04 21-30-05.png"
        );
        assert_eq!(
            file_name("2026-11-04 21-30-05", 2),
            "Screenshot from 2026-11-04 21-30-05 (2).png"
        );
    }
}
