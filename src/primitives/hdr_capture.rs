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

    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS);
    let per_worker = pixels.div_ceil(workers);

    let mut top_code = 0.0f32;
    std::thread::scope(|scope| {
        let handles: Vec<_> = half_rgba[..pixels * 4]
            .chunks(per_worker * 4)
            .zip(codes.chunks_mut(per_worker * 3))
            .map(|(source, target)| {
                scope.spawn(move || {
                    let mut top = 0.0f32;
                    for (px, out) in source.chunks_exact(4).zip(target.chunks_exact_mut(3)) {
                        let working = [px[0], px[1], px[2]].map(|bits| {
                            let v = half_to_f32(bits);
                            if v.is_nan() {
                                0.0
                            } else {
                                v.clamp(0.0, HALF_MAX)
                            }
                        });
                        let coded = pass.apply(working);
                        for (slot, code) in out.iter_mut().zip(coded) {
                            *slot = code;
                            top = top.max(code);
                        }
                    }
                    top
                })
            })
            .collect();
        for handle in handles {
            top_code = top_code.max(handle.join().unwrap_or(0.0));
        }
    });

    PqPicture {
        codes,
        max_nits: pq_decode(top_code) * PQ_PEAK,
    }
}
