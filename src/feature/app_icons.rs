//! Icons for the apps in the overview search.
//!
//! `Icon=` in a desktop file is either an absolute path or a name looked up in the icon
//! themes. SVGs are drawn with `resvg` at exactly the size they're shown at, PNGs are decoded
//! with `png` and scaled. An app whose icon is missing or unreadable keeps the lettered badge.
//!
//! Icons are looked up the first time a row needs them and cached, misses included, so the
//! disk is only touched once per name.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Side of the square an icon is scaled to, in pixels.
pub const ICON_PX: i32 = 64;

/// Themes to look in after `hicolor`, which every theme falls back to.
const THEMES: [&str; 5] = ["hicolor", "Adwaita", "breeze", "Papirus", "gnome"];
/// Sizes to try, best first: the wanted size, then the nearest above it (a big icon scaled
/// down beats a small one scaled up), then the biggest below. Each size is tried in every
/// theme before moving to the next.
const SIZES: [u32; 16] = [
    64, 72, 96, 128, 192, 256, 384, 512, 48, 40, 36, 32, 24, 22, 20, 16,
];
/// Don't read files bigger than this.
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Don't decode pictures bigger than this on a side.
const MAX_SIDE: u32 = 1024;

/// An icon scaled to `ICON_PX` x `ICON_PX`, straight-alpha RGBA.
#[derive(Debug)]
pub struct Icon {
    /// Where it came from - what identifies it.
    pub source: PathBuf,
    pub rgba: Vec<u8>,
}

impl PartialEq for Icon {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for Icon {}
impl Hash for Icon {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
    }
}

type Cache = Mutex<HashMap<String, Option<Arc<Icon>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The icon for an `Icon=` value, `None` when there's nothing we can draw.
pub fn icon_for(name: &str) -> Option<Arc<Icon>> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(found) = cache.get(name) {
        return found.clone();
    }
    let loaded = find_file(name).and_then(|path| load(&path)).map(Arc::new);
    cache.insert(name.to_owned(), loaded.clone());
    loaded
}

/// Forget what was looked up, so installed or removed icons get noticed. Call when the app
/// list is rescanned.
pub fn forget_all() {
    cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Directories that hold icon themes, the user's first.
fn icon_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(home) = home() {
        bases.push(home.join(".icons"));
    }
    match std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(data_home) => bases.push(data_home.join("icons")),
        None => {
            if let Some(home) = home() {
                bases.push(home.join(".local/share/icons"));
            }
        }
    }
    match std::env::var("XDG_DATA_DIRS") {
        Ok(dirs) if !dirs.trim().is_empty() => bases.extend(
            dirs.split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("icons")),
        ),
        _ => {
            bases.push(PathBuf::from("/usr/local/share/icons"));
            bases.push(PathBuf::from("/usr/share/icons"));
        }
    }
    if let Some(home) = home() {
        bases.push(home.join(".local/share/flatpak/exports/share/icons"));
    }
    bases.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    bases
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(wanted))
}

fn is_image(path: &Path) -> bool {
    has_extension(path, "png") || has_extension(path, "svg")
}

