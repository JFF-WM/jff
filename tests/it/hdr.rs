//! Tests for `jff_wm::primitives::hdr`.

use jff_wm::primitives::edid::{Colorimetry, Edid, Eotfs, HdrStaticMetadata};
use jff_wm::primitives::hdr::{plan, KernelSupport, Unavailable, FALLBACK_PEAK_NITS};

fn kernel() -> KernelSupport {
    KernelSupport {
        hdr_metadata: true,
        bt2020_rgb: true,
        max_bpc: Some(8..=12),
    }
}

/// What the AOC AG323FWG3R3 says: PQ without HLG, 301.8 cd/m² peak and frame-average, BT.2020 RGB.
fn panel() -> Edid {
    Edid {
        colorimetry: Colorimetry {
            bt2020_rgb: true,
            ..Colorimetry::default()
        },
        hdr: Some(HdrStaticMetadata {
            eotfs: Eotfs {
                sdr: true,
                hdr: true,
                pq: true,
                hlg: false,
            },
            type1: true,
            max_luminance: Some(301.8),
            max_frame_average: Some(301.8),
            min_luminance: Some(0.0),
        }),
        ..Edid::default()
    }
}

#[test]
fn the_panel_gets_a_pq_plan_at_its_own_peak() {
    let plan = plan(Some(&panel()), &kernel()).unwrap();
    assert_eq!(plan.max_luminance, 302);
    assert_eq!(plan.max_cll, 302);
    assert_eq!(plan.max_fall, 302);
    assert_eq!(plan.min_luminance, 0);
    assert_eq!(plan.max_bpc, Some(10));
    assert!(plan.peak_from_edid);
}

#[test]
fn the_minimum_is_in_ten_thousandths_of_a_nit() {
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().min_luminance = Some(0.05);
    assert_eq!(plan(Some(&edid), &kernel()).unwrap().min_luminance, 500);
}

#[test]
fn a_missing_peak_falls_back_and_says_so() {
    let mut edid = panel();
    let hdr = edid.hdr.as_mut().unwrap();
    hdr.max_luminance = None;
    hdr.max_frame_average = None;
    let plan = plan(Some(&edid), &kernel()).unwrap();
    assert_eq!(plan.max_luminance, FALLBACK_PEAK_NITS as u16);
    assert_eq!(plan.max_fall, FALLBACK_PEAK_NITS as u16);
    assert!(!plan.peak_from_edid);
}

#[test]
fn a_connector_without_max_bpc_still_works() {
    let mut support = kernel();
    support.max_bpc = None;
    assert_eq!(plan(Some(&panel()), &support).unwrap().max_bpc, None);
}

#[test]
fn a_range_that_starts_above_ten_is_respected() {
    let mut support = kernel();
    support.max_bpc = Some(12..=16);
    assert_eq!(plan(Some(&panel()), &support).unwrap().max_bpc, Some(12));
}

#[test]
fn every_missing_piece_has_its_own_reason() {
    assert_eq!(plan(None, &kernel()), Err(Unavailable::NoEdid));

    let mut edid = panel();
    edid.hdr = None;
    assert_eq!(plan(Some(&edid), &kernel()), Err(Unavailable::NoHdrBlock));

    let mut edid = panel();
    edid.hdr.as_mut().unwrap().eotfs.pq = false;
    assert_eq!(plan(Some(&edid), &kernel()), Err(Unavailable::NoPq));

    let mut edid = panel();
    edid.hdr.as_mut().unwrap().type1 = false;
    assert_eq!(plan(Some(&edid), &kernel()), Err(Unavailable::NoPq));

    let mut edid = panel();
    edid.colorimetry.bt2020_rgb = false;
    assert_eq!(plan(Some(&edid), &kernel()), Err(Unavailable::NoBt2020Rgb));

    let mut support = kernel();
    support.hdr_metadata = false;
    assert_eq!(
        plan(Some(&panel()), &support),
        Err(Unavailable::NoMetadataProperty)
    );

    let mut support = kernel();
    support.bt2020_rgb = false;
    assert_eq!(
        plan(Some(&panel()), &support),
        Err(Unavailable::NoBt2020Colorspace)
    );

    let mut support = kernel();
    support.max_bpc = Some(6..=8);
    assert_eq!(
        plan(Some(&panel()), &support),
        Err(Unavailable::BpcTooLow(8))
    );
}

#[test]
fn a_huge_peak_does_not_wrap() {
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().max_luminance = Some(1.0e9);
    assert_eq!(
        plan(Some(&edid), &kernel()).unwrap().max_luminance,
        u16::MAX
    );
}
