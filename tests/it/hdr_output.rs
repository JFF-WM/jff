//! Tests for `jff_wm::primitives::hdr_output` and the HDR side of `gamut::Conversion`.

use jff_wm::primitives::color::{
    ImageDescription, Luminance, NamedPrimaries, NamedTf, TransferFunction,
};
use jff_wm::primitives::gamut::{Conversion, Curve};
use jff_wm::primitives::hdr_output::{
    low_headroom, pq_decode, pq_encode, tone_map, FinalPass, PQ_PEAK,
};

const PEAK: f32 = 301.8;

/// The AOC AG323FWG3R3's primaries as the protocol writes them (millionths).
fn panel() -> jff_wm::primitives::color::Chromaticities {
    jff_wm::primitives::color::Chromaticities {
        red: [677_700, 313_400],
        green: [275_300, 647_400],
        blue: [151_300, 64_400],
        white: [313_400, 329_100],
    }
}

fn pq(nits: f32) -> f32 {
    pq_encode(nits / PQ_PEAK)
}

fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) {
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() <= tolerance, "{a:?} vs {b:?}");
    }
}

#[test]
fn pq_matches_known_code_values() {
    // BT.2408: reference white 203 cd/m² is about 58%, 100 cd/m² about 51%, 10000 is 100%.
    assert!((pq(203.0) - 0.5807).abs() < 1e-3);
    assert!((pq(100.0) - 0.5081).abs() < 1e-3);
    assert!((pq(10_000.0) - 1.0).abs() < 1e-6);
    assert!(pq_encode(0.0) < 1e-6);
}

#[test]
fn pq_goes_back_and_forth() {
    for step in 0..=100 {
        let code = step as f32 / 100.0;
        assert!((pq_encode(pq_decode(code)) - code).abs() < 1e-4, "{code}");
    }
    assert!((pq_decode(1.0) - 1.0).abs() < 1e-5);
    assert_eq!(pq_decode(0.0), 0.0);
}

#[test]
fn the_tone_curve_leaves_the_low_range_alone() {
    for nits in [0.0, 1.0, 100.0, 203.0, 0.75 * PEAK] {
        assert_eq!(tone_map(nits, PEAK), nits);
    }
}

#[test]
fn the_tone_curve_is_smooth_monotonic_and_under_the_peak() {
    let knee = 0.75 * PEAK;
    let just_over = tone_map(knee + 0.01, PEAK);
    assert!(
        (just_over - (knee + 0.01)).abs() < 1e-3,
        "no jump at the knee"
    );
    // the slope is 1 on both sides of the knee
    let slope = (tone_map(knee + 0.02, PEAK) - tone_map(knee + 0.01, PEAK)) / 0.01;
    assert!((slope - 1.0).abs() < 1e-2);
    let mut last = 0.0;
    for step in 0..=2000 {
        let out = tone_map(step as f32 * 10.0, PEAK);
        assert!(out >= last);
        assert!(out <= PEAK);
        last = out;
    }
    assert!(tone_map(10_000.0, PEAK) > 0.99 * PEAK);
}

#[test]
fn headroom_is_low_for_203_on_the_panel_and_fine_for_150() {
    assert!(low_headroom(203.0, PEAK));
    assert!(!low_headroom(150.0, PEAK));
    assert!(!low_headroom(203.0, 1000.0));
}

#[test]
fn sdr_white_lands_on_its_pq_code_when_it_is_under_the_knee() {
    let pass = FinalPass::new(203.0, PEAK, Some(&panel()));
    let out = pass.apply([1.0, 1.0, 1.0]);
    close(out, [pq(203.0); 3], 2e-3);
    close(pass.apply([0.0; 3]), [0.0; 3], 1e-5);
}

#[test]
fn sdr_white_can_be_moved() {
    let pass = FinalPass::new(150.0, PEAK, Some(&panel()));
    close(pass.apply([1.0; 3]), [pq(150.0); 3], 2e-3);
    let half = pass.apply([0.5; 3]);
    close(half, [pq(75.0); 3], 2e-3);
}

#[test]
fn light_above_the_peak_is_squeezed_under_it() {
    let pass = FinalPass::new(203.0, PEAK, Some(&panel()));
    // a 4000 cd/m² highlight
    let out = pass.apply([4000.0 / 203.0; 3]);
    for channel in out {
        assert!(channel <= pq(PEAK) + 2e-3, "{out:?}");
        assert!(channel > pq(0.75 * PEAK));
    }
}

#[test]
fn colors_of_the_panel_gamut_are_not_desaturated() {
    let srgb = ImageDescription::srgb();
    let to_working = Conversion::to_working(&srgb, 203.0);
    let with_map = FinalPass::new(203.0, PEAK, Some(&panel()));
    let without_map = FinalPass::new(203.0, PEAK, None);
    for rgb in [[0.8, 0.3, 0.2], [0.1, 0.6, 0.9], [1.0, 1.0, 1.0], [0.5; 3]] {
        let working = to_working.apply(rgb);
        close(with_map.apply(working), without_map.apply(working), 3e-3);
    }
}

