//! Tests for `jff_wm::primitives::edid`. The EDIDs are built here from the facts of the AOC AG323FWG3R3, not captured from one.

use jff_wm::primitives::edid::{self, EdidError};
use jff_wm::primitives::hdr::{plan, KernelSupport};

const PANEL: [(f64, f64); 4] = [
    (0.6777, 0.3134),
    (0.2753, 0.6474),
    (0.1513, 0.0644),
    (0.3134, 0.3291),
];

/// A base block with a 10-bit digital input and the panel's chromaticities.
fn base(extensions: u8) -> Vec<u8> {
    let mut block = vec![0u8; 128];
    block[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
    block[20] = 0x80 | (3 << 4);
    let raw: Vec<u16> = PANEL
        .iter()
        .flat_map(|&(x, y)| [x, y])
        .map(|v| (v * 1024.0).round() as u16)
        .collect();
    for (i, v) in raw.iter().enumerate() {
        block[27 + i] = (v >> 2) as u8;
        block[25 + i / 4] |= ((v & 3) as u8) << (6 - 2 * (i % 4));
    }
    block[126] = extensions;
    block
}

/// A CTA extension holding the given data blocks.
fn cta(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut block = vec![0u8; 128];
    block[0] = 0x02;
    block[1] = 3;
    let mut at = 4;
    for data in blocks {
        block[at..at + data.len()].copy_from_slice(data);
        at += data.len();
    }
    block[2] = at as u8;
    block
}

/// Extended-tag data block: tag 7, length, extended tag, payload.
fn extended(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = vec![(7 << 5) | (payload.len() as u8 + 1), tag];
    data.extend_from_slice(payload);
    data
}

fn colorimetry() -> Vec<u8> {
    // xvYCC601, xvYCC709, BT2020cYCC, BT2020YCC, BT2020RGB
    extended(5, &[0x01 | 0x02 | 0x20 | 0x40 | 0x80, 0x00])
}

fn hdr(eotfs: u8, max: u8, avg: u8, min: u8) -> Vec<u8> {
    extended(6, &[eotfs, 0x01, max, avg, min])
}

fn monitor() -> Vec<u8> {
    let mut bytes = base(1);
    // SDR, HDR gamma and PQ, no HLG
    bytes.extend(cta(&[colorimetry(), hdr(0x07, 83, 83, 0)]));
    bytes
}

#[test]
fn luminance_codes_follow_the_cta_formula() {
    assert!((edid::max_luminance_nits(83) - 301.8).abs() < 0.05);
    assert_eq!(edid::max_luminance_nits(0), 50.0);
    assert!((edid::max_luminance_nits(96) - 400.0).abs() < 1e-9);
    assert_eq!(edid::min_luminance_nits(0, 300.0), 0.0);
    assert!((edid::min_luminance_nits(255, 300.0) - 3.0).abs() < 1e-9);
}

#[test]
fn the_panel_gives_its_primaries_and_bit_depth() {
    let parsed = edid::parse(&monitor()).unwrap();
    assert_eq!(parsed.bits_per_primary, Some(10));
    let p = parsed.primaries.unwrap();
    for (got, want) in [p.red, p.green, p.blue, p.white].iter().zip(PANEL) {
        assert!(
            (got.0 - want.0).abs() <= 1.0 / 1024.0,
            "{got:?} vs {want:?}"
        );
        assert!(
            (got.1 - want.1).abs() <= 1.0 / 1024.0,
            "{got:?} vs {want:?}"
        );
    }
}

#[test]
fn the_panel_gives_its_colorimetry_and_hdr_block() {
    let parsed = edid::parse(&monitor()).unwrap();
    let c = parsed.colorimetry;
    assert!(c.bt2020_rgb && c.bt2020_ycc && c.bt2020_cycc && c.xv_ycc601 && c.xv_ycc709);
    assert!(!c.dci_p3 && !c.op_rgb && !c.s_ycc601);
    let hdr = parsed.hdr.unwrap();
    assert!(hdr.eotfs.sdr && hdr.eotfs.hdr && hdr.eotfs.pq);
    assert!(!hdr.eotfs.hlg);
    assert!(hdr.type1);
    assert!((hdr.max_luminance.unwrap() - 301.8).abs() < 0.05);
    assert!((hdr.max_frame_average.unwrap() - 301.8).abs() < 0.05);
    assert_eq!(hdr.min_luminance, Some(0.0));
}

#[test]
fn a_short_hdr_block_leaves_the_luminances_empty() {
    let mut bytes = base(1);
    bytes.extend(cta(&[extended(6, &[0x04, 0x01])]));
    let hdr = edid::parse(&bytes).unwrap().hdr.unwrap();
    assert!(hdr.eotfs.pq);
    assert_eq!(hdr.max_luminance, None);
    assert_eq!(hdr.max_frame_average, None);
    assert_eq!(hdr.min_luminance, None);
}

#[test]
fn a_zero_luminance_code_means_not_given() {
    let mut bytes = base(1);
    bytes.extend(cta(&[hdr(0x04, 0, 0, 0)]));
    let hdr = edid::parse(&bytes).unwrap().hdr.unwrap();
    assert_eq!(hdr.max_luminance, None);
    assert_eq!(hdr.min_luminance, None);
}

#[test]
fn data_blocks_after_other_blocks_are_found() {
    // a video data block (tag 2) and an audio block (tag 1) first
    let mut bytes = base(1);
    bytes.extend(cta(&[
        vec![(2 << 5) | 2, 0x10, 0x20],
        vec![(1 << 5) | 3, 1, 2, 3],
        hdr(0x04, 83, 83, 0),
    ]));
    assert!(edid::parse(&bytes).unwrap().hdr.unwrap().eotfs.pq);
}

#[test]
fn a_second_cta_block_adds_to_the_first() {
    let mut bytes = base(2);
    bytes.extend(cta(&[colorimetry()]));
    bytes.extend(cta(&[hdr(0x04, 83, 83, 0)]));
    let parsed = edid::parse(&bytes).unwrap();
    assert!(parsed.colorimetry.bt2020_rgb);
    assert!(parsed.hdr.unwrap().eotfs.pq);
}

#[test]
fn a_data_block_running_past_the_end_is_dropped() {
    let mut bytes = base(1);
    let mut ext = cta(&[hdr(0x04, 83, 83, 0)]);
    // claim 20 bytes for the block, there are 6 before the timings start
    ext[4] = (7 << 5) | 20;
    bytes.extend(ext);
    assert_eq!(edid::parse(&bytes).unwrap().hdr, None);
}

#[test]
fn other_extension_blocks_are_ignored() {
    let mut bytes = base(1);
    let mut ext = cta(&[hdr(0x04, 83, 83, 0)]);
    ext[0] = 0x70;
    bytes.extend(ext);
    assert_eq!(edid::parse(&bytes).unwrap().hdr, None);
}

#[test]
fn an_extension_count_past_the_buffer_is_survived() {
    let bytes = base(3);
    let parsed = edid::parse(&bytes).unwrap();
    assert_eq!(parsed.hdr, None);
    assert!(parsed.primaries.is_some());
}

#[test]
fn an_analog_input_has_no_bit_depth_and_empty_chromaticities_no_primaries() {
    let mut bytes = base(0);
    bytes[20] = 0x00;
    bytes[25..35].fill(0);
    let parsed = edid::parse(&bytes).unwrap();
    assert_eq!(parsed.bits_per_primary, None);
    assert_eq!(parsed.primaries, None);
}

#[test]
fn bytes_that_are_no_edid_are_refused() {
    assert_eq!(edid::parse(&[0; 64]), Err(EdidError::TooShort));
    assert_eq!(edid::parse(&[0; 128]), Err(EdidError::BadHeader));
}

#[test]
fn a_minimum_code_in_the_edid_reaches_the_infoframe() {
    // peak code 96 is 400 cd/m², minimum code 51 is 400 * 0.2^2 / 100 = 0.16 cd/m²
    let mut bytes = base(1);
    bytes.extend(cta(&[colorimetry(), hdr(0x07, 96, 96, 51)]));
    let parsed = edid::parse(&bytes).unwrap();
    let kernel = KernelSupport {
        hdr_metadata: true,
        bt2020_rgb: true,
        max_bpc: Some(8..=12),
    };
    let plan = plan(Some(&parsed), &kernel, None).unwrap();
    assert_eq!(plan.max_luminance, 400);
    assert_eq!(plan.min_luminance, 1600);
}
