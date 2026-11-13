//! The math of an HDR output: PQ and HLG decoding, the tone curve and the final pass that turns the linear working space into the signal.

use super::color::{Chromaticities, NamedPrimaries};
use super::edid::Primaries;
use super::gamut::{conversion_matrix, map_gamut, rgb_to_xyz, GamutMap, Mat3};

/// Luminance PQ's top code stands for, in cd/m².
pub const PQ_PEAK: f32 = 10_000.0;

/// Where the usual HDR reference white sits, in cd/m².
pub const HDR_REFERENCE_WHITE: f32 = 203.0;

/// SDR white of an output that does not set `sdr-white`, in cd/m².
pub const DEFAULT_SDR_WHITE: u32 = 203;

/// Smallest and largest `sdr-white` accepted, in cd/m².
pub const SDR_WHITE_RANGE: std::ops::RangeInclusive<u32> = 10..=1000;

/// Smallest and largest `hdr-peak` accepted, in cd/m².
pub const HDR_PEAK_RANGE: std::ops::RangeInclusive<u32> = 100..=10_000;

/// Below this ratio of peak to SDR white the panel has little room for highlights.
pub const LOW_HEADROOM: f32 = 2.0;

/// Where the tone curve starts to roll off, as a fraction of the peak.
const KNEE: f32 = 0.75;

const M1: f32 = 2610.0 / 16384.0;
const M2: f32 = 2523.0 / 4096.0 * 128.0;
const C1: f32 = 3424.0 / 4096.0;
const C2: f32 = 2413.0 / 4096.0 * 32.0;
const C3: f32 = 2392.0 / 4096.0 * 32.0;

/// PQ code (0 to 1) to light, as a fraction of 10000 cd/m².
pub fn pq_decode(code: f32) -> f32 {
    let p = code.clamp(0.0, 1.0).powf(1.0 / M2);
    let num = (p - C1).max(0.0);
    let den = C2 - C3 * p;
    (num / den).powf(1.0 / M1)
}

/// Light, as a fraction of 10000 cd/m², to PQ code (0 to 1).
pub fn pq_encode(light: f32) -> f32 {
    let y = light.clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

/// Peak luminance of the display HLG's absolute values are given for, in cd/m².
pub const HLG_NOMINAL_PEAK: f32 = 1000.0;

/// The HLG system gamma of a 1000 cd/m² display (BT.2100-2).
pub const HLG_SYSTEM_GAMMA: f32 = 1.2;

/// Y of BT.2020's red, green and blue, what the HLG OOTF weighs a color by when its primaries are BT.2020.
pub const BT2020_LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];

const HLG_A: f32 = 0.178_832_77;
const HLG_B: f32 = 0.284_668_92;
const HLG_C: f32 = 0.559_910_73;

/// HLG signal (0 to 1) to scene light (0 to 1), the inverse OETF of BT.2100-2.
pub fn hlg_inverse_oetf(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// An HLG color (signal values, `luma` the Y row of its primaries) as light on a 1000 cd/m² display, in cd/m²: the inverse OETF, then the BT.2100 OOTF with a system gamma of 1.2 and no black lift.
pub fn hlg_display_nits(signal: [f32; 3], luma: [f32; 3]) -> [f32; 3] {
    let scene = signal.map(hlg_inverse_oetf);
    let y = dot(luma, scene);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let gain = HLG_NOMINAL_PEAK * y.powf(HLG_SYSTEM_GAMMA - 1.0);
    scene.map(|v| v * gain)
}

/// The panel's primaries from an EDID, in the millionths the protocol writes them in.
pub fn chromaticities_of(primaries: &Primaries) -> Chromaticities {
    let pair = |(x, y): (f64, f64)| [(x * 1e6).round() as i32, (y * 1e6).round() as i32];
    Chromaticities {
        red: pair(primaries.red),
        green: pair(primaries.green),
        blue: pair(primaries.blue),
        white: pair(primaries.white),
    }
}

/// Whether `peak` is under [`LOW_HEADROOM`] times `sdr_white`, which leaves little room for highlights.
pub fn low_headroom(sdr_white: f32, peak: f32) -> bool {
    peak < sdr_white * LOW_HEADROOM
}

/// Luminance of a pixel, in cd/m², squeezed under `peak`: untouched up to the knee, then a smooth roll-off that never reaches the peak.
pub fn tone_map(nits: f32, peak: f32) -> f32 {
    let knee = KNEE * peak;
    if nits <= knee {
        return nits;
    }
    let range = peak - knee;
    knee + range * (1.0 - (-(nits - knee) / range).exp())
}

/// What the final pass of an HDR output does, worked out once per output and per setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FinalPass {
    /// Luminance of 1.0 in the working space, cd/m².
    pub sdr_white: f32,
    /// Peak luminance of the panel, cd/m².
    pub peak: f32,
    /// BT.2020 to the panel's primaries, identity when they are not known.
    pub to_panel: [[f32; 3]; 3],
    /// The panel's primaries to BT.2020.
    pub from_panel: [[f32; 3]; 3],
    /// Y of BT.2020's red, green and blue.
    pub luma_2020: [f32; 3],
    /// Y of the panel's red, green and blue.
    pub luma_panel: [f32; 3],
    /// The panel's gamut is known: colors outside it are desaturated into it.
    pub map_gamut: bool,
}

fn narrow(m: &Mat3) -> [[f32; 3]; 3] {
    m.map(|row| row.map(|v| v as f32))
}

fn luma_row(primaries: &Chromaticities) -> Option<[f32; 3]> {
    let m = rgb_to_xyz(primaries)?;
    Some([m[1][0] as f32, m[1][1] as f32, m[1][2] as f32])
}

impl FinalPass {
    /// `panel` is the gamut of the monitor, from its EDID or ICC profile, `None` when unknown.
    pub fn new(sdr_white: f32, peak: f32, panel: Option<&Chromaticities>) -> Self {
        let bt2020 = NamedPrimaries::Bt2020.chromaticities();
        let luma_2020 = luma_row(&bt2020).unwrap_or([0.2627, 0.6780, 0.0593]);
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let matrices = panel.and_then(|panel| {
            let to = conversion_matrix(&bt2020, panel)?;
            let from = conversion_matrix(panel, &bt2020)?;
            Some((narrow(&to), narrow(&from), luma_row(panel)?))
        });
        let (to_panel, from_panel, luma_panel, map_gamut) = match matrices {
            Some((to, from, luma)) => (to, from, luma, true),
            None => (identity, identity, luma_2020, false),
        };
        Self {
            sdr_white,
            peak,
            to_panel,
            from_panel,
            luma_2020,
            luma_panel,
            map_gamut,
        }
    }

    /// The shader on one pixel: linear BT.2020 (1.0 is SDR white) in, PQ code values out.
    pub fn apply(&self, working: [f32; 3]) -> [f32; 3] {
        let mut nits = working.map(|v| v.max(0.0) * self.sdr_white);
        let y = dot(self.luma_2020, nits);
        if y > 1e-6 {
            let scale = tone_map(y, self.peak) / y;
            nits = nits.map(|v| v * scale);
        }
        if self.map_gamut {
            let panel = mul_vec(&self.to_panel, nits).map(|v| v / self.peak);
            let mapped = map_gamut(panel, self.luma_panel, GamutMap::Compress);
            nits = mul_vec(&self.from_panel, mapped.map(|v| v * self.peak));
        }
        nits.map(|v| pq_encode(v.clamp(0.0, PQ_PEAK) / PQ_PEAK))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn mul_vec(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
