//! What the overview's search looks like: a search box and, below the windows, a grid of
//! matching apps, an icon over a name per tile, like GNOME Shell's app grid.
//!
//! Both are painted on the CPU into a picture, the same way the toasts in `osd.rs` are, and
//! drawn as memory buffers. The caller only paints again when the text, the results or the
//! selection change. Without a font (still loading, or not found) the shapes are painted and
//! the text is skipped.
//!
//! An app with no drawable icon gets a round badge with the first letter of its name instead.

use std::sync::Arc;

use crate::feature::app_icons::{Icon, ICON_PX};
use crate::feature::osd::{
    draw_text, half_line, over, sd_round_box, smoothstep, text_width, Fonts, Picture, Pixel, SHADOW,
};

/// Width of the search box and the list, as long as the monitor is wide enough.
pub const BOX_WIDTH: i32 = 520;
pub const BOX_HEIGHT: i32 = 52;
/// Size of one tile of the app grid.
pub const TILE_W: i32 = 132;
pub const TILE_H: i32 = 120;
/// Padding around the tiles, inside the panel.
pub const LIST_PAD: i32 = 10;
/// Most tiles in a row.
const MAX_COLUMNS: i32 = 4;
/// Space between the search box and whatever sits under it.
pub const GAP: i32 = 12;
/// Distance from the top of the usable area to the search box.
pub const TOP_OFFSET: i32 = 8;

const QUERY_PX: f32 = 18.0;
const NAME_PX: f32 = 13.5;
const INITIAL_PX: f32 = 26.0;
pub const BOX_RADIUS: i32 = BOX_HEIGHT / 2;
const LIST_RADIUS: i32 = 26;
const TILE_RADIUS: i32 = 16;
/// Space from the top of a tile to its icon.
const ICON_TOP: i32 = 12;

const FILL: [f32; 3] = [0.110, 0.110, 0.137];
/// How much of the fill's opacity a translucent panel keeps.
const TRANSLUCENT_FILL: f32 = 0.45;
const TEXT: [f32; 3] = [0.95, 0.95, 0.97];
const HINT: [f32; 3] = [0.52, 0.52, 0.58];
const SELECTED: [f32; 3] = [0.35, 0.55, 0.95];
const BADGES: [[f32; 3]; 6] = [
    [0.26, 0.46, 0.80],
    [0.30, 0.64, 0.46],
    [0.78, 0.50, 0.22],
    [0.66, 0.38, 0.74],
    [0.80, 0.34, 0.40],
    [0.24, 0.62, 0.68],
];

/// The colours the user chose for the search box and the app list (`search-background-color`
/// and `search-font-color` in `overview { … }`). `None` keeps the built-in one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Colors {
    /// Fill of the box and of the list, straight alpha.
    pub background: Option<[f32; 4]>,
    /// Colour of the text, the caret and the app names. Its alpha mixes it with the fill.
    pub font: Option<[f32; 4]>,
}

impl Colors {
    /// The fill's colour and opacity. Built in, the box lets what is behind show through when
    /// `translucent` and the list is opaque, a colour of the user's is as opaque as its alpha.
    fn fill(self, translucent: bool) -> ([f32; 3], f32) {
        match self.background {
            Some([r, g, b, a]) => ([r, g, b], a),
            None => (FILL, if translucent { TRANSLUCENT_FILL } else { 1.0 }),
        }
    }

    /// The colour of the text, mixed with the fill by its alpha.
    fn text(self) -> [f32; 3] {
        match self.font {
            Some([r, g, b, a]) => mix(self.fill(false).0, [r, g, b], a),
            None => TEXT,
        }
    }

    /// The hint, the placeholder and the magnifier: the text's colour, half faded into the fill.
    fn hint(self) -> [f32; 3] {
        match self.font {
            Some(_) => mix(self.fill(false).0, self.text(), 0.55),
            None => HINT,
        }
    }

    /// What goes into the key of a painted picture, so a change of colour paints it again.
    pub fn key(self) -> [u32; 8] {
        let [a, b, c, d] = self.background.unwrap_or([-1.0; 4]);
        let [e, f, g, h] = self.font.unwrap_or([-1.0; 4]);
        [a, b, c, d, e, f, g, h].map(f32::to_bits)
    }
}

