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
    red: Vec<u16>,
    green: Vec<u16>,
    blue: Vec<u16>,
    kind: Kind,
}

/// Where the curves came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
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
fn srgb_to_light(v: f64) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal profile with the given tags.
    fn profile_tags(tags: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut data = vec![0u8; 128];
        data[36..40].copy_from_slice(b"acsp");
        data.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut offset = 132 + 12 * tags.len();
        for (sig, tag) in tags {
            data.extend_from_slice(*sig);
            data.extend_from_slice(&(offset as u32).to_be_bytes());
            data.extend_from_slice(&(tag.len() as u32).to_be_bytes());
            offset += tag.len();
        }
        for (_, tag) in tags {
            data.extend_from_slice(tag);
        }
        data
    }

    /// A minimal profile with one tag.
    fn profile(sig: &[u8; 4], tag: &[u8]) -> Vec<u8> {
        profile_tags(&[(sig, tag.to_vec())])
    }

    /// `curv` with a single gamma.
    fn gamma_curve(gamma: f64) -> Vec<u8> {
        let mut tag = b"curv\0\0\0\0".to_vec();
        tag.extend_from_slice(&1u32.to_be_bytes());
        tag.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
        tag
    }

    /// `para` of type 0 (a plain gamma).
    fn parametric_gamma(gamma: f64) -> Vec<u8> {
        let mut tag = b"para\0\0\0\0\0\0\0\0".to_vec();
        tag.extend_from_slice(&((gamma * 65536.0).round() as i32).to_be_bytes());
        tag
    }

    fn trc_profile(curve: Vec<u8>) -> Vec<u8> {
        profile_tags(&[
            (b"rTRC", curve.clone()),
            (b"gTRC", curve.clone()),
            (b"bTRC", curve),
        ])
    }

    fn table_tag(channels: u16, entries: u16, entry_size: u16, values: &[u16]) -> Vec<u8> {
        let mut tag = b"vcgt\0\0\0\0\0\0\0\0".to_vec();
        for v in [channels, entries, entry_size] {
            tag.extend_from_slice(&v.to_be_bytes());
        }
        for v in values {
            if entry_size == 2 {
                tag.extend_from_slice(&v.to_be_bytes());
            } else {
                tag.push(*v as u8);
            }
        }
        tag
    }

    #[test]
    fn reads_a_three_channel_table() {
        let tag = table_tag(3, 2, 2, &[0, 65535, 0, 32768, 100, 200]);
        let table = parse(&profile(b"vcgt", &tag)).unwrap();
        assert_eq!(table.red, [0, 65535]);
        assert_eq!(table.green, [0, 32768]);
        assert_eq!(table.blue, [100, 200]);
    }

    #[test]
    fn one_channel_is_used_for_all_three() {
        let tag = table_tag(1, 2, 1, &[0, 255]);
        let table = parse(&profile(b"vcgt", &tag)).unwrap();
        assert_eq!(table.red, [0, 65535]);
        assert_eq!(table.green, table.red);
        assert_eq!(table.blue, table.red);
    }

    #[test]
    fn formula_with_gamma_one_is_a_ramp() {
        let mut tag = b"vcgt".to_vec();
        tag.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        for _ in 0..3 {
            for v in [1.0f64, 0.0, 1.0] {
                tag.extend_from_slice(&((v * 65536.0) as i32).to_be_bytes());
            }
        }
        let table = parse(&profile(b"vcgt", &tag)).unwrap();
        assert_eq!(table.red.len(), 256);
        assert_eq!((table.red[0], table.red[255]), (0, 65535));
        assert_eq!(
            table.green[128],
            (128.0f64 / 255.0 * 65535.0).round() as u16
        );
    }

    #[test]
    fn resampling_keeps_the_ends() {
        let table = GammaTable {
            red: vec![0, 65535],
            green: vec![0, 65535],
            blue: vec![0, 65535],
            kind: Kind::Vcgt,
        };
        let lut = table.resampled(1024);
        assert_eq!(lut.len(), 1024);
        assert_eq!(lut[0], [0, 0, 0]);
        assert_eq!(lut[1023], [65535, 65535, 65535]);
        assert!(lut[512][0] > 32000 && lut[512][0] < 33500);
    }

    #[test]
    fn tone_curves_make_the_panel_follow_srgb() {
        let table = parse(&trc_profile(gamma_curve(2.2))).unwrap();
        assert_eq!(table.kind, Kind::ToneCurves);
        assert_eq!(table.red.len(), 1024);
        assert_eq!((table.red[0], table.red[1023]), (0, 65535));
        // A gamma 2.2 panel needs the sRGB light, brought back through its own curve.
        for i in [100usize, 512, 900] {
            let x = i as f64 / 1023.0;
            let expected = srgb_to_light(x).powf(1.0 / 2.2) * 65535.0;
            assert!((table.green[i] as f64 - expected).abs() < 300.0, "at {i}");
        }
    }

    #[test]
    fn linear_panel_gets_the_srgb_curve_itself() {
        let curve = b"curv\0\0\0\0\0\0\0\0".to_vec(); // no entries: linear
        let table = parse(&trc_profile(curve)).unwrap();
        let x = 700.0 / 1023.0;
        let expected = srgb_to_light(x) * 65535.0;
        assert!((table.blue[700] as f64 - expected).abs() < 100.0);
    }

    #[test]
    fn parametric_and_gamma_curves_agree() {
        let a = parse(&trc_profile(gamma_curve(2.2))).unwrap();
        let b = parse(&trc_profile(parametric_gamma(2.2))).unwrap();
        for i in 0..1024 {
            assert!((a.red[i] as i32 - b.red[i] as i32).abs() < 80, "at {i}");
        }
    }

    #[test]
    fn vcgt_wins_over_tone_curves() {
        let vcgt = table_tag(1, 2, 2, &[0, 65535]);
        let data = profile_tags(&[
            (b"rTRC", gamma_curve(2.2)),
            (b"gTRC", gamma_curve(2.2)),
            (b"bTRC", gamma_curve(2.2)),
            (b"vcgt", vcgt),
        ]);
        assert_eq!(parse(&data).unwrap().kind, Kind::Vcgt);
    }

    #[test]
    fn rejects_what_is_not_usable() {
        assert!(parse(b"nonsense").is_err());
        assert!(parse(&profile(b"desc", b"vcgt")).is_err()); // no vcgt tag
        assert!(parse(&profile(b"vcgt", b"vcgt\0\0\0\0\0\0\0\0")).is_err()); // truncated
    }
}
