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
//! Colors that already fit are not touched by either, unless the surface says what its content
//! was mastered on (`set_mastering_display_primaries`) and that reaches beyond the output's gamut:
//! then the perceptual intents fit that target volume to the output instead of waiting for a color
//! to leave it ([`target_reach`], [`soften`]). The other intents change something before
//! that: saturation leaves the primaries out of the matrix, relative with black point
//! compensation moves the surface's black onto the output's ([`Conversion::black`]), and absolute
//! without adaptation leaves the white point alone ([`conversion_matrix_absolute`]).
//!
//! A surface with no description is plain sRGB. Unless [`Conversion::to_output`] is told to
//! convert those (`color-management { convert-srgb }`), plain sRGB is drawn as it always was,
//! which also keeps it eligible for direct scanout.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::color::{
    Chromaticities, ImageDescription, Luminance, NamedPrimaries, NamedTf, RenderIntent,
    TransferFunction, CHROMA_SCALE, EXPONENT_SCALE, MIN_LUM_SCALE,
};
use super::hdr_output::{
    hlg_display_nits, pq_decode, pq_encode, BT2020_LUMA, HDR_REFERENCE_WHITE, PQ_PEAK,
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

/// Like [`conversion_matrix`] with no chromatic adaptation: the color (CIE XYZ) stays what it was, so when the white points differ the white of `from` is not (1, 1, 1) in `to`.
pub fn conversion_matrix_absolute(from: &Chromaticities, to: &Chromaticities) -> Option<Mat3> {
    Some(mul(&invert(&rgb_to_xyz(to)?)?, &rgb_to_xyz(from)?))
}

/// Linear sRGB to linear BT.2020, both with a D65 white. Stands in if the primaries cannot be worked out.
const SRGB_TO_BT2020: Mat3 = [
    [0.627404, 0.329283, 0.043313],
    [0.069097, 0.919540, 0.011362],
    [0.016391, 0.088013, 0.895595],
];

/// A transfer curve as the shader evaluates it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// The piecewise sRGB curve.
    Srgb,
    /// A pure power: light is the encoded value to this exponent.
    Power(f32),
    /// SMPTE ST 2084, decoding to a fraction of 10000 cd/m².
    Pq,
    /// BT.2100 HLG with its OOTF for a 1000 cd/m² display, decoding to a fraction of 10000 cd/m². `luma` is the Y row of the surface's primaries.
    Hlg { luma: [f32; 3] },
    /// Extended linear (Windows-scRGB): light is the encoded value, and it is not clamped, so
    /// highlights above 1.0 and colors below 0 survive until the output decides what to do
    /// with them. What 1.0 is worth comes from the surface's reference luminance.
    Linear,
}

impl Curve {
    pub fn of(tf: TransferFunction) -> Self {
        match tf {
            TransferFunction::Named(NamedTf::Srgb) => Self::Srgb,
            TransferFunction::Named(NamedTf::Gamma22) => Self::power(22_000),
            TransferFunction::Named(NamedTf::Gamma28) => Self::power(28_000),
            TransferFunction::Named(NamedTf::St2084Pq) => Self::Pq,
            TransferFunction::Named(NamedTf::Hlg) => Self::Hlg { luma: BT2020_LUMA },
            TransferFunction::Named(NamedTf::ExtLinear) => Self::Linear,
            TransferFunction::Power(raw) => Self::power(raw),
        }
    }

    /// The curve of a surface described as `description`: like [`Curve::of`], with HLG weighing colors by the luminance of the surface's own primaries.
    pub fn of_description(description: &ImageDescription) -> Self {
        match Self::of(description.tf) {
            Self::Hlg { .. } => Self::Hlg {
                luma: rgb_to_xyz(&description.primaries).map_or(BT2020_LUMA, |m| {
                    [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32]
                }),
            },
            curve => curve,
        }
    }

    /// The light is absolute: the curve decodes to a fraction of 10000 cd/m², not to a share of the surface's own white.
    pub fn is_absolute(self) -> bool {
        matches!(self, Self::Pq | Self::Hlg { .. })
    }

    /// `raw` is the exponent times [`EXPONENT_SCALE`].
    fn power(raw: u32) -> Self {
        Self::Power(raw as f32 / EXPONENT_SCALE as f32)
    }

