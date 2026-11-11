//! Converting colors from the image description of a surface to that of an output
//! (wp_color_management_v1, stage 2): the 3x3 matrix between two sets of primaries, the
//! transfer curve on each side of it, and the rule for when a surface needs any of that.
//!
//! The renderer does the same arithmetic in `rounded.frag`. [`Conversion::apply`] is that
//! shader on the CPU, so the tests check what the GPU is asked to do.
//!
//! The surface's values are decoded to linear light, moved from its primaries to the output's
//! (the two white points are matched with a Bradford adaptation), brought into the output's gamut
//! and encoded with the output's curve. Luminance is not scaled: the white of an SDR surface is
//! the white of the output.
//!
//! Colors the output cannot show are handled by the rendering intent of the surface:
//! relative colorimetric clips them to the gamut's edge ([`GamutMap::Clip`]), perceptual
//! desaturates them toward the grey of the same luminance until they fit ([`GamutMap::Compress`]),
//! so a saturated color keeps its hue and its brightness instead of changing hue at the edge.
//! Colors that already fit are not touched by either.
//!
//! A surface with no description is plain sRGB. Unless [`Conversion::to_output`] is told to
//! convert those (`color-management { convert-srgb }`), plain sRGB is drawn as it always was,
//! which also keeps it eligible for direct scanout.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::color::{
    Chromaticities, ImageDescription, NamedTf, RenderIntent, TransferFunction, CHROMA_SCALE,
    EXPONENT_SCALE,
};

/// A 3x3 matrix, row by row.
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The Bradford cone response matrix, what white points are adapted with.
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// How far a conversion matrix may stray from the identity and still count as none. Chromaticities
/// that differ in the sixth digit (a profile's rounding of sRGB) are the same gamut for the eye,
/// and not worth giving up direct scanout for.
const NEAR_IDENTITY: f64 = 1e-3;

fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `a` times `b`: `b` is applied first.
pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (row, values) in out.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

/// `None` for a matrix that can't be turned around.
pub fn invert(m: &Mat3) -> Option<Mat3> {
    let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
    let c01 = m[1][2] * m[2][0] - m[1][0] * m[2][2];
    let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
    let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            c00 * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            c01 * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            c02 * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ])
}

/// CIE XYZ of a chromaticity, with a luminance (Y) of 1.
fn xyz_of(xy: [i32; 2]) -> [f64; 3] {
    let scale = f64::from(CHROMA_SCALE);
    let x = f64::from(xy[0]) / scale;
    let y = f64::from(xy[1]) / scale;
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// The matrix from linear RGB with these primaries to CIE XYZ, scaled so that RGB (1, 1, 1) is
/// the white point with a luminance of 1. `None` when the chromaticities are not a real gamut.
pub fn rgb_to_xyz(c: &Chromaticities) -> Option<Mat3> {
    c.check().ok()?;
    let r = xyz_of(c.red);
    let g = xyz_of(c.green);
    let b = xyz_of(c.blue);
    let w = xyz_of(c.white);
    let columns = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let scale = mul_vec(&invert(&columns)?, w);
    Some([
        [r[0] * scale[0], g[0] * scale[1], b[0] * scale[2]],
        [r[1] * scale[0], g[1] * scale[1], b[1] * scale[2]],
        [r[2] * scale[0], g[2] * scale[1], b[2] * scale[2]],
    ])
}

/// The Bradford matrix that moves colors seen under the white `from` (XYZ) to the white `to`.
fn adaptation(from: [f64; 3], to: [f64; 3]) -> Option<Mat3> {
    let cone_from = mul_vec(&BRADFORD, from);
    let cone_to = mul_vec(&BRADFORD, to);
    if cone_from.iter().any(|v| v.abs() < 1e-12) {
        return None;
    }
    let scale = [
        [cone_to[0] / cone_from[0], 0.0, 0.0],
        [0.0, cone_to[1] / cone_from[1], 0.0],
        [0.0, 0.0, cone_to[2] / cone_from[2]],
    ];
    Some(mul(&invert(&BRADFORD)?, &mul(&scale, &BRADFORD)))
}

/// The matrix that turns linear RGB of the primaries `from` into linear RGB of the primaries
/// `to`. White maps to white. `None` when either set is not a real gamut.
pub fn conversion_matrix(from: &Chromaticities, to: &Chromaticities) -> Option<Mat3> {
    let source = rgb_to_xyz(from)?;
    let target = rgb_to_xyz(to)?;
    let adapt = if from.white == to.white {
        IDENTITY
    } else {
        adaptation(xyz_of(from.white), xyz_of(to.white))?
    };
    Some(mul(&invert(&target)?, &mul(&adapt, &source)))
}

/// A transfer curve as the shader evaluates it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// The piecewise sRGB curve.
    Srgb,
    /// A pure power: light is the encoded value to this exponent.
    Power(f32),
}

