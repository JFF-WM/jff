//! What a monitor's EDID says about color: its primaries, the CTA colorimetry block and the HDR static metadata block.
//! Also who made the monitor and which one it is (`identity`), which is what output profiles are matched on.

/// Size of one EDID block.
const BLOCK: usize = 128;

/// The fixed pattern every EDID starts with.
const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Block tag of a CTA-861 extension.
const CTA_EXTENSION: u8 = 0x02;

/// CTA data block tag that says the next byte is an extended tag.
const CTA_USE_EXTENDED_TAG: u8 = 7;

/// Extended tag of the colorimetry data block.
const EXT_COLORIMETRY: u8 = 5;

/// Extended tag of the HDR static metadata data block.
const EXT_HDR_STATIC_METADATA: u8 = 6;

/// Where the four 18-byte descriptors start in the base block.
const DESCRIPTORS: [usize; 4] = [54, 72, 90, 108];

/// Descriptor tag of the monitor's product name.
const DESC_NAME: u8 = 0xfc;

/// Descriptor tag of the monitor's serial number as text.
const DESC_SERIAL: u8 = 0xff;

/// What an identity part says when the EDID has nothing for it, the word other compositors use.
const UNKNOWN: &str = "Unknown";

/// Why bytes are not an EDID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdidError {
    /// Fewer bytes than one block.
    TooShort,
    /// The first eight bytes are not the EDID header.
    BadHeader,
}

impl std::fmt::Display for EdidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EdidError::TooShort => "EDID is shorter than one block",
            EdidError::BadHeader => "EDID has no valid header",
        })
    }
}

impl std::error::Error for EdidError {}

/// CIE 1931 xy chromaticities of the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primaries {
    pub red: (f64, f64),
    pub green: (f64, f64),
    pub blue: (f64, f64),
    pub white: (f64, f64),
}

/// The signal colorimetry the CTA colorimetry data block lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colorimetry {
    pub xv_ycc601: bool,
    pub xv_ycc709: bool,
    pub s_ycc601: bool,
    pub op_ycc601: bool,
    pub op_rgb: bool,
    pub bt2020_cycc: bool,
    pub bt2020_ycc: bool,
    pub bt2020_rgb: bool,
    pub dci_p3: bool,
}

/// The transfer functions the HDR static metadata data block lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Eotfs {
    /// Traditional gamma, SDR luminance range.
    pub sdr: bool,
    /// Traditional gamma, HDR luminance range.
    pub hdr: bool,
    /// SMPTE ST 2084 (PQ).
    pub pq: bool,
    /// Hybrid log-gamma.
    pub hlg: bool,
}

/// The HDR static metadata data block, luminances in cd/m².
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HdrStaticMetadata {
    pub eotfs: Eotfs,
    /// The sink takes static metadata type 1, the only type there is.
    pub type1: bool,
    /// Desired content peak luminance, `None` when the block does not say.
    pub max_luminance: Option<f64>,
    /// Desired content maximum frame-average luminance, `None` when the block does not say.
    pub max_frame_average: Option<f64>,
    /// Desired content minimum luminance, `None` when the block does not say.
    pub min_luminance: Option<f64>,
}

/// The color facts of one EDID. Every field is empty when the EDID leaves it out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edid {
    /// Bits per color of the digital input, from the base block.
    pub bits_per_primary: Option<u8>,
    pub primaries: Option<Primaries>,
    pub colorimetry: Colorimetry,
    pub hdr: Option<HdrStaticMetadata>,
}

/// Luminance in cd/m² for a CTA-861.3 maximum (or frame-average) luminance code: `50 * 2^(code / 32)`.
pub fn max_luminance_nits(code: u8) -> f64 {
    50.0 * (f64::from(code) / 32.0).exp2()
}

/// Luminance in cd/m² for a CTA-861.3 minimum luminance code, relative to the maximum: `max * (code / 255)^2 / 100`.
pub fn min_luminance_nits(code: u8, max_nits: f64) -> f64 {
    max_nits * (f64::from(code) / 255.0).powi(2) / 100.0
}

