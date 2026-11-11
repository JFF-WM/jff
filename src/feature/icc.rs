//! Reading the calibration curves out of an ICC monitor profile.
//!
//! Calibration tools (DisplayCAL, ArgyllCMS, colord, vendor tools) put the correction a monitor
//! needs in the `vcgt` tag: one curve per channel. We hand those to the display hardware (see
//! `set_gamma` in `tty.rs`), so the whole monitor is corrected with no work in the renderer.
//!
//! Vendor profiles usually have no `vcgt`, they describe the panel with `rTRC` / `gTRC` /
//! `bTRC`. For those, we build the table that makes the panel follow sRGB: each sRGB value
//! mapped to the code the panel needs for that much light. Only the tone curve is handled.
//! Primaries (gamut conversion) need a colour pipeline the GPU may not have, so they're ignored.
//!
//! A profile that has neither but describes the panel with lookup tables (`A2B` / `B2A`) gets
//! the same treatment from a fit of the tables (see [`lut`]): the curves of each channel, and
//! for an `A2B` the colorants too. The tables themselves are not applied pixel by pixel.
//!
//! A profile with none of those is an error.
//!
//! [`description`] reads the same profile for `wp_color_management_v1`: what a client is told
//! the panel looks like once those curves are applied (its primaries, tone curve and white).

pub mod lut;

use std::path::Path;

use anyhow::{bail, Context};

use crate::primitives::color::{
    Chromaticities, ImageDescription, Luminance, NamedTf, TransferFunction, CHROMA_SCALE,
    EXPONENT_SCALE,
};

/// The three correction curves. Each channel has at least two entries, darkest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GammaTable {
    pub red: Vec<u16>,
    pub green: Vec<u16>,
    pub blue: Vec<u16>,
    pub kind: Kind,
}

/// Where the curves came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Stored in the profile (`vcgt`).
    Vcgt,
    /// Built from the panel's tone curves (`rTRC` / `gTRC` / `bTRC`).
    ToneCurves,
    /// Set by a client (wlr-gamma-control).
    Client,
}

impl GammaTable {
    /// Words for the log.
    pub fn describe(&self) -> &'static str {
        match self.kind {
            Kind::Vcgt => "calibration curves (vcgt)",
            Kind::ToneCurves => "panel tone curves (TRC)",
            Kind::Client => "curves set by a client",
        }
    }

    /// The curves as a table of `size` entries for the hardware, linearly interpolated. The
    /// ends stay exactly what the profile has.
    pub fn resampled(&self, size: usize) -> Vec<[u16; 3]> {
        let red = resample(&self.red, size);
        let green = resample(&self.green, size);
        let blue = resample(&self.blue, size);
        (0..size).map(|i| [red[i], green[i], blue[i]]).collect()
    }
}

fn resample(curve: &[u16], size: usize) -> Vec<u16> {
    if size == 1 {
        return vec![curve[0]];
    }
    (0..size)
        .map(|i| {
            let pos = i as f64 * (curve.len() - 1) as f64 / (size - 1) as f64;
            let low = pos.floor() as usize;
            let high = (low + 1).min(curve.len() - 1);
            let frac = pos - low as f64;
            let value = curve[low] as f64 * (1.0 - frac) + curve[high] as f64 * frac;
            value.round() as u16
        })
        .collect()
}

/// Read the calibration curves of the ICC profile at `path`.
pub fn load(path: &Path) -> anyhow::Result<GammaTable> {
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&data)
}

