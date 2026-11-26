//! The mouse cursor: icons from the XCursor theme, cursor surfaces sent by clients, and a small
//! built-in arrow for when the theme has nothing.
//!
//! The theme and size come from the `cursor` config block, else `XCURSOR_THEME` / `XCURSOR_SIZE`,
//! else `default` and 24. Both are exported again so the apps jff starts pick the same cursor.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::utils::{with_renderer_surface_state, CommitCounter};
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Transform};
use smithay::wayland::compositor::{with_states, with_surface_tree_downward, TraversalAction};
use xcursor::parser::parse_xcursor;
use xcursor::CursorTheme;

use crate::config::CursorConfig;
use crate::primitives::cursor_capture::{pixel_at, pixels};
use crate::primitives::cursor_hide::CursorHide;

/// One picture of a cursor, ready to be drawn.
pub struct Frame {
    pub buffer: MemoryRenderBuffer,
    /// The same picture as bytes (`Argb8888`, premultiplied), for an HDR monitor, which codes it
    /// for its own signal before it goes to the cursor plane.
    pub pixels: Vec<u8>,
    /// Tells this picture from every other one made in this run.
    pub serial: u64,
    /// Size of the picture in its own pixels.
    pub size: (i32, i32),
    /// The pixel inside the picture that sits exactly on the pointer position.
    pub hotspot: Point<i32, Logical>,
    delay_ms: u32,
}

/// A number no other picture has, see [`Frame::serial`].
fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A themed cursor: one frame, or several for animated ones (like the busy spinner).
pub struct Cursor {
    frames: Vec<Frame>,
    duration_ms: u32,
}

impl Cursor {
    /// Several frames: the picture changes with time, without any event.
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1 && self.duration_ms > 0
    }

    /// The frame to show `millis` after start. Animations loop.
    pub fn frame(&self, millis: u32) -> &Frame {
        if self.duration_ms == 0 {
            return &self.frames[0];
        }
        let mut left = millis % self.duration_ms;
        for frame in &self.frames {
            if left < frame.delay_ms {
                return frame;
            }
            left -= frame.delay_ms;
        }
        &self.frames[0]
    }
}

/// What the renderer should draw for the cursor right now.
#[derive(Clone)]
pub enum RenderCursor {
    Hidden,
    /// A client drew its own cursor.
    Surface {
        hotspot: Point<i32, Logical>,
        surface: WlSurface,
    },
    Named(Rc<Cursor>),
}

/// What a picture of the cursor is like on a monitor of one scale: how big it is, where its
/// hotspot sits, and a stamp that changes whenever its pixels do. Two looks that are equal are
/// the same picture (`protocol/image_copy.rs` sends a cursor frame only when they differ).
#[derive(Clone, Debug, PartialEq)]
pub struct CursorLook {
    /// Pixel size of the picture.
    pub size: (i32, i32),
    /// The pixel of the picture that sits on the pointer.
    pub hotspot: (i32, i32),
    scale: f64,
    stamp: Stamp,
}

/// What makes one cursor picture differ from another without its size or hotspot changing.
#[derive(Clone, Debug, PartialEq)]
enum Stamp {
    /// A themed picture: which frame of which icon (the frame's address, the icon is cached).
    Themed(usize),
    /// A client's surface: its commits, and those of its subsurfaces.
    Surface(Vec<CommitCounter>),
}

/// The commit counters of `root` and its subsurfaces.
///
/// `with_surface_tree_downward` holds the surface's data lock while it calls back, and
/// `with_renderer_surface_state` takes that same lock: reading the counter inside the callback
/// would deadlock, so the surfaces are collected first (same as `window_signature`).
pub fn surface_tree_signature(root: &WlSurface) -> Vec<CommitCounter> {
    let mut surfaces = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, _, _| surfaces.push(surface.clone()),
        |_, _, _| true,
    );
    surfaces
        .iter()
        .map(|surface| {
            with_renderer_surface_state(surface, |state| state.current_commit()).unwrap_or_default()
        })
        .collect()
}