/// Read the color facts out of an EDID. The kernel only exposes blocks it checked, so checksums are not verified here.
pub fn parse(bytes: &[u8]) -> Result<Edid, EdidError> {
    if bytes.len() < BLOCK {
        return Err(EdidError::TooShort);
    }
    if bytes[..8] != HEADER {
        return Err(EdidError::BadHeader);
    }
    let mut edid = Edid {
        bits_per_primary: bits_per_primary(bytes[20]),
        primaries: primaries(&bytes[25..35]),
        ..Edid::default()
    };
    // A block cut short by the buffer is ignored, the extension count may claim more than there is.
    let extensions = usize::from(bytes[126]);
    for index in 1..=extensions {
        let Some(block) = bytes.get(index * BLOCK..(index + 1) * BLOCK) else {
            break;
        };
        if block[0] == CTA_EXTENSION {
            parse_cta_block(block, &mut edid);
        }
    }
    Ok(edid)
}

/// The color depth field of the video input byte, for digital inputs only.
fn bits_per_primary(video_input: u8) -> Option<u8> {
    if video_input & 0x80 == 0 {
        return None;
    }
    match (video_input >> 4) & 0x07 {
        1 => Some(6),
        2 => Some(8),
        3 => Some(10),
        4 => Some(12),
        5 => Some(14),
        6 => Some(16),
        _ => None,
    }
}

/// The ten chromaticity bytes: two bytes of low bits, then the high eight bits of each of the eight values.
fn primaries(bytes: &[u8]) -> Option<Primaries> {
    let (low, high) = (&bytes[..2], &bytes[2..]);
    // Eight values with two low bits each, packed four to a byte, most significant pair first.
    let value = |index: usize| {
        let pair = (low[index / 4] >> (6 - 2 * (index % 4))) & 0x03;
        f64::from((u16::from(high[index]) << 2) | u16::from(pair)) / 1024.0
    };
    let primaries = Primaries {
        red: (value(0), value(1)),
        green: (value(2), value(3)),
        blue: (value(4), value(5)),
        white: (value(6), value(7)),
    };
    // All zero is what a display without the data writes.
    let all = [
        primaries.red,
        primaries.green,
        primaries.blue,
        primaries.white,
    ];
    all.iter()
        .any(|&(x, y)| x != 0.0 || y != 0.0)
        .then_some(primaries)
}

/// Walk the data blocks of one CTA-861 extension.
fn parse_cta_block(block: &[u8], edid: &mut Edid) {
    // Byte 2 is where the detailed timings start, so the data blocks fill 4 up to there.
    let end = usize::from(block[2]).min(BLOCK - 1);
    let mut at = 4;
    while at < end {
        let tag = block[at] >> 5;
        let len = usize::from(block[at] & 0x1f);
        let Some(body) = block
            .get(at + 1..at + 1 + len)
            .filter(|_| at + 1 + len <= end)
        else {
            break;
        };
        at += 1 + len;
        if tag != CTA_USE_EXTENDED_TAG || body.is_empty() {
            continue;
        }
        match body[0] {
            EXT_COLORIMETRY => parse_colorimetry(&body[1..], &mut edid.colorimetry),
            EXT_HDR_STATIC_METADATA => edid.hdr = Some(parse_hdr_static_metadata(&body[1..])),
            _ => {}
        }
    }
}

/// The colorimetry block: one byte of signal standards, one of gamut metadata profiles and DCI-P3.
fn parse_colorimetry(body: &[u8], out: &mut Colorimetry) {
    let first = body.first().copied().unwrap_or(0);
    let second = body.get(1).copied().unwrap_or(0);
    out.xv_ycc601 |= first & 0x01 != 0;
    out.xv_ycc709 |= first & 0x02 != 0;
    out.s_ycc601 |= first & 0x04 != 0;
    out.op_ycc601 |= first & 0x08 != 0;
    out.op_rgb |= first & 0x10 != 0;
    out.bt2020_cycc |= first & 0x20 != 0;
    out.bt2020_ycc |= first & 0x40 != 0;
    out.bt2020_rgb |= first & 0x80 != 0;
    out.dci_p3 |= second & 0x80 != 0;
}