/// The image file to use for `name`.
fn find_file(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        let path = PathBuf::from(name);
        if is_image(&path) && path.is_file() {
            return Some(path);
        }
        return None;
    }
    // A name never has directories in it, `foo.png` is a name with its extension kept.
    if name.contains('/') {
        return None;
    }
    let name = name
        .strip_suffix(".png")
        .or_else(|| name.strip_suffix(".svg"))
        .unwrap_or(name);
    let bases = icon_bases();
    // An SVG is sharp at any size, so it beats every PNG.
    for theme in THEMES {
        for base in &bases {
            let root = base.join(theme);
            for path in [
                root.join(format!("scalable/apps/{name}.svg")),
                root.join(format!("apps/scalable/{name}.svg")),
            ] {
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    // Some themes keep SVGs in sized directories too (Papirus `64x64/apps/`, breeze
    // `apps/48/`). They are still sharper than any PNG, so try them before the PNGs.
    for extension in ["svg", "png"] {
        for size in SIZES {
            for theme in THEMES {
                for base in &bases {
                    let root = base.join(theme);
                    // `64x64/apps/` (hicolor, Adwaita, Papirus) and `apps/64/` (breeze).
                    for path in [
                        root.join(format!("{size}x{size}/apps/{name}.{extension}")),
                        root.join(format!("apps/{size}/{name}.{extension}")),
                    ] {
                        if path.is_file() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }
    // A few apps only ship a file in the theme's top directory.
    let pixmaps = [
        PathBuf::from("/usr/share/pixmaps"),
        PathBuf::from("/usr/local/share/pixmaps"),
    ];
    let tops = bases.iter().cloned().chain(pixmaps);
    tops.flat_map(|dir| {
        [
            dir.join(format!("{name}.svg")),
            dir.join(format!("{name}.png")),
        ]
    })
    .find(|path| path.is_file())
}

fn load(path: &Path) -> Option<Icon> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let rgba = if has_extension(path, "svg") {
        render_svg(&bytes)?
    } else {
        let (rgba, width, height) = decode(&bytes)?;
        scale(&rgba, width, height, ICON_PX as u32)
    };
    Some(Icon {
        source: path.to_owned(),
        rgba,
    })
}

/// An SVG drawn into a square `ICON_PX` picture, keeping aspect ratio, as straight-alpha RGBA.
fn render_svg(bytes: &[u8]) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let side = ICON_PX as u32;
    let k = side as f32 / width.max(height);
    let (tx, ty) = (
        (side as f32 - width * k) / 2.0,
        (side as f32 - height * k) / 2.0,
    );
    let mut pixmap = tiny_skia::Pixmap::new(side, side)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_row(k, 0.0, 0.0, k, tx, ty),
        &mut pixmap.as_mut(),
    );
    // The pixmap has colour multiplied by alpha, un-premultiply.
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let alpha = px[3] as u32;
        if alpha > 0 && alpha < 255 {
            for channel in &mut px[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Some(rgba)
}

/// A PNG as straight-alpha 8-bit RGBA.
fn decode(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    // The decoder wants something it can read by line and seek in: a slice in a cursor.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > MAX_SIDE || info.height > MAX_SIDE {
        return None;
    }
    // Room for 8-bit RGBA at most, the biggest a frame gets after the transformations.
    let mut buffer = vec![0; info.width as usize * info.height as usize * 4];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let data = &buffer[..frame.buffer_size()];
    let (width, height) = (frame.width, frame.height);
    let pixels = (width * height) as usize;
    let mut rgba = Vec::with_capacity(pixels * 4);
    match frame.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(data.get(..pixels * 4)?),
        png::ColorType::Rgb => {
            for px in data.get(..pixels * 3)?.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in data.get(..pixels * 2)?.chunks_exact(2) {
                rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in data.get(..pixels)? {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        // EXPAND turns these into one of the above.
        png::ColorType::Indexed => return None,
    }
    Some((rgba, width, height))
}

/// `rgba` (`width` x `height`) as a square `side` x `side` picture, averaging the source
/// pixels under each target pixel. Colour is averaged weighted by alpha, so transparent
/// pixels don't darken the edges.
pub fn scale(rgba: &[u8], width: u32, height: u32, side: u32) -> Vec<u8> {
    let mut out = vec![0u8; (side * side * 4) as usize];
    if width == 0 || height == 0 {
        return out;
    }
    // Keep aspect ratio, centred.
    let longest = width.max(height) as f32;
    let (fit_w, fit_h) = (
        (width as f32 / longest * side as f32).max(1.0),
        (height as f32 / longest * side as f32).max(1.0),
    );
    let (off_x, off_y) = ((side as f32 - fit_w) / 2.0, (side as f32 - fit_h) / 2.0);
    let upscaling = width.max(height) < side;
    for ty in 0..side {
        for tx in 0..side {
            if upscaling {
                // Smooth, not blocky.
                let sx = (tx as f32 + 0.5 - off_x) / fit_w * width as f32 - 0.5;
                let sy = (ty as f32 + 0.5 - off_y) / fit_h * height as f32 - 0.5;
                if sx < -0.5 || sy < -0.5 || sx > width as f32 - 0.5 || sy > height as f32 - 0.5 {
                    continue;
                }
                let (x0, y0) = (sx.floor(), sy.floor());
                let (ax, ay) = (sx - x0, sy - y0);
                let at = |x: f32, y: f32| {
                    let x = (x.max(0.0) as u32).min(width - 1);
                    let y = (y.max(0.0) as u32).min(height - 1);
                    ((y * width + x) * 4) as usize
                };
                let taps = [
                    (at(x0, y0), (1.0 - ax) * (1.0 - ay)),
                    (at(x0 + 1.0, y0), ax * (1.0 - ay)),
                    (at(x0, y0 + 1.0), (1.0 - ax) * ay),
                    (at(x0 + 1.0, y0 + 1.0), ax * ay),
                ];
                let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                for (i, weight) in taps {
                    let alpha = rgba[i + 3] as f32 * weight;
                    r += rgba[i] as f32 * alpha;
                    g += rgba[i + 1] as f32 * alpha;
                    b += rgba[i + 2] as f32 * alpha;
                    a += alpha;
                }
                if a > 0.0 {
                    let o = ((ty * side + tx) * 4) as usize;
                    out[o] = (r / a + 0.5) as u8;
                    out[o + 1] = (g / a + 0.5) as u8;
                    out[o + 2] = (b / a + 0.5) as u8;
                    out[o + 3] = (a + 0.5) as u8;
                }
                continue;
            }
            let (fx0, fy0) = (tx as f32 - off_x, ty as f32 - off_y);
            let (fx1, fy1) = (fx0 + 1.0, fy0 + 1.0);
            if fx1 <= 0.0 || fy1 <= 0.0 || fx0 >= fit_w || fy0 >= fit_h {
                continue;
            }
            let sx0 = (fx0.max(0.0) / fit_w * width as f32).floor() as u32;
            let sy0 = (fy0.max(0.0) / fit_h * height as f32).floor() as u32;
            let sx1 = ((fx1.min(fit_w) / fit_w * width as f32).ceil() as u32).min(width);
            let sy1 = ((fy1.min(fit_h) / fit_h * height as f32).ceil() as u32).min(height);
            let (mut r, mut g, mut b, mut a, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in sy0..sy1.max(sy0 + 1).min(height) {
                for sx in sx0..sx1.max(sx0 + 1).min(width) {
                    let i = ((sy * width + sx) * 4) as usize;
                    let alpha = rgba[i + 3] as f32;
                    r += rgba[i] as f32 * alpha;
                    g += rgba[i + 1] as f32 * alpha;
                    b += rgba[i + 2] as f32 * alpha;
                    a += alpha;
                    n += 1.0;
                }
            }
            if n == 0.0 || a == 0.0 {
                continue;
            }
            let o = ((ty * side + tx) * 4) as usize;
            out[o] = (r / a + 0.5) as u8;
            out[o + 1] = (g / a + 0.5) as u8;
            out[o + 2] = (b / a + 0.5) as u8;
            out[o + 3] = (a / n + 0.5) as u8;
        }
    }
    out
}
