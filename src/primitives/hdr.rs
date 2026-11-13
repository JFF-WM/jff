//! Whether an output can be signalled as HDR10 (PQ in BT.2020), and what the signal says.

use std::ops::RangeInclusive;

use super::edid::Edid;

/// Peak assumed when the sink's EDID does not state one, in cd/m².
pub const FALLBACK_PEAK_NITS: f64 = 1000.0;

/// Bits per color asked of the link for HDR, where the connector lets us choose.
pub const HDR_BPC: u32 = 10;

/// What the kernel and driver offer on the connector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KernelSupport {
    /// The connector has the `HDR_OUTPUT_METADATA` property.
    pub hdr_metadata: bool,
    /// The `Colorspace` property has a `BT2020_RGB` value.
    pub bt2020_rgb: bool,
    /// Range of the `max bpc` property, `None` when there is none.
    pub max_bpc: Option<RangeInclusive<u32>>,
}

/// Why an output stays SDR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The connector's EDID is missing or not an EDID.
    NoEdid,
    /// The EDID has no HDR static metadata block.
    NoHdrBlock,
    /// The monitor does not take PQ with static metadata type 1.
    NoPq,
    /// The monitor lists HLG but not PQ, and only PQ is signalled.
    HlgOnly,
    /// The EDID does not list BT.2020 RGB colorimetry.
    NoBt2020Rgb,
    /// The connector has no `HDR_OUTPUT_METADATA` property.
    NoMetadataProperty,
    /// The connector's `Colorspace` property has no `BT2020_RGB`.
    NoBt2020Colorspace,
    /// The link cannot be asked for 10 bits, the most it takes is this many.
    BpcTooLow(u32),
    /// The renderer cannot draw into a 16-bit float texture.
    NoFloatTarget,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::NoEdid => f.write_str("the monitor has no readable EDID"),
            Unavailable::NoHdrBlock => f.write_str("the EDID has no HDR static metadata block"),
            Unavailable::NoPq => f.write_str("the monitor does not list the PQ transfer function"),
            Unavailable::HlgOnly => {
                f.write_str("the monitor lists HLG but not PQ, and only PQ (HDR10) is signalled")
            }
            Unavailable::NoBt2020Rgb => {
                f.write_str("the monitor does not list BT.2020 RGB colorimetry")
            }
            Unavailable::NoMetadataProperty => {
                f.write_str("the connector has no HDR_OUTPUT_METADATA property")
            }
            Unavailable::NoBt2020Colorspace => {
                f.write_str("the connector's Colorspace property has no BT2020_RGB")
            }
            Unavailable::BpcTooLow(max) => {
                write!(
                    f,
                    "the connector's max bpc goes up to {max}, HDR wants {HDR_BPC}"
                )
            }
            Unavailable::NoFloatTarget => {
                f.write_str("the renderer cannot draw into a 16-bit float texture")
            }
        }
    }
}

/// What to put on the connector for HDR10. Luminances are in the units of the infoframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Mastering peak luminance, cd/m².
    pub max_luminance: u16,
    /// Mastering minimum luminance, in units of 0.0001 cd/m².
    pub min_luminance: u16,
    /// Maximum content light level, cd/m².
    pub max_cll: u16,
    /// Maximum frame-average light level, cd/m².
    pub max_fall: u16,
    /// Value for `max bpc`, `None` when the connector has no such property.
    pub max_bpc: Option<u32>,
    /// Where the peak in `max_luminance` comes from.
    pub peak_source: PeakSource,
}

impl Plan {
    /// The same plan with the light levels of the content on screen: each is the client's value when it gave a nonzero one, capped at the panel's peak, and the frame average never exceeds the content peak.
    pub fn with_content(self, max_cll: Option<u32>, max_fall: Option<u32>) -> Plan {
        let cap = |level: Option<u32>, limit: u16| {
            level
                .filter(|&v| v != 0)
                .map(|v| v.min(u32::from(limit)) as u16)
        };
        let max_cll = cap(max_cll, self.max_luminance).unwrap_or(self.max_cll);
        let max_fall = cap(max_fall, max_cll).unwrap_or(self.max_fall).min(max_cll);
        Plan {
            max_cll,
            max_fall,
            ..self
        }
    }
}

/// Where the peak luminance of an output comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeakSource {
    /// The monitor's EDID states it.
    Edid,
    /// The `hdr-peak` setting.
    Override,
    /// Neither gave one, [`FALLBACK_PEAK_NITS`] stands in.
    Assumed,
}

