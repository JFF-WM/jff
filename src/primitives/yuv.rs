//! YUV to RGB arithmetic for color representation (stage 2).
//!
//! A YUV dmabuf is sampled by the driver, which turns it into RGB with a matrix and range of
//! its own choosing (BT.601, limited range, in the drivers this was written for). A client
//! that says its video is BT.709, or full range, is therefore drawn with the wrong matrix.
//! The shader fixes that after the fact: it turns the driver's RGB back into the YCbCr the
//! client sent, and converts that again with the matrix and range the client declared. All of
//! it is one affine map, `rgb_out = rows * rgb_in + offset`, worked out here on the CPU so the
//! shader only does a few multiply-adds per pixel.
//!
//! The round trip clamps what the driver already clamped (YCbCr values outside the RGB cube
//! come back as 0 or 1), so it is exact for in-gamut colors and only differs for the rare
//! out-of-gamut ones.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// How YCbCr values map to RGB: the luma weights of the standard and the quantization range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Encoding {
    /// Luma weight of red (Kr). BT.709: 0.2126.
    pub kr: f64,
    /// Luma weight of blue (Kb). BT.709: 0.0722.
    pub kb: f64,
    /// Limited range (16 to 235 luma, 16 to 240 chroma for 8 bits), else full.
    pub limited: bool,
}

type Matrix = [[f64; 3]; 3];
type Vector = [f64; 3];

/// `rgb = m * [y, cb, cr] + off`, with `y`, `cb` and `cr` as sampled (0 to 1).
fn ycc_to_rgb(e: Encoding) -> (Matrix, Vector) {
    let kg = 1.0 - e.kr - e.kb;
    // Sampled value to luma 0..1 (`sy * y + oy`) and chroma -0.5..0.5 (`sc * c + oc`).
    let (sy, oy, sc, oc) = if e.limited {
        (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0)
    } else {
        (1.0, 0.0, 1.0, -0.5)
    };
    let rr = 2.0 * (1.0 - e.kr);
    let bb = 2.0 * (1.0 - e.kb);
    let gb = e.kb * bb / kg;
    let gr = e.kr * rr / kg;
    let m = [
        [sy, 0.0, rr * sc],
        [sy, -gb * sc, -gr * sc],
        [sy, bb * sc, 0.0],
    ];
    let off = [oy + rr * oc, oy - gb * oc - gr * oc, oy + bb * oc];
    (m, off)
}

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mul_vec(a: &Matrix, v: &Vector) -> Vector {
    [0, 1, 2].map(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

/// The inverse, `None` for a matrix that has none.
fn invert(m: &Matrix) -> Option<Matrix> {
    let cofactor =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * cofactor(1, 2, 1, 2) - m[0][1] * cofactor(1, 2, 0, 2)
        + m[0][2] * cofactor(1, 2, 0, 1);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            cofactor(1, 2, 1, 2) * inv,
            -cofactor(0, 2, 1, 2) * inv,
            cofactor(0, 1, 1, 2) * inv,
        ],
        [
            -cofactor(1, 2, 0, 2) * inv,
            cofactor(0, 2, 0, 2) * inv,
            -cofactor(0, 1, 0, 2) * inv,
        ],
        [
            cofactor(1, 2, 0, 1) * inv,
            -cofactor(0, 2, 0, 1) * inv,
            cofactor(0, 1, 0, 1) * inv,
        ],
    ])
}

/// The map from what the driver produced to what the client's encoding says it should be:
/// `rgb_out = rows * rgb_in + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correction {
    /// Row `i` produces channel `i` (r, g, b) from the driver's r, g, b.
    pub rows: [[f32; 3]; 3],
    pub offset: [f32; 3],
}

impl Correction {
    /// The correction for a driver that converts with `assumed` when the client's data is
    /// encoded as `wanted`. `None` when they are the same, there is nothing to correct.
    pub fn between(assumed: Encoding, wanted: Encoding) -> Option<Self> {
        if assumed == wanted {
            return None;
        }
        let (m_assumed, o_assumed) = ycc_to_rgb(assumed);
        let (m_wanted, o_wanted) = ycc_to_rgb(wanted);
        let back = invert(&m_assumed)?;
        // ycc = back * (rgb_in - o_assumed), rgb_out = m_wanted * ycc + o_wanted.
        let rows = mul(&m_wanted, &back);
        let shift = mul_vec(&rows, &o_assumed);
        let offset = [0, 1, 2].map(|i| o_wanted[i] - shift[i]);
        Some(Self {
            rows: rows.map(|row| row.map(|v| v as f32)),
            offset: offset.map(|v| v as f32),
        })
    }

    /// What the shader does to one pixel, clamped to 0..1 like the shader does.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| {
            let row = self.rows[i];
            (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2] + self.offset[i]).clamp(0.0, 1.0)
        })
    }

    /// A number that differs when the correction does. Draw elements are told apart by it, so
    /// a window that changes its encoding is repainted.
    pub fn tag(&self) -> i32 {
        let mut hasher = DefaultHasher::new();
        for value in self.rows.iter().flatten().chain(self.offset.iter()) {
            value.to_bits().hash(&mut hasher);
        }
        hasher.finish() as i32
    }
}
