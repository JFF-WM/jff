//! Lookup-table profiles: what a monitor profile that describes the panel with `A2B0` / `A2B1`
//! (device to PCS) or `B2A0` / `B2A1` (PCS to device) tables says about it.
//!
//! Many vendor and measured profiles carry only tables, no `rXYZ` / `rTRC`. The tables are read
//! here in full (`lut8`, `lut16`, `lutAToB` and `lutBToA`, three channels in and out, with
//! trilinear interpolation of the CLUT), and then *fitted* by what the rest of jff understands:
//! three colorants and one tone curve per channel (see [`panel_model`]). Nothing here reaches
//! the renderer as a table: the fit is what drives the hardware curves and the primaries that
//! clients and the conversion shader see. How well the fit matches the tables is in
//! [`PanelModel::worst_error`].

use super::{be16, be32, find_tag, parse_trc, s15, Trc, D50};

/// What a table's PCS side holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pcs {
    Xyz,
    Lab,
}

/// One step of a table pipeline.
#[derive(Clone, Debug)]
enum Stage {
    /// One curve per channel.
    Curves(Vec<Trc>),
    /// 3x3 matrix then offset.
    Matrix([[f64; 3]; 3], [f64; 3]),
    /// A 3 to 3 table with trilinear interpolation. Values are 0 to 1.
    Clut(Clut),
}

#[derive(Clone, Debug)]
struct Clut {
    grid: [usize; 3],
    /// First input channel varies slowest. Three values per node.
    nodes: Vec<f64>,
}

impl Clut {
    fn eval(&self, input: [f64; 3]) -> [f64; 3] {
        let mut low = [0usize; 3];
        let mut frac = [0.0f64; 3];
        for i in 0..3 {
            // Every grid has at least two points, so `last` is at least 1.
            let last = self.grid[i] - 1;
            let pos = input[i].clamp(0.0, 1.0) * last as f64;
            low[i] = (pos.floor() as usize).min(last - 1);
            frac[i] = pos - low[i] as f64;
        }
        let node = |a: usize, b: usize, c: usize| -> [f64; 3] {
            let at = ((a * self.grid[1] + b) * self.grid[2] + c) * 3;
            [self.nodes[at], self.nodes[at + 1], self.nodes[at + 2]]
        };
        let step = |i: usize, d: usize| (low[i] + d).min(self.grid[i] - 1);
        let mut out = [0.0; 3];
        for da in 0..2 {
            for db in 0..2 {
                for dc in 0..2 {
                    let weight = (if da == 1 { frac[0] } else { 1.0 - frac[0] })
                        * (if db == 1 { frac[1] } else { 1.0 - frac[1] })
                        * (if dc == 1 { frac[2] } else { 1.0 - frac[2] });
                    if weight == 0.0 {
                        continue;
                    }
                    let value = node(step(0, da), step(1, db), step(2, dc));
                    for (slot, v) in out.iter_mut().zip(value) {
                        *slot += weight * v;
                    }
                }
            }
        }
        out
    }
}

/// A table from a profile, as a pipeline from 0..1 values to 0..1 values.
#[derive(Clone, Debug)]
pub struct Lut {
    stages: Vec<Stage>,
    /// 16-bit Lab on the PCS side uses the old v2 encoding (`lut16`).
    lab_legacy: bool,
}

