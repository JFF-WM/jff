//! Tests for `jff_wm::feature::icc::lut`: reading lookup-table profiles and fitting them.

use jff_wm::feature::icc::lut::{panel_model, pcs_of, read_tag, Pcs};
use jff_wm::feature::icc::{description, parse, Kind};

/// sRGB's colorants as ICC stores them, adapted to D50 (rows are X, Y, Z).
const SRGB_D50: [[f64; 3]; 3] = [
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
];

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

fn profile_with(pcs: &[u8; 4], tags: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut data = vec![0u8; 128];
    data[16..20].copy_from_slice(b"RGB ");
    data[20..24].copy_from_slice(pcs);
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

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

/// A `lut16` with identity input and output tables and a CLUT of `grid` points per side holding
/// `node(input)`. `Some(grid)` values are 0..1 on both sides.
fn lut16(grid: usize, node: impl Fn([f64; 3]) -> [f64; 3]) -> Vec<u8> {
    let mut tag = b"mft2\0\0\0\0".to_vec();
    tag.extend_from_slice(&[3, 3, grid as u8, 0]);
    for row in 0..3 {
        for column in 0..3 {
            tag.extend_from_slice(&s15(if row == column { 1.0 } else { 0.0 }));
        }
    }
    tag.extend_from_slice(&2u16.to_be_bytes());
    tag.extend_from_slice(&2u16.to_be_bytes());
    for _ in 0..3 {
        tag.extend_from_slice(&0u16.to_be_bytes());
        tag.extend_from_slice(&65535u16.to_be_bytes());
    }
    for a in 0..grid {
        for b in 0..grid {
            for c in 0..grid {
                let at = [a, b, c].map(|i| i as f64 / (grid - 1) as f64);
                for v in node(at) {
                    tag.extend_from_slice(
                        &((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes(),
                    );
                }
            }
        }
    }
    for _ in 0..3 {
        tag.extend_from_slice(&0u16.to_be_bytes());
        tag.extend_from_slice(&65535u16.to_be_bytes());
    }
    tag
}

/// The XYZ (D50) of an sRGB-primaries panel with a 2.2 gamma, as a PCS value (0..1).
fn panel_xyz(rgb: [f64; 3]) -> [f64; 3] {
    let light = rgb.map(|v| v.powf(2.2));
    let mut out = [0.0; 3];
    for i in 0..3 {
        let xyz: f64 = (0..3).map(|k| SRGB_D50[i][k] * light[k]).sum();
        out[i] = xyz * 32768.0 / 65535.0;
    }
    out
}

fn a2b_profile() -> Vec<u8> {
    profile_with(b"XYZ ", &[(b"A2B0", lut16(9, panel_xyz))])
}

#[test]
fn a_lut16_tag_reads_back_what_it_holds() {
    let data = a2b_profile();
    assert_eq!(pcs_of(&data), Some(Pcs::Xyz));
    let lut = read_tag(&data, b"A2B0").unwrap();
    // A node of the grid comes back exactly (to 16 bits).
    let got = lut.eval([0.5, 0.25, 1.0]);
    let want = panel_xyz([0.5, 0.25, 1.0]);
    for i in 0..3 {
        assert!((got[i] - want[i]).abs() < 1e-4, "{got:?} {want:?}");
    }
    assert!(read_tag(&data, b"B2A0").is_none());
}

#[test]
fn a_table_that_is_cut_short_is_refused() {
    let mut tag = lut16(9, panel_xyz);
    tag.truncate(tag.len() - 200);
    let data = profile_with(b"XYZ ", &[(b"A2B0", tag)]);
    assert!(read_tag(&data, b"A2B0").is_none());
    assert!(panel_model(&data).is_none());
}

#[test]
fn an_a2b_table_is_fitted_with_its_colorants_and_curves() {
    let model = panel_model(&a2b_profile()).unwrap();
    let colorants = model.colorants.unwrap();
    for channel in 0..3 {
        for i in 0..3 {
            assert!(
                (colorants[channel][i] - SRGB_D50[i][channel]).abs() < 2e-3,
                "colorant {channel}: {colorants:?}"
            );
        }
    }
    // Half drive gives 0.5^2.2 of the light.
    for curve in &model.curves {
        let at = |x: f64| curve[(x * (model.curve_size - 1) as f64).round() as usize];
        assert!((at(0.5) - 0.5f64.powf(2.2)).abs() < 5e-3, "{}", at(0.5));
        assert!((at(0.25) - 0.25f64.powf(2.2)).abs() < 5e-3, "{}", at(0.25));
        assert!(at(0.0) < 1e-6 && at(1.0) > 1.0 - 1e-6);
    }
    // The table is a matrix and curves, so the fit is exact.
    assert!(model.worst_error.unwrap() < 2e-3, "{:?}", model.worst_error);
}

#[test]
fn a_table_profile_gives_hardware_curves_and_a_description() {
    let data = a2b_profile();
    let table = parse(&data).unwrap();
    assert_eq!(table.kind, Kind::ToneCurves);
    // The correction makes the panel (gamma 2.2) follow sRGB: code 0.5 of sRGB asks for
    // 0.2140 of light, which the panel gives at 0.2140^(1/2.2) = 0.5028.
    let code = table.red[512] as f64 / 65535.0;
    assert!((code - 0.503).abs() < 0.01, "{code}");
    let described = description(&data).unwrap();
    // sRGB primaries, to the rounding of the profile.
    assert!(
        (described.primaries.red[0] - 640_000).abs() < 4_000,
        "{described:?}"
    );
    assert!(
        (described.primaries.green[1] - 600_000).abs() < 4_000,
        "{described:?}"
    );
    assert!(
        (described.primaries.blue[1] - 60_000).abs() < 4_000,
        "{described:?}"
    );
}

/// PCS XYZ in, device out, for the same panel: each channel is the matrix-inverted light, with the
/// 2.2 gamma undone.
fn b2a_profile() -> Vec<u8> {
    // Inverse of the colorant matrix.
    let m = SRGB_D50;
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv = [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) / det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) / det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) / det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det,
        ],
    ];
    let table = lut16(33, |pcs| {
        let xyz = pcs.map(|v| v * 65535.0 / 32768.0);
        let mut out = [0.0; 3];
        for i in 0..3 {
            let light: f64 = (0..3).map(|k| inv[i][k] * xyz[k]).sum();
            out[i] = light.max(0.0).powf(1.0 / 2.2);
        }
        out
    });
    profile_with(b"XYZ ", &[(b"B2A0", table)])
}

