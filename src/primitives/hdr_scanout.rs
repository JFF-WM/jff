//! Whether a fullscreen window may be scanned out directly on an HDR output.

use std::fmt;

use super::color::{ImageDescription, NamedPrimaries, NamedTf, TransferFunction};

/// DRM fourcc of the 10-bit format with alpha, red in the high bits (`AR30`).
pub const ARGB2101010: u32 = 0x3033_5241;

/// DRM fourcc of the 10-bit format with alpha, blue in the high bits (`AB30`).
pub const ABGR2101010: u32 = 0x3033_4241;

/// What is drawn above the window, which a buffer on the primary plane cannot have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Over {
    Cursor,
    Layer,
    Popup,
    Toast,
    Lock,
    Screenshot,
    Overview,
}

impl fmt::Display for Over {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cursor => "the cursor or a drag icon is over it",
            Self::Layer => "a layer surface is over it",
            Self::Popup => "a popup is over it",
            Self::Toast => "a toast is over it",
            Self::Lock => "the screen is locked",
            Self::Screenshot => "a screenshot is open",
            Self::Overview => "the overview is open",
        })
    }
}

/// What is known about the frame of an HDR output when a fullscreen window may go straight to the primary plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// An opaque fullscreen window covers the monitor exactly.
    pub covering: bool,
    /// The first thing found above it.
    pub over: Option<Over>,
    /// The window is drawn through a shader of its own (rounded, blurred, or a YUV correction).
    pub effects: bool,
    /// What the window's surface said its colors are, `None` when it said nothing.
    pub tag: Option<ImageDescription>,
    /// The DRM fourcc of the surface's buffer, `None` when it is not a dmabuf.
    pub buffer_format: Option<u32>,
    /// The DRM fourcc of the output's swapchain.
    pub swapchain_format: u32,
    /// The connector already signals what the output was planned with, so nothing is waiting for a modeset.
    pub signalled: bool,
}

/// A window and what it showed when it was put on the primary plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tried {
    pub surface: u32,
    pub format: Option<u32>,
    /// How many surfaces the window's tree holds, which changes when a subsurface comes or goes.
    pub subsurfaces: usize,
}

impl Tried {
    /// Whether the plane already refused this very window, with the same buffer format and the same subsurfaces, so trying again would fail the same way.
    pub fn was_declined(&self, declined: Option<Tried>) -> bool {
        declined == Some(*self)
    }
}

/// Why a window is not scanned out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decline {
    NotCovering,
    DrawnOver(Over),
    Effects,
    Untagged,
    NotPqBt2020,
    NotDmabuf,
    SwapchainNotTenBit(u32),
    FormatMismatch { buffer: u32, swapchain: u32 },
    SignalPending,
}

impl fmt::Display for Decline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCovering => f.write_str("no opaque fullscreen window covers the monitor"),
            Self::DrawnOver(over) => over.fmt(f),
            Self::Effects => f.write_str("the window needs a shader of its own"),
            Self::Untagged => f.write_str("the surface has no color description"),
            Self::NotPqBt2020 => f.write_str("the surface is not PQ in BT.2020"),
            Self::NotDmabuf => f.write_str("the buffer is not a dmabuf"),
            Self::SwapchainNotTenBit(code) => {
                write!(f, "the swapchain format {} is not 10 bit", fourcc(*code))
            }
            Self::FormatMismatch { buffer, swapchain } => write!(
                f,
                "the buffer format {} is not the swapchain's {}",
                fourcc(*buffer),
                fourcc(*swapchain)
            ),
            Self::SignalPending => f.write_str("the HDR signalling is not in place yet"),
        }
    }
}

/// A fourcc as its four letters.
fn fourcc(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

/// Whether the window can be shown by putting its buffer on the primary plane of an HDR output, or the first reason it cannot. The buffer then reaches the panel as the client made it: no tone map, no gamut map, SDR white not applied.
pub fn decide(facts: &Facts) -> Result<(), Decline> {
    if !facts.covering {
        return Err(Decline::NotCovering);
    }
    if let Some(over) = facts.over {
        return Err(Decline::DrawnOver(over));
    }
    if facts.effects {
        return Err(Decline::Effects);
    }
    let tag = facts.tag.ok_or(Decline::Untagged)?;
    if tag.tf != TransferFunction::Named(NamedTf::St2084Pq)
        || tag.primaries != NamedPrimaries::Bt2020.chromaticities()
    {
        return Err(Decline::NotPqBt2020);
    }
    let buffer = facts.buffer_format.ok_or(Decline::NotDmabuf)?;
    if !matches!(facts.swapchain_format, ARGB2101010 | ABGR2101010) {
        return Err(Decline::SwapchainNotTenBit(facts.swapchain_format));
    }
    if buffer != facts.swapchain_format {
        return Err(Decline::FormatMismatch {
            buffer,
            swapchain: facts.swapchain_format,
        });
    }
    if !facts.signalled {
        return Err(Decline::SignalPending);
    }
    Ok(())
}