impl Lut {
    /// Run the table: three encoded values in, three out.
    pub fn eval(&self, input: [f64; 3]) -> [f64; 3] {
        let mut v = input;
        for stage in &self.stages {
            v = match stage {
                Stage::Curves(curves) => [
                    curves[0].eval(v[0]),
                    curves[1].eval(v[1]),
                    curves[2].eval(v[2]),
                ],
                Stage::Matrix(m, offset) => [
                    m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2] + offset[0],
                    m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2] + offset[1],
                    m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2] + offset[2],
                ],
                Stage::Clut(clut) => clut.eval(v),
            };
        }
        v.map(|x| x.clamp(0.0, 1.0))
    }

    /// XYZ (relative to D50) of a PCS-side value the table produced.
    pub fn pcs_to_xyz(&self, pcs: Pcs, encoded: [f64; 3]) -> [f64; 3] {
        match pcs {
            Pcs::Xyz => encoded.map(|v| v * (65535.0 / 32768.0)),
            Pcs::Lab => {
                let (l, a, b) = if self.lab_legacy {
                    (
                        encoded[0] * 65535.0 / 65280.0 * 100.0,
                        encoded[1] * 65535.0 / 256.0 - 128.0,
                        encoded[2] * 65535.0 / 256.0 - 128.0,
                    )
                } else {
                    (
                        encoded[0] * 100.0,
                        encoded[1] * 255.0 - 128.0,
                        encoded[2] * 255.0 - 128.0,
                    )
                };
                let fy = (l + 16.0) / 116.0;
                let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
                let inverse = |t: f64| {
                    const DELTA: f64 = 6.0 / 29.0;
                    if t > DELTA {
                        t * t * t
                    } else {
                        3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
                    }
                };
                [
                    D50[0] * inverse(fx),
                    D50[1] * inverse(fy),
                    D50[2] * inverse(fz),
                ]
            }
        }
    }

    /// The PCS-side value (0..1 encoded) of XYZ relative to D50: the input of a `B2A` table.
    pub fn xyz_to_pcs(&self, pcs: Pcs, xyz: [f64; 3]) -> [f64; 3] {
        match pcs {
            Pcs::Xyz => xyz.map(|v| (v * 32768.0 / 65535.0).clamp(0.0, 1.0)),
            Pcs::Lab => {
                let f = |t: f64| {
                    const DELTA: f64 = 6.0 / 29.0;
                    if t > DELTA * DELTA * DELTA {
                        t.cbrt()
                    } else {
                        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
                    }
                };
                let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
                let (l, a, b) = (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
                let encoded = if self.lab_legacy {
                    [
                        l / 100.0 * 65280.0 / 65535.0,
                        (a + 128.0) * 256.0 / 65535.0,
                        (b + 128.0) * 256.0 / 65535.0,
                    ]
                } else {
                    [l / 100.0, (a + 128.0) / 255.0, (b + 128.0) / 255.0]
                };
                encoded.map(|v| v.clamp(0.0, 1.0))
            }
        }
    }
}

/// The PCS the profile header names. `None` for anything but XYZ and Lab.
pub fn pcs_of(data: &[u8]) -> Option<Pcs> {
    match data.get(20..24)? {
        b"XYZ " => Some(Pcs::Xyz),
        b"Lab " => Some(Pcs::Lab),
        _ => None,
    }
}

/// The table in the tag `sig` (`A2B1`, `B2A0`, ...), `None` when the profile has no such tag or
/// it is not a three-in, three-out table this reader understands.
pub fn read_tag(data: &[u8], sig: &[u8; 4]) -> Option<Lut> {
    let tag = find_tag(data, sig)?;
    match tag.get(0..4)? {
        b"mft1" => read_legacy(tag, 1),
        b"mft2" => read_legacy(tag, 2),
        b"mAB " => read_multi(tag, true),
        b"mBA " => read_multi(tag, false),
        _ => None,
    }
}

/// `lut8` (`width` 1) and `lut16` (`width` 2).
fn read_legacy(tag: &[u8], width: usize) -> Option<Lut> {
    if *tag.get(8)? != 3 || *tag.get(9)? != 3 {
        return None;
    }
    let grid = *tag.get(10)? as usize;
    if grid < 2 {
        return None;
    }
    let (input_entries, output_entries, mut at) = if width == 2 {
        (be16(tag, 48)? as usize, be16(tag, 50)? as usize, 52)
    } else {
        (256, 256, 48)
    };
    if !(2..=4096).contains(&input_entries) || !(2..=4096).contains(&output_entries) {
        return None;
    }
    let value = |at: usize| -> Option<f64> {
        if width == 2 {
            Some(be16(tag, at)? as f64 / 65535.0)
        } else {
            Some(*tag.get(at)? as f64 / 255.0)
        }
    };
    let tables = |count: usize, entries: usize, at: &mut usize| -> Option<Vec<Trc>> {
        let mut curves = Vec::new();
        for _ in 0..count {
            let mut table = Vec::with_capacity(entries);
            for i in 0..entries {
                table.push(value(*at + i * width)?);
            }
            *at += entries * width;
            curves.push(Trc::Table(table));
        }
        Some(curves)
    };
    let input = tables(3, input_entries, &mut at)?;
    let count = grid.checked_pow(3)?.checked_mul(3)?;
    if count > 8_000_000 {
        return None;
    }
    let mut nodes = Vec::with_capacity(count);
    for i in 0..count {
        nodes.push(value(at + i * width)?);
    }
    at += count * width;
    let output = tables(3, output_entries, &mut at)?;
    Some(Lut {
        stages: vec![
            Stage::Curves(input),
            Stage::Clut(Clut {
                grid: [grid; 3],
                nodes,
            }),
            Stage::Curves(output),
        ],
        lab_legacy: width == 2,
    })
}