impl RenderCursor {
    /// The look of this cursor at `scale`, `millis` after start (themed cursors can animate).
    /// `None` when there is no picture: hidden, or a client's surface that has nothing in it.
    pub fn look(&self, millis: u32, scale: f64) -> Option<CursorLook> {
        match self {
            RenderCursor::Hidden => None,
            RenderCursor::Named(cursor) => {
                let frame = cursor.frame(millis);
                Some(CursorLook {
                    size: (pixels(frame.size.0, scale), pixels(frame.size.1, scale)),
                    hotspot: (
                        pixel_at(frame.hotspot.x, scale),
                        pixel_at(frame.hotspot.y, scale),
                    ),
                    scale,
                    stamp: Stamp::Themed(frame as *const Frame as usize),
                })
            }
            RenderCursor::Surface { hotspot, surface } => {
                let bbox = bbox_from_surface_tree(surface, (0, 0));
                // A cursor surface with nothing attached is how a client hides its cursor.
                if bbox.size.w <= 0 || bbox.size.h <= 0 {
                    return None;
                }
                // The picture starts at the top-left of the surface tree, which can be above
                // and left of the surface itself when a subsurface sticks out.
                Some(CursorLook {
                    size: (pixels(bbox.size.w, scale), pixels(bbox.size.h, scale)),
                    hotspot: (
                        pixel_at(hotspot.x - bbox.loc.x, scale),
                        pixel_at(hotspot.y - bbox.loc.y, scale),
                    ),
                    scale,
                    stamp: Stamp::Surface(surface_tree_signature(surface)),
                })
            }
        }
    }
}

pub struct CursorManager {
    theme: CursorTheme,
    theme_name: String,
    size: i32,
    /// The theme and size jff was started with, used where the config says nothing.
    env_theme: Option<String>,
    env_size: Option<i32>,
    hide: CursorHide,
    /// The timer that hides the cursor after the pointer rests is running.
    pub idle_timer_armed: bool,
    /// A fullscreen window has the focus and nothing of jff's own needs the cursor, so
    /// `hide-after-idle` may hide it (derived each loop pass, see `focus/mod.rs`).
    fullscreen_gate: bool,
    status: CursorImageStatus,
    /// A finger is the input device in use: there is no cursor to look at (see `input/touch.rs`).
    hidden_for_touch: bool,
    cache: HashMap<CursorIcon, Rc<Cursor>>,
    fallback: Rc<Cursor>,
}

impl CursorManager {
    pub fn new(config: &CursorConfig) -> Self {
        let env_theme = std::env::var("XCURSOR_THEME")
            .ok()
            .filter(|t| !t.is_empty());
        let env_size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|s| *s > 0);
        let (theme_name, size) = config.resolved(env_theme.as_deref(), env_size);
        export_theme(&theme_name, size);

