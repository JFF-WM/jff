//! Tests for `jff_wm::primitives::gamut`: the conversion from the colors of a surface to those
//! of an output, and when a surface needs none.

use jff_wm::primitives::color::{
    Chromaticities, ImageDescription, Luminance, NamedPrimaries, NamedTf, RenderIntent,
    TransferFunction,
};
use jff_wm::primitives::gamut::{
    conversion_matrix, invert, is_plain_srgb, map_gamut, mul, rgb_to_xyz, Conversion, Curve,
    GamutMap, Mat3, IDENTITY,
};

fn described(primaries: NamedPrimaries, tf: NamedTf) -> ImageDescription {
    ImageDescription {
        primaries: primaries.chromaticities(),
        tf: TransferFunction::Named(tf),
        ..ImageDescription::srgb()
    }
}

/// The primaries of the AOC AG323FWG3R3, as its EDID gives them: wider than sRGB, and a white
/// that is not quite D65.
fn aoc_panel() -> Chromaticities {
    Chromaticities {
        red: [677_700, 313_400],
        green: [275_300, 647_400],
        blue: [151_300, 64_400],
        white: [313_400, 329_100],
    }
}

/// What a tagged surface needs to be shown on a plain sRGB output.
fn to_srgb(primaries: NamedPrimaries) -> Conversion {
    Conversion::between(
        &described(primaries, NamedTf::Srgb),
        &ImageDescription::srgb(),
    )
    .unwrap()
}

fn close(a: &Mat3, b: &Mat3, tolerance: f64) -> bool {
    a.iter()
        .flatten()
        .zip(b.iter().flatten())
        .all(|(x, y)| (x - y).abs() < tolerance)
}

fn near(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < tolerance)
}

#[test]
fn a_matrix_times_its_inverse_is_the_identity() {
    let m = rgb_to_xyz(&NamedPrimaries::Srgb.chromaticities()).unwrap();
    assert!(close(&mul(&m, &invert(&m).unwrap()), &IDENTITY, 1e-9));
}

#[test]
fn a_matrix_with_no_inverse_is_refused() {
    assert_eq!(
        invert(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [1.0, 0.0, 1.0]]),
        None
    );
}

#[test]
fn srgb_primaries_make_the_known_xyz_matrix() {
    let known = [
        [0.4124, 0.3576, 0.1805],
        [0.2126, 0.7152, 0.0722],
        [0.0193, 0.1192, 0.9505],
    ];
    let m = rgb_to_xyz(&NamedPrimaries::Srgb.chromaticities()).unwrap();
    assert!(close(&m, &known, 1e-3), "{m:?}");
}

#[test]
fn srgb_to_display_p3_is_the_published_matrix() {
    let known = [
        [0.8225, 0.1775, 0.0],
        [0.0332, 0.9668, 0.0],
        [0.0171, 0.0724, 0.9105],
    ];
    let m = conversion_matrix(
        &NamedPrimaries::Srgb.chromaticities(),
        &NamedPrimaries::DisplayP3.chromaticities(),
    )
    .unwrap();
    assert!(close(&m, &known, 1e-3), "{m:?}");
}

#[test]
fn the_same_primaries_give_the_identity() {
    let p3 = NamedPrimaries::DisplayP3.chromaticities();
    assert!(close(
        &conversion_matrix(&p3, &p3).unwrap(),
        &IDENTITY,
        1e-9
    ));
}

#[test]
fn white_maps_to_white_even_when_the_white_points_differ() {
    // DCI-P3 has its own white, the adaptation has to land on D65 exactly.
    let m = conversion_matrix(
        &NamedPrimaries::DciP3.chromaticities(),
        &NamedPrimaries::Srgb.chromaticities(),
    )
    .unwrap();
    for row in m {
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{row:?}");
    }
    // And onto the panel's white, which is not D65.
    let m = conversion_matrix(&NamedPrimaries::DisplayP3.chromaticities(), &aoc_panel()).unwrap();
    for row in m {
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{row:?}");
    }
}

#[test]
fn going_there_and_back_gives_the_same_colors() {
    let srgb = NamedPrimaries::Srgb.chromaticities();
    let dci = NamedPrimaries::DciP3.chromaticities();
    let there = conversion_matrix(&srgb, &dci).unwrap();
    let back = conversion_matrix(&dci, &srgb).unwrap();
    assert!(close(&mul(&there, &back), &IDENTITY, 1e-9));

    let p3 = NamedPrimaries::DisplayP3.chromaticities();
    let there = conversion_matrix(&aoc_panel(), &p3).unwrap();
    let back = conversion_matrix(&p3, &aoc_panel()).unwrap();
    assert!(close(&mul(&there, &back), &IDENTITY, 1e-9));
}

