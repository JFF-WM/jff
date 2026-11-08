//! Software rendering: a solid-colour wl_shm buffer with a few lines of text in a tiny
//! 5x7 bitmap font.

use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;

use memmap2::MmapMut;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_shm::{self, WlShm},
};
use wayland_client::QueueHandle;

use crate::app::App;

/// One colour per window, picked by window index.
pub const PALETTE: [u32; 6] = [0x2E86DE, 0xEE5253, 0x10AC84, 0xF39C12, 0x8E44AD, 0x01A3A4];

const WHITE: u32 = 0xFFFFFF;

pub struct Frame<'a> {
    pub name: &'a str,
    pub color: u32,
    pub width: i32,
    pub height: i32,
    pub status: &'a str,
    pub states: &'a str,
}

/// Draw `frame` into a fresh buffer. The pool is destroyed right away (the buffer keeps
/// the memory alive on the compositor side), the caller attaches the buffer and destroys
/// it on `wl_buffer.release`.
pub fn render(shm: &WlShm, qh: &QueueHandle<App>, frame: &Frame) -> Result<WlBuffer, String> {
    let (w, h) = (frame.width.max(1), frame.height.max(1));
    let stride = w.checked_mul(4).ok_or("window too wide")?;
    let len = stride.checked_mul(h).ok_or("window too large")?;

    let file = shm_file(len as u64).map_err(|e| format!("cannot create shm file: {e}"))?;
    // SAFETY: the file is private to this process until the compositor maps it, and we
    // never truncate it.
    let mut map =
        unsafe { MmapMut::map_mut(&file) }.map_err(|e| format!("cannot mmap shm file: {e}"))?;
    paint(&mut map[..], w, h, frame);

    let pool = shm.create_pool(file.as_fd(), len, qh, ());
    let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    Ok(buffer)
}

fn shm_file(len: u64) -> std::io::Result<File> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir());
    let file = match runtime_dir {
        Some(dir) => tempfile::tempfile_in(dir)?,
        None => tempfile::tempfile()?,
    };
    file.set_len(len)?;
    Ok(file)
}

fn paint(buf: &mut [u8], w: i32, h: i32, frame: &Frame) {
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&frame.color.to_ne_bytes());
    }

    // Border, so the window edges are easy to see.
    let b = 3.min(w / 2).min(h / 2);
    rect(buf, w, h, 0, 0, w, b, WHITE);
    rect(buf, w, h, 0, h - b, w, b, WHITE);
    rect(buf, w, h, 0, 0, b, h, WHITE);
    rect(buf, w, h, w - b, 0, b, h, WHITE);

    let size = format!("{}x{}", frame.width, frame.height);
    let base1 = (h / 8).clamp(1, 10);
    let base2 = (base1 / 2).max(1);
    let states = if frame.states.is_empty() {
        "no states"
    } else {
        frame.states
    };
    let lines = [
        (frame.name, base1),
        (size.as_str(), base2),
        (frame.status, base2),
        (states, base2),
    ];
    let scaled: Vec<(&str, i32)> = lines
        .iter()
        .map(|&(text, base)| (text, fit(text, w, base)))
        .collect();

    let gap = scaled.get(1).map_or(1, |l| l.1) * 3;
    let total: i32 = scaled.iter().map(|&(_, s)| 7 * s).sum::<i32>() + gap * (scaled.len() as i32 - 1);
    let mut y = ((h - total) / 2).max(b + 2);
    for (text, s) in scaled {
        let x = ((w - text_width(text, s)) / 2).max(b + 2);
        draw_text(buf, w, h, x, y, text, s, WHITE);
        y += 7 * s + gap;
    }
}

/// Largest scale (up to `max`) at which `text` fits the window width.
fn fit(text: &str, w: i32, max: i32) -> i32 {
    let n = text.chars().count().max(1) as i32;
    ((w - 16) / (n * 6)).clamp(1, max.max(1))
}

fn text_width(text: &str, scale: i32) -> i32 {
    let n = text.chars().count() as i32;
    if n == 0 {
        0
    } else {
        n * 6 * scale - scale
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(buf: &mut [u8], w: i32, h: i32, x: i32, y: i32, text: &str, scale: i32, color: u32) {
    let mut cx = x;
    for ch in text.chars() {
        for (row, bits) in glyph(ch).iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    rect(
                        buf,
                        w,
                        h,
                        cx + col * scale,
                        y + row as i32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
        cx += 6 * scale;
    }
}

#[allow(clippy::too_many_arguments)]
fn rect(buf: &mut [u8], w: i32, h: i32, x: i32, y: i32, rw: i32, rh: i32, color: u32) {
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + rw).min(w), (y + rh).min(h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let bytes = color.to_ne_bytes();
    for yy in y0..y1 {
        let row = (yy * w) as usize * 4;
        for xx in x0..x1 {
            let o = row + xx as usize * 4;
            buf[o..o + 4].copy_from_slice(&bytes);
        }
    }
}

/// 5x7 glyphs, one byte per row, bit 4 is the leftmost column. Lower case only (upper
/// case is folded), plus digits and a little punctuation.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_lowercase() {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        'a' => [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110],
        'c' => [0b00000, 0b00000, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110],
        'd' => [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111],
        'e' => [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'g' => [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'j' => [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001],
        'n' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0b00000, 0b00000, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000],
        'q' => [0b00000, 0b00000, 0b01111, 0b10001, 0b01111, 0b00001, 0b00001],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'x' => [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0b00000, 0b00000, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'z' => [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        ':' => [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        _ => [0b01110, 0b10001, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100],
    }
}
