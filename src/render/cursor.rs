//! The mouse cursor: icons from the XCursor theme, cursor surfaces sent by clients, and a small
//! built-in arrow for when the theme has nothing.
//!
//! The theme comes from `XCURSOR_THEME` / `XCURSOR_SIZE` (defaults `default`, 24). Both are
//! exported again so the apps jff starts pick the same cursor.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::rc::Rc;

use anyhow::Context;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Transform};
use smithay::wayland::compositor::with_states;
use xcursor::parser::parse_xcursor;
use xcursor::CursorTheme;

/// One picture of a cursor, ready to be drawn.
pub struct Frame {
    pub buffer: MemoryRenderBuffer,
    /// The pixel inside the picture that sits exactly on the pointer position.
    pub hotspot: Point<i32, Logical>,
    delay_ms: u32,
}

/// A themed cursor: one frame, or several for animated ones (like the busy spinner).
pub struct Cursor {
    frames: Vec<Frame>,
    duration_ms: u32,
}

impl Cursor {
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
pub enum RenderCursor {
    Hidden,
    /// A client drew its own cursor.
    Surface {
        hotspot: Point<i32, Logical>,
        surface: WlSurface,
    },
    Named(Rc<Cursor>),
}

pub struct CursorManager {
    theme: CursorTheme,
    size: i32,
    status: CursorImageStatus,
    cache: HashMap<CursorIcon, Rc<Cursor>>,
    fallback: Rc<Cursor>,
}

impl CursorManager {
    pub fn new() -> Self {
        let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_string());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(24);
        std::env::set_var("XCURSOR_THEME", &theme_name);
        std::env::set_var("XCURSOR_SIZE", size.to_string());

        Self {
            theme: CursorTheme::load(&theme_name),
            size,
            status: CursorImageStatus::default_named(),
            cache: HashMap::new(),
            fallback: Rc::new(builtin_arrow()),
        }
    }

    /// The focused client asked for a different cursor (`wl_pointer.set_cursor`).
    pub fn set_status(&mut self, status: CursorImageStatus) {
        self.status = status;
    }

    pub fn render_cursor(&mut self) -> RenderCursor {
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