#[test]
fn primaries_on_one_line_make_no_matrix() {
    let flat = Chromaticities {
        red: [200_000, 200_000],
        green: [300_000, 300_000],
        blue: [400_000, 400_000],
        white: [312_700, 329_000],
    };
    assert_eq!(rgb_to_xyz(&flat), None);
    assert_eq!(
        conversion_matrix(&flat, &NamedPrimaries::Srgb.chromaticities()),
        None
    );
}

#[test]
fn the_curves_have_their_known_values() {
    let gamma22 = Curve::of(TransferFunction::Named(NamedTf::Gamma22));
    let gamma28 = Curve::of(TransferFunction::Named(NamedTf::Gamma28));
    assert!((Curve::Srgb.decode(0.5) - 0.214_04).abs() < 1e-4);
    assert!((gamma22.decode(0.5) - 0.217_64).abs() < 1e-4);
    assert!((gamma28.decode(0.5) - 0.143_59).abs() < 1e-4);
    // The straight part near black.
    assert!((Curve::Srgb.decode(0.02) - 0.02 / 12.92).abs() < 1e-6);
}

#[test]
fn a_power_curve_is_the_named_one_with_the_same_exponent() {
    assert_eq!(
        Curve::of(TransferFunction::Named(NamedTf::Gamma22)),
        Curve::of(TransferFunction::Power(22_000))
    );
    assert_eq!(
        Curve::of(TransferFunction::Named(NamedTf::Gamma28)),
        Curve::of(TransferFunction::Power(28_000))
    );
}

#[test]
fn encoding_undoes_decoding() {
    for curve in [
        Curve::Srgb,
        Curve::of(TransferFunction::Named(NamedTf::Gamma22)),
        Curve::of(TransferFunction::Power(18_000)),
    ] {
        assert_eq!(curve.decode(0.0), 0.0);
        assert!((curve.decode(1.0) - 1.0).abs() < 1e-6);
        for step in 0..=20 {
            let v = step as f32 / 20.0;
            assert!(
                (curve.encode(curve.decode(v)) - v).abs() < 1e-4,
                "{curve:?} at {v}"
            );
        }
    }
}

#[test]
fn the_curves_clip_what_is_out_of_range() {
    assert_eq!(Curve::Srgb.decode(-1.0), 0.0);
    assert!((Curve::Srgb.decode(2.0) - 1.0).abs() < 1e-6);
    assert_eq!(Curve::Srgb.encode(-0.5), 0.0);
    assert!((Curve::Srgb.encode(2.0) - 1.0).abs() < 1e-6);
}

#[test]
fn the_shader_gets_the_curve_as_two_numbers() {
    assert_eq!(Curve::Srgb.shader_params(), [1.0, 0.0]);
    assert_eq!(Curve::Power(2.2).shader_params(), [0.0, 2.2]);
}

#[test]
fn plain_srgb_is_told_from_the_rest() {
    assert!(is_plain_srgb(&ImageDescription::srgb()));
    assert!(!is_plain_srgb(&described(
        NamedPrimaries::DisplayP3,
        NamedTf::Srgb
    )));
    assert!(!is_plain_srgb(&described(
        NamedPrimaries::Srgb,
        NamedTf::Gamma22
    )));
    // The luminance range is not part of it.
    let mut brighter = ImageDescription::srgb();
    brighter.luminance.max = 400;
    assert!(is_plain_srgb(&brighter));
}

#[test]
fn plain_srgb_is_never_converted() {
    // Not even for an output with a wide gamut: such a surface is drawn as it always was, so
    // it can still be scanned out.
    let wide = ImageDescription {
        primaries: aoc_panel(),
        ..ImageDescription::srgb()
    };
    assert_eq!(Conversion::between(&ImageDescription::srgb(), &wide), None);
    assert_eq!(
        Conversion::between(&ImageDescription::srgb(), &ImageDescription::srgb()),
        None
    );
}

#[test]
fn a_surface_that_already_has_the_outputs_colors_needs_nothing() {
    let p3 = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    assert_eq!(Conversion::between(&p3, &p3), None);
}

