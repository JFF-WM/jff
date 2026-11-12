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
    NoEdid,
    NoHdrBlock,
    NoPq,
    NoBt2020Rgb,
    NoMetadataProperty,
    NoBt2020Colorspace,
    /// The link cannot be asked for 10 bits, the most it takes is this many.
    BpcTooLow(u32),
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::NoEdid => f.write_str("the monitor has no readable EDID"),
            Unavailable::NoHdrBlock => f.write_str("the EDID has no HDR static metadata block"),
            Unavailable::NoPq => f.write_str("the monitor does not list the PQ transfer function"),
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
        }
    }
}

/// What to put on the connector for HDR10. Luminances are in the units of the infoframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Mastering peak luminance, cd/m².
    pub max_luminance: u16,
    /// Mastering minimum luminance, 0.0001 cd/m².
    pub min_luminance: u16,
    /// Maximum content light level, cd/m².
    pub max_cll: u16,
    /// Maximum frame-average light level, cd/m².
    pub max_fall: u16,
    /// Value for `max bpc`, `None` when the connector has no such property.
    pub max_bpc: Option<u32>,
    /// The EDID gave a peak. When not, [`FALLBACK_PEAK_NITS`] stands in for it.
    pub peak_from_edid: bool,
}

/// Decide whether `edid` and the connector allow HDR10, and with which metadata.
pub fn plan(edid: Option<&Edid>, kernel: &KernelSupport) -> Result<Plan, Unavailable> {
    let edid = edid.ok_or(Unavailable::NoEdid)?;
    let hdr = edid.hdr.as_ref().ok_or(Unavailable::NoHdrBlock)?;
    if !hdr.eotfs.pq || !hdr.type1 {
        return Err(Unavailable::NoPq);
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

    let peak = hdr.max_luminance.unwrap_or(FALLBACK_PEAK_NITS);
    let average = hdr.max_frame_average.unwrap_or(peak);
    let minimum = hdr.min_luminance.unwrap_or(0.0);
    Ok(Plan {
        max_luminance: nits_to_u16(peak),
        min_luminance: nits_to_u16(minimum * 10_000.0),
        max_cll: nits_to_u16(peak),
        max_fall: nits_to_u16(average),
        max_bpc,
        peak_from_edid: hdr.max_luminance.is_some(),
    })
}

/// Round to the nearest whole unit, clamped to what the infoframe field holds.
fn nits_to_u16(value: f64) -> u16 {
    value.round().clamp(0.0, f64::from(u16::MAX)) as u16
}