impl Curve {
    pub fn of(tf: TransferFunction) -> Self {
        match tf {
            TransferFunction::Named(NamedTf::Srgb) => Self::Srgb,
            TransferFunction::Named(NamedTf::Gamma22) => Self::power(22_000),
            TransferFunction::Named(NamedTf::Gamma28) => Self::power(28_000),
            TransferFunction::Power(raw) => Self::power(raw),
        }
    }

    /// `raw` is the exponent times [`EXPONENT_SCALE`].
    fn power(raw: u32) -> Self {
        Self::Power(raw as f32 / EXPONENT_SCALE as f32)
    }

    /// Encoded value (0 to 1) to linear light.
    pub fn decode(self, value: f32) -> f32 {
        let v = value.clamp(0.0, 1.0);
        match self {
            Self::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Power(exponent) => v.powf(exponent),
        }
    }

    /// Linear light to encoded value (0 to 1). Light outside 0 to 1 is clipped first, which is
    /// where colors outside the output's gamut end up on its edge.
    pub fn encode(self, light: f32) -> f32 {
        let l = light.clamp(0.0, 1.0);
        match self {
            Self::Srgb => {
                if l <= 0.0031308 {
                    l * 12.92
                } else {
                    1.055 * l.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::Power(exponent) => l.powf(1.0 / exponent),
        }
    }

    /// The two numbers the shader takes for a curve: 1 and an unused 0 for sRGB, 0 and the
    /// exponent for a power.
    pub fn shader_params(self) -> [f32; 2] {
        match self {
            Self::Srgb => [1.0, 0.0],
            Self::Power(exponent) => [0.0, exponent],
        }
    }
}

/// Whether `description` is what a surface without one is taken to be: sRGB primaries and the
/// sRGB curve. Its luminance range is not looked at, SDR white is SDR white.
pub fn is_plain_srgb(description: &ImageDescription) -> bool {
    let srgb = ImageDescription::srgb();
    description.primaries == srgb.primaries && description.tf == srgb.tf
}

/// What happens to a color the output's gamut cannot show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamutMap {
    /// Each channel is clipped to 0..1. Hue shifts at the edge of the gamut.
    Clip,
    /// The color is moved toward the grey of its own luminance, just far enough to fit.
    Compress,
}

impl GamutMap {
    /// What the rendering intent of a surface asks for.
    pub fn of(intent: RenderIntent) -> Self {
        match intent {
            RenderIntent::Perceptual => Self::Compress,
            RenderIntent::Relative => Self::Clip,
        }
    }