        let mut hide = CursorHide::new(Instant::now());
        hide.configure(config.hide_when_typing, hide_after(config));
        hide.set_fullscreen_after(hide_after_idle(config));
        Self {
            theme: CursorTheme::load(&theme_name),
            theme_name,
            size,
            env_theme,
            env_size,
            hide,
            idle_timer_armed: false,
            fullscreen_gate: false,
            status: CursorImageStatus::default_named(),
            hidden_for_touch: false,
            cache: HashMap::new(),
            fallback: Rc::new(builtin_arrow()),
        }
    }

    /// Take the `cursor` settings after a config reload. A changed theme or size is loaded again.
    pub fn configure(&mut self, config: &CursorConfig) {
        let (theme_name, size) = config.resolved(self.env_theme.as_deref(), self.env_size);
        if (theme_name.as_str(), size) != (self.theme_name.as_str(), self.size) {
            export_theme(&theme_name, size);
            self.theme = CursorTheme::load(&theme_name);
            self.theme_name = theme_name;
            self.size = size;
            self.cache.clear();
        }
        self.hide
            .configure(config.hide_when_typing, hide_after(config));
        self.hide.set_fullscreen_after(hide_after_idle(config));
    }

    /// A key went down: with `hide-when-typing` the cursor hides.
    pub fn key_pressed(&mut self) {
        self.hide.key_pressed();
    }

    /// The pointer moved, clicked or scrolled: a hidden cursor comes back.
    pub fn pointer_activity(&mut self) {
        self.hide.pointer_activity(Instant::now());
    }

    /// `hide-after-idle` is set: the focus has to be checked for a fullscreen window.
    pub fn fullscreen_hide_enabled(&self) -> bool {
        self.hide.fullscreen_after().is_some()
    }

    /// Say whether the cursor may hide for resting now (a fullscreen window has the focus and
    /// jff itself has no use for the cursor). `true` when that changed.
    pub fn set_fullscreen_gate(&mut self, gate: bool) -> bool {
        std::mem::replace(&mut self.fullscreen_gate, gate) != gate
    }

    /// The cursor is not drawn because of typing or resting (a client's own `Hidden` or the
    /// touch rule don't count).
    pub fn is_hidden(&self) -> bool {
        self.hide.hidden_in(self.fullscreen_gate)
    }

    /// How long the pointer may rest before the cursor hides, `None` when it never does.
    pub fn hide_after(&self) -> Option<Duration> {
        self.hide.after()
    }

    /// Time left before the cursor hides for resting, hiding it when it is up. `None` when
    /// there is nothing more to wait for.
    pub fn idle_remaining(&mut self) -> Option<Duration> {
        self.hide.remaining(Instant::now())
    }

    /// The focused client asked for a different cursor (`wl_pointer.set_cursor`).
    pub fn set_status(&mut self, status: CursorImageStatus) {
        self.status = status;
    }

    /// The surface a client drew its cursor with, when that is the cursor now. Counts while the
    /// cursor is hidden for touch too: the client still waits for its frame callbacks.
    pub fn client_surface(&self) -> Option<&WlSurface> {
        match &self.status {
            CursorImageStatus::Surface(surface) if surface.alive() => Some(surface),
            _ => None,
        }
    }

    /// Hide the cursor while a touchscreen is in use, show it again when a mouse, touchpad or
    /// pen moves. The client's own choice of cursor (`set_status`) is left as it is.
    pub fn set_touch_hidden(&mut self, hidden: bool) {
        self.hidden_for_touch = hidden;
    }

    /// The cursor being drawn is a themed one that animates (the busy spinner).
    pub fn is_animated(&mut self) -> bool {
        matches!(self.render_cursor(), RenderCursor::Named(cursor) if cursor.is_animated())
    }

    pub fn render_cursor(&mut self) -> RenderCursor {
        if self.hidden_for_touch || self.is_hidden() {
            return RenderCursor::Hidden;
        }
        // A client that set a cursor surface and then went away leaves a dead surface behind.
        if let CursorImageStatus::Surface(surface) = &self.status {
            if !surface.alive() {
                self.status = CursorImageStatus::default_named();
            }
        }

        match self.status.clone() {
            CursorImageStatus::Hidden => RenderCursor::Hidden,
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|data| data.lock().unwrap().hotspot)
                        .unwrap_or_else(|| (0, 0).into())
                });
                RenderCursor::Surface { hotspot, surface }
            }
            CursorImageStatus::Named(icon) => RenderCursor::Named(self.named(icon)),
        }
    }

    /// The themed cursor for `icon`, whatever the status is now. For a size to offer while there
    /// is no picture to show (`protocol/image_copy.rs`).
    pub fn themed(&mut self, icon: CursorIcon) -> Rc<Cursor> {
        self.named(icon)
    }

    fn named(&mut self, icon: CursorIcon) -> Rc<Cursor> {
        if let Some(cursor) = self.cache.get(&icon) {
            return cursor.clone();
        }

        // Some themes only ship the older spellings of an icon name.
        let loaded = load_cursor(&self.theme, icon.name(), self.size).or_else(|_| {
            icon.alt_names()
                .iter()
                .find_map(|name| load_cursor(&self.theme, name, self.size).ok())
                .context("no icon under any alternative name")
        });
        let cursor = match loaded {
            Ok(cursor) => Rc::new(cursor),
            Err(err) => {
                tracing::warn!("cursor {:?} not found in the theme: {err:#}", icon.name());
                self.fallback.clone()
            }
        };
        self.cache.insert(icon, cursor.clone());
        cursor
    }
}