/// `from` moved `t` of the way to `to`.
fn mix(from: [f32; 3], to: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t.clamp(0.0, 1.0))
}

/// Width of the picture for something `width` wide: room for the shadow on both sides.
pub fn picture_width(width: i32) -> i32 {
    width + 2 * SHADOW
}

/// Tiles per row for `count` apps in a panel at most `max_width` wide.
pub fn columns(count: usize, max_width: i32) -> i32 {
    let fit = ((max_width - 2 * LIST_PAD) / TILE_W).clamp(1, MAX_COLUMNS);
    (count as i32).clamp(1, fit)
}

/// Tiles per row in a panel `width` wide (as made by [`grid_size`]).
pub fn columns_for_width(width: i32) -> i32 {
    ((width - 2 * LIST_PAD) / TILE_W).max(1)
}

/// Width and height of the panel for `count` apps, at most `max_width` wide. `(0, 0)` for none.
pub fn grid_size(count: usize, max_width: i32) -> (i32, i32) {
    if count == 0 {
        return (0, 0);
    }
    let cols = columns(count, max_width);
    let rows = (count as i32 + cols - 1) / cols;
    (cols * TILE_W + 2 * LIST_PAD, rows * TILE_H + 2 * LIST_PAD)
}

/// The tile under (`x`, `y`), measured from the panel's top-left, in a panel `width` wide with
/// `count` tiles.
pub fn tile_at(x: i32, y: i32, width: i32, count: usize) -> Option<usize> {
    let (x, y) = (x - LIST_PAD, y - LIST_PAD);
    let cols = columns_for_width(width);
    if x < 0 || y < 0 || x >= cols * TILE_W {
        return None;
    }
    let index = (y / TILE_H * cols + x / TILE_W) as usize;
    (index < count).then_some(index)
}

/// One tile of the app grid.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    pub name: String,
    /// The generic name ("Web Browser"). Not drawn - only tells tiles apart.
    pub detail: String,
    /// The app's icon, `None` for the lettered badge.
    pub icon: Option<Arc<Icon>>,
}

fn new_canvas(width: i32, height: i32) -> (Vec<Pixel>, i32, i32) {
    let (cw, ch) = (picture_width(width), height + 2 * SHADOW);
    (vec![[0.0; 4]; (cw * ch) as usize], cw, ch)
}