/// `vcgt` if the profile has one, otherwise built from the tone response curves.
pub fn parse(data: &[u8]) -> anyhow::Result<GammaTable> {
    // 128-byte header, "acsp" at offset 36, then tag count + tag table.
    if data.len() < 132 || &data[36..40] != b"acsp" {
        bail!("not an ICC profile");
    }
    let count = be32(data, 128).context("truncated profile")? as usize;
    let tag = |sig: &[u8; 4]| -> Option<&[u8]> {
        (0..count.min(1000)).find_map(|i| {
            let entry = 132 + 12 * i;
            if data.get(entry..entry + 4)? != sig {
                return None;
            }
            let offset = be32(data, entry + 4)? as usize;
            let size = be32(data, entry + 8)? as usize;
            data.get(offset..offset.checked_add(size)?)
        })
    };

    if let Some(vcgt) = tag(b"vcgt") {
        return parse_vcgt(vcgt);
    }
    if let (Some(red), Some(green), Some(blue)) = (tag(b"rTRC"), tag(b"gTRC"), tag(b"bTRC")) {
        return Ok(GammaTable {
            red: correction(&parse_trc(red).context("red tone curve")?),
            green: correction(&parse_trc(green).context("green tone curve")?),
            blue: correction(&parse_trc(blue).context("blue tone curve")?),
            kind: Kind::ToneCurves,
        });
    }
    // A profile that describes the panel with lookup tables only: the tone curves fitted to them.
    if let Some(model) = lut::panel_model(data) {
        let [red, green, blue] = model.trcs();
        return Ok(GammaTable {
            red: correction(&red),
            green: correction(&green),
            blue: correction(&blue),
            kind: Kind::ToneCurves,
        });
    }
    bail!("the profile has neither vcgt calibration curves, tone curves nor lookup tables")
}

/// A panel tone response curve: code value (0..=1) to light (0..=1).
#[derive(Clone, Debug)]
enum Trc {
    /// `curv`, tabulated.
    Table(Vec<f64>),
    /// `curv`, single gamma (or none: linear).
    Gamma(f64),
    /// `para`: type (0..=4) and its g/a/b/c/d/e/f parameters.
    Parametric(u16, [f64; 7]),
}

impl Trc {
    fn eval(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Trc::Gamma(g) => x.powf(*g),
            Trc::Table(table) => {
                let pos = x * (table.len() - 1) as f64;
                let low = pos.floor() as usize;
                let high = (low + 1).min(table.len() - 1);
                table[low] + (table[high] - table[low]) * (pos - low as f64)
            }
            Trc::Parametric(kind, [g, a, b, c, d, e, f]) => {
                let pow = |v: f64| if v <= 0.0 { 0.0 } else { v.powf(*g) };
                let knee = if *a != 0.0 { -b / a } else { f64::INFINITY };
                match kind {
                    0 => x.powf(*g),
                    1 => {
                        if x >= knee {
                            pow(a * x + b)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if x >= knee {
                            pow(a * x + b) + c
                        } else {
                            *c
                        }
                    }
                    3 => {
                        if x >= *d {
                            pow(a * x + b)
                        } else {
                            c * x
                        }
                    }
                    _ => {
                        if x >= *d {
                            pow(a * x + b) + e
                        } else {
                            c * x + f
                        }
                    }
                }
            }
        }
    }
}

fn parse_trc(tag: &[u8]) -> anyhow::Result<Trc> {
    if tag.len() < 12 {
        bail!("truncated tone curve");
    }
    match &tag[0..4] {
        b"curv" => {
            let count = be32(tag, 8).unwrap() as usize;
            match count {
                0 => Ok(Trc::Gamma(1.0)),
                // u8Fixed8: 2.2 is stored as 0x0233.
                1 => Ok(Trc::Gamma(
                    be16(tag, 12).context("truncated tone curve")? as f64 / 256.0,
                )),
                _ => {
                    if count > 65536 || tag.len() < 12 + 2 * count {
                        bail!("truncated tone curve");
                    }
                    Ok(Trc::Table(
                        (0..count)
                            .map(|i| be16(tag, 12 + 2 * i).unwrap() as f64 / 65535.0)
                            .collect(),
                    ))
                }
            }
        }
        b"para" => {
            let kind = be16(tag, 8).unwrap();
            let count = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                other => bail!("unknown parametric curve type {other}"),
            };
            if tag.len() < 12 + 4 * count {
                bail!("truncated tone curve");
            }
            let mut params = [0.0; 7];
            for (i, param) in params.iter_mut().enumerate().take(count) {
                *param = be32(tag, 12 + 4 * i).unwrap() as i32 as f64 / 65536.0;
            }
            Ok(Trc::Parametric(kind, params))
        }
        _ => bail!("unknown tone curve type"),
    }
}