    /// Encoded value (0 to 1) to linear light. For HLG this is a neutral grey, colors go through [`Curve::decode_rgb`].
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
            Self::Pq => pq_decode(v),
            Self::Hlg { luma } => hlg_display_nits([v; 3], luma)[0] / PQ_PEAK,
            // Not clamped: scRGB goes above 1 for highlights and below 0 for wide colors.
            Self::Linear => value,
        }
    }

    /// A color (encoded values) to linear light, the shader's `cm_to_light`. HLG's OOTF ties the channels together, so it cannot be done one channel at a time.
    pub fn decode_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Hlg { luma } => {
                hlg_display_nits(rgb.map(|v| v.clamp(0.0, 1.0)), luma).map(|nits| nits / PQ_PEAK)
            }
            _ => rgb.map(|v| self.decode(v)),
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
            Self::Pq => pq_encode(l),
            // Not output curves: an HDR output is encoded by its final pass.
            Self::Hlg { .. } | Self::Linear => l,
        }
    }

    /// The two numbers the shader takes for a curve: 1 and an unused 0 for sRGB, 0 and the
    /// exponent for a power, 2 for PQ, 3 for HLG and 4 for extended linear.
    pub fn shader_params(self) -> [f32; 2] {
        match self {
            Self::Srgb => [1.0, 0.0],
            Self::Power(exponent) => [0.0, exponent],
            Self::Pq => [2.0, 0.0],
            Self::Hlg { .. } => [3.0, 0.0],
            Self::Linear => [4.0, 0.0],
        }
    }

    /// The Y row of the surface's primaries that HLG weighs colors by, zeros for the other curves.
    pub fn shader_luma(self) -> [f32; 3] {
        match self {
            Self::Hlg { luma } => luma,
            _ => [0.0; 3],
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
    /// The names the config takes, in the order of the variants.
    pub const NAMES: [&'static str; 2] = ["clip", "compress"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "clip" => Some(Self::Clip),
            "compress" => Some(Self::Compress),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Compress => "compress",
        }
    }

    /// What the rendering intent of a surface asks for.
    pub fn of(intent: RenderIntent) -> Self {
        match intent {
            RenderIntent::Perceptual | RenderIntent::Saturation => Self::Compress,
            RenderIntent::Relative
            | RenderIntent::Absolute
            | RenderIntent::RelativeBpc
            | RenderIntent::AbsoluteNoAdaptation => Self::Clip,
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

/// Where the knee of [`soften`] starts, as a share of the way from the grey axis to the edge of the
/// output's gamut. `rounded.frag` has the same number (`CM_KNEE`).
pub const KNEE_START: f32 = 0.8;

/// The spread between the channels of a color below which it is taken to be a grey. `rounded.frag` has the same number.
const GREY_SPREAD: f32 = 1e-6;

/// How far `light` (output primaries) is from the grey axis, as a share of the way from the axis to
/// the edge of the output's gamut along the same line: 0 for a grey, 1 on the edge, more outside.
/// `luma` is the row of the output's RGB-to-XYZ matrix that gives Y. A color that cannot be
/// brought in by moving it toward its grey (its grey is already past the edge) is infinitely far.
pub fn edge_distance(light: [f32; 3], luma: [f32; 3]) -> f32 {
    let y = (luma[0] * light[0] + luma[1] * light[1] + luma[2] * light[2]).clamp(0.0, 1.0);
    let low = light[0].min(light[1]).min(light[2]);
    let high = light[0].max(light[1]).max(light[2]);
    // A grey is on the axis, whatever rounding makes of its luminance (white is the corner where
    // the distance would be 0 over 0).
    if high - low <= GREY_SPREAD {
        return 0.0;
    }
    let mut distance = 0.0f32;
    if low < y {
        distance = if y > 0.0 {
            (y - low) / y
        } else if low < 0.0 {
            f32::INFINITY
        } else {
            0.0
        };
    }
    if high > y {
        let room = 1.0 - y;
        let high_distance = if room > 0.0 {
            (high - y) / room
        } else if high > 1.0 {
            f32::INFINITY
        } else {
            0.0
        };
        distance = distance.max(high_distance);
    }
    distance
}

/// The distance `distance` ([`edge_distance`]) after a knee that fits `reach` onto the edge: what
/// is closer than [`KNEE_START`] stays, the rest is squeezed linearly so that `reach` ends on 1.
/// Nothing moves when `reach` is not above 1.
pub fn knee(distance: f32, reach: f32) -> f32 {
    if reach <= 1.0 || distance <= KNEE_START {
        return distance;
    }
    KNEE_START + (1.0 - KNEE_START) * (distance - KNEE_START) / (reach - KNEE_START)
}

/// Move `light` toward its grey as [`knee`] says, so that a target volume that reaches `reach` times
/// the output's gamut edge ([`target_reach`]) ends inside it, and what is near the edge of the
/// output's own gamut is eased in instead of piling up on it. `light` keeps its luminance and
/// hue. Colors farther out than the target volume are left for [`map_gamut`]. A `reach` of 0 (or
/// anything not above 1) is no knee.
pub fn soften(light: [f32; 3], luma: [f32; 3], reach: f32) -> [f32; 3] {
    if reach <= 1.0 {
        return light;
    }
    let distance = edge_distance(light, luma);
    if !distance.is_finite() || distance <= KNEE_START {
        return light;
    }
    let y = (luma[0] * light[0] + luma[1] * light[1] + luma[2] * light[2]).clamp(0.0, 1.0);
    let scale = knee(distance, reach) / distance;
    light.map(|v| y + (v - y) * scale)
}

/// How far the target volume of `source` (its mastering primaries) reaches beyond the gamut of an
/// output, as the largest [`edge_distance`] of the target's red, green, blue, cyan, magenta and
/// yellow at full strength. `matrix` takes linear light of the source's primaries to the output's
/// (what `Conversion` carries) and `luma` is the output's Y row. 1 or less: the target fits. `None`
/// when the surface has no target volume of its own, or its primaries are no real gamut.
///
/// The target's colors are the same colors (CIE XYZ) in the container, with no adaptation even if
/// the white points differ, as the protocol says, `matrix` then moves them like every other color.
pub fn target_reach(source: &ImageDescription, matrix: &Mat3, luma: [f32; 3]) -> Option<f32> {
    let target = source.mastering_primaries.as_ref()?;
    let in_container = mul(
        &invert(&rgb_to_xyz(&source.primaries)?)?,
        &rgb_to_xyz(target)?,
    );
    let to_output = mul(matrix, &in_container);
    let corners = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
    ];
    let reach = corners
        .into_iter()
        .map(|corner| {
            let light = mul_vec(&to_output, corner);
            edge_distance(light.map(|v| v as f32), luma)
        })
        .fold(0.0f32, f32::max);
    // A corner that cannot be brought in at all would make every color infinitely far: that is no
    // target worth fitting to.
    reach.is_finite().then_some(reach)
}

/// No black point compensation: gain 1, lift 0.
const NO_BLACK: [f32; 2] = [1.0, 0.0];

/// Gain and lift that put the black of `source` on the black of `target` and leave white where it is, in light as a fraction of each one's reference white. Black is the minimum luminance of a description. `NO_BLACK` when they agree.
fn black_point_compensation(source: &Luminance, target: &Luminance) -> [f32; 2] {
    let black = |l: &Luminance| {
        let reference = l.reference.max(1) as f32;
        (l.min as f32 / MIN_LUM_SCALE as f32 / reference).clamp(0.0, 0.99)
    };
    let (from, to) = (black(source), black(target));
    if (from - to).abs() < 1e-9 {
        return NO_BLACK;
    }
    let gain = (1.0 - to) / (1.0 - from);
    [gain, to - from * gain]
}

/// How far past 1 a target volume has to reach before the knee is used: a target that only touches the edge (a profile's rounding of the same gamut) needs none.
const REACH_MARGIN: f32 = 1e-3;

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
    /// Black point compensation: linear light is multiplied by the first value and the second is added, after the matrix. `[1.0, 0.0]` for none.
    pub black: [f32; 2],
    /// The output's luminance row (Y of its red, green and blue), for [`GamutMap::Compress`].
    pub luma: [f32; 3],
    /// How far the target volume of the surface reaches beyond the output's gamut ([`target_reach`]): above 1 the colors are eased in with [`soften`] before [`GamutMap::Compress`] brings in the rest. 0 for none.
    pub reach: f32,
    /// Multiplies linear light after the matrix: turns PQ's or HLG's fraction of 10000 cd/m² into the output's white.
    pub scale: f32,
    /// The output is a linear working space (see [`Conversion::to_working`]): no gamut map and no encode.
    pub working: bool,
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
        let matrix = if source.primaries == target.primaries || intent == RenderIntent::Saturation {
            IDENTITY
        } else if intent == RenderIntent::AbsoluteNoAdaptation {
            conversion_matrix_absolute(&source.primaries, &target.primaries)?
        } else {
            conversion_matrix(&source.primaries, &target.primaries)?
        };
        let black = if intent == RenderIntent::RelativeBpc {
            black_point_compensation(&source.luminance, &target.luminance)
        } else {
            NO_BLACK
        };
        let decode = Curve::of_description(source);
        let encode = Curve::of(target.tf);
        let moves_colors = matrix.iter().enumerate().any(|(i, row)| {
            row.iter()
                .enumerate()
                .any(|(j, value)| (*value - IDENTITY[i][j]).abs() > NEAR_IDENTITY)
        });
        // PQ and HLG are absolute light. Everything else, extended linear included, is placed
        // relative to its own reference white, which an SDR output shows as its white: scRGB's
        // 1.0 (80 cd/m²) is white, what is above it clips.
        let scale = if decode.is_absolute() {
            PQ_PEAK / HDR_REFERENCE_WHITE
        } else {
            1.0
        };
        let luma = rgb_to_xyz(&target.primaries).map_or([0.2126, 0.7152, 0.0722], |m| {
            [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32]
        });
        // Only a mapping that brings colors in gradually has a use for the target volume: the
        // colorimetric intents clip, and say so.
        let map = GamutMap::of(intent);
        let reach = match map {
            GamutMap::Compress => target_reach(source, &matrix, luma)
                .filter(|reach| *reach > 1.0 + REACH_MARGIN)
                .unwrap_or(0.0),
            GamutMap::Clip => 0.0,
        };
        if !moves_colors && decode == encode && black == NO_BLACK && reach == 0.0 {
            return None;
        }
        let mut rows = [[0.0f32; 3]; 3];
        for (out, values) in rows.iter_mut().zip(matrix) {
            for (slot, value) in out.iter_mut().zip(values) {
                *slot = value as f32;
            }
        }
        Some(Self {
            rows,
            decode,
            encode,
            map,
            black,
            luma,
            reach,
            scale,
            working: false,
        })
    }

    /// Into the linear working space of an HDR output: BT.2020 primaries, 1.0 is the output's SDR white of `sdr_white` cd/m². Every surface needs it, plain sRGB too.
    pub fn to_working(source: &ImageDescription, sdr_white: f32) -> Self {
        Self::to_working_at(source, sdr_white, None)
    }

    /// [`Conversion::to_working`] for a window with its own SDR white, `window_white` cd/m² (`None` for the output's): the white of the surface's own system is shown at that level instead of at `sdr_white`. That is 1.0 for sRGB and the other relative curves, the description's reference white for PQ and HLG (203 cd/m² unless it says otherwise), and 1.0 (80 cd/m²) for scRGB. Light above it goes up and down with it, so the window is brighter or dimmer as a whole.
    pub fn to_working_at(
        source: &ImageDescription,
        sdr_white: f32,
        window_white: Option<f32>,
    ) -> Self {
        let bt2020 = NamedPrimaries::Bt2020.chromaticities();
        let matrix = if source.primaries == bt2020 {
            IDENTITY
        } else {
            conversion_matrix(&source.primaries, &bt2020).unwrap_or(SRGB_TO_BT2020)
        };
        let decode = Curve::of_description(source);
        // How far the window's own white moves the white of an absolute or scRGB surface: its
        // reference white is shown at `window_white`, so 1.0 when there is none.
        let own_gain = window_white.map_or(1.0, |white| {
            white.max(1.0) / source.luminance.reference.max(1) as f32
        });
        let mut rows = [[0.0f32; 3]; 3];
        for (out, values) in rows.iter_mut().zip(matrix) {
            for (slot, value) in out.iter_mut().zip(values) {
                *slot = value as f32;
            }
        }
        Self {
            rows,
            decode,
            encode: Curve::Power(1.0),
            map: GamutMap::Clip,
            black: NO_BLACK,
            luma: [0.2627, 0.6780, 0.0593],
            reach: 0.0,
            scale: if decode.is_absolute() {
                PQ_PEAK / sdr_white.max(1.0) * own_gain
            } else if decode == Curve::Linear {
                // scRGB's 1.0 is its reference white (80 cd/m²), the working space's 1.0 is
                // the monitor's SDR white: 1.0 in scRGB is 80 / sdr_white there, so with the
                // usual 203 cd/m² SDR white a game's HDR highlights keep their real brightness
                // and its SDR white is not pushed up to the desktop's.
                source.luminance.reference as f32 / sdr_white.max(1.0) * own_gain
            } else {
                // The relative curves: 1.0 is the surface's white, which is the output's.
                window_white.map_or(1.0, |white| white.max(1.0) / sdr_white.max(1.0))
            },
            working: true,
        }
    }

    /// The shader on one color: straight (not premultiplied) values of the surface in, values
    /// for the output out.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let light = self.decode.decode_rgb(rgb);
        let mut mapped = [0.0f32; 3];
        for (value, row) in mapped.iter_mut().zip(self.rows) {
            *value = (row[0] * light[0] + row[1] * light[1] + row[2] * light[2]) * self.scale;
        }
        if self.working {
            return mapped.map(|v| v.max(0.0));
        }
        let [gain, lift] = self.black;
        let mapped = mapped.map(|v| v * gain + lift);
        let mapped = match self.map {
            GamutMap::Compress => soften(mapped, self.luma, self.reach),
            GamutMap::Clip => mapped,
        };
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
        self.reach.to_bits().hash(&mut hasher);
        for value in self.black {
            value.to_bits().hash(&mut hasher);
        }
        self.scale.to_bits().hash(&mut hasher);
        self.working.hash(&mut hasher);
        for value in self
            .decode
            .shader_params()
            .into_iter()
            .chain(self.encode.shader_params())
            .chain(self.decode.shader_luma())
        {
            value.to_bits().hash(&mut hasher);
        }
        hasher.finish() as i32
    }
}