fn into_picture(canvas: Vec<Pixel>, width: i32, height: i32) -> Picture {
    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for pixel in &canvas {
        for channel in pixel {
            rgba.push((channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Picture {
        rgba,
        width,
        height,
    }
}

/// A rounded panel `width` x `height` with a soft shadow and a faint rim, at the canvas's
/// shadow margin. `translucent` lets what is behind show through the fill, and keeps the
/// shadow outside the panel.
#[allow(clippy::too_many_arguments)]
fn panel(
    canvas: &mut [Pixel],
    cw: i32,
    ch: i32,
    width: i32,
    height: i32,
    radius: i32,
    translucent: bool,
    colors: Colors,
) {
    let (fill, fill_alpha) = colors.fill(translucent);
    // A fill that lets the background through must not show the shadow under it.
    let see_through = translucent || fill_alpha < 1.0;
    let (half_w, half_h, radius) = (width as f32 / 2.0, height as f32 / 2.0, radius as f32);
    for py in 0..ch {
        for px in 0..cw {
            let fx = px as f32 + 0.5 - SHADOW as f32;
            let fy = py as f32 + 0.5 - SHADOW as f32;
            let pixel = &mut canvas[(py * cw + px) as usize];
            let shadow = sd_round_box(fx, fy - 4.0, half_w, half_h, half_w, half_h, radius);
            let d = sd_round_box(fx, fy, half_w, half_h, half_w, half_h, radius);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            let hidden = if see_through { cover } else { 0.0 };
            over(
                pixel,
                [0.0; 3],
                0.5 * (1.0 - smoothstep(-6.0, 18.0, shadow)) * (1.0 - hidden),
            );
            if cover > 0.0 {
                over(pixel, fill, cover * fill_alpha);
                let inner = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner).max(0.0) * 0.08);
            }
        }
    }
}

/// A rounded rectangle at (`x`, `y`) of the canvas, blended over what's there.
#[allow(clippy::too_many_arguments)]
fn round_rect(
    canvas: &mut [Pixel],
    cw: i32,
    ch: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: [f32; 3],
    alpha: f32,
) {
    let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
    for py in (y - 1).max(0)..(y + height + 1).min(ch) {
        for px in (x - 1).max(0)..(x + width + 1).min(cw) {
            let fx = px as f32 + 0.5 - x as f32;
            let fy = py as f32 + 0.5 - y as f32;
            let d = sd_round_box(fx, fy, half_w, half_h, half_w, half_h, radius as f32);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(&mut canvas[(py * cw + px) as usize], color, cover * alpha);
            }
        }
    }
}

/// A filled circle centred on (`cx`, `cy`).
fn disc(canvas: &mut [Pixel], cw: i32, ch: i32, cx: f32, cy: f32, radius: f32, color: [f32; 3]) {
    let reach = radius.ceil() as i32 + 1;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let px = (cx as i32 + dx).clamp(0, cw - 1);
            let py = (cy as i32 + dy).clamp(0, ch - 1);
            let (fx, fy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
            let d = (fx * fx + fy * fy).sqrt() - radius;
            over(
                &mut canvas[(py * cw + px) as usize],
                color,
                (0.5 - d).clamp(0.0, 1.0),
            );
        }
    }
}