#[test]
fn a_gamut_that_differs_in_the_last_digits_is_the_same_gamut() {
    // sRGB as a profile rounds it.
    let mut source = described(NamedPrimaries::Srgb, NamedTf::Gamma22);
    source.primaries.red = [640_001, 330_000];
    source.primaries.blue = [150_000, 60_001];
    let target = described(NamedPrimaries::Srgb, NamedTf::Gamma22);
    assert_eq!(Conversion::between(&source, &target), None);
}

#[test]
fn a_different_curve_alone_is_a_conversion() {
    // Gamma 2.2 content on an output that follows the sRGB curve: same colors, other curve.
    let source = described(NamedPrimaries::Srgb, NamedTf::Gamma22);
    let target = described(NamedPrimaries::Srgb, NamedTf::Srgb);
    let conversion = Conversion::between(&source, &target).unwrap();
    assert!(near(conversion.apply([0.0; 3]), [0.0; 3], 1e-6));
    assert!(near(conversion.apply([1.0; 3]), [1.0; 3], 1e-5));
    // Dark values differ the most: 0.1 to the power 2.2 is 0.0063 of light, which sRGB
    // encodes as 0.0728.
    assert!(near(conversion.apply([0.1; 3]), [0.0728; 3], 1e-3));
}

#[test]
fn display_p3_on_an_srgb_output_is_pulled_into_the_gamut() {
    let conversion = to_srgb(NamedPrimaries::DisplayP3);
    // A color well inside both gamuts: reference values from the matrices above.
    assert!(near(
        conversion.apply([0.9, 0.3, 0.2]),
        [0.9768, 0.2299, 0.1290],
        1e-3
    ));
}

#[test]
fn greys_stay_grey() {
    let conversion = to_srgb(NamedPrimaries::DisplayP3);
    for level in [0.0, 0.1, 0.5, 0.9, 1.0] {
        assert!(
            near(conversion.apply([level; 3]), [level; 3], 1e-4),
            "grey {level} became {:?}",
            conversion.apply([level; 3])
        );
    }
}

#[test]
fn a_color_outside_the_outputs_gamut_is_clipped_to_its_edge() {
    // Display P3 red is more saturated than sRGB can show: its green and blue come out
    // negative in sRGB and are cut to zero, the red is cut to full.
    let conversion = to_srgb(NamedPrimaries::DisplayP3);
    assert!(near(
        conversion.apply([1.0, 0.0, 0.0]),
        [1.0, 0.0, 0.0],
        1e-5
    ));
    assert!(near(
        conversion.apply([0.0, 1.0, 0.0]),
        [0.0, 1.0, 0.0],
        1e-5
    ));
}

#[test]
fn tagged_srgb_primaries_lose_saturation_on_the_wide_panel() {
    // The reason for managing colors on this monitor: the panel's red is further out than
    // sRGB red, so sRGB red has to be shown as a mix. Sent as it is, it would use the panel's
    // own red and look oversaturated. White stays white.
    let source = described(NamedPrimaries::Srgb, NamedTf::Gamma22);
    let panel = ImageDescription {
        primaries: aoc_panel(),
        tf: TransferFunction::Named(NamedTf::Gamma22),
        ..ImageDescription::srgb()
    };
    let conversion = Conversion::between(&source, &panel).unwrap();
    let red = conversion.apply([1.0, 0.0, 0.0]);
    assert!(red[0] > 0.9 && red[0] < 1.0, "{red:?}");
    assert!(red[1] > 0.1 && red[1] < 0.3, "{red:?}");
    assert!(red[2] > 0.05 && red[2] < 0.2, "{red:?}");
    assert!(near(conversion.apply([1.0; 3]), [1.0; 3], 1e-4));
}

#[test]
fn the_tag_follows_the_conversion() {
    let p3 = to_srgb(NamedPrimaries::DisplayP3);
    let again = to_srgb(NamedPrimaries::DisplayP3);
    let bt2020 = to_srgb(NamedPrimaries::Bt2020);
    assert_eq!(p3.tag(), again.tag());
    assert_ne!(p3.tag(), bt2020.tag());
}

// ---- plain sRGB, and what happens to colors the output cannot show --------------------------

fn wide_panel() -> ImageDescription {
    ImageDescription {
        primaries: aoc_panel(),
        ..ImageDescription::srgb()
    }
}