#[test]
fn colors_outside_the_panel_gamut_come_back_inside_it() {
    // pure BT.2020 green is far outside the panel
    let pass = FinalPass::new(203.0, PEAK, Some(&panel()));
    let out = pass.apply([0.0, 0.5, 0.0]);
    let nits = out.map(|c| pq_decode(c) * PQ_PEAK);
    let to_panel = pass.to_panel;
    let in_panel: Vec<f32> = to_panel
        .iter()
        .map(|row| row[0] * nits[0] + row[1] * nits[1] + row[2] * nits[2])
        .collect();
    for channel in in_panel {
        assert!(channel >= -0.5, "{channel}");
        assert!(channel <= PEAK + 0.5, "{channel}");
    }
    // and without a panel gamut there is nothing to desaturate
    let plain = FinalPass::new(203.0, PEAK, None);
    assert!(!plain.map_gamut);
}

#[test]
fn surfaces_go_to_the_working_space_with_white_at_one() {
    let srgb = Conversion::to_working(&ImageDescription::srgb(), 203.0);
    close(srgb.apply([1.0; 3]), [1.0; 3], 1e-3);
    assert_eq!(srgb.apply([0.0; 3]), [0.0; 3]);
    // sRGB mid grey is decoded to light, not left encoded
    let grey = srgb.apply([0.5; 3]);
    close(grey, [0.2140; 3], 1e-3);
    // red moves into BT.2020's smaller red
    let red = srgb.apply([1.0, 0.0, 0.0]);
    assert!(red[0] > 0.6 && red[0] < 0.65);
    assert!(red[1] > 0.06 && red[2] > 0.01);
    assert!(srgb.working);
}

#[test]
fn a_bt2020_surface_keeps_its_values() {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::Bt2020.chromaticities();
    let conversion = Conversion::to_working(&description, 203.0);
    close(conversion.apply([1.0, 0.5, 0.0]), [1.0, 0.2140, 0.0], 1e-3);
}

#[test]
fn a_pq_surface_is_placed_by_its_absolute_light() {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::Bt2020.chromaticities();
    description.tf = TransferFunction::Named(NamedTf::St2084Pq);
    description.luminance = Luminance::PQ;
    let conversion = Conversion::to_working(&description, 203.0);
    assert_eq!(conversion.decode, Curve::Pq);
    // 203 cd/m² is SDR white, 1000 cd/m² is 1000/203 of it
    let white = conversion.apply([pq(203.0); 3]);
    close(white, [1.0; 3], 5e-3);
    let bright = conversion.apply([pq(1000.0); 3]);
    close(bright, [1000.0 / 203.0; 3], 0.05);
    // moving SDR white moves PQ's share of it
    let other = Conversion::to_working(&description, 100.0);
    close(other.apply([pq(203.0); 3]), [2.03; 3], 0.02);
}

#[test]
fn the_conversion_tag_tells_working_and_output_apart() {
    let srgb = ImageDescription::srgb();
    let a = Conversion::to_working(&srgb, 203.0);
    let b = Conversion::to_working(&srgb, 203.0);
    assert_eq!(a.tag(), b.tag());
    let mut pq_description = srgb;
    pq_description.tf = TransferFunction::Named(NamedTf::St2084Pq);
    assert_ne!(
        a.tag(),
        Conversion::to_working(&pq_description, 203.0).tag()
    );
}

#[test]
fn a_pq_surface_on_an_sdr_output_is_clipped_at_reference_white() {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::Bt2020.chromaticities();
    description.tf = TransferFunction::Named(NamedTf::St2084Pq);
    description.luminance = Luminance::PQ;
    let to_srgb = Conversion::to_output(
        &description,
        &ImageDescription::srgb(),
        jff_wm::primitives::color::RenderIntent::Relative,
        false,
    )
    .unwrap();
    // 203 cd/m² grey is white on the SDR output, 100 cd/m² is a darker grey
    close(to_srgb.apply([pq(203.0); 3]), [1.0; 3], 5e-3);
    let dim = to_srgb.apply([pq(100.0); 3]);
    assert!(dim[0] > 0.5 && dim[0] < 0.85);
    assert_eq!(to_srgb.apply([0.0; 3]), [0.0; 3]);
}

#[test]
fn edid_primaries_become_millionths() {
    use jff_wm::primitives::edid::Primaries;
    let c = jff_wm::primitives::hdr_output::chromaticities_of(&Primaries {
        red: (0.64, 0.33),
        green: (0.3, 0.6),
        blue: (0.15, 0.06),
        white: (0.3127, 0.329),
    });
    assert_eq!(c.red, [640_000, 330_000]);
    assert_eq!(c.white, [312_700, 329_000]);
}