/// How many bytes the curve (`curv` or `para`) at the start of `data` takes.
fn curve_len(data: &[u8]) -> Option<usize> {
    match data.get(0..4)? {
        b"curv" => Some(12 + 2 * be32(data, 8)? as usize),
        b"para" => {
            let count = match be16(data, 8)? {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            Some(12 + 4 * count)
        }
        _ => None,
    }
}

/// `count` curves in a row, each starting on a 4-byte boundary.
fn curves_at(tag: &[u8], offset: usize, count: usize) -> Option<Vec<Trc>> {
    let mut at = offset;
    let mut curves = Vec::new();
    for _ in 0..count {
        let rest = tag.get(at..)?;
        let len = curve_len(rest)?;
        curves.push(parse_trc(rest.get(..len)?).ok()?);
        at += (len + 3) & !3;
    }
    Some(curves)
}

/// `lutAToB` (`a_to_b`) and `lutBToA`.
fn read_multi(tag: &[u8], a_to_b: bool) -> Option<Lut> {
    if *tag.get(8)? != 3 || *tag.get(9)? != 3 {
        return None;
    }
    let offset = |at: usize| be32(tag, at).map(|v| v as usize).filter(|v| *v != 0);
    let (b_off, matrix_off, m_off, clut_off, a_off) =
        (offset(12), offset(16), offset(20), offset(24), offset(28));

    let stage = |at: Option<usize>| -> Option<Option<Stage>> {
        match at {
            None => Some(None),
            Some(o) => Some(Some(Stage::Curves(curves_at(tag, o, 3)?))),
        }
    };
    let (b, m, a) = (stage(b_off)?, stage(m_off)?, stage(a_off)?);
    let matrix = match matrix_off {
        Some(o) => {
            let mut values = [0.0; 12];
            for (i, value) in values.iter_mut().enumerate() {
                *value = s15(tag, o + 4 * i)?;
            }
            Some(Stage::Matrix(
                [
                    [values[0], values[1], values[2]],
                    [values[3], values[4], values[5]],
                    [values[6], values[7], values[8]],
                ],
                [values[9], values[10], values[11]],
            ))
        }
        None => None,
    };
    let clut = match clut_off {
        Some(o) => {
            let grid = [
                *tag.get(o)? as usize,
                *tag.get(o + 1)? as usize,
                *tag.get(o + 2)? as usize,
            ];
            if grid.iter().any(|g| *g < 2) {
                return None;
            }
            let precision = *tag.get(o + 16)? as usize;
            if precision != 1 && precision != 2 {
                return None;
            }
            let count = grid[0]
                .checked_mul(grid[1])?
                .checked_mul(grid[2])?
                .checked_mul(3)?;
            if count > 8_000_000 {
                return None;
            }
            let base = o + 20;
            let mut nodes = Vec::with_capacity(count);
            for i in 0..count {
                nodes.push(if precision == 2 {
                    be16(tag, base + 2 * i)? as f64 / 65535.0
                } else {
                    *tag.get(base + i)? as f64 / 255.0
                });
            }
            Some(Stage::Clut(Clut { grid, nodes }))
        }
        None => None,
    };
    // Order of the stages as the spec runs them.
    let stages: Vec<Option<Stage>> = if a_to_b {
        vec![a, clut, m, matrix, b]
    } else {
        vec![b, matrix, m, clut, a]
    };
    let stages: Vec<Stage> = stages.into_iter().flatten().collect();
    if stages.is_empty() {
        return None;
    }
    Some(Lut {
        stages,
        lab_legacy: false,
    })
}

/// What a table profile is reduced to: the light each colorant gives and the tone curve of each
/// channel. The tables themselves are richer (they bend hues, and treat the corners of the gamut
/// their own way), [`PanelModel::worst_error`] says by how much.
#[derive(Clone, Debug)]
pub struct PanelModel {
    /// XYZ (D50) of the red, green and blue colorants at full drive, black subtracted. `None`
    /// from a table that goes the other way (`B2A`): its colorants would have to be found by
    /// inverting it.
    pub colorants: Option<[[f64; 3]; 3]>,
    /// Number of entries of each curve in `curves`.
    pub curve_size: usize,
    /// For each channel, the share of the colorant's light (0..1) at `curve_size` evenly spaced
    /// drive values from 0 to 1, rising.
    pub curves: [Vec<f64>; 3],
    /// Largest distance between the table and the fitted model over a 5x5x5 grid of drive
    /// values (A2B only, `None` for B2A), in XYZ units of the white's luminance.
    pub worst_error: Option<f64>,
}

/// Number of samples of each fitted curve.
const CURVE_SIZE: usize = 256;

/// Fit the profile's tables. `A2B1` (relative colorimetric) is preferred to `A2B0`, then
/// `B2A1` to `B2A0` when the profile has no `A2B`. `None` when the profile has none of them, or
/// they are not of a kind this reader understands.
pub fn panel_model(data: &[u8]) -> Option<PanelModel> {
    let pcs = pcs_of(data)?;
    if let Some(lut) = read_tag(data, b"A2B1").or_else(|| read_tag(data, b"A2B0")) {
        return Some(fit_a2b(&lut, pcs));
    }
    let lut = read_tag(data, b"B2A1").or_else(|| read_tag(data, b"B2A0"))?;
    fit_b2a(&lut, pcs)
}

fn fit_a2b(lut: &Lut, pcs: Pcs) -> PanelModel {
    let xyz = |rgb: [f64; 3]| lut.pcs_to_xyz(pcs, lut.eval(rgb));
    let black = xyz([0.0; 3]);
    let mut colorants = [[0.0; 3]; 3];
    for (channel, colorant) in colorants.iter_mut().enumerate() {
        let mut drive = [0.0; 3];
        drive[channel] = 1.0;
        let full = xyz(drive);
        for i in 0..3 {
            colorant[i] = full[i] - black[i];
        }
    }
    // The share of each colorant's luminance a drive value gives, forced to rise.
    let mut curves: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (channel, curve) in curves.iter_mut().enumerate() {
        let full = colorants[channel][1];
        let mut previous = 0.0f64;
        for i in 0..CURVE_SIZE {
            let x = i as f64 / (CURVE_SIZE - 1) as f64;
            let mut drive = [0.0; 3];
            drive[channel] = x;
            let share = if full > 1e-9 {
                ((xyz(drive)[1] - black[1]) / full).clamp(0.0, 1.0)
            } else {
                x
            };
            previous = previous.max(share);
            curve.push(previous);
        }
    }
    let model = PanelModel {
        colorants: Some(colorants),
        curve_size: CURVE_SIZE,
        curves,
        worst_error: None,
    };
    let white_y = xyz([1.0; 3])[1].max(1e-9);
    let mut worst = 0.0f64;
    for r in 0..5 {
        for g in 0..5 {
            for b in 0..5 {
                let drive = [r, g, b].map(|i| i as f64 / 4.0);
                let want = xyz(drive);
                let got = model.predict(drive, black);
                for i in 0..3 {
                    worst = worst.max((want[i] - got[i]).abs() / white_y);
                }
            }
        }
    }
    PanelModel {
        worst_error: Some(worst),
        ..model
    }
}

/// Tone curves of a table that goes from PCS to device: for a grey of luminance `t` (0..1 of the
/// white), the drive each channel needs. Inverted, that is the light a drive value gives.
fn fit_b2a(lut: &Lut, pcs: Pcs) -> Option<PanelModel> {
    const STEPS: usize = 512;
    let mut pairs: [Vec<(f64, f64)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for i in 0..=STEPS {
        let t = i as f64 / STEPS as f64;
        let device = lut.eval(lut.xyz_to_pcs(pcs, D50.map(|v| v * t)));
        for (channel, list) in pairs.iter_mut().enumerate() {
            list.push((device[channel], t));
        }
    }
    let mut curves: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (channel, curve) in curves.iter_mut().enumerate() {
        // Drive rises with luminance, make it so, then read the light off at each drive value.
        let list = &mut pairs[channel];
        for i in 1..list.len() {
            list[i].0 = list[i].0.max(list[i - 1].0);
        }
        let (first, last) = (list[0].0, list[list.len() - 1].0);
        if last - first < 0.05 {
            return None;
        }
        let mut previous = 0.0f64;
        for i in 0..CURVE_SIZE {
            let x = i as f64 / (CURVE_SIZE - 1) as f64;
            let light = if x <= first {
                0.0
            } else if x >= last {
                1.0
            } else {
                let at = list.partition_point(|(drive, _)| *drive < x);
                let (d0, l0) = list[at - 1];
                let (d1, l1) = list[at];
                if d1 > d0 {
                    l0 + (l1 - l0) * (x - d0) / (d1 - d0)
                } else {
                    l1
                }
            };
            previous = previous.max(light);
            curve.push(previous);
        }
    }
    Some(PanelModel {
        colorants: None,
        curve_size: CURVE_SIZE,
        curves,
        worst_error: None,
    })
}

impl PanelModel {
    /// XYZ (D50) the fitted model gives for a drive value, with `black` added. Needs colorants.
    pub fn predict(&self, drive: [f64; 3], black: [f64; 3]) -> [f64; 3] {
        let mut out = black;
        let Some(colorants) = &self.colorants else {
            return out;
        };
        for channel in 0..3 {
            let light = Trc::Table(self.curves[channel].clone()).eval(drive[channel]);
            for i in 0..3 {
                out[i] += colorants[channel][i] * light;
            }
        }
        out
    }

    /// Each channel's curve as a panel tone response, code to light, as the profile's own tone
    /// curves would be.
    pub(super) fn trcs(&self) -> [Trc; 3] {
        [
            Trc::Table(self.curves[0].clone()),
            Trc::Table(self.curves[1].clone()),
            Trc::Table(self.curves[2].clone()),
        ]
    }
}
