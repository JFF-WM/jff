//! Tests for `jff_wm::primitives::color`: what jff offers clients, how parametric image
//! descriptions are checked, and what an information object tells.

use jff_wm::primitives::color::{
    Chromaticities, ContentLight, CreateError, CreatorError, Feature, Identities, ImageDescription,
    InfoEvent, Luminance, MasteringLuminance, NamedPrimaries, NamedTf, ParamsBuilder, RenderIntent,
    TransferFunction, SUPPORTED_FEATURES, SUPPORTED_INTENTS, SUPPORTED_PRIMARIES, SUPPORTED_TFS,
};

const SRGB: u32 = 1;
const BT2020: u32 = 6;
const PQ: u32 = 11;
const HLG: u32 = 13;
const GAMMA22: u32 = 2;
const DISPLAY_P3: u32 = 9;
const SRGB_TF: u32 = 9;

fn protocol_error(result: Result<ImageDescription, CreateError>) -> CreatorError {
    match result {
        Err(CreateError::Protocol(violation)) => violation.error,
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

fn error_of(result: Result<(), jff_wm::primitives::color::Violation>) -> CreatorError {
    result.expect_err("expected a protocol error").error
}

fn srgb_builder() -> ParamsBuilder {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(SRGB_TF).unwrap();
    builder.set_primaries_named(SRGB).unwrap();
    builder
}

/// Twice the area of the triangle of the primaries, a measure of the size of a gamut.
fn gamut_size(c: Chromaticities) -> i64 {
    let [rx, ry] = c.red.map(i64::from);
    let [gx, gy] = c.green.map(i64::from);
    let [bx, by] = c.blue.map(i64::from);
    ((gx - rx) * (by - ry) - (gy - ry) * (bx - rx)).abs()
}

#[test]
fn offered_values_have_the_protocol_numbers() {
    assert_eq!(RenderIntent::Perceptual.raw(), 0);
    assert_eq!(RenderIntent::Relative.raw(), 1);
    assert_eq!(RenderIntent::from_raw(2), None); // saturation
    assert_eq!(RenderIntent::from_raw(3), None); // absolute
    assert!(SUPPORTED_INTENTS.contains(&RenderIntent::Perceptual));

    assert_eq!(
        SUPPORTED_FEATURES
            .iter()
            .map(|f| f.raw())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 7]
    );
    // No ICC (0) and no extended target volume (6), Windows-scRGB (7) is offered.
    assert!(SUPPORTED_FEATURES.contains(&Feature::Parametric));
    assert!(SUPPORTED_FEATURES.contains(&Feature::WindowsScrgb));

    let primaries: Vec<u32> = SUPPORTED_PRIMARIES.iter().map(|p| p.raw()).collect();
    assert_eq!(primaries, [1, 3, 4, 6, 8, 9, 10]);
    for raw in primaries {
        assert_eq!(NamedPrimaries::from_raw(raw).unwrap().raw(), raw);
    }
    // PAL-M, generic film and CIE XYZ are in the protocol, not offered.
    for raw in [0, 2, 5, 7, 11] {
        assert_eq!(NamedPrimaries::from_raw(raw), None);
    }

    let tfs: Vec<u32> = SUPPORTED_TFS.iter().map(|tf| tf.raw()).collect();
    assert_eq!(tfs, [9, 2, 3, 11, 13]);
    for raw in tfs {
        assert_eq!(NamedTf::from_raw(raw).unwrap().raw(), raw);
    }
    // BT.1886, linear, log and the rest are not offered. Extended linear (5) is not one a client
    // can name either: only the Windows-scRGB description has it.
    for raw in [0, 1, 4, 5, 6, 7, 8, 10, 12, 14] {
        assert_eq!(NamedTf::from_raw(raw), None);
    }
    assert_eq!(NamedTf::ExtLinear.raw(), 5);
    assert!(!SUPPORTED_TFS.contains(&NamedTf::ExtLinear));
}

/// The numbers jff sends are the protocol's: if a wayland-protocols update renumbers the feature
/// or the curve, `supported_feature` would announce something else.
#[test]
fn the_scrgb_numbers_are_the_protocols() {
    use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1;

    assert_eq!(
        u32::from(wp_color_manager_v1::Feature::WindowsScrgb),
        Feature::WindowsScrgb.raw()
    );
    assert_eq!(
        u32::from(wp_color_manager_v1::TransferFunction::ExtLinear),
        NamedTf::ExtLinear.raw()
    );
}

#[test]
fn windows_scrgb_is_srgb_primaries_with_an_extended_linear_curve() {
    let scrgb = ImageDescription::windows_scrgb();
    let srgb = ImageDescription::srgb();
    assert_eq!(scrgb.primaries, srgb.primaries);
    assert_eq!(scrgb.tf, TransferFunction::Named(NamedTf::ExtLinear));
    // 1.0 is 80 cd/m², and that is also the reference white
    assert_eq!(scrgb.luminance.reference, 80);
    assert_eq!(scrgb.luminance, Luminance::SCRGB);
    assert_eq!((scrgb.max_cll, scrgb.max_fall), (None, None));
    assert_ne!(scrgb, srgb);
    assert_eq!(NamedTf::ExtLinear.name(), "ext_linear");
}

#[test]
fn the_scrgb_description_tells_what_it_is() {
    let events = ImageDescription::windows_scrgb().info();
    assert!(events.contains(&InfoEvent::TfNamed(NamedTf::ExtLinear.raw())));
    assert!(events.contains(&InfoEvent::PrimariesNamed(SRGB)));
    assert!(events.contains(&InfoEvent::Luminances {
        min: Luminance::SCRGB.min,
        max: Luminance::SCRGB.max,
        reference: 80,
    }));
}

#[test]
fn the_named_primaries_are_distinct_real_gamuts() {
    for (i, a) in NamedPrimaries::ALL.into_iter().enumerate() {
        a.chromaticities().check().unwrap();
        for b in NamedPrimaries::ALL.into_iter().skip(i + 1) {
            assert_ne!(a.chromaticities(), b.chromaticities(), "{a:?} and {b:?}");
        }
        assert_eq!(NamedPrimaries::detect(&a.chromaticities()), Some(a));
    }
}

#[test]
fn the_gamuts_grow_the_way_the_standards_say() {
    let size = |p: NamedPrimaries| gamut_size(p.chromaticities());
    assert!(size(NamedPrimaries::Srgb) < size(NamedPrimaries::DisplayP3));
    assert!(size(NamedPrimaries::DisplayP3) < size(NamedPrimaries::Bt2020));
    assert!(size(NamedPrimaries::Srgb) < size(NamedPrimaries::AdobeRgb));
    // The two P3 sets share the primaries and differ in the white point only.
    let display = NamedPrimaries::DisplayP3.chromaticities();
    let dci = NamedPrimaries::DciP3.chromaticities();
    assert_eq!(
        (display.red, display.green, display.blue),
        (dci.red, dci.green, dci.blue)
    );
    assert_ne!(display.white, dci.white);
    // sRGB is D65 (0.3127, 0.3290).
    assert_eq!(
        NamedPrimaries::Srgb.chromaticities().white,
        [312_700, 329_000]
    );
}

#[test]
fn named_srgb_is_the_default_description() {
    assert_eq!(srgb_builder().build().unwrap(), ImageDescription::srgb());
    assert_eq!(
        ImageDescription::srgb().luminance,
        Luminance {
            min: 2_000,
            max: 80,
            reference: 80
        }
    );
}

#[test]
fn a_description_needs_a_transfer_function_and_primaries() {
    assert_eq!(
        protocol_error(ParamsBuilder::default().build()),
        CreatorError::IncompleteSet
    );
    let mut only_tf = ParamsBuilder::default();
    only_tf.set_tf_named(SRGB_TF).unwrap();
    assert_eq!(protocol_error(only_tf.build()), CreatorError::IncompleteSet);
    let mut only_primaries = ParamsBuilder::default();
    only_primaries.set_primaries_named(SRGB).unwrap();
    assert_eq!(
        protocol_error(only_primaries.build()),
        CreatorError::IncompleteSet
    );
}

#[test]
fn each_property_is_set_once() {
    let mut builder = srgb_builder();
    assert_eq!(
        error_of(builder.set_tf_named(GAMMA22)),
        CreatorError::AlreadySet
    );
    assert_eq!(
        error_of(builder.set_tf_power(22_000)),
        CreatorError::AlreadySet
    );
    assert_eq!(
        error_of(builder.set_primaries_named(DISPLAY_P3)),
        CreatorError::AlreadySet
    );
    assert_eq!(
        error_of(builder.set_primaries([0; 8])),
        CreatorError::AlreadySet
    );
    builder.set_luminances(2_000, 300, 203).unwrap();
    assert_eq!(
        error_of(builder.set_luminances(2_000, 300, 203)),
        CreatorError::AlreadySet
    );
}

#[test]
fn names_that_were_not_offered_are_refused() {
    let mut builder = ParamsBuilder::default();
    for raw in [0, 1, 12, 14, 99] {
        assert_eq!(error_of(builder.set_tf_named(raw)), CreatorError::InvalidTf);
    }
    for raw in [0, 2, 7, 11, 99] {
        assert_eq!(
            error_of(builder.set_primaries_named(raw)),
            CreatorError::InvalidPrimariesNamed
        );
    }
    // A refused value sets nothing.
    builder.set_tf_named(GAMMA22).unwrap();
    builder.set_primaries_named(DISPLAY_P3).unwrap();
}

#[test]
fn a_power_curve_has_an_exponent_from_one_to_ten() {
    for exponent in [0, 9_999, 100_001, u32::MAX] {
        let mut builder = ParamsBuilder::default();
        assert_eq!(
            error_of(builder.set_tf_power(exponent)),
            CreatorError::InvalidTf
        );
    }
    for exponent in [10_000, 22_000, 100_000] {
        let mut builder = ParamsBuilder::default();
        builder.set_tf_power(exponent).unwrap();
        builder.set_primaries_named(SRGB).unwrap();
        assert_eq!(
            builder.build().unwrap().tf,
            TransferFunction::Power(exponent)
        );
    }
}

#[test]
fn luminances_must_rise_above_the_minimum() {
    let mut builder = srgb_builder();
    // min 0.5 cd/m² (5000): a maximum or reference of 0 is not above it, 1 is.
    assert_eq!(
        error_of(builder.set_luminances(5_000, 0, 80)),
        CreatorError::InvalidLuminance
    );
    assert_eq!(
        error_of(builder.set_luminances(5_000, 80, 0)),
        CreatorError::InvalidLuminance
    );
    // A refused request sets nothing, so a correct one still works.
    builder.set_luminances(5_000, 1, 1).unwrap();
    let described = builder.build().unwrap();
    assert_eq!(
        described.luminance,
        Luminance {
            min: 5_000,
            max: 1,
            reference: 1
        }
    );
}

#[test]
fn luminance_defaults_to_sdr_and_big_numbers_do_not_overflow() {
    assert_eq!(srgb_builder().build().unwrap().luminance, Luminance::SDR);
    let mut builder = srgb_builder();
    builder
        .set_luminances(u32::MAX, u32::MAX, u32::MAX)
        .unwrap();
    let mut builder = srgb_builder();
    assert_eq!(
        error_of(builder.set_luminances(u32::MAX, 429_496, 429_496)),
        CreatorError::InvalidLuminance
    );
}

const BT2020_PRIMARIES: [i32; 8] = [
    708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000,
];

#[test]
fn mastering_metadata_is_taken_once_each() {
    let mut builder = srgb_builder();
    builder
        .set_mastering_display_primaries(BT2020_PRIMARIES)
        .unwrap();
    builder.set_mastering_luminance(50, 1_000).unwrap();
    assert_eq!(
        error_of(builder.set_mastering_display_primaries(BT2020_PRIMARIES)),
        CreatorError::AlreadySet
    );
    assert_eq!(
        error_of(builder.set_mastering_luminance(50, 1_000)),
        CreatorError::AlreadySet
    );
    // The primaries are a hint only, and the container's are the description's. The luminance is
    // kept (it used to be thrown away, so this compared equal to a plain description).
    let described = builder.build().unwrap();
    assert_eq!(
        described.mastering_luminance,
        Some(MasteringLuminance {
            min: 50,
            max: 1_000
        })
    );
    assert_eq!(
        ImageDescription {
            mastering_luminance: None,
            ..described
        },
        srgb_builder().build().unwrap()
    );
}

#[test]
fn mastering_luminance_needs_a_maximum_above_the_minimum() {
    let mut builder = srgb_builder();
    // min is cd/m² times 10000: 100 cd/m² against a maximum of 100
    assert_eq!(
        error_of(builder.set_mastering_luminance(1_000_000, 100)),
        CreatorError::InvalidLuminance
    );
    assert_eq!(
        error_of(builder.set_mastering_luminance(0, 0)),
        CreatorError::InvalidLuminance
    );
    builder.set_mastering_luminance(0, 1).unwrap();
}

#[test]
fn nonsense_mastering_primaries_fail_the_description_gracefully() {
    let mut builder = srgb_builder();
    builder
        .set_mastering_display_primaries([0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    assert!(matches!(
        builder.build(),
        Err(jff_wm::primitives::color::CreateError::Unsupported(_))
    ));
}

#[test]
fn light_levels_follow_the_version_1_rules() {
    // Not set, or 0 (unknown): nothing to check.
    assert_eq!(srgb_builder().build().unwrap().max_cll, None);
    let mut unknown = srgb_builder();
    unknown.set_max_cll(0);
    unknown.set_max_fall(0);
    let described = unknown.build().unwrap();
    assert_eq!((described.max_cll, described.max_fall), (None, None));

    // Within the 0.2 to 80 cd/m² an sRGB description has.
    let mut within = srgb_builder();
    within.set_max_cll(80);
    within.set_max_fall(50);
    let described = within.build().unwrap();
    assert_eq!(
        (described.max_cll, described.max_fall),
        (Some(80), Some(50))
    );

    // Above the maximum.
    let mut above = srgb_builder();
    above.set_max_cll(1000);
    assert_eq!(
        protocol_error(above.build()),
        CreatorError::InvalidLuminance
    );

    // Frame average above the content light level.
    let mut swapped = srgb_builder();
    swapped.set_max_cll(40);
    swapped.set_max_fall(60);
    assert_eq!(
        protocol_error(swapped.build()),
        CreatorError::InvalidLuminance
    );

    // Setting a level again replaces it.
    let mut again = srgb_builder();
    again.set_max_cll(1000);
    again.set_max_cll(60);
    assert_eq!(again.build().unwrap().max_cll, Some(60));
}

#[test]
fn custom_primaries_that_match_a_name_are_that_name() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(GAMMA22).unwrap();
    builder
        .set_primaries(NamedPrimaries::DisplayP3.chromaticities().to_array())
        .unwrap();
    let described = builder.build().unwrap();
    assert_eq!(described.named_primaries(), Some(NamedPrimaries::DisplayP3));

    // The panel of the test monitor (from its EDID) is not a named set.
    let mut panel = ParamsBuilder::default();
    panel.set_tf_named(GAMMA22).unwrap();
    panel
        .set_primaries([
            677_700, 313_400, 275_300, 647_400, 151_300, 64_400, 313_400, 329_100,
        ])
        .unwrap();
    assert_eq!(panel.build().unwrap().named_primaries(), None);
}

#[test]
fn primaries_that_cannot_be_drawn_fail_without_a_protocol_error() {
    let cases: [[i32; 8]; 5] = [
        [0; 8],
        // All three on one line.
        [
            100_000, 100_000, 200_000, 200_000, 300_000, 300_000, 312_700, 329_000,
        ],
        // x + y above 1.
        [
            900_000, 900_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000,
        ],
        // Negative.
        [
            -1, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000,
        ],
        // White point at y = 0.
        [
            640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 0,
        ],
    ];
    for values in cases {
        let mut builder = ParamsBuilder::default();
        builder.set_tf_named(SRGB_TF).unwrap();
        builder.set_primaries(values).unwrap();
        assert!(
            matches!(builder.build(), Err(CreateError::Unsupported(_))),
            "{values:?}"
        );
    }
}

#[test]
fn creator_errors_have_the_protocol_values() {
    let codes = [
        CreatorError::IncompleteSet,
        CreatorError::AlreadySet,
        CreatorError::UnsupportedFeature,
        CreatorError::InvalidTf,
        CreatorError::InvalidPrimariesNamed,
        CreatorError::InvalidLuminance,
    ]
    .map(CreatorError::code);
    assert_eq!(codes, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn info_for_a_named_description_has_the_numbers_and_the_names() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(GAMMA22).unwrap();
    builder.set_primaries_named(DISPLAY_P3).unwrap();
    builder.set_max_cll(60);
    let info = builder.build().unwrap().info();
    let p3 = NamedPrimaries::DisplayP3.chromaticities().to_array();
    assert_eq!(
        info,
        [
            InfoEvent::Primaries(p3),
            InfoEvent::PrimariesNamed(DISPLAY_P3),
            InfoEvent::TfNamed(GAMMA22),
            InfoEvent::Luminances {
                min: 2_000,
                max: 80,
                reference: 80
            },
            InfoEvent::TargetPrimaries(p3),
            InfoEvent::TargetLuminance {
                min: 2_000,
                max: 80
            },
            InfoEvent::TargetMaxCll(60),
        ]
    );
}

#[test]
fn info_for_a_power_curve_and_unnamed_primaries_has_no_names() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_power(24_000).unwrap();
    let values = [
        677_700, 313_400, 275_300, 647_400, 151_300, 64_400, 313_400, 329_100,
    ];
    builder.set_primaries(values).unwrap();
    let info = builder.build().unwrap().info();
    assert_eq!(info[0], InfoEvent::Primaries(values));
    assert_eq!(info[1], InfoEvent::TfPower(24_000));
    assert!(!info
        .iter()
        .any(|e| matches!(e, InfoEvent::PrimariesNamed(_) | InfoEvent::TfNamed(_))));
    assert_eq!(
        info.last(),
        Some(&InfoEvent::TargetLuminance {
            min: 2_000,
            max: 80
        })
    );
}

#[test]
fn identity_numbers_are_never_zero_or_repeated() {
    let mut ids = Identities::new();
    assert_eq!(ids.next(), Some(1));
    assert_eq!(ids.next(), Some(2));
    // Zero is not a valid identity, also when asked to start there.
    assert_eq!(Identities::starting_at(0).next(), Some(1));
    let mut last = Identities::starting_at(u32::MAX - 1);
    assert_eq!(last.next(), Some(u32::MAX - 1));
    assert_eq!(last.next(), Some(u32::MAX));
    assert_eq!(last.next(), None);
    assert_eq!(last.next(), None);
}

#[test]
fn pq_gets_its_own_default_luminances() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(11).unwrap();
    builder.set_primaries_named(9).unwrap();
    let description = builder.build().unwrap();
    assert_eq!(description.luminance, Luminance::PQ);
    assert_eq!(
        (
            Luminance::PQ.min,
            Luminance::PQ.max,
            Luminance::PQ.reference
        ),
        (50, 10_000, 203)
    );
}

#[test]
fn hlg_gets_its_own_default_luminances() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(13).unwrap();
    builder.set_primaries_named(6).unwrap();
    let description = builder.build().unwrap();
    assert_eq!(description.tf, TransferFunction::Named(NamedTf::Hlg));
    assert_eq!(description.luminance, Luminance::HLG);
    assert_eq!(
        (
            Luminance::HLG.min,
            Luminance::HLG.max,
            Luminance::HLG.reference
        ),
        (50, 1_000, 203)
    );
    assert_eq!(NamedTf::Hlg.name(), "hlg");
}