/// The sRGB curve: encoded value to light.
pub fn srgb_to_light(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Build the table that makes a panel with response `trc` follow sRGB: each sRGB value mapped
/// to the code the panel needs for that much light.
fn correction(trc: &Trc) -> Vec<u16> {
    const SIZE: usize = 1024;
    let step = |i: usize| i as f64 / (SIZE - 1) as f64;
    // What the panel does for each code, forced monotonic so we can search it.
    let mut panel: Vec<f64> = (0..SIZE).map(|i| trc.eval(step(i))).collect();
    for i in 1..SIZE {
        panel[i] = panel[i].max(panel[i - 1]);
    }
    (0..SIZE)
        .map(|i| {
            let wanted = srgb_to_light(step(i));
            let at = panel.partition_point(|&light| light < wanted);
            let code = if at == 0 {
                0.0
            } else if at >= SIZE {
                1.0
            } else {
                let (low, high) = (panel[at - 1], panel[at]);
                let frac = if high > low {
                    (wanted - low) / (high - low)
                } else {
                    0.0
                };
                ((at - 1) as f64 + frac) / (SIZE - 1) as f64
            };
            (code * 65535.0).round() as u16
        })
        .collect()
}

fn parse_vcgt(tag: &[u8]) -> anyhow::Result<GammaTable> {
    if tag.len() < 12 || &tag[0..4] != b"vcgt" {
        bail!("malformed vcgt tag");
    }
    match be32(tag, 8).context("malformed vcgt tag")? {
        // Table: channels, entries per channel, bytes per entry, then the values.
        0 => {
            let (Some(channels), Some(entries), Some(entry_size)) =
                (be16(tag, 12), be16(tag, 14), be16(tag, 16))
            else {
                bail!("truncated vcgt table");
            };
            let (channels, entries, entry_size) =
                (channels as usize, entries as usize, entry_size as usize);
            if !(channels == 1 || channels == 3) || !(entry_size == 1 || entry_size == 2) {
                bail!("unsupported vcgt table ({channels} channels, {entry_size} bytes each)");
            }
            if entries < 2 {
                bail!("the vcgt table is too short");
            }
            let end = 18 + channels * entries * entry_size;
            if tag.len() < end {
                bail!("truncated vcgt table");
            }
            let curve = |channel: usize| -> Vec<u16> {
                (0..entries)
                    .map(|i| {
                        let at = 18 + (channel * entries + i) * entry_size;
                        if entry_size == 2 {
                            u16::from_be_bytes([tag[at], tag[at + 1]])
                        } else {
                            tag[at] as u16 * 257
                        }
                    })
                    .collect()
            };
            let red = curve(0);
            let (green, blue) = if channels == 3 {
                (curve(1), curve(2))
            } else {
                (red.clone(), red.clone())
            };
            Ok(GammaTable {
                red,
                green,
                blue,
                kind: Kind::Vcgt,
            })
        }
        // Formula: value = min + (max - min) * input ^ gamma, per channel.
        1 => {
            if tag.len() < 12 + 9 * 4 {
                bail!("truncated vcgt formula");
            }
            let fixed = |index: usize| be32(tag, 12 + index * 4).unwrap() as i32 as f64 / 65536.0;
            let curve = |channel: usize| -> Vec<u16> {
                let (gamma, min, max) = (
                    fixed(channel * 3),
                    fixed(channel * 3 + 1),
                    fixed(channel * 3 + 2),
                );
                (0..256)
                    .map(|i| {
                        let x = i as f64 / 255.0;
                        let value = (min + (max - min) * x.powf(gamma)).clamp(0.0, 1.0);
                        (value * 65535.0).round() as u16
                    })
                    .collect()
            };
            Ok(GammaTable {
                red: curve(0),
                green: curve(1),
                blue: curve(2),
                kind: Kind::Vcgt,
            })
        }
        other => bail!("unknown vcgt type {other}"),
    }
}

// ---- what the profile says about the panel, for wp_color_management_v1 ---------------------

/// The 3x3 matrix type of the helpers below, row-major.
type Mat = [[f64; 3]; 3];

/// Bradford cone response matrix, what ICC profiles adapt white points with.
const BRADFORD: Mat = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// The ICC profile connection space white (D50), as XYZ.
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// D65, as XYZ. Where a profile does not say what white its panel has, this is assumed.
const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

/// Read the profile at `path` as the image description of the monitor it calibrates.
pub fn load_description(path: &Path) -> anyhow::Result<ImageDescription> {
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    description(&data)
}

/// What clients are told about a monitor whose profile is `data`, as it looks with the curves
/// of [`parse`] applied.
///
/// - Primaries and white come from `rXYZ` / `gXYZ` / `bXYZ`. Profiles store them adapted to D50,
///   `chad` is the matrix that did it, so the panel's own values are got by undoing it. Without
///   a `chad` the panel's white is taken to be D65, which nearly every monitor is set to.
/// - The tone curve is sRGB when the correction was built from the panel's curves (that is what
///   it makes the panel follow). With a `vcgt` the profile's curves describe the calibrated
///   panel, they become the named curve closest to them, or a power curve.
/// - `lumi` gives the white's luminance, when the profile has it.
///
/// What the profile does not say stays as sRGB has it. Only a file that is not an ICC profile
/// is an error.
pub fn description(data: &[u8]) -> anyhow::Result<ImageDescription> {
    if data.len() < 132 || &data[36..40] != b"acsp" {
        bail!("not an ICC profile");
    }
    let mut description = ImageDescription::srgb();
    if let Some(primaries) = panel_primaries(data) {
        description.primaries = primaries;
    }
    description.tf = panel_tf(data);
    if let Some(max) = panel_max_luminance(data) {
        description.luminance = Luminance {
            min: description.luminance.min,
            max,
            reference: max,
        };
    }
    Ok(description)
}

/// The tag `sig` of the profile.
fn find_tag<'a>(data: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
    let count = be32(data, 128)? as usize;
    (0..count.min(1000)).find_map(|i| {
        let entry = 132 + 12 * i;
        if data.get(entry..entry + 4)? != sig {
            return None;
        }
        let offset = be32(data, entry + 4)? as usize;
        let size = be32(data, entry + 8)? as usize;
        data.get(offset..offset.checked_add(size)?)
    })
}

