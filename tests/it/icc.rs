//! Tests for `jff_wm::feature::icc`.

use jff_wm::feature::icc::{description, parse, srgb_to_light, GammaTable, Kind};
use jff_wm::primitives::color::{ImageDescription, NamedTf, TransferFunction};

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

// ---- description: what clients are told about the panel ----------------------------------

/// An `XYZ ` tag.
fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut tag = b"XYZ \0\0\0\0".to_vec();
    for v in [x, y, z] {
        tag.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
    }
    tag
}

/// A `chad` tag.
fn chad_tag(matrix: [[f64; 3]; 3]) -> Vec<u8> {
    let mut tag = b"sf32\0\0\0\0".to_vec();
    for row in matrix {
        for v in row {
            tag.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
        }
    }
    tag
}

/// The sRGB colorants as profiles store them, adapted to D50.
fn srgb_colorants() -> Vec<(&'static [u8; 4], Vec<u8>)> {
    vec![
        (b"rXYZ", xyz_tag(0.4360747, 0.2225045, 0.0139322)),
        (b"gXYZ", xyz_tag(0.3850649, 0.7168786, 0.0971045)),
        (b"bXYZ", xyz_tag(0.1430804, 0.0606169, 0.7141733)),
    ]
}

fn near(a: [i32; 2], b: [i32; 2]) -> bool {
    (a[0] - b[0]).abs() < 2_000 && (a[1] - b[1]).abs() < 2_000
}

#[test]
fn colorants_without_chad_are_taken_as_d65_panels() {
    let d = description(&profile_tags(&srgb_colorants())).unwrap();
    let srgb = ImageDescription::srgb().primaries;
    assert!(near(d.primaries.red, srgb.red), "{:?}", d.primaries);
    assert!(near(d.primaries.green, srgb.green), "{:?}", d.primaries);
    assert!(near(d.primaries.blue, srgb.blue), "{:?}", d.primaries);
    assert!(near(d.primaries.white, srgb.white), "{:?}", d.primaries);
}

#[test]
fn chad_is_undone_to_get_the_panels_own_primaries() {
    // The Bradford D65 -> D50 matrix: what a profile of a D65 panel carries.
    let chad = chad_tag([
        [1.0478112, 0.0228866, -0.0501270],
        [0.0295424, 0.9904844, -0.0170491],
        [-0.0092345, 0.0150436, 0.7521316],
    ]);
    let mut tags = srgb_colorants();
    tags.push((b"chad", chad));
    let d = description(&profile_tags(&tags)).unwrap();
    let srgb = ImageDescription::srgb().primaries;
    assert!(near(d.primaries.red, srgb.red), "{:?}", d.primaries);
    assert!(near(d.primaries.white, srgb.white), "{:?}", d.primaries);
}

#[test]
fn a_profile_without_colorants_keeps_the_srgb_primaries() {
    let d = description(&trc_profile(gamma_curve(2.2))).unwrap();
    assert_eq!(d.primaries, ImageDescription::srgb().primaries);
}

#[test]
fn without_vcgt_the_panel_follows_srgb() {
    let d = description(&trc_profile(gamma_curve(2.6))).unwrap();
    assert_eq!(d.tf, TransferFunction::Named(NamedTf::Srgb));
}

#[test]
fn with_vcgt_the_tone_curve_is_the_profiles() {
    let vcgt = table_tag(1, 2, 2, &[0, 65535]);
    let with = |curve: Vec<u8>| {
        description(&profile_tags(&[(b"gTRC", curve), (b"vcgt", vcgt.clone())]))
            .unwrap()
            .tf
    };
    assert_eq!(
        with(gamma_curve(2.2)),
        TransferFunction::Named(NamedTf::Gamma22)
    );
    assert_eq!(
        with(gamma_curve(2.8)),
        TransferFunction::Named(NamedTf::Gamma28)
    );
    match with(gamma_curve(1.8)) {
        TransferFunction::Power(exponent) => {
            assert!((exponent as i64 - 18_000).abs() < 200, "{exponent}")
        }
        other => panic!("expected a power curve, got {other:?}"),
    }
}

#[test]
fn lumi_gives_the_white_luminance() {
    let d = description(&profile(b"lumi", &xyz_tag(0.0, 120.0, 0.0))).unwrap();
    assert_eq!((d.luminance.max, d.luminance.reference), (120, 120));
    // Nonsense stays as sRGB has it.
    let d = description(&profile(b"lumi", &xyz_tag(0.0, 0.5, 0.0))).unwrap();
    assert_eq!(d.luminance, ImageDescription::srgb().luminance);
}

#[test]
fn description_rejects_what_is_not_a_profile() {
    assert!(description(b"nonsense").is_err());
}
