//! Toasts drawn by the compositor itself, used for the system bell.
//!
//! A toast is painted on the CPU once per change and drawn as a memory buffer. The font is
//! found with fc-match on a helper thread the first time a bell rings.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use fontdue::{Font, FontSettings};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::layer_map_for_output;
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Physical, Point, Transform};
use smithay::wayland::compositor::{get_parent, with_states};
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::feature::outputs::output_rect;
use crate::render::elements::JffElement;
use crate::state::JffState;

pub const HEIGHT: i32 = 56;
/// Extra room on every side of the picture, for the shadow.
pub const SHADOW: i32 = 28;
/// Per monitor, the oldest goes first.
pub const MAX_TOASTS: usize = 3;
pub const FADE_IN: Duration = Duration::from_millis(140);
pub const FADE_OUT: Duration = Duration::from_millis(220);
/// Time left after the pointer leaves a toast.
pub const GRACE: Duration = Duration::from_millis(600);
pub const ERROR_TIMEOUT: Duration = Duration::from_secs(8);

const ICON: i32 = 32;
const PAD_LEFT: i32 = 12;
const GAP: i32 = 12;
const PAD_RIGHT: i32 = 24;
const MAX_TEXT: f32 = 380.0;
const MAX_TEXT_ERROR: f32 = 620.0;
const TITLE_PX: f32 = 15.0;
const SUBTITLE_PX: f32 = 12.0;
const STACK_GAP: i32 = 8;
const TOP_MARGIN: i32 = 16;
const SLIDE: f32 = 10.0;

const FILL: [f32; 3] = [0.110, 0.110, 0.137];
const TITLE_COLOR: [f32; 3] = [0.95, 0.95, 0.97];
const SUBTITLE_COLOR: [f32; 3] = [0.62, 0.62, 0.68];
const BELL_DISC: [f32; 3] = [0.29, 0.24, 0.157];
const BELL_GLYPH: [f32; 3] = [0.96, 0.72, 0.18];
const INFO_DISC: [f32; 3] = [0.15, 0.21, 0.31];
const INFO_GLYPH: [f32; 3] = [0.35, 0.66, 0.96];
const ERROR_DISC: [f32; 3] = [0.33, 0.15, 0.16];
const ERROR_GLYPH: [f32; 3] = [0.93, 0.36, 0.36];

/// Used when fc-match finds nothing.
const FALLBACK_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Bell,
    Info,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Same key, icon, title and monitor share one toast. `None` is no window.
    pub key: Option<u64>,
    pub icon: Icon,
    pub title: String,
    pub subtitle: String,
    pub output: String,
}

struct Painted {
    buffer: MemoryRenderBuffer,
    width: i32,
}

struct Toast {
    notice: Notice,
    count: u32,
    born: Instant,
    /// Fade-out starts here.
    expires: Instant,
    painted: Option<Painted>,
}

/// Pointer on the toast: keep it at least `GRACE` longer. Brings back a fading one.
fn held(expires: Instant, now: Instant) -> Instant {
    expires.max(now + GRACE)
}

impl Toast {
    fn headline(&self) -> String {
        if self.count > 1 {
            format!("{} ×{}", self.notice.title, self.count)
        } else {
            self.notice.title.clone()
        }
    }
}

pub struct Osd {
    toasts: Vec<Toast>,
    family: String,
    /// `Some(None)` means loading failed.
    fonts: Arc<OnceLock<Option<Fonts>>>,
    started: bool,
}

