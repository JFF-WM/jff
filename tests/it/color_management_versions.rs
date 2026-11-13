//! Tests for what versions 2 and 3 of wp-color-management-v1 change in `jff_wm::primitives::color`.

use jff_wm::primitives::color::{
    short_identity, split_identity, supported_features, ContentLight, CreateError, CreatorError,
    Feature, ImageDescription, LongIdentities, Luminance, NamedPrimaries, NamedTf, ParamsBuilder,
    TransferFunction, SUPPORTED_FEATURES,
};

const SRGB: u32 = 1;
const BT2020: u32 = 6;
const PQ: u32 = 11;
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

fn pq_builder() -> ParamsBuilder {
    let mut builder = ParamsBuilder::default();
    builder.set_tf_named(PQ).unwrap();
    builder.set_primaries_named(BT2020).unwrap();
    builder
}

// ---- versions 2 and 3 of the protocol -----------------------------------------------------------

/// The numbers jff sends are the protocol's, in the bindings it generates from the XML at version 3.
#[test]
fn the_version_2_and_3_numbers_are_the_protocols() {
    use jff_wm::protocol::color_management_bindings::server::wp_color_manager_v1;

    assert_eq!(
        u32::from(wp_color_manager_v1::Feature::WindowsBt2100),
        Feature::WindowsBt2100.raw()
    );
    assert_eq!(
        u32::from(wp_color_manager_v1::Feature::WindowsScrgb),
        Feature::WindowsScrgb.raw()
    );
    assert_eq!(
        u32::from(wp_color_manager_v1::TransferFunction::CompoundPower24),
        14
    );
    assert_eq!(
        u32::from(wp_color_manager_v1::TransferFunction::Srgb),
        NamedTf::Srgb.raw()
    );
}

#[test]
fn features_follow_the_version_the_client_bound() {
    assert_eq!(supported_features(1), SUPPORTED_FEATURES);
    assert_eq!(supported_features(2), SUPPORTED_FEATURES);
    let three = supported_features(3);
    assert!(three.contains(&Feature::WindowsBt2100));
    assert!(SUPPORTED_FEATURES.iter().all(|f| three.contains(f)));
    assert_eq!(Feature::WindowsBt2100.raw(), 8);
    assert_eq!(Feature::WindowsBt2100.since(), 3);
    assert_eq!(Feature::WindowsScrgb.since(), 1);
}

#[test]
fn the_piecewise_srgb_curve_is_compound_power_2_4_from_version_2() {
    // version 1: `srgb` is the name, 14 is not a transfer function it knows
    assert_eq!(NamedTf::from_raw_in(9, 1), Some(NamedTf::Srgb));
    assert_eq!(NamedTf::from_raw_in(14, 1), None);
    assert_eq!(NamedTf::Srgb.wire_raw(1), 9);
    // version 2 and 3: 14 is the same curve, and `srgb` was not advertised
    for version in [2, 3] {
        assert_eq!(NamedTf::from_raw_in(14, version), Some(NamedTf::Srgb));
        assert_eq!(NamedTf::from_raw_in(9, version), None);
        assert_eq!(NamedTf::Srgb.wire_raw(version), 14);
        // the others are as they were
        assert_eq!(NamedTf::from_raw_in(2, version), Some(NamedTf::Gamma22));
        assert_eq!(NamedTf::from_raw_in(11, version), Some(NamedTf::St2084Pq));
        assert_eq!(NamedTf::St2084Pq.wire_raw(version), 11);
    }
    // ext_linear (5) is still not something a creator can name
    assert_eq!(NamedTf::from_raw_in(5, 2), None);
}

#[test]
fn a_version_2_creator_takes_compound_power_2_4_and_refuses_srgb() {
    let mut builder = ParamsBuilder::for_version(2);
    assert_eq!(error_of(builder.set_tf_named(9)), CreatorError::InvalidTf);
    builder.set_tf_named(14).unwrap();
    builder.set_primaries_named(SRGB).unwrap();
    // the same description as a version 1 client's `srgb`
    assert_eq!(builder.build().unwrap(), srgb_builder().build().unwrap());
    // and 14 means nothing to a version 1 creator
    let mut v1 = ParamsBuilder::for_version(1);
    assert_eq!(error_of(v1.set_tf_named(14)), CreatorError::InvalidTf);
    // the default builder is version 1
    let mut default = ParamsBuilder::default();
    assert_eq!(error_of(default.set_tf_named(14)), CreatorError::InvalidTf);
    default.set_tf_named(9).unwrap();
}

