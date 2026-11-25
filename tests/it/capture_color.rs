//! Tests for `jff_wm::primitives::capture_color`.

use jff_wm::primitives::capture_color::{frame_description_fits, preferred};
use jff_wm::primitives::color::TransferFunction;
use jff_wm::primitives::color::{ImageDescription, Luminance, NamedPrimaries, NamedTf};

fn hdr(peak: u32) -> ImageDescription {
    ImageDescription {
        primaries: NamedPrimaries::Bt2020.chromaticities(),
        tf: TransferFunction::Named(NamedTf::St2084Pq),
        luminance: Luminance {
            min: Luminance::PQ.min,
            max: peak,
            reference: 203,
        },
        ..ImageDescription::srgb()
    }
}

#[test]
fn a_source_prefers_what_its_picture_holds() {
    assert_eq!(preferred(None), ImageDescription::srgb());
    assert_eq!(preferred(Some(hdr(1_000))), hdr(1_000));
    // an HDR description that is not PQ in BT.2020 is not what a capture holds
    let hlg = ImageDescription {
        tf: TransferFunction::Named(NamedTf::Hlg),
        ..hdr(1_000)
    };
    assert_eq!(preferred(Some(hlg)), ImageDescription::srgb());
}

#[test]
fn an_sdr_picture_can_only_be_called_srgb() {
    assert!(frame_description_fits(&ImageDescription::srgb(), false));
    // luminance metadata does not matter for a relative curve
    let described = ImageDescription {
        luminance: Luminance {
            min: 0,
            max: 120,
            reference: 120,
        },
        ..ImageDescription::srgb()
    };
    assert!(frame_description_fits(&described, false));
    assert!(!frame_description_fits(&hdr(1_000), false));
    let p3 = ImageDescription {
        primaries: NamedPrimaries::DisplayP3.chromaticities(),
        ..ImageDescription::srgb()
    };
    assert!(!frame_description_fits(&p3, false));
}

#[test]
fn an_hdr_picture_can_only_be_called_pq_bt2020() {
    assert!(frame_description_fits(&hdr(1_000), true));
    // PQ is absolute: the peak and reference it states are metadata
    assert!(frame_description_fits(&hdr(4_000), true));
    assert!(!frame_description_fits(&ImageDescription::srgb(), true));
}