impl Osd {
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            toasts: Vec::new(),
            family: family.into(),
            fonts: Arc::new(OnceLock::new()),
            started: false,
        }
    }

    /// For tests: no font, no loader thread.
    pub fn without_fonts() -> Self {
        let osd = Self::new("sans-serif");
        let _ = osd.fonts.set(None);
        Self {
            started: true,
            ..osd
        }
    }

    fn start_loading(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let slot = Arc::clone(&self.fonts);
        let family = self.family.clone();
        let spawned = std::thread::Builder::new()
            .name("jff-osd-fonts".into())
            .spawn(move || {
                let _ = slot.set(Fonts::load(&family));
            });
        if let Err(err) = spawned {
            tracing::warn!("osd: cannot start the font loader: {err}");
            let _ = self.fonts.set(None);
        }
    }

    pub fn show(&mut self, notice: Notice, timeout: Duration, now: Instant) {
        self.start_loading();
        let same = |t: &Toast| {
            t.expires > now
                && t.notice.key == notice.key
                && t.notice.icon == notice.icon
                && t.notice.output == notice.output
                && t.notice.title == notice.title
        };
        if let Some(toast) = self.toasts.iter_mut().find(|t| same(t)) {
            toast.count = toast.count.saturating_add(1);
            // the title may have changed
            toast.notice = notice;
            toast.expires = now + timeout;
            toast.painted = None;
            return;
        }
        let output = notice.output.clone();
        self.toasts.push(Toast {
            notice,
            count: 1,
            born: now,
            expires: now + timeout,
            painted: None,
        });
        while self
            .toasts
            .iter()
            .filter(|t| t.notice.output == output)
            .count()
            > MAX_TOASTS
        {
            if let Some(oldest) = self.toasts.iter().position(|t| t.notice.output == output) {
                self.toasts.remove(oldest);
            }
        }
    }

    /// Pointer is on toast `index`. `elements` does the same every frame.
    pub fn hold(&mut self, index: usize, now: Instant) {
        if let Some(toast) = self.toasts.get_mut(index) {
            toast.expires = held(toast.expires, now);
        }
    }

    pub fn fades_at(&self, index: usize) -> Option<Instant> {
        self.toasts.get(index).map(|t| t.expires)
    }

    pub fn dismiss(&mut self, icon: Icon) {
        self.toasts.retain(|t| t.notice.icon != icon);
    }

    pub fn expire(&mut self, now: Instant) {
        self.toasts.retain(|t| now < t.expires + FADE_OUT);
    }

    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn active(&self) -> impl Iterator<Item = (&Notice, u32)> {
        self.toasts.iter().map(|t| (&t.notice, t.count))
    }

    pub fn headlines(&self) -> Vec<String> {
        self.toasts.iter().map(Toast::headline).collect()
    }

    /// Draws the toasts of `output` top-centered, below any panel. `pointer` is in the
    /// shared space.
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        pointer: Point<f64, Logical>,
        out: &mut Vec<JffElement>,
    ) {
        let now = Instant::now();
        self.expire(now);
        if self.toasts.is_empty() {
            return;
        }
        // font still loading
        let fonts: Option<&Fonts> = match self.fonts.get() {
            None => return,
            Some(fonts) => fonts.as_ref(),
        };
        let rect = output_rect(output);
        let zone = layer_map_for_output(output).non_exclusive_zone();
        // no panels arranged yet
        let (zone_x, zone_y, zone_w) = if zone.size.w > 0 && zone.size.h > 0 {
            (zone.loc.x, zone.loc.y, zone.size.w)
        } else {
            (0, 0, rect.2)
        };
        let name = output.name();
        let mut y = zone_y + TOP_MARGIN;
        for toast in self.toasts.iter_mut().filter(|t| t.notice.output == name) {
            let headline = toast.headline();
            let icon = toast.notice.icon;
            let subtitle = &toast.notice.subtitle;
            let painted = toast.painted.get_or_insert_with(|| {
                let picture = paint(fonts, &headline, subtitle, icon);
                Painted {
                    width: picture.width,
                    buffer: MemoryRenderBuffer::from_slice(
                        &picture.rgba,
                        Fourcc::Abgr8888,
                        (picture.width, picture.height),
                        1,
                        Transform::Normal,
                        None,
                    ),
                }
            });
            let width = painted.width - 2 * SHADOW;
            let left = zone_x + (zone_w - width) / 2;
            let (rel_x, rel_y) = (pointer.x - f64::from(rect.0), pointer.y - f64::from(rect.1));
            if rel_x >= f64::from(left)
                && rel_x < f64::from(left + width)
                && rel_y >= f64::from(y)
                && rel_y < f64::from(y + HEIGHT)
            {
                toast.expires = held(toast.expires, now);
            }
            let alpha = alpha_at(toast.born, toast.expires, now);
            let slide = (1.0 - progress(now.saturating_duration_since(toast.born), FADE_IN))
                .powi(3)
                * -SLIDE;
            let x = left - SHADOW;
            let loc: Point<f64, Physical> =
                (f64::from(x), f64::from(y - SHADOW) + f64::from(slide)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                &painted.buffer,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(JffElement::Memory(element)),
                Err(err) => tracing::warn!("error drawing a toast: {err:?}"),
            }
            y += HEIGHT + STACK_GAP;
        }
    }
}