/// `icon` with its top-left corner at (`x`, `y`) of the canvas, blended over what's there.
fn blit_icon(canvas: &mut [Pixel], cw: i32, ch: i32, x: i32, y: i32, icon: &Icon) {
    let side = ICON_PX;
    for iy in 0..side {
        for ix in 0..side {
            let (px, py) = (x + ix, y + iy);
            if px < 0 || py < 0 || px >= cw || py >= ch {
                continue;
            }
            let i = ((iy * side + ix) * 4) as usize;
            let Some(chunk) = icon.rgba.get(i..i + 4) else {
                continue;
            };
            let alpha = chunk[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let rgb = [
                chunk[0] as f32 / 255.0,
                chunk[1] as f32 / 255.0,
                chunk[2] as f32 / 255.0,
            ];
            over(&mut canvas[(py * cw + px) as usize], rgb, alpha);
        }
    }
}

fn distance_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let length2 = abx * abx + aby * aby;
    let t = if length2 == 0.0 {
        0.0
    } else {
        (((px - a.0) * abx + (py - a.1) * aby) / length2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (a.0 + t * abx, a.1 + t * aby);
    ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
}

/// A magnifying glass centred on (`cx`, `cy`), about 18 px across.
fn magnifier(canvas: &mut [Pixel], cw: i32, ch: i32, cx: f32, cy: f32, color: [f32; 3]) {
    let (lens_x, lens_y, lens_r) = (cx - 2.0, cy - 2.0, 6.5f32);
    let (from, to) = ((cx + 2.6, cy + 2.6), (cx + 8.0, cy + 8.0));
    for dy in -12..=12 {
        for dx in -12..=12 {
            let px = (cx as i32 + dx).clamp(0, cw - 1);
            let py = (cy as i32 + dy).clamp(0, ch - 1);
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let ring =
                (((fx - lens_x).powi(2) + (fy - lens_y).powi(2)).sqrt() - lens_r).abs() - 1.0;
            let handle = distance_to_segment(fx, fy, from, to) - 1.2;
            let d = ring.min(handle);
            over(
                &mut canvas[(py * cw + px) as usize],
                color,
                (0.5 - d).clamp(0.0, 1.0),
            );
        }
    }
}

/// `text` shortened from the left, with a leading `…`, until it fits in `max`: what's being
/// typed is at the end, so that's the part to keep.
fn keep_tail(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    for skip in 1..chars.len() {
        let tail: String = chars[skip..].iter().collect();
        let candidate = format!("…{}", tail.trim_start());
        if width(&candidate) <= max {
            return candidate;
        }
    }
    "…".to_owned()
}

/// The search box with `query` in it, or a hint when it's empty. `width` is the box itself,
/// the picture is wider by the shadow.
pub fn paint_box(
    fonts: Option<&Fonts>,
    query: &str,
    width: i32,
    translucent: bool,
    colors: Colors,
) -> Picture {
    let (text, hint) = (colors.text(), colors.hint());
    let (mut canvas, cw, ch) = new_canvas(width, BOX_HEIGHT);
    panel(
        &mut canvas,
        cw,
        ch,
        width,
        BOX_HEIGHT,
        BOX_RADIUS,
        translucent,
        colors,
    );

    let middle = SHADOW as f32 + BOX_HEIGHT as f32 / 2.0;
    let glass_x = (SHADOW + 26) as f32;
    magnifier(&mut canvas, cw, ch, glass_x, middle, hint);

    if let Some(f) = fonts {
        let left = glass_x + 22.0;
        let room = (width - 26 - 22 - 24) as f32;
        let base = middle + half_line(&f.regular, QUERY_PX);
        if query.is_empty() {
            draw_text(
                &mut canvas,
                cw,
                ch,
                &f.regular,
                "Type to search",
                QUERY_PX,
                left + 4.0,
                base,
                hint,
            );
            round_rect(
                &mut canvas,
                cw,
                ch,
                left as i32,
                middle as i32 - 10,
                2,
                20,
                1,
                text,
                0.9,
            );
        } else {
            let shown = keep_tail(query, room, |s| text_width(&f.regular, s, QUERY_PX));
            draw_text(
                &mut canvas,
                cw,
                ch,
                &f.regular,
                &shown,
                QUERY_PX,
                left,
                base,
                text,
            );
            let end = left + text_width(&f.regular, &shown, QUERY_PX);
            round_rect(
                &mut canvas,
                cw,
                ch,
                end as i32 + 1,
                middle as i32 - 10,
                2,
                20,
                1,
                text,
                0.9,
            );
        }
    }
    into_picture(canvas, cw, ch)
}

/// The grid of apps, `selected` highlighted. `width` is the panel's own width, which decides
/// how many tiles go in a row (see [`grid_size`]).
pub fn paint_list(
    fonts: Option<&Fonts>,
    rows: &[Row],
    selected: Option<usize>,
    width: i32,
    colors: Colors,
) -> Picture {
    let text = colors.text();
    let cols = columns_for_width(width) as usize;
    let lines = rows.len().div_ceil(cols.max(1)) as i32;
    let height = if rows.is_empty() {
        0
    } else {
        lines * TILE_H + 2 * LIST_PAD
    };
    let (mut canvas, cw, ch) = new_canvas(width, height);
    panel(
        &mut canvas,
        cw,
        ch,
        width,
        height,
        LIST_RADIUS,
        false,
        colors,
    );

    for (i, row) in rows.iter().enumerate() {
        let left = SHADOW + LIST_PAD + (i % cols) as i32 * TILE_W;
        let top = SHADOW + LIST_PAD + (i / cols) as i32 * TILE_H;
        if selected == Some(i) {
            round_rect(
                &mut canvas,
                cw,
                ch,
                left + 2,
                top + 2,
                TILE_W - 4,
                TILE_H - 4,
                TILE_RADIUS,
                SELECTED,
                0.32,
            );
        }
        let center_x = left as f32 + TILE_W as f32 / 2.0;
        let icon_top = top + ICON_TOP;
        let icon_middle = icon_top as f32 + ICON_PX as f32 / 2.0;
        if let Some(icon) = &row.icon {
            blit_icon(
                &mut canvas,
                cw,
                ch,
                center_x as i32 - ICON_PX / 2,
                icon_top,
                icon,
            );
        } else {
            let color = BADGES[row.name.chars().map(|c| c as usize).sum::<usize>() % BADGES.len()];
            disc(
                &mut canvas,
                cw,
                ch,
                center_x,
                icon_middle,
                ICON_PX as f32 / 2.0,
                color,
            );
            if let Some(f) = fonts {
                if let Some(initial) = row.name.chars().find(|c| c.is_alphanumeric()) {
                    let initial = initial.to_uppercase().to_string();
                    let w = text_width(&f.medium, &initial, INITIAL_PX);
                    draw_text(
                        &mut canvas,
                        cw,
                        ch,
                        &f.medium,
                        &initial,
                        INITIAL_PX,
                        center_x - w / 2.0,
                        icon_middle + half_line(&f.medium, INITIAL_PX),
                        [1.0; 3],
                    );
                }
            }
        }

        let Some(f) = fonts else {
            continue;
        };
        let room = (TILE_W - 16) as f32;
        let name =
            crate::feature::osd::elide(&row.name, room, |s| text_width(&f.medium, s, NAME_PX));
        let w = text_width(&f.medium, &name, NAME_PX);
        let label_middle = (icon_top + ICON_PX) as f32 + (TILE_H - ICON_TOP - ICON_PX) as f32 / 2.0;
        draw_text(
            &mut canvas,
            cw,
            ch,
            &f.medium,
            &name,
            NAME_PX,
            center_x - w / 2.0,
            label_middle + half_line(&f.medium, NAME_PX),
            text,
        );
    }
    into_picture(canvas, cw, ch)
}

/// Room around an app icon badge for its shadow.
pub const BADGE_MARGIN: i32 = 10;
/// Space between the icon and the edge of the badge.
const BADGE_PAD: i32 = 8;
/// Side of the badge itself, at the size it is painted.
pub const BADGE_BODY: i32 = ICON_PX + 2 * BADGE_PAD;
/// Side of the whole picture, shadow margin included. The caller scales it to fit a thumbnail.
pub const BADGE_PICTURE: i32 = BADGE_BODY + 2 * BADGE_MARGIN;
const BADGE_RADIUS: f32 = 22.0;

/// Shadow margin around the body of a close button whose body is `body` pixels wide.
pub fn close_margin(body: i32) -> i32 {
    (body + 4) / 5
}
/// Fill of the close button under the pointer.
const CLOSE_HOT: [f32; 3] = [0.86, 0.22, 0.22];

/// The close button of the selected thumbnail: a dark disc with a soft shadow and a white
/// cross, red while the pointer is on it (`hot`).
///
/// The disc is `body` pixels wide and the picture adds `close_margin(body)` around it, so the
/// picture maps 1:1 to screen pixels and is never resampled.
pub fn paint_close_button(body: i32, hot: bool) -> Picture {
    let margin = close_margin(body);
    let side = body + 2 * margin;
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (side * side) as usize];
    let center = side as f32 / 2.0;
    let radius = body as f32 / 2.0;
    let k = body as f32 / 24.0;
    let (reach, half_stroke) = (body as f32 * 0.19, (0.95 * k).max(0.9));
    for py in 0..side {
        for px in 0..side {
            let (fx, fy) = (px as f32 + 0.5 - center, py as f32 + 0.5 - center);
            let pixel = &mut canvas[(py * side + px) as usize];
            let shadow = (fx * fx + (fy - k) * (fy - k)).sqrt() - radius;
            over(
                pixel,
                [0.0; 3],
                0.45 * (1.0 - smoothstep(-k, 3.5 * k, shadow)),
            );
            let d = (fx * fx + fy * fy).sqrt() - radius;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(pixel, if hot { CLOSE_HOT } else { FILL }, cover * 0.94);
                let inner = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner).max(0.0) * 0.12);
            }
            // The two diagonals: distance to each segment from (-reach, -reach) to (reach, reach).
            for flip in [1.0f32, -1.0] {
                let (ax, ay) = (-reach, -reach * flip);
                let (bx, by) = (reach, reach * flip);
                let (dx, dy) = (bx - ax, by - ay);
                let t = (((fx - ax) * dx + (fy - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                let (qx, qy) = (ax + t * dx, ay + t * dy);
                let dist = ((fx - qx) * (fx - qx) + (fy - qy) * (fy - qy)).sqrt();
                over(pixel, [1.0; 3], (half_stroke + 0.5 - dist).clamp(0.0, 1.0));
            }
        }
    }
    into_picture(canvas, side, side)
}

/// The badge of a window that asked for attention, on its thumbnail: the close button's dark disc
/// with its soft shadow, and a white exclamation mark. Same size and margin as the close button, so it maps
/// 1:1 to screen pixels.
pub fn paint_urgent_badge(body: i32) -> Picture {
    let margin = close_margin(body);
    let side = body + 2 * margin;
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (side * side) as usize];
    let center = side as f32 / 2.0;
    let radius = body as f32 / 2.0;
    let k = body as f32 / 24.0;
    // The mark: a bar over a dot, both with round ends.
    let (bar_half_w, bar_half_h) = (1.5 * k, 4.4 * k);
    let bar_y = center - 2.6 * k;
    let (dot_r, dot_y) = (1.7 * k, center + 6.0 * k);
    for py in 0..side {
        for px in 0..side {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let (dx, dy) = (fx - center, fy - center);
            let pixel = &mut canvas[(py * side + px) as usize];
            let shadow = (dx * dx + (dy - k) * (dy - k)).sqrt() - radius;
            over(
                pixel,
                [0.0; 3],
                0.45 * (1.0 - smoothstep(-k, 3.5 * k, shadow)),
            );
            let d = (dx * dx + dy * dy).sqrt() - radius;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(pixel, FILL, cover * 0.94);
                let inner = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner).max(0.0) * 0.12);
            }
            let bar = sd_round_box(fx, fy, center, bar_y, bar_half_w, bar_half_h, bar_half_w);
            over(pixel, [1.0; 3], (0.5 - bar).clamp(0.0, 1.0));
            let dot = ((fx - center).powi(2) + (fy - dot_y).powi(2)).sqrt() - dot_r;
            over(pixel, [1.0; 3], (0.5 - dot).clamp(0.0, 1.0));
        }
    }
    into_picture(canvas, side, side)
}

