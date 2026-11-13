//! Tests for `jff_wm::primitives::hdr_scanout`.

use jff_wm::primitives::color::{
    ImageDescription, Luminance, NamedPrimaries, NamedTf, TransferFunction,
};
use jff_wm::primitives::hdr_scanout::{decide, Decline, Facts, Over, ABGR2101010, ARGB2101010};

fn pq_bt2020() -> ImageDescription {
    let mut description = ImageDescription::srgb();
    description.primaries = NamedPrimaries::Bt2020.chromaticities();
    description.tf = TransferFunction::Named(NamedTf::St2084Pq);
    description.luminance = Luminance::PQ;
    description
}

/// Everything in order, with a swapchain and a buffer of the same 10-bit format.
fn good() -> Facts {
    Facts {
        covering: true,
        over: None,
        effects: false,
        tag: Some(pq_bt2020()),
        buffer_format: Some(ARGB2101010),
        swapchain_format: ARGB2101010,
        signalled: true,
    }
}

#[test]
fn the_fourcc_constants_are_the_drm_codes() {
    let code = |name: &[u8; 4]| u32::from_le_bytes(*name);
    assert_eq!(ARGB2101010, code(b"AR30"));
    assert_eq!(ABGR2101010, code(b"AB30"));
}

#[test]
fn a_pq_bt2020_dmabuf_in_the_swapchain_format_goes_to_the_plane() {
    assert_eq!(decide(&good()), Ok(()));
    let other = Facts {
        buffer_format: Some(ABGR2101010),
        swapchain_format: ABGR2101010,
        ..good()
    };
    assert_eq!(decide(&other), Ok(()));
}

#[test]
fn each_condition_alone_declines() {
    let cases = [
        (
            Facts {
                covering: false,
                ..good()
            },
            Decline::NotCovering,
        ),
        (
            Facts {
                effects: true,
                ..good()
            },
            Decline::Effects,
        ),
        (
            Facts {
                tag: None,
                ..good()
            },
            Decline::Untagged,
        ),
        (
            Facts {
                buffer_format: None,
                ..good()
            },
            Decline::NotDmabuf,
        ),
        (
            Facts {
                signalled: false,
                ..good()
            },
            Decline::SignalPending,
        ),
        (
            Facts {
                buffer_format: Some(ABGR2101010),
                ..good()
            },
            Decline::FormatMismatch {
                buffer: ABGR2101010,
                swapchain: ARGB2101010,
            },
        ),
    ];
    for (facts, reason) in cases {
        assert_eq!(decide(&facts), Err(reason), "{facts:?}");
    }
}

#[test]
fn anything_drawn_over_the_window_declines() {
    for over in [
        Over::Cursor,
        Over::Layer,
        Over::Popup,
        Over::Toast,
        Over::Lock,
        Over::Screenshot,
        Over::Overview,
    ] {
        let facts = Facts {
            over: Some(over),
            ..good()
        };
        assert_eq!(decide(&facts), Err(Decline::DrawnOver(over)));
    }
}

#[test]
fn only_pq_in_bt2020_is_taken_as_it_is() {
    let mut hlg = pq_bt2020();
    hlg.tf = TransferFunction::Named(NamedTf::Hlg);
    let mut wide_srgb = pq_bt2020();
    wide_srgb.primaries = ImageDescription::srgb().primaries;
    let mut p3 = pq_bt2020();
    p3.primaries = NamedPrimaries::DisplayP3.chromaticities();
    for tag in [ImageDescription::srgb(), hlg, wide_srgb, p3] {
        let facts = Facts {
            tag: Some(tag),
            ..good()
        };
        assert_eq!(decide(&facts), Err(Decline::NotPqBt2020), "{tag:?}");
    }
}

#[test]
fn an_8_bit_swapchain_never_takes_a_buffer() {
    // 'AR24' and 'AB24', what the SDR swapchain uses
    for swapchain in [0x3432_5241, 0x3432_4241] {
        let facts = Facts {
            buffer_format: Some(swapchain),
            swapchain_format: swapchain,
            ..good()
        };
        assert_eq!(decide(&facts), Err(Decline::SwapchainNotTenBit(swapchain)));
    }
}

#[test]
fn the_first_reason_is_the_one_given() {
    // not covering outranks everything else, then what is over it
    let nothing_right = Facts {
        covering: false,
        over: Some(Over::Cursor),
        effects: true,
        tag: None,
        buffer_format: None,
        swapchain_format: 0,
        signalled: false,
    };
    assert_eq!(decide(&nothing_right), Err(Decline::NotCovering));
    let covering = Facts {
        covering: true,
        ..nothing_right
    };
    assert_eq!(decide(&covering), Err(Decline::DrawnOver(Over::Cursor)));
    let clear = Facts {
        over: None,
        ..covering
    };
    assert_eq!(decide(&clear), Err(Decline::Effects));
}

#[test]
fn the_reasons_read_in_plain_words() {
    assert_eq!(
        Decline::FormatMismatch {
            buffer: ABGR2101010,
            swapchain: ARGB2101010
        }
        .to_string(),
        "the buffer format AB30 is not the swapchain's AR30"
    );
    assert_eq!(
        Decline::DrawnOver(Over::Cursor).to_string(),
        "the cursor or a drag icon is over it"
    );
    assert_eq!(
        Decline::SwapchainNotTenBit(0x3432_5241).to_string(),
        "the swapchain format AR24 is not 10 bit"
    );
}