fn progress(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub fn alpha_at(born: Instant, expires: Instant, now: Instant) -> f32 {
    let fade_in = ease_out_cubic(progress(now.saturating_duration_since(born), FADE_IN));
    let fade_out = 1.0 - progress(now.saturating_duration_since(expires), FADE_OUT);
    fade_in * fade_out
}

pub struct Fonts {
    regular: Font,
    medium: Font,
}

impl Fonts {
    fn load(family: &str) -> Option<Fonts> {
        let regular = locate(family)
            .and_then(|path| open(&path))
            .or_else(|| FALLBACK_FONTS.iter().find_map(|p| open(Path::new(p))));
        let Some(regular) = regular else {
            tracing::warn!("osd: no usable font found (is fontconfig installed?), no toasts");
            return None;
        };
        let medium = locate(&format!("{family}:weight=medium")).and_then(|path| open(&path));
        Some(Fonts {
            medium: medium.unwrap_or_else(|| regular.clone()),
            regular,
        })
    }
}

fn locate(pattern: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn open(path: &Path) -> Option<Font> {
    let bytes = std::fs::read(path).ok()?;
    Font::from_bytes(bytes, FontSettings::default()).ok()
}

fn drawable(font: &Font, c: char) -> char {
    if font.lookup_glyph_index(c) == 0 {
        '?'
    } else {
        c
    }
}

fn text_width(font: &Font, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|c| font.metrics(drawable(font, c), px).advance_width)
        .sum()
}

pub fn elide(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    // no point measuring more than this
    let text: String = text.chars().take(240).collect();
    if width(&text) <= max {
        return text;
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.pop().is_some() {
        let kept: String = chars.iter().collect();
        let candidate = format!("{}…", kept.trim_end());
        if width(&candidate) <= max {
            return candidate;
        }
    }
    "…".to_owned()
}

pub struct Picture {
    pub rgba: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

type Pixel = [f32; 4];

fn over(dst: &mut Pixel, rgb: [f32; 3], a: f32) {
    let keep = 1.0 - a;
    dst[0] = rgb[0] * a + dst[0] * keep;
    dst[1] = rgb[1] * a + dst[1] * keep;
    dst[2] = rgb[2] * a + dst[2] * keep;
    dst[3] = a + dst[3] * keep;
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn sd_round_box(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn dist(x: f32, y: f32, cx: f32, cy: f32) -> f32 {
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()
}

fn dist_to_hsegment(x: f32, y: f32, x0: f32, x1: f32, y0: f32) -> f32 {
    dist(x, y, x.clamp(x0, x1), y0)
}

// Glyph shapes on a 24x24 grid, y down.

fn bell_hit(x: f32, y: f32) -> bool {
    let dx = (x - 12.0).abs();
    if dist(x, y, 12.0, 3.6) <= 1.5 {
        return true;
    }
    if (4.6..=17.0).contains(&y) {
        let half = if y < 10.0 {
            (5.4f32 * 5.4 - (10.0 - y).powi(2)).max(0.0).sqrt()
        } else {
            5.4 + (y - 10.0) * 0.4
        };
        if dx <= half {
            return true;
        }
    }
    if dist_to_hsegment(x, y, 4.4, 19.6, 17.2) <= 1.0 {
        return true;
    }
    dist(x, y, 12.0, 20.2) <= 2.0
}

fn info_disc(x: f32, y: f32) -> bool {
    dist(x, y, 12.0, 12.0) <= 9.0
}

fn info_mark(x: f32, y: f32) -> bool {
    dist(x, y, 12.0, 8.3) <= 1.25 || ((10.9..=13.1).contains(&x) && (10.8..=16.4).contains(&y))
}

fn error_mark(x: f32, y: f32) -> bool {
    dist(x, y, 12.0, 16.6) <= 1.3 || ((10.9..=13.1).contains(&x) && (6.6..=13.6).contains(&y))
}

/// With no `fonts` only the badge is painted.
pub fn paint(fonts: Option<&Fonts>, title: &str, subtitle: &str, icon: Icon) -> Picture {
    let (title, subtitle, text_w) = match fonts {
        Some(f) => {
            let max = if icon == Icon::Error {
                MAX_TEXT_ERROR
            } else {
                MAX_TEXT
            };
            let title = elide(title, max, |s| text_width(&f.medium, s, TITLE_PX));
            let subtitle = elide(subtitle, max, |s| text_width(&f.regular, s, SUBTITLE_PX));
            let w = text_width(&f.medium, &title, TITLE_PX).max(text_width(
                &f.regular,
                &subtitle,
                SUBTITLE_PX,
            ));
            (title, subtitle, w.ceil() as i32)
        }
        None => (String::new(), String::new(), 0),
    };
    let width = if text_w == 0 {
        HEIGHT
    } else {
        PAD_LEFT + ICON + GAP + text_w + PAD_RIGHT
    };
    let (cw, ch) = (width + 2 * SHADOW, HEIGHT + 2 * SHADOW);
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (cw * ch) as usize];

    let (half_w, half_h, radius) = (width as f32 / 2.0, HEIGHT as f32 / 2.0, HEIGHT as f32 / 2.0);
    for py in 0..ch {
        for px in 0..cw {
            let fx = px as f32 + 0.5 - SHADOW as f32;
            let fy = py as f32 + 0.5 - SHADOW as f32;
            let pixel = &mut canvas[(py * cw + px) as usize];
            let shadow_d = sd_round_box(fx, fy - 4.0, half_w, half_h, half_w, half_h, radius);
            over(
                pixel,
                [0.0; 3],
                0.5 * (1.0 - smoothstep(-6.0, 18.0, shadow_d)),
            );
            let d = sd_round_box(fx, fy, half_w, half_h, half_w, half_h, radius);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(pixel, FILL, cover);
                let inner = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner).max(0.0) * 0.08);
            }
        }
    }

    let (disc, glyph_color) = match icon {
        Icon::Bell => (BELL_DISC, BELL_GLYPH),
        Icon::Info => (INFO_DISC, INFO_GLYPH),
        Icon::Error => (ERROR_DISC, ERROR_GLYPH),
    };
    let badge_x = (SHADOW + PAD_LEFT) as f32 + ICON as f32 / 2.0;
    let badge_y = (SHADOW as f32) + HEIGHT as f32 / 2.0;
    let layers: Vec<([f32; 3], fn(f32, f32) -> bool)> = match icon {
        Icon::Bell => vec![(glyph_color, bell_hit as fn(f32, f32) -> bool)],
        Icon::Info => vec![
            (glyph_color, info_disc as fn(f32, f32) -> bool),
            ([1.0; 3], info_mark as fn(f32, f32) -> bool),
        ],
        Icon::Error => vec![
            (glyph_color, info_disc as fn(f32, f32) -> bool),
            ([1.0; 3], error_mark as fn(f32, f32) -> bool),
        ],
    };
    const K: f32 = 0.8; // pixels per glyph unit
    let reach = ICON / 2 + 1;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let px = (badge_x as i32 + dx).clamp(0, cw - 1);
            let py = (badge_y as i32 + dy).clamp(0, ch - 1);
            let pixel = &mut canvas[(py * cw + px) as usize];
            let (fx, fy) = (px as f32 + 0.5 - badge_x, py as f32 + 0.5 - badge_y);
            let d = dist(fx, fy, 0.0, 0.0) - ICON as f32 / 2.0;
            over(pixel, disc, (0.5 - d).clamp(0.0, 1.0));
            for (color, hit) in &layers {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let ox = fx + (sx as f32 + 0.5) / 4.0 - 0.5;
                        let oy = fy + (sy as f32 + 0.5) / 4.0 - 0.5;
                        if hit(ox / K + 12.0, oy / K + 12.0) {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    over(pixel, *color, hits as f32 / 16.0);
                }
            }
        }
    }

    if let Some(f) = fonts {
        let x = (SHADOW + PAD_LEFT + ICON + GAP) as f32;
        let top = SHADOW as f32;
        if subtitle.is_empty() {
            let base = top + HEIGHT as f32 / 2.0 + half_line(&f.medium, TITLE_PX);
            draw_text(
                &mut canvas,
                cw,
                ch,
                &f.medium,
                &title,
                TITLE_PX,
                x,
                base,
                TITLE_COLOR,
            );
        } else {
            let base = top + 19.0 + half_line(&f.medium, TITLE_PX);
            draw_text(
                &mut canvas,
                cw,
                ch,
                &f.medium,
                &title,
                TITLE_PX,
                x,
                base,
                TITLE_COLOR,
            );
            let base = top + 37.5 + half_line(&f.regular, SUBTITLE_PX);
            draw_text(
                &mut canvas,
                cw,
                ch,
                &f.regular,
                &subtitle,
                SUBTITLE_PX,
                x,
                base,
                SUBTITLE_COLOR,
            );
        }
    }

    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for p in &canvas {
        for c in p {
            rgba.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Picture {
        rgba,
        width: cw,
        height: ch,
    }
}

fn half_line(font: &Font, px: f32) -> f32 {
    let (ascent, descent) = font
        .horizontal_line_metrics(px)
        .map_or((px * 0.9, -px * 0.25), |m| (m.ascent, m.descent));
    (ascent + descent) / 2.0
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut [Pixel],
    cw: i32,
    ch: i32,
    font: &Font,
    text: &str,
    px: f32,
    x: f32,
    baseline: f32,
    color: [f32; 3],
) {
    let mut pen = x;
    for c in text.chars() {
        let (metrics, bitmap) = font.rasterize(drawable(font, c), px);
        let gx = (pen + metrics.xmin as f32).round() as i32;
        let gy = (baseline - metrics.height as f32 - metrics.ymin as f32).round() as i32;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let cover = f32::from(bitmap[row * metrics.width + col]) / 255.0;
                let (cx, cy) = (gx + col as i32, gy + row as i32);
                if cover > 0.0 && (0..cw).contains(&cx) && (0..ch).contains(&cy) {
                    over(&mut canvas[(cy * cw + cx) as usize], color, cover);
                }
            }
        }
        pen += metrics.advance_width;
    }
}

