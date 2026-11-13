//! Tests for `jff_wm::primitives::hdr_capture`.

use jff_wm::primitives::hdr_capture::{half_to_f32, pq_picture};
use jff_wm::primitives::hdr_output::{pq_encode, tone_map, FinalPass, ToneMap};

const ONE: u16 = 0x3C00;
const FOUR: u16 = 0x4400;

#[test]
fn half_floats_read_back_as_they_were_written() {
    assert_eq!(half_to_f32(0x0000), 0.0);
    assert_eq!(half_to_f32(ONE), 1.0);
    assert_eq!(half_to_f32(0x3800), 0.5);
    assert_eq!(half_to_f32(0xC000), -2.0);
    assert_eq!(half_to_f32(0x7BFF), 65504.0);
}

#[test]
fn the_smallest_half_is_a_subnormal() {
    assert!((half_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-14);
}

#[test]
fn infinity_and_nan_stay_what_they_are() {
    assert_eq!(half_to_f32(0x7C00), f32::INFINITY);
    assert_eq!(half_to_f32(0xFC00), f32::NEG_INFINITY);
    assert!(half_to_f32(0x7E00).is_nan());
}

#[test]
fn sdr_white_lands_on_its_pq_code() {
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture(&pass, &[ONE, ONE, ONE, ONE]);
    let want = pq_encode(203.0 / 10_000.0);
    for code in picture.codes {
        assert!((code - want).abs() < 1e-4, "{code} vs {want}");
    }
}

#[test]
fn black_is_the_bottom_of_the_signal_and_alpha_is_ignored() {
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture(&pass, &[0, 0, 0, ONE]);
    assert_eq!(picture.codes.len(), 3);
    assert!(picture.codes.iter().all(|c| *c < 1e-5));
    assert!(picture.max_nits < 0.01);
}

#[test]
fn the_picture_has_the_tone_curve_of_the_monitor() {
    // 4.0 is 812 cd/m² at an SDR white of 203, which a 1000 cd/m² panel rolls off
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture(&pass, &[FOUR, FOUR, FOUR, ONE]);
    assert!((picture.max_nits - tone_map(812.0, 1000.0)).abs() < 1.0);
    let bt2390 = pq_picture(
        &pass.with_tone_map(ToneMap::Bt2390),
        &[FOUR, FOUR, FOUR, ONE],
    );
    assert!(bt2390.max_nits <= 1000.0);
}

#[test]
fn a_big_frame_comes_out_the_same_whichever_thread_did_the_pixel() {
    let pass = FinalPass::new(203.0, 1000.0, None);
    let frame: Vec<u16> = (0..100_003).flat_map(|_| [ONE, 0, FOUR, ONE]).collect();
    let picture = pq_picture(&pass, &frame);
    assert_eq!(picture.codes.len(), 100_003 * 3);
    let first = &picture.codes[..3];
    assert!(picture.codes.chunks(3).all(|px| px == first));
}

#[test]
fn overflow_and_nan_do_not_leave_the_signal_range() {
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture(&pass, &[0x7C00, 0x7E00, 0xFC00, ONE]);
    assert!(picture
        .codes
        .iter()
        .all(|c| c.is_finite() && (0.0..=1.0).contains(c)));
}

#[test]
fn an_empty_frame_is_an_empty_picture() {
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture(&pass, &[]);
    assert!(picture.codes.is_empty());
    assert_eq!(picture.max_nits, 0.0);
}

#[test]
fn corners_are_cut_by_the_alpha_and_nothing_else() {
    use jff_wm::primitives::hdr_capture::corner_coverage;
    // a 20 by 20 window with a radius of 6: the very corner is gone, the middle and the edges are not
    assert!(corner_coverage(0, 0, 20, 20, 6) < 0.05);
    assert!(corner_coverage(19, 19, 20, 20, 6) < 0.05);
    assert_eq!(corner_coverage(10, 10, 20, 20, 6), 1.0);
    assert_eq!(corner_coverage(10, 0, 20, 20, 6), 1.0);
    assert_eq!(corner_coverage(0, 10, 20, 20, 6), 1.0);
    // no radius, no cut, a radius bigger than the window is held to half of it
    assert_eq!(corner_coverage(0, 0, 20, 20, 0), 1.0);
    assert!(corner_coverage(0, 0, 4, 4, 100) < 0.5);
}

#[test]
fn a_half_transparent_pixel_keeps_the_color_it_had() {
    use jff_wm::primitives::hdr_capture::pq_picture_rgba;
    let pass = FinalPass::new(203.0, 1000.0, None);
    // white at half alpha, premultiplied: 0.5 in every component
    let half_white = [0x3800, 0x3800, 0x3800, 0x3800];
    let faded = pq_picture_rgba(&pass, &half_white, 1, 0);
    let solid = pq_picture_rgba(&pass, &[ONE, ONE, ONE, ONE], 1, 0);
    assert_eq!(faded.codes.len(), 4);
    for i in 0..3 {
        assert!((faded.codes[i] - solid.codes[i]).abs() < 1e-3);
    }
    assert!((faded.codes[3] - 0.5).abs() < 1e-3);
    assert!((solid.codes[3] - 1.0).abs() < 1e-6);
}

#[test]
fn a_clear_pixel_has_no_alpha_and_no_brightness() {
    use jff_wm::primitives::hdr_capture::pq_picture_rgba;
    let pass = FinalPass::new(203.0, 1000.0, None);
    let picture = pq_picture_rgba(&pass, &[0, 0, 0, 0], 1, 0);
    assert_eq!(picture.codes[3], 0.0);
    assert!(picture.codes.iter().all(|c| c.is_finite()));
    assert!(picture.max_nits < 0.01);
}

#[test]
fn the_rounded_corners_of_a_picture_come_out_of_its_alpha() {
    use jff_wm::primitives::hdr_capture::pq_picture_rgba;
    let pass = FinalPass::new(203.0, 1000.0, None);
    let frame: Vec<u16> = (0..20 * 20).flat_map(|_| [ONE, ONE, ONE, ONE]).collect();
    let picture = pq_picture_rgba(&pass, &frame, 20, 6);
    let alpha = |x: usize, y: usize| picture.codes[(y * 20 + x) * 4 + 3];
    assert!(alpha(0, 0) < 0.05);
    assert!(alpha(19, 0) < 0.05);
    assert!(alpha(0, 19) < 0.05);
    assert_eq!(alpha(10, 10), 1.0);
    // the color under the cut corner is the same as everywhere else
    assert_eq!(picture.codes[0], picture.codes[(10 * 20 + 10) * 4]);
}