#[test]
fn an_hlg_description_may_set_its_own_luminances() {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(13).unwrap();
    builder.set_primaries_named(6).unwrap();
    builder.set_luminances(100, 600, 203).unwrap();
    let luminance = builder.build().unwrap().luminance;
    assert_eq!(
        (luminance.min, luminance.max, luminance.reference),
        (100, 600, 203)
    );
    // the same checks as for any other curve
    let mut bad = ParamsBuilder::default();
    bad.set_tf_named(13).unwrap();
    assert_eq!(
        error_of(bad.set_luminances(2_000_000, 100, 203)),
        CreatorError::InvalidLuminance
    );
}

// ---- mastering luminance and the content peak ---------------------------------------------

fn pq_builder() -> ParamsBuilder {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(PQ).unwrap();
    builder.set_primaries_named(BT2020).unwrap();
    builder
}

#[test]
fn a_description_with_mastering_luminance_is_still_copy_eq_and_hash() {
    fn needs<T: Copy + Eq + std::hash::Hash>() {}
    needs::<ImageDescription>();
    needs::<MasteringLuminance>();
}

#[test]
fn the_builder_keeps_the_mastering_luminance() {
    let mut builder = pq_builder();
    builder
        .set_mastering_display_primaries(BT2020_PRIMARIES)
        .unwrap();
    builder.set_mastering_luminance(50, 1_000).unwrap();
    let described = builder.build().unwrap();
    assert_eq!(
        described.mastering_luminance,
        Some(MasteringLuminance {
            min: 50,
            max: 1_000
        })
    );
    // not set: nothing kept, and a description without it is another one
    let plain = pq_builder().build().unwrap();
    assert_eq!(plain.mastering_luminance, None);
    assert_ne!(plain, described);
    assert_eq!(ImageDescription::srgb().mastering_luminance, None);
    assert_eq!(ImageDescription::windows_scrgb().mastering_luminance, None);
}

