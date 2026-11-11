//! Tests for `jff_wm::primitives::color_names`: the words the control socket uses for a
//! monitor's colors.

use jff_wm::primitives::color::{Chromaticities, NamedPrimaries, NamedTf, TransferFunction};
use jff_wm::primitives::color_names::{gamut_class, primaries_name, transfer_name, white_name};

const D65: [i32; 2] = [312_700, 329_000];

/// `c` with every coordinate moved by `by`, the way a profile's rounding and its white point
/// adaptation move them.
fn nudged(c: Chromaticities, by: i32) -> Chromaticities {
    let [rx, ry, gx, gy, bx, by_, wx, wy] = c.to_array();
    Chromaticities::from_array([
        rx + by,
        ry - by,
        gx - by,
        gy + by,
        bx + by,
        by_ - by,
        wx - by,
        wy + by,
    ])
}

#[test]
fn every_offered_set_of_primaries_has_its_name() {
    let names: Vec<_> = NamedPrimaries::ALL
        .into_iter()
        .map(|p| primaries_name(&p.chromaticities()))
        .collect();
    assert_eq!(
        names,
        [
            Some("srgb"),
            Some("pal"),
            Some("ntsc"),
            Some("bt2020"),
            Some("dci-p3"),
            Some("display-p3"),
            Some("adobe-rgb"),
        ]
    );
}

#[test]
fn a_profiles_rounding_does_not_lose_the_name() {
    for named in NamedPrimaries::ALL {
        let c = nudged(named.chromaticities(), 300);
        assert_eq!(primaries_name(&c), Some(named.name()), "{named:?}");
    }
}

#[test]
fn near_primaries_are_not_taken_for_their_neighbours() {
    // sRGB, PAL and NTSC are each 0.01 apart in some corner: outside the tolerance.
    let srgb = NamedPrimaries::Srgb.chromaticities();
    assert_eq!(primaries_name(&nudged(srgb, 4_000)), Some("srgb"));
    assert_eq!(primaries_name(&nudged(srgb, 6_000)), None);
}

#[test]
fn the_p3_white_tells_display_p3_from_dci_p3() {
    let mut c = NamedPrimaries::DisplayP3.chromaticities();
    assert_eq!(primaries_name(&c), Some("display-p3"));
    c.white = [314_100, 351_100];
    assert_eq!(primaries_name(&c), Some("dci-p3"));
}

#[test]
fn a_measured_panel_has_no_name() {
    let c = Chromaticities {
        red: [690_000, 310_000],
        green: [200_000, 720_000],
        blue: [150_000, 60_000],
        white: D65,
    };
    assert_eq!(primaries_name(&c), None);
    assert_eq!(gamut_class(&c), "wide");
}

#[test]
fn transfer_functions_are_named_or_a_power() {
    assert_eq!(
        transfer_name(TransferFunction::Named(NamedTf::Srgb)),
        "srgb"
    );
    assert_eq!(
        transfer_name(TransferFunction::Named(NamedTf::Gamma22)),
        "gamma22"
    );
    assert_eq!(
        transfer_name(TransferFunction::Named(NamedTf::Gamma28)),
        "gamma28"
    );
    assert_eq!(transfer_name(TransferFunction::Power(24_000)), "power:2.4");
    assert_eq!(transfer_name(TransferFunction::Power(20_000)), "power:2");
    assert_eq!(
        transfer_name(TransferFunction::Power(22_222)),
        "power:2.2222"
    );
}

#[test]
fn white_points_are_named_or_given_as_xy() {
    assert_eq!(white_name(D65), "d65");
    assert_eq!(white_name([312_900, 329_100]), "d65");
    assert_eq!(white_name([345_700, 358_500]), "d50");
    assert_eq!(white_name([314_000, 351_000]), "dci");
    assert_eq!(white_name([283_000, 297_000]), "xy:0.2830,0.2970");
}

#[test]
fn only_a_clearly_larger_gamut_is_wide() {
    for (named, wide) in [
        (NamedPrimaries::Srgb, false),
        (NamedPrimaries::Pal, false),
        (NamedPrimaries::Ntsc, false),
        (NamedPrimaries::DisplayP3, true),
        (NamedPrimaries::DciP3, true),
        (NamedPrimaries::AdobeRgb, true),
        (NamedPrimaries::Bt2020, true),
    ] {
        let expected = if wide { "wide" } else { "srgb" };
        assert_eq!(gamut_class(&named.chromaticities()), expected, "{named:?}");
    }
}