fn key_of(id: &ObjectId) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn toplevel_names(surface: &WlSurface) -> (String, String) {
    with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|data| {
                let data = data.lock().unwrap();
                (
                    clean(data.app_id.as_deref().unwrap_or_default()),
                    clean(data.title.as_deref().unwrap_or_default()),
                )
            })
            .unwrap_or_default()
    })
}

/// The first line with something in it. Parse errors come with a source excerpt below.
pub fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(clean)
        .unwrap_or_default()
}

pub fn subtitle_of(app_id: &str, title: &str, workspace: Option<usize>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !app_id.is_empty() {
        parts.push(app_id.to_owned());
    }
    if !title.is_empty() && title != app_id {
        parts.push(title.to_owned());
    }
    if let Some(workspace) = workspace {
        parts.push(format!("workspace {workspace}"));
    }
    parts.join(" · ")
}

impl JffState {
    pub fn show_bell(&mut self, surface: Option<WlSurface>) {
        let (off, timeout) = {
            let osd = self.config.osd();
            (osd.off, Duration::from_millis(u64::from(osd.timeout_ms)))
        };
        if off {
            return;
        }
        let mut notice = Notice {
            key: None,
            icon: Icon::Bell,
            title: "Bell".to_owned(),
            subtitle: String::new(),
            output: String::new(),
        };
        let mut output = None;
        if let Some(surface) = surface {
            // popups and subsurfaces ring for their window
            let mut root = surface;
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            notice.key = Some(key_of(&root.id()));
            let window = self
                .desk
                .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
                .map(|entry| (entry.pos, entry.window.geometry().size, entry.workspace));
            if let Some((pos, size, workspace)) = window {
                let (app_id, title) = toplevel_names(&root);
                // only named when it isn't the current one
                let hidden = (workspace != self.desk.current()).then_some(workspace + 1);
                notice.subtitle = subtitle_of(&app_id, &title, hidden);
                let rect = self.rect_for_window(pos, (size.w, size.h));
                output = self
                    .outputs
                    .iter()
                    .find(|o| output_rect(o) == rect)
                    .map(Output::name);
            }
        }
        let output = output.or_else(|| self.output_at_pointer().map(|o| o.name()));
        let Some(output) = output else {
            return;
        };
        notice.output = output;
        self.osd.show(notice, timeout, Instant::now());
    }

    /// Show an error toast. Not affected by `osd { off }`, an error you can't see is worse.
    pub fn show_error(&mut self, title: &str, message: &str) {
        let Some(output) = self.output_at_pointer().map(|o| o.name()) else {
            return;
        };
        let notice = Notice {
            key: None,
            icon: Icon::Error,
            title: title.to_owned(),
            subtitle: first_line(message),
            output,
        };
        self.osd.show(notice, ERROR_TIMEOUT, Instant::now());
    }
}
