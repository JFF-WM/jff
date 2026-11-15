//! Tests for `jff_wm::protocol::color_representation`: the protocol's numbers and what jff
//! tells clients it supports.

use jff_wm::protocol::color_representation::{
    format_is_yuv, pixel_format_conflict, supported_coefficients, AlphaMode, ChromaLocation,
    Coefficients, ColorRepresentation, Range, DRIVER_COEFFICIENTS, SUPPORTED_ALPHA_MODES,
};
use smithay::backend::allocator::Fourcc;

#[test]
fn alpha_modes_have_the_protocol_values() {
    assert_eq!(AlphaMode::PremultipliedElectrical.raw(), 0);
    assert_eq!(AlphaMode::PremultipliedOptical.raw(), 1);
    assert_eq!(AlphaMode::Straight.raw(), 2);
    for raw in 0..=2 {
        assert_eq!(AlphaMode::from_raw(raw).unwrap().raw(), raw);
    }
    assert_eq!(AlphaMode::from_raw(3), None);
}

#[test]
fn coefficients_ranges_and_chroma_locations_round_trip() {
    // Zero is invalid for all three, and so is anything past the last value.
    assert_eq!(Coefficients::from_raw(0), None);
    assert_eq!(Coefficients::from_raw(9), None);
    for raw in 1..=8 {
        assert_eq!(Coefficients::from_raw(raw).unwrap().raw(), raw);
    }
    assert_eq!(Range::from_raw(0), None);
    assert_eq!(Range::from_raw(3), None);
    for raw in 1..=2 {
        assert_eq!(Range::from_raw(raw).unwrap().raw(), raw);
    }
    assert_eq!(ChromaLocation::from_raw(0), None);
    assert_eq!(ChromaLocation::from_raw(7), None);
    for raw in 1..=6 {
        assert_eq!(ChromaLocation::from_raw(raw).unwrap().raw(), raw);
    }
    assert_eq!(Coefficients::from_raw(2), Some(Coefficients::Bt709));
    assert_eq!(Coefficients::from_raw(6), Some(Coefficients::Bt2020));
    assert_eq!(Range::from_raw(2), Some(Range::Limited));
}

#[test]
fn a_surface_with_nothing_set_is_premultiplied_and_leaves_the_rest_to_the_compositor() {
    let none = ColorRepresentation::default();
    assert_eq!(none.alpha_mode, AlphaMode::PremultipliedElectrical);
    assert_eq!(none.coefficients, None);
    assert_eq!(none.chroma_location, None);
}

#[test]
fn every_alpha_mode_is_advertised() {
    // The default one is what unset means, the shader turns the other two into it.
    assert!(SUPPORTED_ALPHA_MODES.contains(&AlphaMode::default()));
    assert!(SUPPORTED_ALPHA_MODES.contains(&AlphaMode::PremultipliedOptical));
    assert!(SUPPORTED_ALPHA_MODES.contains(&AlphaMode::Straight));
    assert_eq!(SUPPORTED_ALPHA_MODES.len(), 3);
}

#[test]
fn without_yuv_only_rgb_is_advertised() {
    assert_eq!(
        supported_coefficients(false),
        [(Coefficients::Identity, Range::Full)]
    );
}

#[test]
fn with_yuv_the_common_matrices_come_in_both_ranges() {
    let pairs = supported_coefficients(true);
    assert!(pairs.contains(&(Coefficients::Identity, Range::Full)));
    for coefficients in [
        Coefficients::Bt709,
        Coefficients::Bt601,
        Coefficients::Bt2020,
    ] {
        assert!(pairs.contains(&(coefficients, Range::Limited)));
        assert!(pairs.contains(&(coefficients, Range::Full)));
    }
    // Not drawn right by a plain matrix, so not offered.
    assert!(pairs
        .iter()
        .all(|(c, _)| c.luma_weights().is_some() || *c == Coefficients::Identity));
    assert!(!pairs
        .iter()
        .any(|(c, _)| matches!(c, Coefficients::Ictcp | Coefficients::Bt2020Cl)));
    // Identity only as RGB: it has no limited range.
    assert!(!pairs.contains(&(Coefficients::Identity, Range::Limited)));
}

#[test]
fn luma_weights_sum_below_one_and_the_driver_default_is_known() {
    for raw in 1..=8 {
        let coefficients = Coefficients::from_raw(raw).unwrap();
        if let Some((kr, kb)) = coefficients.luma_weights() {
            assert!(kr > 0.0 && kb > 0.0 && kr + kb < 1.0, "{coefficients:?}");
        }
    }
    assert_eq!(Coefficients::Bt709.luma_weights(), Some((0.2126, 0.0722)));
    assert_eq!(DRIVER_COEFFICIENTS, (Coefficients::Bt601, Range::Limited));
}

#[test]
fn yuv_formats_are_told_from_rgb_ones() {
    for yuv in [Fourcc::Nv12, Fourcc::Yuyv, Fourcc::Yuv420, Fourcc::P010] {
        assert!(format_is_yuv(yuv), "{yuv:?}");
    }
    for rgb in [
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
        Fourcc::Abgr8888,
        Fourcc::Abgr2101010,
        Fourcc::Rgb565,
    ] {
        assert!(!format_is_yuv(rgb), "{rgb:?}");
    }
}

#[test]
fn a_matrix_has_to_fit_the_buffer_format() {
    let identity = Some((Coefficients::Identity.raw(), Range::Full.raw()));
    let bt709 = Some((Coefficients::Bt709.raw(), Range::Limited.raw()));
    // A YCbCr matrix is for YUV buffers, `identity` for RGB ones.
    assert!(!pixel_format_conflict(true, bt709));
    assert!(pixel_format_conflict(false, bt709));
    assert!(!pixel_format_conflict(false, identity));
    assert!(pixel_format_conflict(true, identity));
    // Nothing set, nothing to contradict, whatever the buffer is.
    assert!(!pixel_format_conflict(true, None));
    assert!(!pixel_format_conflict(false, None));
}