/// Tell the apps jff starts which cursor to use.
fn export_theme(theme: &str, size: i32) {
    std::env::set_var("XCURSOR_THEME", theme);
    std::env::set_var("XCURSOR_SIZE", size.to_string());
}

/// `hide-after-ms` as a duration, `None` for 0.
fn hide_after(config: &CursorConfig) -> Option<Duration> {
    (config.hide_after_ms > 0).then(|| Duration::from_millis(u64::from(config.hide_after_ms)))
}

/// `hide-after-idle` as a duration, `None` for 0.
fn hide_after_idle(config: &CursorConfig) -> Option<Duration> {
    (config.hide_after_idle > 0).then(|| Duration::from_secs(u64::from(config.hide_after_idle)))
}

/// Read `name` from the theme, using the image size closest to `size`.
fn load_cursor(theme: &CursorTheme, name: &str, size: i32) -> anyhow::Result<Cursor> {
    let path = theme.load_icon(name).context("icon not in theme")?;
    let mut bytes = Vec::new();
    File::open(path)
        .context("error opening the cursor file")?
        .read_to_end(&mut bytes)
        .context("error reading the cursor file")?;
    let mut images = parse_xcursor(&bytes).context("error parsing the cursor file")?;

    let (width, height) = images
        .iter()
        .min_by_key(|image| (size - image.size as i32).abs())
        .map(|image| (image.width, image.height))
        .context("cursor file has no images")?;
    // One cursor file holds several sizes, keep the frames of the chosen one.
    images.retain(|image| image.width == width && image.height == height);

    let frames: Vec<Frame> = images
        .iter()
        .map(|image| Frame {
            pixels: image.pixels_rgba.clone(),
            serial: next_serial(),
            size: (image.width as i32, image.height as i32),
            // The pixels are stored as ARGB words, which is what Argb8888 expects in memory.
            buffer: MemoryRenderBuffer::from_slice(
                &image.pixels_rgba,
                Fourcc::Argb8888,
                (image.width as i32, image.height as i32),
                1,
                Transform::Normal,
                None,
            ),
            hotspot: (image.xhot as i32, image.yhot as i32).into(),
            delay_ms: image.delay,
        })
        .collect();
    let duration_ms = frames.iter().map(|f| f.delay_ms).sum();
    Ok(Cursor {
        frames,
        duration_ms,
    })
}

/// A plain black-and-white arrow, drawn from a bitmap, used when no theme can be found.
fn builtin_arrow() -> Cursor {
    // X = outline, . = fill, space = transparent.
    const ART: [&str; 17] = [
        "X           ",
        "XX          ",
        "X.X         ",
        "X..X        ",
        "X...X       ",
        "X....X      ",
        "X.....X     ",
        "X......X    ",
        "X.......X   ",
        "X........X  ",
        "X.....XXXXX ",
        "X..X..X     ",
        "X.X X..X    ",
        "XX  X..X    ",
        "X    X..X   ",
        "     X..X   ",
        "      XX    ",
    ];
    const ZOOM: usize = 2;
    let (w, h) = (ART[0].len() * ZOOM, ART.len() * ZOOM);

    // Argb8888 in memory is B, G, R, A.
    let mut pixels = Vec::with_capacity(w * h * 4);
    for row in ART {
        let line: Vec<u8> = row
            .bytes()
            .flat_map(|c| {
                let px = match c {
                    b'X' => [0, 0, 0, 255],
                    b'.' => [255, 255, 255, 255],
                    _ => [0, 0, 0, 0],
                };
                std::iter::repeat(px).take(ZOOM).flatten()
            })
            .collect();
        for _ in 0..ZOOM {
            pixels.extend_from_slice(&line);
        }
    }

    let frame = Frame {
        pixels: pixels.clone(),
        serial: next_serial(),
        size: (w as i32, h as i32),
        buffer: MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (w as i32, h as i32),
            1,
            Transform::Normal,
            None,
        ),
        hotspot: (0, 0).into(),
        delay_ms: 0,
    };
    Cursor {
        frames: vec![frame],
        duration_ms: 0,
    }
}