/// A signed 15.16 fixed point number.
fn s15(data: &[u8], at: usize) -> Option<f64> {
    Some(be32(data, at)? as i32 as f64 / 65536.0)
}

/// An `XYZ ` tag with its one value.
fn xyz(tag: &[u8]) -> Option<[f64; 3]> {
    if tag.get(0..4)? != b"XYZ " {
        return None;
    }
    Some([s15(tag, 8)?, s15(tag, 12)?, s15(tag, 16)?])
}

/// A `chad` tag (`sf32`): nine numbers, row by row.
fn chad(tag: &[u8]) -> Option<Mat> {
    if tag.get(0..4)? != b"sf32" {
        return None;
    }
    let mut matrix = [[0.0; 3]; 3];
    for (row, values) in matrix.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = s15(tag, 8 + 4 * (row * 3 + column))?;
        }
    }
    Some(matrix)
}

fn mul(m: &Mat, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [[0.0; 3]; 3];
    for (row, values) in out.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

/// `None` for a matrix that can't be turned around.
fn invert(m: &Mat) -> Option<Mat> {
    let cofactor =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * cofactor(1, 2, 1, 2) - m[0][1] * cofactor(1, 2, 0, 2)
        + m[0][2] * cofactor(1, 2, 0, 1);
    if det.abs() < 1e-12 {
        return None;
    }
    Some([
        [
            cofactor(1, 2, 1, 2) / det,
            -cofactor(0, 2, 1, 2) / det,
            cofactor(0, 1, 1, 2) / det,
        ],
        [
            -cofactor(1, 2, 0, 2) / det,
            cofactor(0, 2, 0, 2) / det,
            -cofactor(0, 1, 0, 2) / det,
        ],
        [
            cofactor(1, 2, 0, 1) / det,
            -cofactor(0, 2, 0, 1) / det,
            cofactor(0, 1, 0, 1) / det,
        ],
    ])
}

/// The Bradford matrix that moves colors seen under white `from` to white `to`.
fn adaptation(from: [f64; 3], to: [f64; 3]) -> Option<Mat> {
    let cone_from = mul(&BRADFORD, from);
    let cone_to = mul(&BRADFORD, to);
    let scale = [
        [cone_to[0] / cone_from[0], 0.0, 0.0],
        [0.0, cone_to[1] / cone_from[1], 0.0],
        [0.0, 0.0, cone_to[2] / cone_from[2]],
    ];
    Some(mat_mul(&invert(&BRADFORD)?, &mat_mul(&scale, &BRADFORD)))
}

/// CIE 1931 xy of an XYZ color, as it goes on the wire.
fn chromaticity(v: [f64; 3]) -> Option<[i32; 2]> {
    let sum = v[0] + v[1] + v[2];
    if !sum.is_finite() || sum <= 0.0 {
        return None;
    }
    let scale = CHROMA_SCALE as f64;
    Some([
        (v[0] / sum * scale).round() as i32,
        (v[1] / sum * scale).round() as i32,
    ])
}

/// Whether eight chromaticity numbers can be a real gamut: all inside the xy plane, and the
/// corners spread out into a triangle.
fn plausible(c: &Chromaticities) -> bool {
    let inside = |p: [i32; 2]| p.iter().all(|&v| v > 0 && v < CHROMA_SCALE);
    if ![c.red, c.green, c.blue, c.white].into_iter().all(inside) {
        return false;
    }
    let scale = CHROMA_SCALE as f64;
    let at = |p: [i32; 2]| (p[0] as f64 / scale, p[1] as f64 / scale);
    let (r, g, b) = (at(c.red), at(c.green), at(c.blue));
    let area = ((g.0 - r.0) * (b.1 - r.1) - (b.0 - r.0) * (g.1 - r.1)).abs() / 2.0;
    area > 0.02
}

/// The panel's primaries and white, `None` when the profile has no colorants or they make no
/// sense.
fn panel_primaries(data: &[u8]) -> Option<Chromaticities> {
    let tagged = || -> Option<[[f64; 3]; 3]> {
        Some([
            xyz(find_tag(data, b"rXYZ")?)?,
            xyz(find_tag(data, b"gXYZ")?)?,
            xyz(find_tag(data, b"bXYZ")?)?,
        ])
    };
    // Without colorant tags, the colorants of a fitted lookup table stand in.
    let [red, green, blue] = tagged().or_else(|| lut::panel_model(data)?.colorants)?;
    // The colorants are relative to D50. `chad` took the panel's white there, so its inverse
    // brings them back.
    let to_panel = match find_tag(data, b"chad").and_then(chad) {
        Some(matrix) => invert(&matrix)?,
        None => adaptation(D50, D65)?,
    };
    let primaries = Chromaticities {
        red: chromaticity(mul(&to_panel, red))?,
        green: chromaticity(mul(&to_panel, green))?,
        blue: chromaticity(mul(&to_panel, blue))?,
        white: chromaticity(mul(&to_panel, D50))?,
    };
    plausible(&primaries).then_some(primaries)
}

/// The largest gap between a panel curve and `curve`, over the whole range.
fn worst_gap(trc: &Trc, curve: fn(f64) -> f64) -> f64 {
    const STEPS: usize = 32;
    (1..=STEPS)
        .map(|i| {
            let x = i as f64 / STEPS as f64;
            (trc.eval(x) - curve(x)).abs()
        })
        .fold(0.0, f64::max)
}

/// The transfer function of the panel with the correction of [`parse`] applied.
fn panel_tf(data: &[u8]) -> TransferFunction {
    let srgb = TransferFunction::Named(NamedTf::Srgb);
    // Built from the panel's curves, the correction makes it follow sRGB. With a `vcgt` the
    // panel was calibrated to the curves the profile describes.
    if find_tag(data, b"vcgt").is_none() {
        return srgb;
    }
    let Some(trc) = find_tag(data, b"gTRC").and_then(|tag| parse_trc(tag).ok()) else {
        return srgb;
    };
    let fits = [
        (worst_gap(&trc, srgb_to_light), srgb),
        (
            worst_gap(&trc, |x| x.powf(2.2)),
            TransferFunction::Named(NamedTf::Gamma22),
        ),
        (
            worst_gap(&trc, |x| x.powf(2.8)),
            TransferFunction::Named(NamedTf::Gamma28),
        ),
    ];
    let mut best = fits[0];
    for fit in fits {
        if fit.0 < best.0 {
            best = fit;
        }
    }
    if best.0 < 0.01 {
        return best.1;
    }
    // Not close to a named curve: the power that gives the same light at half the code value.
    let half = trc.eval(0.5);
    if half > 0.0 && half < 1.0 {
        let exponent = (half.ln() / 0.5f64.ln()).clamp(1.0, 10.0);
        return TransferFunction::Power((exponent * EXPONENT_SCALE as f64).round() as u32);
    }
    srgb
}

/// The luminance of the panel's white in cd/m², from `lumi`, if it is a believable one.
fn panel_max_luminance(data: &[u8]) -> Option<u32> {
    let y = xyz(find_tag(data, b"lumi")?)?[1];
    (10.0..=10_000.0).contains(&y).then(|| y.round() as u32)
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}