#[test]
fn plain_srgb_is_converted_when_asked_to() {
    // The oversaturation of sRGB content on a wide-gamut panel: its red is the panel's own, far
    // more saturated red unless it is converted.
    let conversion = Conversion::to_output(
        &ImageDescription::srgb(),
        &wide_panel(),
        RenderIntent::Perceptual,
        true,
    )
    .unwrap();
    let red = conversion.apply([1.0, 0.0, 0.0]);
    assert!(red[0] > 0.9 && red[0] < 1.0, "{red:?}");
    assert!(red[1] > 0.05, "{red:?}");
    assert!(red[2] > 0.02, "{red:?}");
    assert!(near(conversion.apply([1.0; 3]), [1.0; 3], 1e-4));
    assert!(near(conversion.apply([0.0; 3]), [0.0; 3], 1e-6));
    // Not asked to: drawn as it always was.
    assert_eq!(
        Conversion::to_output(
            &ImageDescription::srgb(),
            &wide_panel(),
            RenderIntent::Perceptual,
            false
        ),
        None
    );
}

#[test]
fn plain_srgb_on_an_srgb_monitor_needs_nothing_even_when_asked() {
    let srgb = ImageDescription::srgb();
    assert_eq!(
        Conversion::to_output(&srgb, &srgb, RenderIntent::Perceptual, true),
        None
    );
}

fn light_luminance(conversion: &Conversion, encoded: [f32; 3]) -> f32 {
    let light = encoded.map(|v| conversion.encode.decode(v));
    conversion.luma[0] * light[0] + conversion.luma[1] * light[1] + conversion.luma[2] * light[2]
}

#[test]
fn perceptual_keeps_the_luminance_of_a_color_it_has_to_pull_in() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let target = ImageDescription::srgb();
    let compress =
        Conversion::to_output(&source, &target, RenderIntent::Perceptual, false).unwrap();
    let clip = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    // Display P3 red in linear sRGB is (1.2249, -0.0421, -0.0197): luminance 0.2289.
    let wanted = 0.2289;
    let kept = light_luminance(&compress, compress.apply([1.0, 0.0, 0.0]));
    let clipped = light_luminance(&clip, clip.apply([1.0, 0.0, 0.0]));
    assert!((kept - wanted).abs() < 2e-3, "{kept}");
    assert!((clipped - wanted).abs() > 1e-2, "{clipped}");
    // Every channel is inside the gamut afterwards, and it is less saturated than clipping gives.
    let out = compress.apply([1.0, 0.0, 0.0]);
    assert!(out.iter().all(|v| (0.0..=1.0).contains(v)), "{out:?}");
    assert!(out[1] > 0.0 || out[2] > 0.0, "{out:?}");
}

#[test]
fn both_intents_agree_on_colors_that_fit() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let target = ImageDescription::srgb();
    let compress =
        Conversion::to_output(&source, &target, RenderIntent::Perceptual, false).unwrap();
    let clip = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    for color in [
        [0.9, 0.3, 0.2],
        [0.5, 0.5, 0.5],
        [0.5, 0.4, 0.45],
        [0.0; 3],
        [1.0; 3],
    ] {
        assert!(
            near(compress.apply(color), clip.apply(color), 1e-5),
            "{color:?}"
        );
    }
}

#[test]
fn gamut_mapping_always_lands_inside_and_leaves_what_fits_alone() {
    let luma = [0.2126, 0.7152, 0.0722];
    let steps = [-0.4f32, -0.1, 0.0, 0.3, 0.7, 1.0, 1.2, 1.6];
    for r in steps {
        for g in steps {
            for b in steps {
                for map in [GamutMap::Clip, GamutMap::Compress] {
                    let out = map_gamut([r, g, b], luma, map);
                    assert!(
                        out.iter().all(|v| (0.0..=1.0).contains(v)),
                        "{map:?} {r} {g} {b} gave {out:?}"
                    );
                    if [r, g, b].iter().all(|v| (0.0..=1.0).contains(v)) {
                        assert!(near(out, [r, g, b], 1e-6), "{map:?} moved {r} {g} {b}");
                    }
                }
            }
        }
    }
}

