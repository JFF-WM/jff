//! Tests for `jff_wm::primitives::hdr`.

use jff_wm::primitives::edid::{Colorimetry, Edid, Eotfs, HdrStaticMetadata};
use jff_wm::primitives::hdr::{
    plan, KernelSupport, PeakSource, Status, Unavailable, FALLBACK_PEAK_NITS,
};

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
    let plan = plan(Some(&panel()), &kernel(), None).unwrap();
    assert_eq!(plan.max_luminance, 302);
    assert_eq!(plan.max_cll, 302);
    assert_eq!(plan.max_fall, 302);
    assert_eq!(plan.min_luminance, 0);
    assert_eq!(plan.max_bpc, Some(10));
    assert_eq!(plan.peak_source, PeakSource::Edid);
}

#[test]
fn the_minimum_is_in_ten_thousandths_of_a_nit() {
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().min_luminance = Some(0.05);
    assert_eq!(
        plan(Some(&edid), &kernel(), None).unwrap().min_luminance,
        500
    );
}

#[test]
fn a_missing_peak_falls_back_and_says_so() {
    let mut edid = panel();
    let hdr = edid.hdr.as_mut().unwrap();
    hdr.max_luminance = None;
    hdr.max_frame_average = None;
    let plan = plan(Some(&edid), &kernel(), None).unwrap();
    assert_eq!(plan.max_luminance, FALLBACK_PEAK_NITS as u16);
    assert_eq!(plan.max_fall, FALLBACK_PEAK_NITS as u16);
    assert_eq!(plan.peak_source, PeakSource::Assumed);
}

#[test]
fn a_connector_without_max_bpc_still_works() {
    let mut support = kernel();
    support.max_bpc = None;
    assert_eq!(plan(Some(&panel()), &support, None).unwrap().max_bpc, None);
}

#[test]
fn a_range_that_starts_above_ten_is_respected() {
    let mut support = kernel();
    support.max_bpc = Some(12..=16);
    assert_eq!(
        plan(Some(&panel()), &support, None).unwrap().max_bpc,
        Some(12)
    );
}

#[test]
fn every_missing_piece_has_its_own_reason() {
    assert_eq!(plan(None, &kernel(), None), Err(Unavailable::NoEdid));

    let mut edid = panel();
    edid.hdr = None;
    assert_eq!(
        plan(Some(&edid), &kernel(), None),
        Err(Unavailable::NoHdrBlock)
    );

    let mut edid = panel();
    edid.hdr.as_mut().unwrap().eotfs.pq = false;
    assert_eq!(plan(Some(&edid), &kernel(), None), Err(Unavailable::NoPq));

    let mut edid = panel();
    edid.hdr.as_mut().unwrap().type1 = false;
    assert_eq!(plan(Some(&edid), &kernel(), None), Err(Unavailable::NoPq));

    let mut edid = panel();
    edid.colorimetry.bt2020_rgb = false;
    assert_eq!(
        plan(Some(&edid), &kernel(), None),
        Err(Unavailable::NoBt2020Rgb)
    );

    let mut support = kernel();
    support.hdr_metadata = false;
    assert_eq!(
        plan(Some(&panel()), &support, None),
        Err(Unavailable::NoMetadataProperty)
    );

    let mut support = kernel();
    support.bt2020_rgb = false;
    assert_eq!(
        plan(Some(&panel()), &support, None),
        Err(Unavailable::NoBt2020Colorspace)
    );

    let mut support = kernel();
    support.max_bpc = Some(6..=8);
    assert_eq!(
        plan(Some(&panel()), &support, None),
        Err(Unavailable::BpcTooLow(8))
    );
}

#[test]
fn a_huge_peak_does_not_wrap() {
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().max_luminance = Some(1.0e9);
    assert_eq!(
        plan(Some(&edid), &kernel(), None).unwrap().max_luminance,
        u16::MAX
    );
}

#[test]
fn an_override_replaces_the_edid_peak() {
    let plan = plan(Some(&panel()), &kernel(), Some(400)).unwrap();
    assert_eq!(plan.max_luminance, 400);
    assert_eq!(plan.max_cll, 400);
    assert_eq!(plan.peak_source, PeakSource::Override);
}

#[test]
fn an_override_stands_in_for_a_missing_peak() {
    let mut edid = panel();
    let hdr = edid.hdr.as_mut().unwrap();
    hdr.max_luminance = None;
    hdr.max_frame_average = None;
    let plan = plan(Some(&edid), &kernel(), Some(600)).unwrap();
    assert_eq!(plan.max_luminance, 600);
    assert_eq!(plan.max_fall, 600);
    assert_eq!(plan.peak_source, PeakSource::Override);
}

#[test]
fn the_frame_average_never_exceeds_the_peak() {
    let lower = plan(Some(&panel()), &kernel(), Some(200)).unwrap();
    assert_eq!(lower.max_fall, 200);
    let higher = plan(Some(&panel()), &kernel(), Some(600)).unwrap();
    assert_eq!(higher.max_fall, 302);
}

#[test]
fn an_override_does_not_get_past_the_other_checks() {
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().eotfs.pq = false;
    assert_eq!(
        plan(Some(&edid), &kernel(), Some(400)),
        Err(Unavailable::NoPq)
    );
    assert_eq!(plan(None, &kernel(), Some(400)), Err(Unavailable::NoEdid));
}

#[test]
fn the_status_reads_as_a_log_line() {
    let hdr = plan(Some(&panel()), &kernel(), None).unwrap();
    let status = Status::hdr10(&hdr, 203);
    assert_eq!(
        status.to_string(),
        "HDR10, peak 302 cd/m² (from the EDID), SDR white 203 cd/m², 10 bpc"
    );
    let overridden = plan(Some(&panel()), &kernel(), Some(400)).unwrap();
    assert!(Status::hdr10(&overridden, 160)
        .to_string()
        .contains("400 cd/m² (from hdr-peak)"));
    let mut edid = panel();
    edid.hdr.as_mut().unwrap().max_luminance = None;
    let assumed = plan(Some(&edid), &kernel(), None).unwrap();
    assert!(Status::hdr10(&assumed, 203)
        .to_string()
        .contains("(assumed, the EDID has none)"));
    let mut support = kernel();
    support.max_bpc = None;
    let no_bpc = plan(Some(&panel()), &support, None).unwrap();
    assert!(Status::hdr10(&no_bpc, 203)
        .to_string()
        .ends_with("default bpc"));
    assert_eq!(Status::Off.to_string(), "HDR off");
    assert_eq!(
        Status::Sdr(Unavailable::NoPq).to_string(),
        "staying SDR, the monitor does not list the PQ transfer function"
    );
}

#[test]
fn only_hdr10_can_have_little_headroom() {
    let hdr = plan(Some(&panel()), &kernel(), None).unwrap();
    // 302 cd/m² over 203 is 1.49 times: not enough.
    assert!(Status::hdr10(&hdr, 203).low_headroom());
    assert!(!Status::hdr10(&hdr, 120).low_headroom());
    assert!(!Status::Off.low_headroom());
    assert!(!Status::Sdr(Unavailable::NoFloatTarget).low_headroom());
}

#[test]
fn the_runtime_reasons_have_messages() {
    assert!(Unavailable::NoFloatTarget.to_string().contains("float"));
}