/// The badge of an app that goes on its window's thumbnail: its icon on a dark rounded square
/// with a soft shadow. Without an icon, the lettered round badge of the search grid, with
/// `name`'s first letter (the letter is skipped while there's no font).
pub fn paint_badge(fonts: Option<&Fonts>, icon: Option<&Icon>, name: &str) -> Picture {
    let side = BADGE_PICTURE;
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (side * side) as usize];
    let center = side as f32 / 2.0;
    let half = BADGE_BODY as f32 / 2.0;
    for py in 0..side {
        for px in 0..side {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let pixel = &mut canvas[(py * side + px) as usize];
            let shadow = sd_round_box(fx, fy - 2.0, center, center, half, half, BADGE_RADIUS);
            over(pixel, [0.0; 3], 0.5 * (1.0 - smoothstep(-2.0, 9.0, shadow)));
            let d = sd_round_box(fx, fy, center, center, half, half, BADGE_RADIUS);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(pixel, FILL, cover * 0.92);
                let inner = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner).max(0.0) * 0.10);
            }
        }
    }
    let icon_left = BADGE_MARGIN + BADGE_PAD;
    match icon {
        Some(icon) => blit_icon(&mut canvas, side, side, icon_left, icon_left, icon),
        None => {
            let color = BADGES[name.chars().map(|c| c as usize).sum::<usize>() % BADGES.len()];
            disc(
                &mut canvas,
                side,
                side,
                center,
                center,
                ICON_PX as f32 / 2.0 - 2.0,
                color,
            );
            if let (Some(f), Some(initial)) = (fonts, name.chars().find(|c| c.is_alphanumeric())) {
                let initial = initial.to_uppercase().to_string();
                let w = text_width(&f.medium, &initial, INITIAL_PX);
                draw_text(
                    &mut canvas,
                    side,
                    side,
                    &f.medium,
                    &initial,
                    INITIAL_PX,
                    center - w / 2.0,
                    center + half_line(&f.medium, INITIAL_PX),
                    [1.0; 3],
                );
            }
        }
    }
    into_picture(canvas, side, side)
}