#[test]
fn compressing_moves_a_color_toward_its_grey_and_not_past_it() {
    let luma = [0.2126, 0.7152, 0.0722];
    let out = map_gamut([1.2, -0.1, 0.1], luma, GamutMap::Compress);
    let y = 0.2126 * 1.2 - 0.7152 * 0.1 + 0.0722 * 0.1;
    // Red stays the biggest channel and green the smallest: the hue does not turn.
    assert!(out[0] > out[2] && out[2] > out[1], "{out:?}");
    // The first channel to leave the gamut ends exactly on its edge.
    assert!(out[0] <= 1.0 + 1e-6 && out[1] >= -1e-6, "{out:?}");
    let kept = luma[0] * out[0] + luma[1] * out[1] + luma[2] * out[2];
    assert!((kept - y).abs() < 1e-5, "{kept} vs {y}");
}

#[test]
fn the_tag_follows_the_gamut_mapping() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let target = ImageDescription::srgb();
    let compress =
        Conversion::to_output(&source, &target, RenderIntent::Perceptual, false).unwrap();
    let clip = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    assert_ne!(compress.tag(), clip.tag());
}

fn row_sums(conversion: &Conversion) -> [f32; 3] {
    conversion.rows.map(|row| row.iter().sum())
}

#[test]
fn saturation_keeps_the_primaries_pure() {
    let mut source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    source.tf = TransferFunction::Named(NamedTf::Gamma22);
    let target = ImageDescription::srgb();
    let saturated =
        Conversion::to_output(&source, &target, RenderIntent::Saturation, false).unwrap();
    // The matrix is left out: a pure primary is the output's own, where relative would pull it in.
    assert_eq!(
        saturated.rows,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    );
    assert_eq!(saturated.map, GamutMap::Compress);
    assert!(near(
        saturated.apply([1.0, 0.0, 0.0]),
        [1.0, 0.0, 0.0],
        1e-5
    ));
    let relative = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    assert!(relative.rows[1][0] != 0.0);
    // Nothing to do at all when the curves agree too.
    let same_curve = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    assert!(Conversion::to_output(&same_curve, &target, RenderIntent::Saturation, false).is_none());
}

#[test]
fn absolute_is_relative_for_a_description_of_primaries_and_a_curve() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let target = ImageDescription::srgb();
    let relative = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    let absolute = Conversion::to_output(&source, &target, RenderIntent::Absolute, false).unwrap();
    assert_eq!(relative.rows, absolute.rows);
    assert_eq!(relative.map, absolute.map);
    assert_eq!(relative.black, absolute.black);
}

#[test]
fn absolute_without_adaptation_does_not_move_the_white_point() {
    let mut source = ImageDescription::srgb();
    source.primaries = aoc_panel();
    let target = ImageDescription::srgb();
    let relative = Conversion::to_output(&source, &target, RenderIntent::Relative, true).unwrap();
    let kept =
        Conversion::to_output(&source, &target, RenderIntent::AbsoluteNoAdaptation, true).unwrap();
    // Relative maps the white of the source to the white of the output.
    assert!(
        near(row_sums(&relative), [1.0; 3], 1e-4),
        "{:?}",
        row_sums(&relative)
    );
    // Its white point is not D65: unadapted it comes out tinted, but still near white.
    let sums = row_sums(&kept);
    assert!(!near(sums, [1.0; 3], 1e-4), "{sums:?}");
    assert!(near(sums, [1.0; 3], 0.05), "{sums:?}");
    assert_eq!(kept.map, GamutMap::Clip);
    // With the same white point there is nothing to leave out.
    let p3 = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let a = Conversion::to_output(&p3, &target, RenderIntent::Relative, false).unwrap();
    let b = Conversion::to_output(&p3, &target, RenderIntent::AbsoluteNoAdaptation, false).unwrap();
    for (x, y) in a.rows.iter().flatten().zip(b.rows.iter().flatten()) {
        assert!((x - y).abs() < 1e-4);
    }
}

#[test]
fn black_point_compensation_puts_the_source_black_on_the_output_black() {
    let mut source = ImageDescription::srgb();
    source.luminance = Luminance {
        min: 0,
        max: 80,
        reference: 80,
    };
    // Same primaries and curve: relative has nothing to do, so the surface is shown as it is.
    let target = ImageDescription::srgb();
    assert!(Conversion::to_output(&source, &target, RenderIntent::Relative, true).is_none());
    let bpc = Conversion::to_output(&source, &target, RenderIntent::RelativeBpc, true).unwrap();
    // 0.2 cd/m² on 80: the output's black is 0.0025 of its white, which is code 0.0323 in sRGB.
    let black = bpc.apply([0.0; 3]);
    assert!(near(black, [0.0323; 3], 2e-3), "{black:?}");
    assert!(near(bpc.apply([1.0; 3]), [1.0; 3], 1e-5));
    // Monotonic between the two.
    assert!(bpc.apply([0.2; 3])[0] > 0.2);
}

