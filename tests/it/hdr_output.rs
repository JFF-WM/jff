//! Tests for `jff_wm::primitives::hdr_output` and the HDR side of `gamut::Conversion`.

use jff_wm::primitives::color::{
    ContentLight, ImageDescription, Luminance, NamedPrimaries, NamedTf, TransferFunction,
};
use jff_wm::primitives::gamut::{Conversion, Curve};
use jff_wm::primitives::hdr_output::{
    bt2390_source_peak, bt2390_tone_map, fitted_tone_map, frame_content_peak, hlg_display_nits,
    hlg_inverse_oetf, low_headroom, pq_decode, pq_encode, tone_map, FinalPass, ToneMap,
    BT2020_LUMA, BT2390_SOURCE_PEAK, DEFAULT_SDR_WHITE, HDR_PEAK_RANGE, PQ_PEAK, SDR_WHITE_RANGE,
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
            // the PQ round trip in f32 can step back by a few millionths of the peak
            assert!(
                out >= last - 1e-4 * peak,
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
fn bt2390_rolls_a_bright_panel_off_instead_of_clipping() {
    // 4000 cd/m² content on a 1000 cd/m² panel keeps its order under the peak
    let low = bt2390_tone_map(2000.0, 1000.0);
    let mid = bt2390_tone_map(3000.0, 1000.0);
    let top = bt2390_tone_map(4000.0, 1000.0);
    assert!(low < mid && mid < top && top <= 1000.0, "{low} {mid} {top}");
    // the dark half is left alone
    for nits in [0.0, 50.0, 100.0, 203.0] {
        assert_eq!(bt2390_tone_map(nits, 1000.0), nits);
    }
}

#[test]
fn bt2390_leaves_a_panel_at_the_pq_top_alone() {
    for nits in [0.0, 100.0, 1000.0, 9000.0] {
        assert_eq!(bt2390_tone_map(nits, 10_000.0), nits);
    }
}

#[test]
fn the_source_peak_follows_the_panel_from_1000_up() {
    assert_eq!(bt2390_source_peak(302.0), BT2390_SOURCE_PEAK);
    assert_eq!(bt2390_source_peak(999.0), BT2390_SOURCE_PEAK);
    assert_eq!(bt2390_source_peak(1000.0), 4000.0);
    assert_eq!(bt2390_source_peak(4000.0), PQ_PEAK);
    assert_eq!(bt2390_source_peak(PQ_PEAK), PQ_PEAK);
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

// ---- Windows-scRGB (create_windows_scrgb): fp16 games on an HDR output

#[test]
fn extended_linear_is_not_clamped() {
    let curve = Curve::of(TransferFunction::Named(NamedTf::ExtLinear));
    assert_eq!(curve, Curve::Linear);
    assert_eq!(curve.decode(0.5), 0.5);
    assert_eq!(curve.decode(3.0), 3.0);
    assert_eq!(curve.decode(-0.25), -0.25);
    assert_eq!(curve.shader_params(), [4.0, 0.0]);
    assert!(!curve.is_absolute());
    // as an output curve it would only clip, an HDR output is encoded by its final pass
    assert_eq!(curve.encode(2.0), 1.0);
}

#[test]
fn scrgb_reference_white_lands_below_sdr_white_and_highlights_keep_their_nits() {
    let scrgb = ImageDescription::windows_scrgb();
    let conversion = Conversion::to_working(&scrgb, 203.0);
    assert_eq!(conversion.decode, Curve::Linear);
    // 1.0 is 80 cd/m², the working space counts in units of 203 cd/m²
    close(conversion.apply([1.0; 3]), [80.0 / 203.0; 3], 1e-4);
    // 203 cd/m² is 203 / 80 in scRGB, and SDR white in the working space
    close(conversion.apply([203.0 / 80.0; 3]), [1.0; 3], 1e-4);
    // a 1000 cd/m² highlight is not clipped on its way in
    close(
        conversion.apply([1000.0 / 80.0; 3]),
        [1000.0 / 203.0; 3],
        1e-3,
    );
    // moving SDR white moves where scRGB's 1.0 lands, light stays where it is in cd/m²
    let other = Conversion::to_working(&scrgb, 100.0);
    close(other.apply([1.0; 3]), [0.8; 3], 1e-4);
}

#[test]
fn scrgb_colors_outside_the_srgb_gamut_are_not_lost_before_the_matrix() {
    let conversion = Conversion::to_working(&ImageDescription::windows_scrgb(), 203.0);
    // a negative channel is a color outside sRGB, which BT.2020 can hold: nothing is clamped
    // before the matrix, so a saturated green (negative red and blue in sRGB) stays green
    let green = conversion.apply([-0.2, 1.0, -0.1]);
    assert!(green[1] > 0.0);
    assert!(green.iter().all(|v| *v >= 0.0));
    // darkness below zero in every channel ends at zero
    close(conversion.apply([-0.5; 3]), [0.0; 3], 1e-6);
}

#[test]
fn scrgb_is_converted_everywhere_plain_srgb_is_left_alone() {
    let scrgb = ImageDescription::windows_scrgb();
    assert!(!jff_wm::primitives::gamut::is_plain_srgb(&scrgb));
    // on an SDR output it needs a conversion even with the same primaries
    let to_sdr = Conversion::between(&scrgb, &ImageDescription::srgb()).unwrap();
    // scRGB's reference white is the SDR output's white, what is above clips
    close(to_sdr.apply([1.0; 3]), [1.0; 3], 1e-4);
    close(to_sdr.apply([4.0; 3]), [1.0; 3], 1e-4);
    // and the curve is the sRGB one on the way out: linear 0.2140 is about the code 0.5
    close(to_sdr.apply([0.2140; 3]), [0.5; 3], 1e-3);
}

#[test]
fn the_scrgb_conversion_has_its_own_tag() {
    let srgb = Conversion::to_working(&ImageDescription::srgb(), 203.0);
    let scrgb = Conversion::to_working(&ImageDescription::windows_scrgb(), 203.0);
    assert_ne!(srgb.tag(), scrgb.tag());
    assert_ne!(
        scrgb.tag(),
        Conversion::to_working(&ImageDescription::windows_scrgb(), 100.0).tag()
    );
}

// ---- tone mapping fitted to the content's peak ----------------------------------------------

/// A panel of 1000 cd/m² with SDR white at 203, and no gamut to map: only the luminance moves.
fn fitted_panel(content_peak: Option<f32>) -> FinalPass {
    FinalPass::new(203.0, 1000.0, None).with_content_peak(content_peak)
}

/// What a grey of `nits` cd/m² in the working space is shown as, in cd/m².
fn shown(pass: &FinalPass, nits: f32) -> f32 {
    let working = nits / pass.sdr_white;
    pq_decode(pass.apply([working; 3])[0]) * PQ_PEAK
}

fn near(a: f32, b: f32, tolerance: f32) {
    assert!((a - b).abs() <= tolerance, "{a} vs {b}");
}

#[test]
fn content_under_the_panels_peak_is_shown_as_it_is_up_to_its_peak() {
    let pass = fitted_panel(Some(600.0));
    for nits in [0.0, 5.0, 100.0, 203.0, 450.0, 600.0] {
        near(shown(&pass, nits), nits, 0.5 + nits * 1e-3);
    }
    // what the knee would have done to it: it starts at 750, but 900 is above the content's
    // own peak, so nothing in a well-formed picture is there. Light above the panel is clipped.
    near(shown(&pass, 5000.0), 1000.0, 1.0);
}

#[test]
fn content_equal_to_the_panels_peak_is_shown_as_it_is() {
    let pass = fitted_panel(Some(1000.0));
    for nits in [0.0, 100.0, 203.0, 750.0, 900.0, 1000.0] {
        near(shown(&pass, nits), nits, 0.5 + nits * 1e-3);
    }
}

#[test]
fn the_knee_does_not_squeeze_content_that_is_already_fitted() {
    // The knee starts at 0.75 of the peak. Content that says it ends at 350 on a panel of 400
    // is not rolled off, whatever the configured curve is.
    let knee = FinalPass::new(203.0, 400.0, None);
    assert_eq!(knee.tone, ToneMap::Knee);
    let squeezed = shown(&knee, 350.0);
    assert!(squeezed < 345.0, "{squeezed}");
    for pass in [knee, knee.with_tone_map(ToneMap::Bt2390)] {
        let fitted = pass.with_content_peak(Some(350.0));
        near(shown(&fitted, 350.0), 350.0, 0.5);
        near(shown(&fitted, 320.0), 320.0, 0.5);
    }
}

#[test]
fn content_above_the_panels_peak_lands_its_peak_on_the_panels() {
    for content in [1200.0, 4000.0, 10_000.0] {
        let pass = fitted_panel(Some(content));
        near(shown(&pass, content), 1000.0, 1.0);
        // brighter than it says is shown at the peak too
        near(shown(&pass, content * 1.5), 1000.0, 1.0);
        // and it is rolled off, not clipped: the highlights below the content's peak stay apart
        assert!(shown(&pass, content * 0.5) < 1000.0 - 1.0, "{content}");
    }
    // the curve is BT.2390 with the content's peak as the source
    let pass = fitted_panel(Some(4000.0));
    for nits in [300.0, 800.0, 1500.0, 3000.0] {
        near(
            fitted_tone_map(nits, 4000.0, 1000.0),
            bt2390_tone_map_for(nits, 4000.0, 1000.0),
            1e-3,
        );
        near(
            shown(&pass, nits),
            fitted_tone_map(nits, 4000.0, 1000.0),
            1.0 + nits * 1e-3,
        );
    }
}

/// The same curve as `bt2390_tone_map` has for a panel whose default source peak is `source`:
/// 4000 is the default for a panel of exactly 1000.
fn bt2390_tone_map_for(nits: f32, source: f32, peak: f32) -> f32 {
    assert_eq!(bt2390_source_peak(peak), source);
    bt2390_tone_map(nits, peak)
}

#[test]
fn a_fitted_pass_is_monotonic_and_never_over_the_panel() {
    for content in [300.0, 1000.0, 1001.0, 4000.0, 10_000.0] {
        let pass = fitted_panel(Some(content));
        let mut last = -1.0f32;
        let mut nits = 0.0f32;
        while nits <= 12_000.0 {
            let now = shown(&pass, nits);
            // a quarter of a cd/m² of slack: the PQ round trip in f32 is not exact up there
            assert!(now >= last - 0.25, "{content}: {last} then {now} at {nits}");
            assert!(now <= 1000.5, "{content}: {now} at {nits}");
            last = now;
            nits += 25.0;
        }
    }
}

#[test]
fn without_a_content_peak_the_pass_is_what_it_was() {
    for tone in [ToneMap::Knee, ToneMap::Bt2390] {
        for peak in [302.0, 600.0, 1000.0, 4000.0] {
            let old = FinalPass::new(203.0, peak, None).with_tone_map(tone);
            let new = old.with_content_peak(None);
            assert_eq!(old.content_peak, None);
            assert_eq!(old, new);
            for working in [0.0, 0.25, 1.0, 2.0, 4.0, 12.0, 60.0] {
                let rgb = [working, working * 0.5, working * 0.1];
                assert_eq!(old.apply(rgb), new.apply(rgb));
                // and it is the configured curve on the luminance, as before
                let nits = rgb.map(|v| v * 203.0);
                let y =
                    BT2020_LUMA[0] * nits[0] + BT2020_LUMA[1] * nits[1] + BT2020_LUMA[2] * nits[2];
                let mapped = match tone {
                    ToneMap::Knee => tone_map(y, peak),
                    ToneMap::Bt2390 => bt2390_tone_map(y, peak),
                };
                if y > 1e-3 {
                    let expected = nits.map(|v| pq_encode(v * (mapped / y) / PQ_PEAK));
                    for (got, want) in old.apply(rgb).iter().zip(expected) {
                        assert!(
                            (got - want).abs() < 2e-3,
                            "{tone:?} {peak} {rgb:?}: {got} {want}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_content_peak_makes_no_difference_to_what_is_below_the_panels_knee_or_its_own() {
    // The same pass with and without a peak agrees where neither curve touches the light.
    let plain = fitted_panel(None);
    for content in [400.0, 1000.0, 4000.0] {
        let fitted = fitted_panel(Some(content));
        for working in [0.0, 0.1, 0.5, 1.0] {
            let rgb = [working, working, working];
            for (a, b) in plain.apply(rgb).iter().zip(fitted.apply(rgb)) {
                assert!((a - b).abs() < 1e-4, "{content} {working}: {a} {b}");
            }
        }
    }
}

#[test]
fn sdr_content_is_untouched_whatever_the_content_peak() {
    // SDR white and the dark half of the range, on a panel of 1000 cd/m².
    for tone in [ToneMap::Knee, ToneMap::Bt2390] {
        for content in [None, Some(300.0), Some(600.0), Some(1000.0), Some(4000.0)] {
            let pass = fitted_panel(content).with_tone_map(tone);
            for working in [0.0, 0.1, 0.5, 1.0] {
                let coded = pass.apply([working; 3]);
                let expected = pq_encode(working * 203.0 / PQ_PEAK);
                for v in coded {
                    assert!(
                        (v - expected).abs() < 1e-4,
                        "{tone:?} {content:?} {working}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_content_peak_belongs_to_the_pass_and_not_to_its_settings() {
    let pass = FinalPass::new(203.0, 400.0, None);
    assert_eq!(pass.content_peak, None);
    let fitted = pass.with_content_peak(Some(1000.0));
    assert_ne!(pass, fitted);
    assert_eq!(fitted.with_content_peak(None), pass);
    // the other settings are carried over
    assert_eq!(fitted.sdr_white, pass.sdr_white);
    assert_eq!(fitted.peak, pass.peak);
    assert_eq!(fitted.tone, pass.tone);
}

#[test]
fn a_frame_without_a_stated_peak_has_none() {
    assert_eq!(frame_content_peak(1000.0, []), None);
    // SDR surfaces, and light that says nothing, are not metadata
    assert_eq!(
        frame_content_peak(1000.0, [ContentLight::Sdr, ContentLight::Sdr]),
        None
    );
    assert_eq!(
        frame_content_peak(400.0, [ContentLight::Sdr, ContentLight::Unknown]),
        None
    );
}

#[test]
fn a_frame_takes_the_largest_stated_peak() {
    assert_eq!(
        frame_content_peak(1000.0, [ContentLight::Peak(600.0)]),
        Some(600.0)
    );
    assert_eq!(
        frame_content_peak(
            1000.0,
            [
                ContentLight::Sdr,
                ContentLight::Peak(600.0),
                ContentLight::Peak(1500.0),
            ]
        ),
        Some(1500.0)
    );
}

#[test]
fn a_surface_that_says_nothing_counts_as_the_default_source_peak() {
    // Under 1000 the default source is 1000, so a PQ video that says 600 next to a game that
    // says nothing is fitted to 1000, like the game is on its own.
    assert_eq!(
        frame_content_peak(400.0, [ContentLight::Peak(600.0), ContentLight::Unknown]),
        Some(bt2390_source_peak(400.0))
    );
    // From 1000 up it is four times the panel's
    assert_eq!(
        frame_content_peak(1000.0, [ContentLight::Unknown, ContentLight::Peak(600.0)]),
        Some(bt2390_source_peak(1000.0))
    );
    // a stated peak above the default wins
    assert_eq!(
        frame_content_peak(400.0, [ContentLight::Unknown, ContentLight::Peak(4000.0)]),
        Some(4000.0)
    );
    // and none goes over the top of PQ
    assert_eq!(
        frame_content_peak(
            4000.0,
            [ContentLight::Unknown, ContentLight::Peak(10_000.0)]
        ),
        Some(PQ_PEAK)
    );
}

#[test]
fn a_surface_that_says_nothing_is_not_mapped_harder_beside_one_that_does() {
    // A panel of 400: a PQ surface with no metadata alone is mapped with a source of 1000
    // (BT.2390 mode). Beside a video that says 600 the pass is fitted to 1000 too, so its
    // brightest light is shown the same, where fitting to 600 would show it brighter.
    let alone = FinalPass::new(203.0, 400.0, None).with_tone_map(ToneMap::Bt2390);
    let beside = alone.with_content_peak(frame_content_peak(
        400.0,
        [ContentLight::Peak(600.0), ContentLight::Unknown],
    ));
    for nits in [500.0, 800.0, 1000.0] {
        near(shown(&beside, nits), shown(&alone, nits), 0.5);
    }
    let too_hard = alone.with_content_peak(Some(600.0));
    for nits in [400.0, 500.0] {
        assert!(shown(&too_hard, nits) > shown(&alone, nits) + 5.0, "{nits}");
    }
}
