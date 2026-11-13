//! The step between a capture of an HDR output and its picture file: what was read back from the working space becomes the PQ codes the PNG holds.

use super::hdr::OutputEotf;
use super::hdr_output::{pq_decode, FinalPass, PQ_PEAK};

/// Largest finite half float. Anything above it (and infinity) is read as this, so one stray
/// pixel cannot turn into NaN further down.
const HALF_MAX: f32 = 65504.0;

/// More threads than this do not make the conversion faster, memory is the limit by then.
const MAX_WORKERS: usize = 8;

/// An IEEE half float, given by its bits, as an `f32`.
pub fn half_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let fraction = f32::from(bits & 0x3ff);
    match exponent {
        // zero and the subnormals: fraction * 2^-24
        0 => sign * fraction * 2f32.powi(-24),
        31 if fraction == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// One component read back from the working space: no NaN, nothing below zero, nothing above
/// the largest half.
fn component(bits: u16) -> f32 {
    let v = half_to_f32(bits);
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, HALF_MAX)
    }
}

/// Run `work` over `source` (four values a pixel) and `target` (`out` values a pixel) split
/// across threads. `work` gets the index of the first pixel of its part, and gives back the
/// brightest code it wrote.
fn convert_in_parallel<F>(source: &[u16], target: &mut [f32], out: usize, work: F) -> f32
where
    F: Fn(usize, &[u16], &mut [f32]) -> f32 + Sync,
{
    let pixels = source.len() / 4;
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS);
    let per_worker = pixels.div_ceil(workers).max(1);
    let work = &work;
    let mut top = 0.0f32;
    std::thread::scope(|scope| {
        let handles: Vec<_> = source[..pixels * 4]
            .chunks(per_worker * 4)
            .zip(target.chunks_mut(per_worker * out))
            .enumerate()
            .map(|(n, (src, dst))| scope.spawn(move || work(n * per_worker, src, dst)))
            .collect();
        for handle in handles {
            top = top.max(handle.join().unwrap_or(0.0));
        }
    });
    top
}

/// How much of the pixel at (`x`, `y`) of a `width` by `height` picture is left once its
/// corners are rounded by `radius` pixels: 1 inside, a soft edge on the quarter circles.
pub fn corner_coverage(x: u32, y: u32, width: u32, height: u32, radius: u32) -> f32 {
    let radius = radius.min(width / 2).min(height / 2);
    if radius == 0 {
        return 1.0;
    }
    // distance from the nearest corner, as if that corner were the top left one
    let cx = x.min(width - 1 - x);
    let cy = y.min(height - 1 - y);
    if cx >= radius || cy >= radius {
        return 1.0;
    }
    let r = radius as f32;
    let (dx, dy) = (r - (cx as f32 + 0.5), r - (cy as f32 + 0.5));
    (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0)
}

/// A frame of the working space, as the PQ picture the output shows.
#[derive(Clone, Debug, PartialEq)]
pub struct PqPicture {
    /// PQ code values (0 to 1), R G B per pixel, top row first.
    pub codes: Vec<f32>,
    /// Brightest component of any pixel, in cd/m².
    pub max_nits: f32,
}

/// Run `pass` over a frame of the working space (`half_rgba`: four half floats per pixel, the
/// alpha ignored) and code the result as PQ. The pass is the one the monitor is drawn with, so
/// the picture has the tone curve and the gamut mapping that were on screen, only the signal
/// is always PQ, because that is what the file says it is.
pub fn pq_picture(pass: &FinalPass, half_rgba: &[u16]) -> PqPicture {
    let pass = pass.with_eotf(OutputEotf::Pq);
    let pixels = half_rgba.len() / 4;
    let mut codes = vec![0.0f32; pixels * 3];
    if pixels == 0 {
        return PqPicture {
            codes,
            max_nits: 0.0,
        };
    }
    let top_code = convert_in_parallel(half_rgba, &mut codes, 3, |_, source, target| {
        let mut top = 0.0f32;
        for (px, out) in source.chunks_exact(4).zip(target.chunks_exact_mut(3)) {
            let coded = pass.apply([px[0], px[1], px[2]].map(component));
            for (slot, code) in out.iter_mut().zip(coded) {
                *slot = code;
                top = top.max(code);
            }
        }
        top
    });
    PqPicture {
        codes,
        max_nits: pq_decode(top_code) * PQ_PEAK,
    }
}

/// [`pq_picture`] for a picture with an alpha channel, such as a window: `half_rgba` is
/// premultiplied, `width` pixels a row, and `codes` comes back as R G B A per pixel with the
/// color straight (so it is the color of the window, not the color of the window over black). The
/// corners are rounded by `corner_radius` pixels by taking the alpha down there, the color
/// stays what it was.
pub fn pq_picture_rgba(
    pass: &FinalPass,
    half_rgba: &[u16],
    width: u32,
    corner_radius: u32,
) -> PqPicture {
    let pass = pass.with_eotf(OutputEotf::Pq);
    let pixels = half_rgba.len() / 4;
    let mut codes = vec![0.0f32; pixels * 4];
    if pixels == 0 || width == 0 {
        return PqPicture {
            codes,
            max_nits: 0.0,
        };
    }
    let height = (pixels / width as usize) as u32;
    let top_code = convert_in_parallel(half_rgba, &mut codes, 4, |first, source, target| {
        let mut top = 0.0f32;
        for (n, (px, out)) in source
            .chunks_exact(4)
            .zip(target.chunks_exact_mut(4))
            .enumerate()
        {
            let at = first + n;
            let alpha = component(px[3]).min(1.0);
            let mut working = [px[0], px[1], px[2]].map(component);
            if alpha > 0.0 {
                working = working.map(|v| v / alpha);
            }
            let coded = pass.apply(working);
            let x = (at % width as usize) as u32;
            let y = (at / width as usize) as u32;
            let coverage = corner_coverage(x, y, width, height, corner_radius);
            // nothing there, nothing to say about its brightness
            if alpha > 0.0 {
                top = top.max(coded[0]).max(coded[1]).max(coded[2]);
            }
            out[..3].copy_from_slice(&coded);
            out[3] = alpha * coverage;
        }
        top
    });
    PqPicture {
        codes,
        max_nits: pq_decode(top_code) * PQ_PEAK,
    }
}