#[test]
fn bad_mastering_metadata_is_still_refused() {
    // luminance: the maximum has to be above the minimum, and nothing is kept for a refused one
    let mut builder = pq_builder();
    assert_eq!(
        error_of(builder.set_mastering_luminance(1_000_000, 100)),
        CreatorError::InvalidLuminance
    );
    assert_eq!(builder.build().unwrap().mastering_luminance, None);
    // primaries: nonsense ones fail the description, however good the luminance is
    let mut bad = pq_builder();
    bad.set_mastering_luminance(50, 1_000).unwrap();
    bad.set_mastering_display_primaries([0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    assert!(matches!(bad.build(), Err(CreateError::Unsupported(_))));
}

#[test]
fn the_content_peak_is_max_cll_then_the_mastering_maximum_then_unknown() {
    // max_cll wins over the mastering luminance, even when it is the lower one
    let mut both = pq_builder();
    both.set_mastering_luminance(50, 1_000).unwrap();
    both.set_max_cll(600);
    let described = both.build().unwrap();
    assert_eq!(described.content_peak(), Some(600.0));
    assert_eq!(described.content_light(), ContentLight::Peak(600.0));

    // no max_cll: the mastering maximum
    let mut mastering = pq_builder();
    mastering.set_mastering_luminance(50, 1_000).unwrap();
    assert_eq!(mastering.build().unwrap().content_peak(), Some(1_000.0));

    // a max_cll of 0 is "unknown" (build drops it), so the mastering maximum is used
    let mut zero = pq_builder();
    zero.set_max_cll(0);
    zero.set_mastering_luminance(50, 4_000).unwrap();
    assert_eq!(zero.build().unwrap().content_peak(), Some(4_000.0));

    // a max_cll alone
    let mut cll = pq_builder();
    cll.set_max_cll(800);
    assert_eq!(cll.build().unwrap().content_peak(), Some(800.0));

    // neither: no peak, and the light is as bright as PQ allows
    let neither = pq_builder().build().unwrap();
    assert_eq!(neither.content_peak(), None);
    assert_eq!(neither.content_light(), ContentLight::Unknown);
}

#[test]
fn the_content_peak_stays_inside_what_pq_carries() {
    let mut builder = pq_builder();
    builder.set_mastering_luminance(0, 20_000).unwrap();
    assert_eq!(builder.build().unwrap().content_peak(), Some(10_000.0));
}

#[test]
fn only_pq_has_a_content_peak() {
    // sRGB with metadata (the protocol allows it) is bounded by its own white: nothing to fit
    let mut srgb = srgb_builder();
    srgb.set_mastering_luminance(50, 1_000).unwrap();
    srgb.set_max_cll(80);
    let described = srgb.build().unwrap();
    assert_eq!(described.content_peak(), None);
    assert_eq!(described.content_light(), ContentLight::Sdr);

    let mut gamma = ParamsBuilder::default();
    gamma.set_tf_power(24_000).unwrap();
    gamma.set_primaries_named(SRGB).unwrap();
    assert_eq!(gamma.build().unwrap().content_light(), ContentLight::Sdr);

    // HLG and scRGB can be as bright as they like and say nothing: unknown, but not a peak
    let mut hlg = ParamsBuilder::default();
    hlg.set_tf_named(HLG).unwrap();
    hlg.set_primaries_named(BT2020).unwrap();
    hlg.set_mastering_luminance(50, 600).unwrap();
    let hlg = hlg.build().unwrap();
    assert_eq!(hlg.content_peak(), None);
    assert_eq!(hlg.content_light(), ContentLight::Unknown);
    let scrgb = ImageDescription::windows_scrgb();
    assert_eq!(scrgb.content_peak(), None);
    assert_eq!(scrgb.content_light(), ContentLight::Unknown);
    assert_eq!(ImageDescription::srgb().content_light(), ContentLight::Sdr);
}

#[test]
fn info_reports_the_mastering_luminance_as_the_target_luminance() {
    let mut builder = pq_builder();
    builder.set_mastering_luminance(50, 1_000).unwrap();
    builder.set_max_cll(600);
    let info = builder.build().unwrap().info();
    assert!(info.contains(&InfoEvent::Luminances {
        min: 50,
        max: 10_000,
        reference: 203
    }));
    assert!(info.contains(&InfoEvent::TargetLuminance {
        min: 50,
        max: 1_000
    }));
    assert!(!info.contains(&InfoEvent::TargetLuminance {
        min: 50,
        max: 10_000
    }));
    assert!(info.contains(&InfoEvent::TargetMaxCll(600)));
    // not set: the primary volume's luminance, as it was
    let plain = pq_builder().build().unwrap().info();
    assert!(plain.contains(&InfoEvent::TargetLuminance {
        min: 50,
        max: 10_000
    }));
}