#[test]
fn a_b2a_only_profile_gives_tone_curves() {
    let data = b2a_profile();
    let model = panel_model(&data).unwrap();
    assert!(model.colorants.is_none());
    for curve in &model.curves {
        let at = |x: f64| curve[(x * (model.curve_size - 1) as f64).round() as usize];
        assert!((at(0.5) - 0.5f64.powf(2.2)).abs() < 0.02, "{}", at(0.5));
        assert!((at(0.8) - 0.8f64.powf(2.2)).abs() < 0.02, "{}", at(0.8));
    }
    let table = parse(&data).unwrap();
    assert_eq!(table.kind, Kind::ToneCurves);
    // No colorants: what the profile does not say stays sRGB.
    let described = description(&data).unwrap();
    assert_eq!(described.primaries.red, [640_000, 330_000]);
}

#[test]
fn a_lut_with_lab_on_the_pcs_side_decodes_to_xyz() {
    // L* 100, a* 0, b* 0 in the v2 16-bit encoding is white: D50.
    let data = profile_with(
        b"Lab ",
        &[(
            b"A2B0",
            lut16(2, |_| {
                [65280.0 / 65535.0, 32768.0 / 65535.0, 32768.0 / 65535.0]
            }),
        )],
    );
    assert_eq!(pcs_of(&data), Some(Pcs::Lab));
    let lut = read_tag(&data, b"A2B0").unwrap();
    let xyz = lut.pcs_to_xyz(Pcs::Lab, lut.eval([1.0, 1.0, 1.0]));
    for i in 0..3 {
        assert!((xyz[i] - D50[i]).abs() < 3e-3, "{xyz:?}");
    }
    // And back.
    let back = lut.xyz_to_pcs(Pcs::Lab, D50);
    assert!((back[0] - 65280.0 / 65535.0).abs() < 1e-3, "{back:?}");
}

/// `curv` that is a single u8Fixed8 gamma, padded to four bytes.
fn gamma_curv(gamma: f64) -> Vec<u8> {
    let mut tag = b"curv\0\0\0\0".to_vec();
    tag.extend_from_slice(&1u32.to_be_bytes());
    tag.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
    tag.extend_from_slice(&[0, 0]);
    tag
}

/// `curv` with no entries: the identity.
fn identity_curv() -> Vec<u8> {
    let mut tag = b"curv\0\0\0\0".to_vec();
    tag.extend_from_slice(&0u32.to_be_bytes());
    tag
}

/// A `lutAToB` of curves (gamma 2.2) and a matrix only.
fn mab_matrix_curves() -> Vec<u8> {
    let b_off = 32usize;
    let matrix_off = b_off + 3 * 12;
    let m_off = matrix_off + 48;
    let mut tag = b"mAB \0\0\0\0".to_vec();
    tag.extend_from_slice(&[3, 3, 0, 0]);
    for off in [b_off, matrix_off, m_off, 0, 0] {
        tag.extend_from_slice(&(off as u32).to_be_bytes());
    }
    for _ in 0..3 {
        tag.extend_from_slice(&identity_curv());
    }
    // The matrix leaves XYZ scaled to the PCS encoding.
    for row in SRGB_D50 {
        for v in row {
            tag.extend_from_slice(&s15(v * 32768.0 / 65535.0));
        }
    }
    for v in [0.0, 0.0, 0.0] {
        tag.extend_from_slice(&s15(v));
    }
    for _ in 0..3 {
        tag.extend_from_slice(&gamma_curv(2.2));
    }
    tag
}

#[test]
fn a_lut_a_to_b_of_curves_and_a_matrix_is_read_in_the_spec_order() {
    let data = profile_with(b"XYZ ", &[(b"A2B1", mab_matrix_curves())]);
    let lut = read_tag(&data, b"A2B1").unwrap();
    let got = lut.eval([0.5, 0.5, 0.5]);
    let want = panel_xyz([0.5, 0.5, 0.5]);
    for i in 0..3 {
        // The gamma is stored as u8Fixed8, 2.2 is 2.1992.
        assert!((got[i] - want[i]).abs() < 2e-3, "{got:?} {want:?}");
    }
    let model = panel_model(&data).unwrap();
    assert!(model.colorants.is_some());
    assert!(model.worst_error.unwrap() < 5e-3);
}

#[test]
fn a_profile_with_no_table_has_no_model() {
    let data = profile_with(b"XYZ ", &[]);
    assert!(panel_model(&data).is_none());
    assert!(parse(&data).is_err());
}
