//! Tests for `jff_wm::primitives::yuv`.

use jff_wm::primitives::yuv::{Correction, Encoding};

const BT601: (f64, f64) = (0.299, 0.114);
const BT709: (f64, f64) = (0.2126, 0.0722);

fn enc((kr, kb): (f64, f64), limited: bool) -> Encoding {
    Encoding { kr, kb, limited }
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// For range changes: neutral chroma is 128/255 in limited range and 127.5/255 in full range,
/// so a grey comes out up to a third of a percent off neutral, which is correct.
fn roughly(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
}

#[test]
fn the_same_encoding_needs_no_correction() {
    assert_eq!(
        Correction::between(enc(BT601, true), enc(BT601, true)),
        None
    );
}

#[test]
fn greys_stay_grey_when_only_the_matrix_differs() {
    // Every YCbCr standard maps neutral chroma to R = G = B = luma, so a matrix change must
    // not tint greys, black or white.
    let fix = Correction::between(enc(BT601, true), enc(BT709, true)).unwrap();
    for level in [0.0, 0.25, 0.5, 0.75, 1.0] {
        assert!(
            near(fix.apply([level; 3]), [level; 3]),
            "grey {level} became {:?}",
            fix.apply([level; 3])
        );
    }
}

#[test]
fn the_matrix_change_moves_saturated_colors() {
    // The classic mix-up: BT.709 red decoded as BT.601 comes out a little too orange, and the
    // correction has to move it. A red that isn't touched means the matrix is not applied.
    let fix = Correction::between(enc(BT601, true), enc(BT709, true)).unwrap();
    let driver_red = [1.0, 0.0, 0.0];
    assert!(!near(fix.apply(driver_red), driver_red));
}

#[test]
fn full_range_data_decoded_as_limited_is_stretched_back() {
    // The driver treats 235 as white. Full range data whose luma is 235/255 is light grey,
    // so the white the driver drew has to come back as 235/255.
    let fix = Correction::between(enc(BT601, true), enc(BT601, false)).unwrap();
    let grey = 235.0 / 255.0;
    assert!(roughly(fix.apply([1.0; 3]), [grey; 3]));
    // Luma 16 is black for the driver, and 16/255 for full range data.
    let black = 16.0 / 255.0;
    assert!(roughly(fix.apply([0.0; 3]), [black; 3]));
}

#[test]
fn limited_range_data_decoded_as_full_is_squeezed_back() {
    // The reverse: a driver that treats the data as full range shows limited black as 16/255
    // grey. Corrected to limited, that is black again.
    let fix = Correction::between(enc(BT601, false), enc(BT601, true)).unwrap();
    let black = 16.0 / 255.0;
    assert!(roughly(fix.apply([black; 3]), [0.0; 3]));
    let white = 235.0 / 255.0;
    assert!(roughly(fix.apply([white; 3]), [1.0; 3]));
}

#[test]
fn a_correction_and_its_reverse_cancel_for_in_gamut_colors() {
    let there = Correction::between(enc(BT601, true), enc(BT709, true)).unwrap();
    let back = Correction::between(enc(BT709, true), enc(BT601, true)).unwrap();
    for color in [[0.5, 0.5, 0.5], [0.6, 0.4, 0.3], [0.3, 0.5, 0.7]] {
        assert!(near(back.apply(there.apply(color)), color), "{color:?}");
    }
}

#[test]
fn the_tag_changes_with_the_correction() {
    let a = Correction::between(enc(BT601, true), enc(BT709, true)).unwrap();
    let b = Correction::between(enc(BT601, true), enc(BT709, false)).unwrap();
    assert_eq!(a.tag(), a.tag());
    assert_ne!(a.tag(), b.tag());
}