impl std::fmt::Display for PeakSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PeakSource::Edid => "from the EDID",
            PeakSource::Override => "from hdr-peak",
            PeakSource::Assumed => "assumed, the EDID has none",
        })
    }
}

/// What became of an output's `hdr` setting, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// `hdr` is not set.
    Off,
    /// `hdr` is set and the output stays SDR, for this reason.
    Sdr(Unavailable),
    /// The output is signalled as HDR10.
    Hdr10 {
        /// Peak the frame is mapped to, cd/m².
        peak: u16,
        peak_source: PeakSource,
        /// Luminance of SDR white, cd/m².
        sdr_white: u32,
        /// Value set on `max bpc`, `None` when the connector has no such property.
        max_bpc: Option<u32>,
    },
}

impl Status {
    /// HDR10 as planned, with SDR white at `sdr_white` cd/m².
    pub fn hdr10(plan: &Plan, sdr_white: u32) -> Self {
        Status::Hdr10 {
            peak: plan.max_luminance,
            peak_source: plan.peak_source,
            sdr_white,
            max_bpc: plan.max_bpc,
        }
    }

    /// HDR10 with so little room above SDR white that highlights barely show.
    pub fn low_headroom(&self) -> bool {
        match self {
            Status::Hdr10 {
                peak, sdr_white, ..
            } => super::hdr_output::low_headroom(*sdr_white as f32, f32::from(*peak)),
            _ => false,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Off => f.write_str("HDR off"),
            Status::Sdr(reason) => write!(f, "staying SDR, {reason}"),
            Status::Hdr10 {
                peak,
                peak_source,
                sdr_white,
                max_bpc,
            } => {
                write!(
                    f,
                    "HDR10, peak {peak} cd/m² ({peak_source}), SDR white {sdr_white} cd/m², "
                )?;
                match max_bpc {
                    Some(bpc) => write!(f, "{bpc} bpc"),
                    None => f.write_str("default bpc"),
                }
            }
        }
    }
}

/// Decide whether `edid` and the connector allow HDR10, and with which metadata. `peak_override` (cd/m²) replaces the peak the EDID states.
pub fn plan(
    edid: Option<&Edid>,
    kernel: &KernelSupport,
    peak_override: Option<u32>,
) -> Result<Plan, Unavailable> {
    let edid = edid.ok_or(Unavailable::NoEdid)?;
    let hdr = edid.hdr.as_ref().ok_or(Unavailable::NoHdrBlock)?;
    if !hdr.eotfs.pq || !hdr.type1 {
        return Err(if hdr.eotfs.hlg && !hdr.eotfs.pq {
            Unavailable::HlgOnly
        } else {
            Unavailable::NoPq
        });
    }
    if !edid.colorimetry.bt2020_rgb {
        return Err(Unavailable::NoBt2020Rgb);
    }
    if !kernel.hdr_metadata {
        return Err(Unavailable::NoMetadataProperty);
    }
    if !kernel.bt2020_rgb {
        return Err(Unavailable::NoBt2020Colorspace);
    }
    let max_bpc = match &kernel.max_bpc {
        Some(range) if *range.end() < HDR_BPC => return Err(Unavailable::BpcTooLow(*range.end())),
        Some(range) => Some(HDR_BPC.max(*range.start())),
        None => None,
    };

    let (peak, peak_source) = match (peak_override, hdr.max_luminance) {
        (Some(nits), _) => (f64::from(nits), PeakSource::Override),
        (None, Some(nits)) => (nits, PeakSource::Edid),
        (None, None) => (FALLBACK_PEAK_NITS, PeakSource::Assumed),
    };
    let average = hdr.max_frame_average.map_or(peak, |nits| nits.min(peak));
    let minimum = hdr.min_luminance.unwrap_or(0.0);
    Ok(Plan {
        max_luminance: nits_to_u16(peak),
        min_luminance: nits_to_u16(minimum * 10_000.0),
        max_cll: nits_to_u16(peak),
        max_fall: nits_to_u16(average),
        max_bpc,
        peak_source,
    })
}

/// Round to the nearest whole unit, clamped to what the infoframe field holds.
fn nits_to_u16(value: f64) -> u16 {
    value.round().clamp(0.0, f64::from(u16::MAX)) as u16
}