/// The HDR block: EOTF bits, descriptor types, then the optional luminance codes (0 means not given).
fn parse_hdr_static_metadata(body: &[u8]) -> HdrStaticMetadata {
    let eotfs = body.first().copied().unwrap_or(0);
    let types = body.get(1).copied().unwrap_or(0);
    let code = |index: usize| body.get(index).copied().filter(|&code| code != 0);
    let max_luminance = code(2).map(max_luminance_nits);
    HdrStaticMetadata {
        eotfs: Eotfs {
            sdr: eotfs & 0x01 != 0,
            hdr: eotfs & 0x02 != 0,
            pq: eotfs & 0x04 != 0,
            hlg: eotfs & 0x08 != 0,
        },
        type1: types & 0x01 != 0,
        max_luminance,
        max_frame_average: code(3).map(max_luminance_nits),
        // The minimum is a fraction of the maximum, so it means nothing without one. A zero code is a real, dark minimum.
        min_luminance: max_luminance
            .zip(body.get(4).copied())
            .map(|(max, code)| min_luminance_nits(code, max)),
    }
}

/// Who made a monitor and which one it is, from the base block of its EDID. Each part is `None`
/// when the EDID leaves it out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// The manufacturer's PNP ID, three capital letters like `DEL`. Not spelled out: that needs the
    /// whole PNP registry.
    pub make: Option<String>,
    /// The product name.
    pub model: Option<String>,
    /// The serial number text, or the number of the base block when there is no text.
    pub serial: Option<String>,
}

impl Identity {
    /// The make, or what clients are told when there is none.
    pub fn make(&self) -> &str {
        self.make.as_deref().unwrap_or(UNKNOWN)
    }

    /// The model, or what clients are told when there is none.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(UNKNOWN)
    }

    /// The serial number, or what clients are told when there is none.
    pub fn serial(&self) -> &str {
        self.serial.as_deref().unwrap_or(UNKNOWN)
    }
}

/// Read who made a monitor and which one it is. Bytes that are no EDID give an empty identity.
pub fn identity(bytes: &[u8]) -> Identity {
    if bytes.len() < BLOCK || bytes[..8] != HEADER {
        return Identity::default();
    }
    let mut identity = Identity {
        make: pnp_id([bytes[8], bytes[9]]),
        ..Identity::default()
    };
    let number = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    if number != 0 {
        identity.serial = Some(number.to_string());
    }
    for start in DESCRIPTORS {
        let descriptor = &bytes[start..start + 18];
        // A display descriptor starts with three zero bytes, a detailed timing does not.
        if descriptor[..3] != [0, 0, 0] {
            continue;
        }
        let text = descriptor_text(&descriptor[5..]);
        match descriptor[3] {
            DESC_NAME => identity.model = text.or(identity.model),
            // The text is the serial number when there is one, the number is a poor second.
            DESC_SERIAL if text.is_some() => identity.serial = text,
            _ => {}
        }
    }
    identity
}

/// The three letters packed into two bytes, five bits each with `A` as 1. `None` for anything else.
fn pnp_id(bytes: [u8; 2]) -> Option<String> {
    let packed = u16::from_be_bytes(bytes);
    [10, 5, 0]
        .into_iter()
        .map(|shift| {
            let letter = ((packed >> shift) & 0x1f) as u8;
            (1..=26)
                .contains(&letter)
                .then(|| char::from(b'A' + letter - 1))
        })
        .collect()
}

/// The text of a descriptor: it ends at a newline, is padded with spaces, and is plain ASCII.
fn descriptor_text(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let text: String = bytes[..end]
        .iter()
        .filter(|b| (0x20..0x7f).contains(*b))
        .map(|&b| char::from(b))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