    /// The number the shader takes: 1 for [`GamutMap::Compress`], 0 for clipping.
    pub fn shader_param(self) -> f32 {
        match self {
            Self::Clip => 0.0,
            Self::Compress => 1.0,
        }
    }
}

/// Bring linear light `light` (output primaries, may be outside 0..1) into 0..1.
/// `luma` is the row of the output's RGB-to-XYZ matrix that gives Y.
pub fn map_gamut(light: [f32; 3], luma: [f32; 3], map: GamutMap) -> [f32; 3] {
    match map {
        GamutMap::Clip => light.map(|v| v.clamp(0.0, 1.0)),
        GamutMap::Compress => {
            let y = (luma[0] * light[0] + luma[1] * light[1] + luma[2] * light[2]).clamp(0.0, 1.0);
            let low = light[0].min(light[1]).min(light[2]);
            let high = light[0].max(light[1]).max(light[2]);
            let mut keep = 1.0f32;
            if low < 0.0 {
                keep = keep.min(y / (y - low));
            }
            if high > 1.0 {
                keep = keep.min((1.0 - y) / (high - y));
            }
            light.map(|v| (y + (v - y) * keep).clamp(0.0, 1.0))
        }
    }
}

/// What the shader does to a surface to show its colors on an output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Conversion {
    /// Row `i` makes channel `i` (r, g, b) of the output from linear r, g, b of the surface.
    pub rows: [[f32; 3]; 3],
    /// The surface's curve.
    pub decode: Curve,
    /// The output's curve.
    pub encode: Curve,
    /// What to do with colors outside the output's gamut.
    pub map: GamutMap,
    /// The output's luminance row (Y of its red, green and blue), for [`GamutMap::Compress`].
    pub luma: [f32; 3],
}

impl Conversion {
    /// What a surface described as `source` needs to look right on an output described as
    /// `target`, relative colorimetric, plain sRGB left alone. `None` when it needs nothing: it
    /// is plain sRGB (see [`is_plain_srgb`]), or its gamut and curve are already the output's.
    pub fn between(source: &ImageDescription, target: &ImageDescription) -> Option<Self> {
        Self::to_output(source, target, RenderIntent::Relative, false)
    }

    /// Like [`Conversion::between`], with the rendering intent of the surface and the choice of
    /// converting plain sRGB too (`convert_plain_srgb`). That is what makes sRGB content look
    /// right on a wide-gamut monitor instead of oversaturated.
    pub fn to_output(
        source: &ImageDescription,
        target: &ImageDescription,
        intent: RenderIntent,
        convert_plain_srgb: bool,
    ) -> Option<Self> {
        if is_plain_srgb(source) && !convert_plain_srgb {
            return None;
        }
        let matrix = if source.primaries == target.primaries {
            IDENTITY
        } else {
            conversion_matrix(&source.primaries, &target.primaries)?
        };
        let decode = Curve::of(source.tf);
        let encode = Curve::of(target.tf);
        let moves_colors = matrix.iter().enumerate().any(|(i, row)| {
            row.iter()
                .enumerate()
                .any(|(j, value)| (*value - IDENTITY[i][j]).abs() > NEAR_IDENTITY)
        });
        if !moves_colors && decode == encode {
            return None;
        }
        let mut rows = [[0.0f32; 3]; 3];
        for (out, values) in rows.iter_mut().zip(matrix) {
            for (slot, value) in out.iter_mut().zip(values) {
                *slot = value as f32;
            }
        }
        let luma = rgb_to_xyz(&target.primaries).map_or([0.2126, 0.7152, 0.0722], |m| {
            [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32]
        });
        Some(Self {
            rows,
            decode,
            encode,
            map: GamutMap::of(intent),
            luma,
        })
    }

    /// The shader on one color: straight (not premultiplied) values of the surface in, values
    /// for the output out.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let light = rgb.map(|v| self.decode.decode(v));
        let mut mapped = [0.0f32; 3];
        for (value, row) in mapped.iter_mut().zip(self.rows) {
            *value = row[0] * light[0] + row[1] * light[1] + row[2] * light[2];
        }
        map_gamut(mapped, self.luma, self.map).map(|v| self.encode.encode(v))
    }

    /// A number that differs when the conversion does. It goes into the id of the element drawn
    /// with it, so a window repaints when its description changes.
    pub fn tag(&self) -> i32 {
        let mut hasher = DefaultHasher::new();
        for value in self.rows.iter().flatten() {
            value.to_bits().hash(&mut hasher);
        }
        self.map.shader_param().to_bits().hash(&mut hasher);
        for value in self
            .decode
            .shader_params()
            .into_iter()
            .chain(self.encode.shader_params())
        {
            value.to_bits().hash(&mut hasher);
        }
        hasher.finish() as i32
    }
}
