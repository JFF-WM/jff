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
