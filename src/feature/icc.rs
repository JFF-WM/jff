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
//! A profile with neither `vcgt` nor the three curves is an error.

use std::path::Path;

use anyhow::{bail, Context};

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
}

impl GammaTable {
    /// Words for the log.
    pub fn describe(&self) -> &'static str {
        match self.kind {
            Kind::Vcgt => "calibration curves (vcgt)",
            Kind::ToneCurves => "panel tone curves (TRC)",
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
    bail!("the profile has neither vcgt calibration curves nor tone curves")
}

/// A panel tone response curve: code value (0..=1) to light (0..=1).
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

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}