#[test]
fn version_2_does_not_hold_light_levels_to_a_range() {
    // sRGB goes up to 80 cd/m²: version 1 refuses a level of 1000, version 2 takes it
    let mut v1 = ParamsBuilder::for_version(1);
    v1.set_tf_named(SRGB_TF).unwrap();
    v1.set_primaries_named(SRGB).unwrap();
    v1.set_max_cll(1000);
    assert_eq!(protocol_error(v1.build()), CreatorError::InvalidLuminance);

    let mut v2 = ParamsBuilder::for_version(2);
    v2.set_tf_named(14).unwrap();
    v2.set_primaries_named(SRGB).unwrap();
    v2.set_max_cll(1000);
    v2.set_max_fall(500);
    let described = v2.build().unwrap();
    assert_eq!(
        (described.max_cll, described.max_fall),
        (Some(1000), Some(500))
    );

    // a level under the minimum is fine too, and PQ above its range is clamped for the tone mapping
    let mut pq = ParamsBuilder::for_version(3);
    pq.set_tf_named(PQ).unwrap();
    pq.set_primaries_named(BT2020).unwrap();
    pq.set_max_cll(20_000);
    assert_eq!(pq.build().unwrap().content_peak(), Some(10_000.0));

    // but a frame average above the content light level is an error in every version
    for version in [1, 2, 3] {
        let mut swapped = ParamsBuilder::for_version(version);
        swapped.set_tf_named(PQ).unwrap();
        swapped.set_primaries_named(BT2020).unwrap();
        swapped.set_max_cll(400);
        swapped.set_max_fall(600);
        assert_eq!(
            protocol_error(swapped.build()),
            CreatorError::InvalidLuminance
        );
    }
}

#[test]
fn windows_bt2100_is_pq_in_bt2020_with_the_default_pq_luminances() {
    let described = ImageDescription::windows_bt2100();
    assert_eq!(described.primaries, NamedPrimaries::Bt2020.chromaticities());
    assert_eq!(described.tf, TransferFunction::Named(NamedTf::St2084Pq));
    assert_eq!(described.luminance, Luminance::PQ);
    assert_eq!(described.luminance.reference, 203);
    assert_eq!(
        (
            described.max_cll,
            described.max_fall,
            described.mastering_luminance
        ),
        (None, None, None)
    );
    // it says nothing about its peak, so it is as bright as PQ allows
    assert_eq!(described.content_light(), ContentLight::Unknown);
    // and it is what a client would get by naming the same things
    assert_eq!(described, pq_builder().build().unwrap());
}

#[test]
fn identities_are_64_bits_never_zero_and_never_given_twice() {
    let mut ids = LongIdentities::new();
    assert_eq!(ids.next(), Some(1));
    assert_eq!(ids.next(), Some(2));
    assert_eq!(LongIdentities::starting_at(0).next(), Some(1));
    // past 32 bits, where the 32-bit numbers ran out
    let mut past = LongIdentities::starting_at(u64::from(u32::MAX));
    assert_eq!(past.next(), Some(u64::from(u32::MAX)));
    assert_eq!(past.next(), Some(u64::from(u32::MAX) + 1));
    let mut last = LongIdentities::starting_at(u64::MAX);
    assert_eq!(last.next(), Some(u64::MAX));
    assert_eq!(last.next(), None);
}

#[test]
fn a_64_bit_identity_is_two_halves_and_a_version_1_client_sees_the_low_one() {
    assert_eq!(split_identity(1), (0, 1));
    assert_eq!(split_identity(0x0000_0002_0000_0003), (2, 3));
    assert_eq!(split_identity(u64::MAX), (u32::MAX, u32::MAX));
    let (hi, lo) = split_identity(0xDEAD_BEEF_0123_4567);
    assert_eq!((u64::from(hi) << 32) | u64::from(lo), 0xDEAD_BEEF_0123_4567);

    assert_eq!(short_identity(1), Some(1));
    assert_eq!(short_identity(u64::from(u32::MAX)), Some(u32::MAX));
    // it does not fit, and a truncated number could be zero or somebody else's
    assert_eq!(short_identity(u64::from(u32::MAX) + 1), None);
    assert_eq!(short_identity(0), None);
}
