//! Words for a monitor's colors, for the control socket (`jff msg watch outputs`).
//!
//! An [`ImageDescription`] is numbers: eight chromaticities and a curve. A panel or a script
//! that only wants to know "is this a P3 monitor?" is better off with a name, so these turn
//! the numbers into the same names the config and the protocol use.
//!
//! The numbers of a monitor come out of an ICC profile (see `feature::icc::description`), and
//! a profile never repeats a standard's numbers to the sixth digit: it has them rounded,
//! adapted to D50 and adapted back. So primaries and white are matched to a name within a
//! tolerance, not exactly like `NamedPrimaries::detect`.

use super::color::{
    Chromaticities, NamedPrimaries, TransferFunction, CHROMA_SCALE, EXPONENT_SCALE,
};

/// How far a chromaticity may be from a named one and still be it: 0.004 in x and in y. Far
/// less than the 0.01 that sets sRGB apart from PAL and NTSC, far more than the rounding of a
/// profile.
const TOLERANCE: i32 = 4_000;

/// How far a white point may be from D65, D50 or DCI white and still be that: 0.003.
const WHITE_TOLERANCE: i32 = 3_000;

/// A gamut this much larger than sRGB's (the area of its triangle in the xy plane) is wide.
/// P3 is 1.36 times sRGB, Adobe RGB 1.35, BT.2020 1.89, while PAL (1.01) and NTSC (0.93) are
/// not wide, and a panel that is sRGB with some error is within a couple of percent of 1.
const WIDE_AREA_RATIO: f64 = 1.05;

const D65: [i32; 2] = [312_700, 329_000];
const D50: [i32; 2] = [345_700, 358_500];
const DCI: [i32; 2] = [314_000, 351_000];

fn near(a: [i32; 2], b: [i32; 2], tolerance: i32) -> bool {
    (a[0] - b[0]).abs() <= tolerance && (a[1] - b[1]).abs() <= tolerance
}

/// The name of the primaries (`srgb`, `display-p3`, `bt2020`, ...) if their three corners are
/// those of an offered set, `None` for any other gamut, a measured panel for one.
///
/// The white point is not asked for, it has a field of its own ([`white_name`]). It only
/// decides between `display-p3` and `dci-p3`, which have the same corners: the one whose white
/// is closer wins.
pub fn primaries_name(c: &Chromaticities) -> Option<&'static str> {
    let mut best: Option<(i32, NamedPrimaries)> = None;
    for named in NamedPrimaries::ALL {
        let n = named.chromaticities();
        if !(near(c.red, n.red, TOLERANCE)
            && near(c.green, n.green, TOLERANCE)
            && near(c.blue, n.blue, TOLERANCE))
        {
            continue;
        }
        let off = (c.white[0] - n.white[0]).abs() + (c.white[1] - n.white[1]).abs();
        if best.map_or(true, |(least, _)| off < least) {
            best = Some((off, named));
        }
    }
    best.map(|(_, named)| named.name())
}

/// `srgb`, `gamma22`, `gamma28`, or `power:2.4` for a power curve.
pub fn transfer_name(tf: TransferFunction) -> String {
    match tf {
        TransferFunction::Named(named) => named.name().to_string(),
        TransferFunction::Power(exponent) => {
            let value = exponent as f64 / EXPONENT_SCALE as f64;
            let text = format!("{value:.4}");
            let text = text.trim_end_matches('0').trim_end_matches('.');
            format!("power:{text}")
        }
    }
}

/// `d65`, `d50` or `dci` for those white points, `xy:0.3127,0.3290` for any other.
pub fn white_name(white: [i32; 2]) -> String {
    for (name, point) in [("d65", D65), ("d50", D50), ("dci", DCI)] {
        if near(white, point, WHITE_TOLERANCE) {
            return name.to_string();
        }
    }
    let scale = CHROMA_SCALE as f64;
    format!(
        "xy:{:.4},{:.4}",
        white[0] as f64 / scale,
        white[1] as f64 / scale
    )
}

/// The area of the triangle of the three primaries in the xy plane.
fn area(c: &Chromaticities) -> f64 {
    let point = |v: [i32; 2]| (v[0] as f64, v[1] as f64);
    let (rx, ry) = point(c.red);
    let (gx, gy) = point(c.green);
    let (bx, by) = point(c.blue);
    0.5 * (rx * (gy - by) + gx * (by - ry) + bx * (ry - gy)).abs()
}

/// `wide` when the gamut is clearly larger than sRGB's, `srgb` when it is about that or less
/// (a panel that cannot show all of sRGB is still handled as an sRGB one).
pub fn gamut_class(c: &Chromaticities) -> &'static str {
    let srgb = area(&NamedPrimaries::Srgb.chromaticities());
    if area(c) >= srgb * WIDE_AREA_RATIO {
        "wide"
    } else {
        "srgb"
    }
}