#[test]
fn black_point_compensation_leaves_alone_what_has_the_output_black() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Srgb);
    let target = ImageDescription::srgb();
    let relative = Conversion::to_output(&source, &target, RenderIntent::Relative, false).unwrap();
    let bpc = Conversion::to_output(&source, &target, RenderIntent::RelativeBpc, false).unwrap();
    assert_eq!(bpc.black, [1.0, 0.0]);
    assert_eq!(bpc.rows, relative.rows);
    assert_eq!(bpc.apply([0.0; 3]), [0.0; 3]);
    assert!(near(
        bpc.apply([0.9, 0.3, 0.2]),
        relative.apply([0.9, 0.3, 0.2]),
        1e-6
    ));
}

#[test]
fn intents_that_convert_differently_make_their_own_tag() {
    let source = described(NamedPrimaries::DisplayP3, NamedTf::Gamma22);
    let target = ImageDescription::srgb();
    let tags: Vec<i32> = [
        RenderIntent::Perceptual,
        RenderIntent::Relative,
        RenderIntent::Saturation,
    ]
    .iter()
    .map(|&intent| {
        Conversion::to_output(&source, &target, intent, false)
            .unwrap()
            .tag()
    })
    .collect();
    for (i, a) in tags.iter().enumerate() {
        for b in &tags[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

// ---- the mastering display as the target volume --------------------------------------------

use jff_wm::primitives::gamut::{edge_distance, knee, soften, target_reach, KNEE_START};

const SRGB_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// BT.2020 content that says it was mastered on a Display P3 monitor.
fn mastered_on_p3() -> ImageDescription {
    ImageDescription {
        mastering_primaries: Some(NamedPrimaries::DisplayP3.chromaticities()),
        ..described(NamedPrimaries::Bt2020, NamedTf::Gamma22)
    }
}

fn srgb_gamma22() -> ImageDescription {
    described(NamedPrimaries::Srgb, NamedTf::Gamma22)
}

#[test]
fn the_distance_to_the_edge_is_zero_on_grey_and_one_on_the_edge() {
    // white too, where the distance would be 0 over 0
    for grey in [0.4, 0.0, 1.0] {
        assert_eq!(edge_distance([grey; 3], SRGB_LUMA), 0.0);
    }
    // a channel at 0 or at 1 is on the edge
    assert!((edge_distance([1.0, 0.2, 0.3], SRGB_LUMA) - 1.0).abs() < 1e-6);
    assert!((edge_distance([0.6, 0.3, 0.0], SRGB_LUMA) - 1.0).abs() < 1e-6);
    // inside is less, outside is more
    assert!(edge_distance([0.6, 0.4, 0.3], SRGB_LUMA) < 1.0);
    assert!(edge_distance([1.2, 0.3, 0.3], SRGB_LUMA) > 1.0);
    assert!(edge_distance([0.6, 0.3, -0.1], SRGB_LUMA) > 1.0);
}

#[test]
fn a_color_that_cannot_be_brought_in_is_infinitely_far() {
    // no luminance and a negative channel: nothing to move toward
    assert!(edge_distance([0.0, 0.0, -0.1], [0.0, 0.0, 0.0]).is_infinite());
    // white and above: the grey is already past the edge
    assert!(edge_distance([2.0, 1.2, 1.0], [0.5, 0.5, 0.5]).is_infinite());
}

#[test]
fn the_knee_leaves_the_near_alone_and_ends_the_reach_on_the_edge() {
    for distance in [0.0, 0.3, KNEE_START] {
        assert_eq!(knee(distance, 1.4), distance);
    }
    assert!((knee(1.4, 1.4) - 1.0).abs() < 1e-6);
    // in between it rises, slower than the color moves
    let (a, b) = (knee(1.0, 1.4), knee(1.2, 1.4));
    assert!(KNEE_START < a && a < b && b < 1.0, "{a} {b}");
    assert!(a < 1.0, "the edge itself is eased in");
    // a target that fits needs nothing
    assert_eq!(knee(0.95, 1.0), 0.95);
    assert_eq!(knee(0.95, 0.0), 0.95);
}

#[test]
fn soften_keeps_colors_that_are_far_from_the_edge_and_all_greys() {
    let reach = 1.5;
    for light in [[0.5, 0.4, 0.3], [0.2; 3], [0.0; 3], [1.0; 3]] {
        assert_eq!(soften(light, SRGB_LUMA, reach), light);
    }
    // not above 1 is no knee, wherever the color is
    let near_edge = [0.95, 0.3, 0.02];
    assert_eq!(soften(near_edge, SRGB_LUMA, 1.0), near_edge);
    assert_eq!(soften(near_edge, SRGB_LUMA, 0.0), near_edge);
    // and one that cannot be brought in is left for the hard mapping
    let hopeless = [0.0, 0.0, -0.1];
    assert_eq!(soften(hopeless, [0.0; 3], reach), hopeless);
}

#[test]
fn soften_keeps_the_luminance_and_the_hue_of_what_it_moves() {
    let light = [1.1, 0.35, 0.1];
    let softened = soften(light, SRGB_LUMA, 1.6);
    assert_ne!(softened, light);
    let y = |c: [f32; 3]| SRGB_LUMA[0] * c[0] + SRGB_LUMA[1] * c[1] + SRGB_LUMA[2] * c[2];
    assert!((y(softened) - y(light)).abs() < 1e-5);
    // moved along the line to its grey: each channel's offset from the grey shrinks by one factor
    let grey = y(light);
    let factors: Vec<f32> = (0..3)
        .map(|i| (softened[i] - grey) / (light[i] - grey))
        .collect();
    assert!(
        factors.iter().all(|f| (f - factors[0]).abs() < 1e-4),
        "{factors:?}"
    );
    assert!(factors[0] > 0.0 && factors[0] < 1.0);
}

#[test]
fn a_target_that_fits_the_output_has_a_reach_of_one_or_less() {
    // sRGB-mastered BT.2020 content on a Display P3 output: the target is well inside
    let source = ImageDescription {
        mastering_primaries: Some(NamedPrimaries::Srgb.chromaticities()),
        ..described(NamedPrimaries::Bt2020, NamedTf::Gamma22)
    };
    let p3 = NamedPrimaries::DisplayP3.chromaticities();
    let matrix = conversion_matrix(&NamedPrimaries::Bt2020.chromaticities(), &p3).unwrap();
    let luma = rgb_to_xyz(&p3).unwrap()[1].map(|v| v as f32);
    assert!(target_reach(&source, &matrix, luma).unwrap() <= 1.0);
    // and so the conversion has no knee at all
    let output = described(NamedPrimaries::DisplayP3, NamedTf::Gamma22);
    let conversion =
        Conversion::to_output(&source, &output, RenderIntent::Perceptual, false).unwrap();
    assert_eq!(conversion.reach, 0.0);
}

#[test]
fn a_target_wider_than_the_output_reaches_past_its_edge() {
    let source = mastered_on_p3();
    let matrix = conversion_matrix(
        &NamedPrimaries::Bt2020.chromaticities(),
        &NamedPrimaries::Srgb.chromaticities(),
    )
    .unwrap();
    let reach = target_reach(&source, &matrix, SRGB_LUMA).unwrap();
    // what the P3 corners are in sRGB, worked out the other way round
    let p3_to_srgb = conversion_matrix(
        &NamedPrimaries::DisplayP3.chromaticities(),
        &NamedPrimaries::Srgb.chromaticities(),
    )
    .unwrap();
    let corners = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
    ];
    let expected = corners
        .into_iter()
        .map(|corner: [f64; 3]| {
            let light = [0, 1, 2]
                .map(|row| (0..3).map(|k| p3_to_srgb[row][k] * corner[k]).sum::<f64>() as f32);
            edge_distance(light, SRGB_LUMA)
        })
        .fold(0.0f32, f32::max);
    assert!(reach > 1.1, "{reach}");
    assert!(
        (reach - expected).abs() < 1e-3,
        "{reach} against {expected}"
    );
}

#[test]
fn a_surface_with_no_target_has_no_reach() {
    let matrix = conversion_matrix(
        &NamedPrimaries::Bt2020.chromaticities(),
        &NamedPrimaries::Srgb.chromaticities(),
    )
    .unwrap();
    let plain = described(NamedPrimaries::Bt2020, NamedTf::Gamma22);
    assert_eq!(target_reach(&plain, &matrix, SRGB_LUMA), None);
}

#[test]
fn the_perceptual_intents_use_the_target_volume_and_the_others_do_not() {
    let target = srgb_gamma22();
    let conversion =
        Conversion::to_output(&mastered_on_p3(), &target, RenderIntent::Perceptual, false).unwrap();
    assert!(conversion.reach > 1.1, "{}", conversion.reach);
    // Saturation takes the container's primaries as the output's own, and a P3 target is within
    // BT.2020 to a rounding: nothing to ease in.
    let conversion =
        Conversion::to_output(&mastered_on_p3(), &target, RenderIntent::Saturation, false).unwrap();
    assert_eq!(conversion.map, GamutMap::Compress);
    assert!(conversion.reach < 1.1, "{}", conversion.reach);
    for intent in [
        RenderIntent::Relative,
        RenderIntent::Absolute,
        RenderIntent::RelativeBpc,
        RenderIntent::AbsoluteNoAdaptation,
    ] {
        let conversion = Conversion::to_output(&mastered_on_p3(), &target, intent, false).unwrap();
        assert_eq!(conversion.reach, 0.0, "{intent:?}");
    }
    // the same content without the metadata is mapped as it always was
    let plain = described(NamedPrimaries::Bt2020, NamedTf::Gamma22);
    let conversion =
        Conversion::to_output(&plain, &target, RenderIntent::Perceptual, false).unwrap();
    assert_eq!(conversion.reach, 0.0);
}

#[test]
fn the_corners_of_the_target_end_inside_the_output() {
    let target = srgb_gamma22();
    let source = mastered_on_p3();
    let with = Conversion::to_output(&source, &target, RenderIntent::Perceptual, false).unwrap();
    let without = Conversion::to_output(
        &ImageDescription {
            mastering_primaries: None,
            ..source
        },
        &target,
        RenderIntent::Perceptual,
        false,
    )
    .unwrap();
    // P3 red, as the BT.2020 container holds it: the knee eases it in, so it is less
    // saturated than when only what leaves the gamut is brought back.
    let p3_red_in_bt2020 = {
        let m = mul(
            &invert(&rgb_to_xyz(&NamedPrimaries::Bt2020.chromaticities()).unwrap()).unwrap(),
            &rgb_to_xyz(&NamedPrimaries::DisplayP3.chromaticities()).unwrap(),
        );
        let linear = [m[0][0] as f32, m[1][0] as f32, m[2][0] as f32];
        linear.map(|v| Curve::Power(2.2).encode(v.max(0.0)))
    };
    let a = with.apply(p3_red_in_bt2020);
    let b = without.apply(p3_red_in_bt2020);
    assert!(a.iter().all(|v| (0.0..=1.0).contains(v)), "{a:?}");
    assert!(
        a[1] > b[1] - 1e-6,
        "green of the eased color is not less: {a:?} {b:?}"
    );
    // and a color well inside sRGB is as it was
    let inside = [0.5, 0.45, 0.4];
    let (x, y) = (with.apply(inside), without.apply(inside));
    assert!(near(x, y, 1e-6), "{x:?} {y:?}");
}

#[test]
fn the_tag_follows_the_reach() {
    let target = srgb_gamma22();
    let with =
        Conversion::to_output(&mastered_on_p3(), &target, RenderIntent::Perceptual, false).unwrap();
    let mut other = with;
    other.reach = with.reach + 0.5;
    assert_ne!(with.tag(), other.tag());
}

#[test]
fn the_shader_has_the_knee_the_rust_has() {
    let shader = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/render/effects/shaders/rounded.frag"
    ));
    assert!(
        shader.contains(&format!("const float CM_KNEE = {KNEE_START:?};")),
        "rounded.frag and KNEE_START disagree"
    );
    assert!(shader.contains("uniform float cm_reach;"));
}

#[test]
fn the_gamut_map_names_round_trip() {
    for name in GamutMap::NAMES {
        assert_eq!(GamutMap::from_name(name).unwrap().name(), name);
    }
    assert_eq!(GamutMap::from_name("clip"), Some(GamutMap::Clip));
    assert_eq!(GamutMap::from_name("compress"), Some(GamutMap::Compress));
    assert_eq!(GamutMap::from_name("perceptual"), None);
    assert_eq!(GamutMap::from_name("Clip"), None);
}
