//! Tests for `jff_wm::primitives::hdr_output` and the HDR side of `gamut::Conversion`.

use jff_wm::primitives::color::{
    ImageDescription, Luminance, NamedPrimaries, NamedTf, TransferFunction,
};
use jff_wm::primitives::gamut::{Conversion, Curve};
use jff_wm::primitives::hdr_output::{
    bt2390_tone_map, hlg_display_nits, hlg_inverse_oetf, low_headroom, pq_decode, pq_encode,
    tone_map, FinalPass, ToneMap, BT2020_LUMA, BT2390_SOURCE_PEAK, DEFAULT_SDR_WHITE,
    HDR_PEAK_RANGE, PQ_PEAK, SDR_WHITE_RANGE,
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

#[test]
fn the_accepted_ranges_fit_together() {
    assert!(HDR_PEAK_RANGE.contains(&(PEAK as u32)));
    assert!(SDR_WHITE_RANGE.contains(&DEFAULT_SDR_WHITE));
    assert!(*HDR_PEAK_RANGE.end() as f32 <= PQ_PEAK);
    assert!(HDR_PEAK_RANGE.start() > SDR_WHITE_RANGE.start());
}

#[test]
fn a_higher_peak_leaves_more_room_for_highlights() {
    let bright = [4.0; 3];
    let under_300 = FinalPass::new(100.0, 300.0, None).apply(bright);
    let under_400 = FinalPass::new(100.0, 400.0, None).apply(bright);
    assert!(under_400[0] > under_300[0]);
    // Light under the knee does not care about the peak.
    let dim = [1.0; 3];
    assert_eq!(
        FinalPass::new(100.0, 300.0, None).apply(dim),
        FinalPass::new(100.0, 400.0, None).apply(dim)
    );
}

#[test]
fn the_hlg_inverse_oetf_has_its_known_values() {
    assert_eq!(hlg_inverse_oetf(0.0), 0.0);
    // the two pieces meet at half signal, where the square law gives 1/12
    assert!((hlg_inverse_oetf(0.5) - 1.0 / 12.0).abs() < 1e-6);
    // full signal is full scene light
    assert!((hlg_inverse_oetf(1.0) - 1.0).abs() < 1e-5);
    // 0.75 signal, from the log piece
    assert!((hlg_inverse_oetf(0.75) - 0.264_94).abs() < 1e-4);
    // out of range clips
    assert_eq!(hlg_inverse_oetf(-1.0), 0.0);
    assert!((hlg_inverse_oetf(2.0) - 1.0).abs() < 1e-5);
}

#[test]
fn the_hlg_inverse_oetf_is_continuous_and_rising() {
    let below = hlg_inverse_oetf(0.5 - 1e-4);
    let above = hlg_inverse_oetf(0.5 + 1e-4);
    assert!((above - below).abs() < 1e-3);
    let mut last = -1.0;
    for step in 0..=1000 {
        let v = hlg_inverse_oetf(step as f32 / 1000.0);
        assert!(v > last, "not rising at {step}");
        last = v;
    }
}

#[test]
fn hlg_reference_white_is_about_203_nits_on_a_1000_nit_display() {
    // BT.2408: signal 0.75 is the reference white, 203 cd/m² on a 1000 cd/m² display
    let white = hlg_display_nits([0.75; 3], BT2020_LUMA);
    for v in white {
        assert!((v - 203.0).abs() < 1.0, "{white:?}");
    }
    // full signal is the display's peak, black is black
    let peak = hlg_display_nits([1.0; 3], BT2020_LUMA);
    for v in peak {
        assert!((v - 1000.0).abs() < 0.1, "{peak:?}");
    }
    assert_eq!(hlg_display_nits([0.0; 3], BT2020_LUMA), [0.0; 3]);
}

#[test]
fn the_hlg_ootf_keeps_the_hue_and_weighs_by_luminance() {
    // the OOTF multiplies the three channels by one gain, so their ratios are the scene's
    let signal = [0.9, 0.5, 0.2];
    let scene = signal.map(hlg_inverse_oetf);
    let nits = hlg_display_nits(signal, BT2020_LUMA);
    for i in 1..3 {
        assert!((nits[i] / nits[0] - scene[i] / scene[0]).abs() < 1e-4);
    }
    // a grey of scene light e shows as 1000 e^1.2, red alone as 1000 y^0.2 e with y = 0.2627 e
    let grey = hlg_display_nits([0.5; 3], BT2020_LUMA)[0];
    assert!((grey - 1000.0 * (1.0f32 / 12.0).powf(1.2)).abs() < 0.01);
    let red = hlg_display_nits([0.5, 0.0, 0.0], BT2020_LUMA);
    let e = 1.0f32 / 12.0;
    assert!((red[0] - 1000.0 * (0.2627 * e).powf(0.2) * e).abs() < 0.01);
    assert_eq!(red[1], 0.0);
}

#[test]
fn an_hlg_surface_is_placed_by_its_absolute_light() {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::Bt2020.chromaticities();
    description.tf = TransferFunction::Named(NamedTf::Hlg);
    description.luminance = Luminance::HLG;
    let conversion = Conversion::to_working(&description, 203.0);
    assert!(matches!(conversion.decode, Curve::Hlg { .. }));
    // reference white lands on SDR white, the peak at 1000/203 of it
    close(conversion.apply([0.75; 3]), [1.0; 3], 6e-3);
    close(conversion.apply([1.0; 3]), [1000.0 / 203.0; 3], 0.01);
    // moving SDR white moves HLG's share of it, as it does PQ's
    let other = Conversion::to_working(&description, 100.0);
    close(other.apply([0.75; 3]), [2.03; 3], 0.02);
    assert_eq!(conversion.apply([0.0; 3]), [0.0; 3]);
}

#[test]
fn hlg_weighs_colors_by_the_luma_of_the_surface_primaries() {
    let mut bt2020 = ImageDescription::srgb();
    bt2020.primaries = NamedPrimaries::Bt2020.chromaticities();
    bt2020.tf = TransferFunction::Named(NamedTf::Hlg);
    let mut srgb = bt2020;
    srgb.primaries = ImageDescription::srgb().primaries;
    let Curve::Hlg { luma: wide } = Curve::of_description(&bt2020) else {
        panic!("not HLG");
    };
    let Curve::Hlg { luma: narrow } = Curve::of_description(&srgb) else {
        panic!("not HLG");
    };
    for (a, b) in wide.iter().zip(BT2020_LUMA) {
        assert!((a - b).abs() < 1e-3);
    }
    assert!((narrow[0] - 0.2126).abs() < 1e-3 && (narrow[1] - 0.7152).abs() < 1e-3);
    // a grey is the same either way
    assert!(
        (Curve::of_description(&bt2020).decode(0.6) - Curve::of_description(&srgb).decode(0.6))
            .abs()
            < 1e-6
    );
}

#[test]
fn an_hlg_surface_on_an_sdr_output_is_scaled_to_its_white() {
    let mut description = ImageDescription::srgb();
    description.tf = TransferFunction::Named(NamedTf::Hlg);
    description.luminance = Luminance::HLG;
    let conversion = Conversion::between(&description, &ImageDescription::srgb()).unwrap();
    // reference white is SDR white (203 cd/m² is the white of the output, see PQ)
    let out = conversion.apply([0.75; 3]);
    let white_encoded = Curve::Srgb.encode(1.0);
    assert!((out[0] - white_encoded).abs() < 0.01, "{out:?}");
}

#[test]
fn the_hlg_conversion_tag_changes_with_the_curve() {
    let mut pq = ImageDescription::srgb();
    pq.tf = TransferFunction::Named(NamedTf::St2084Pq);
    let mut hlg = pq;
    hlg.tf = TransferFunction::Named(NamedTf::Hlg);
    assert_ne!(
        Conversion::to_working(&pq, 203.0).tag(),
        Conversion::to_working(&hlg, 203.0).tag()
    );
    assert_eq!(
        Conversion::to_working(&hlg, 203.0).tag(),
        Conversion::to_working(&hlg, 203.0).tag()
    );
}

#[test]
fn the_hlg_shader_params_are_kind_three_and_the_source_luma() {
    let curve = Curve::Hlg { luma: BT2020_LUMA };
    assert_eq!(curve.shader_params(), [3.0, 0.0]);
    assert_eq!(curve.shader_luma(), BT2020_LUMA);
    assert_eq!(Curve::Pq.shader_luma(), [0.0; 3]);
}

const PEAKS: [f32; 5] = [100.0, 302.0, 600.0, 999.0, 4000.0];

#[test]
fn bt2390_never_exceeds_the_peak_and_never_falls() {
    for peak in PEAKS {
        let mut last = 0.0;
        for step in 0..=24_000 {
            let nits = step as f32 * 0.5;
            let out = bt2390_tone_map(nits, peak);
            assert!(out <= peak, "peak {peak}: {nits} gave {out}");
            assert!(
                out >= last,
                "peak {peak}: fell at {nits}, {last} then {out}"
            );
            last = out;
        }
    }
}

#[test]
fn bt2390_has_its_endpoints() {
    for peak in PEAKS {
        assert_eq!(bt2390_tone_map(0.0, peak), 0.0);
        // the source's own peak lands on the panel's peak, and so does anything above it
        let top = bt2390_tone_map(BT2390_SOURCE_PEAK, peak);
        assert!(
            (top - peak.min(BT2390_SOURCE_PEAK)).abs() < 0.5,
            "peak {peak}: {top}"
        );
        // (the PQ round trip in f32 is good to about 3e-5 of the value)
        assert!((bt2390_tone_map(10_000.0, peak) - peak).abs() < 1e-3 * peak);
    }
    // a negative value is black
    assert_eq!(bt2390_tone_map(-5.0, 302.0), 0.0);
}

#[test]
fn bt2390_is_the_identity_under_its_knee() {
    // Knees in cd/m² for a source of 1000: about 28 at a peak of 100, 163 at 302, 464 at 600.
    for (peak, knee) in [
        (100.0, 27.0),
        (302.0, 162.0),
        (600.0, 463.0),
        (999.0, 997.0),
    ] {
        for step in 0..=100 {
            let nits = knee * step as f32 / 100.0;
            assert_eq!(bt2390_tone_map(nits, peak), nits, "peak {peak}");
        }
        // and it bends above it
        assert!(bt2390_tone_map(knee * 1.5 + 20.0, peak) < knee * 1.5 + 20.0);
    }
}

#[test]
fn bt2390_is_continuous_at_its_knee() {
    // the spline starts on the identity, so the step across the knee is no bigger than the step in the input, up to the f32 round trip
    let peak = 302.0;
    let mut last = bt2390_tone_map(100.0, peak);
    for step in 1..=400 {
        let nits = 100.0 + step as f32 * 0.25;
        let out = bt2390_tone_map(nits, peak);
        assert!(out - last <= 0.25 + 0.02, "jump at {nits}: {last} to {out}");
        last = out;
    }
}

#[test]
fn bt2390_leaves_a_bright_enough_panel_alone() {
    for peak in [1000.0, 1500.0, 4000.0] {
        for nits in [0.0, 100.0, 500.0, 1000.0] {
            assert_eq!(bt2390_tone_map(nits, peak), nits.min(peak));
        }
    }
}

#[test]
fn the_final_pass_follows_the_tone_curve_it_is_given() {
    let knee = FinalPass::new(203.0, 302.0, None);
    assert_eq!(knee.tone, ToneMap::Knee);
    let bt2390 = knee.with_tone_map(ToneMap::Bt2390);
    assert_ne!(knee, bt2390);
    assert_eq!(bt2390.with_tone_map(ToneMap::Knee), knee);
    // under both knees the two agree: 0.25 of SDR white is 51 cd/m²
    let dim = [0.25; 3];
    assert_eq!(knee.apply(dim), bt2390.apply(dim));
    // above them they differ, and neither goes past the panel's peak code
    let bright = [4.0; 3];
    assert_ne!(knee.apply(bright), bt2390.apply(bright));
    let peak_code = pq_encode(302.0 / PQ_PEAK);
    for pass in [knee, bt2390] {
        for v in pass.apply(bright) {
            assert!(v <= peak_code + 1e-4, "{v} over {peak_code}");
        }
    }
}

#[test]
fn the_tone_curve_names_round_trip() {
    for (name, tone) in ToneMap::NAMES
        .into_iter()
        .zip([ToneMap::Knee, ToneMap::Bt2390])
    {
        assert_eq!(ToneMap::from_name(name), Some(tone));
        assert_eq!(tone.name(), name);
    }
    assert_eq!(ToneMap::from_name("BT2390"), None);
    assert_eq!(ToneMap::from_name(""), None);
    assert_eq!(ToneMap::default(), ToneMap::Knee);
    assert_eq!(ToneMap::Knee.shader_param(), 0.0);
    assert_eq!(ToneMap::Bt2390.shader_param(), 1.0);
}
