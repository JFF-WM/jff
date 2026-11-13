//! What jff tells clients about color (wp_color_management_v1, stage 1) and how a parametric
//! image description is built and checked.
//!
//! The protocol's numbers stay raw `u32`s at the edge: the Wayland module turns the ones a
//! request carries into these types and back, so the rules live here and are tested without a
//! display.
//!
//! Only what the renderer will do is offered: the primaries and transfer functions of the SDR
//! pipeline (stage 2). Transfer functions for HDR (PQ, extended linear) are added with the HDR
//! path (stage 4), and are refused until then, like every other name that is not listed.
//!
//! Chromaticities are CIE 1931 xy times 1,000,000, minimum luminance is cd/m² times 10,000 and
//! the other luminances are whole cd/m², all as on the wire, so descriptions compare exactly.

/// Scale of a chromaticity coordinate on the wire.
pub const CHROMA_SCALE: i32 = 1_000_000;

/// Scale of a minimum luminance on the wire.
pub const MIN_LUM_SCALE: u32 = 10_000;

/// Exponent of a power transfer function is sent times this.
pub const EXPONENT_SCALE: u32 = 10_000;

/// How out-of-gamut colors are treated (see `primitives/gamut.rs`): perceptual desaturates a
/// color toward the grey of the same luminance until the output can show it, relative
/// colorimetric clips it to the edge of the output's gamut. Colors that fit are the same for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderIntent {
    /// 0. Mandatory.
    Perceptual,
    /// 1.
    Relative,
}

impl RenderIntent {
    /// `None` for an intent that is not offered.
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Perceptual),
            1 => Some(Self::Relative),
            _ => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Perceptual => 0,
            Self::Relative => 1,
        }
    }
}

/// Rendering intents clients are told they can use.
pub const SUPPORTED_INTENTS: &[RenderIntent] = &[RenderIntent::Perceptual, RenderIntent::Relative];

/// Optional parts of the protocol jff offers. ICC profiles, mastering display primaries,
/// extended target volumes and the Windows formats are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// 1.
    Parametric,
    /// 2.
    SetPrimaries,
    /// 3.
    SetTfPower,
    /// 4.
    SetLuminances,
}

impl Feature {
    pub fn raw(self) -> u32 {
        match self {
            Self::Parametric => 1,
            Self::SetPrimaries => 2,
            Self::SetTfPower => 3,
            Self::SetLuminances => 4,
        }
    }
}

/// Features clients are told they can use.
pub const SUPPORTED_FEATURES: &[Feature] = &[
    Feature::Parametric,
    Feature::SetPrimaries,
    Feature::SetTfPower,
    Feature::SetLuminances,
];

fn supports(feature: Feature) -> bool {
    SUPPORTED_FEATURES.contains(&feature)
}

/// The corners of an RGB gamut and its white point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chromaticities {
    pub red: [i32; 2],
    pub green: [i32; 2],
    pub blue: [i32; 2],
    pub white: [i32; 2],
}

impl Chromaticities {
    /// In the order of the protocol: r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y.
    pub fn from_array(v: [i32; 8]) -> Self {
        Self {
            red: [v[0], v[1]],
            green: [v[2], v[3]],
            blue: [v[4], v[5]],
            white: [v[6], v[7]],
        }
    }

    /// The same eight numbers, in the order of the protocol.
    pub fn to_array(self) -> [i32; 8] {
        [
            self.red[0],
            self.red[1],
            self.green[0],
            self.green[1],
            self.blue[0],
            self.blue[1],
            self.white[0],
            self.white[1],
        ]
    }

    /// Whether these can be turned into a conversion matrix: every point is a real chromaticity
    /// (inside the unit triangle, with y above zero) and the three primaries span an area.
    pub fn check(&self) -> Result<(), &'static str> {
        for [x, y] in [self.red, self.green, self.blue, self.white] {
            if x < 0 || y <= 0 || x > CHROMA_SCALE || y > CHROMA_SCALE {
                return Err("a chromaticity is outside 0 to 1, or has y of 0");
            }
            if x + y > CHROMA_SCALE {
                return Err("a chromaticity has x + y above 1");
            }
        }
        let [rx, ry] = self.red.map(i64::from);
        let [gx, gy] = self.green.map(i64::from);
        let [bx, by] = self.blue.map(i64::from);
        if (gx - rx) * (by - ry) - (gy - ry) * (bx - rx) == 0 {
            return Err("the primaries are on one line");
        }
        Ok(())
    }
}

/// Named primaries jff offers. Others the protocol has (PAL-M, generic film, CIE XYZ) are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedPrimaries {
    /// 1. BT.709, the primaries of sRGB.
    Srgb,
    /// 3. BT.601 for 625 lines.
    Pal,
    /// 4. BT.601 for 525 lines (SMPTE 170M).
    Ntsc,
    /// 6.
    Bt2020,
    /// 8. P3 primaries with the DCI white point.
    DciP3,
    /// 9. P3 primaries with D65.
    DisplayP3,
    /// 10.
    AdobeRgb,
}

impl NamedPrimaries {
    pub const ALL: [Self; 7] = [
        Self::Srgb,
        Self::Pal,
        Self::Ntsc,
        Self::Bt2020,
        Self::DciP3,
        Self::DisplayP3,
        Self::AdobeRgb,
    ];

    /// `None` for a value that is not an offered name.
    pub fn from_raw(raw: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.raw() == raw)
    }

    /// The name IPC clients see (`jff msg watch outputs`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::Pal => "pal",
            Self::Ntsc => "ntsc",
            Self::Bt2020 => "bt2020",
            Self::DciP3 => "dci-p3",
            Self::DisplayP3 => "display-p3",
            Self::AdobeRgb => "adobe-rgb",
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Srgb => 1,
            Self::Pal => 3,
            Self::Ntsc => 4,
            Self::Bt2020 => 6,
            Self::DciP3 => 8,
            Self::DisplayP3 => 9,
            Self::AdobeRgb => 10,
        }
    }

    pub fn chromaticities(self) -> Chromaticities {
        const D65: [i32; 2] = [312_700, 329_000];
        let (red, green, blue, white) = match self {
            Self::Srgb => (
                [640_000, 330_000],
                [300_000, 600_000],
                [150_000, 60_000],
                D65,
            ),
            Self::Pal => (
                [640_000, 330_000],
                [290_000, 600_000],
                [150_000, 60_000],
                D65,
            ),
            Self::Ntsc => (
                [630_000, 340_000],
                [310_000, 595_000],
                [155_000, 70_000],
                D65,
            ),
            Self::Bt2020 => (
                [708_000, 292_000],
                [170_000, 797_000],
                [131_000, 46_000],
                D65,
            ),
            Self::DciP3 => (
                [680_000, 320_000],
                [265_000, 690_000],
                [150_000, 60_000],
                [314_000, 351_000],
            ),
            Self::DisplayP3 => (
                [680_000, 320_000],
                [265_000, 690_000],
                [150_000, 60_000],
                D65,
            ),
            Self::AdobeRgb => (
                [640_000, 330_000],
                [210_000, 710_000],
                [150_000, 60_000],
                D65,
            ),
        };
        Chromaticities {
            red,
            green,
            blue,
            white,
        }
    }

    /// The name of a set of chromaticities, if it is exactly one of the offered ones.
    pub fn detect(chromaticities: &Chromaticities) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.chromaticities() == *chromaticities)
    }
}

/// Primaries clients are told they can name.
pub const SUPPORTED_PRIMARIES: &[NamedPrimaries] = &NamedPrimaries::ALL;

/// Named transfer functions jff offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedTf {
    /// 2. Display gamma 2.2.
    Gamma22,
    /// 3. Display gamma 2.8.
    Gamma28,
    /// 9. The piecewise sRGB curve (version 1 of the protocol only).
    Srgb,
    /// 11. SMPTE ST 2084 (PQ): absolute luminance up to 10000 cd/m².
    St2084Pq,
    /// 13. Hybrid log-gamma (BT.2100-2), taken as shown on a 1000 cd/m² display.
    Hlg,
}

impl NamedTf {
    pub const ALL: [Self; 5] = [
        Self::Srgb,
        Self::Gamma22,
        Self::Gamma28,
        Self::St2084Pq,
        Self::Hlg,
    ];

    /// `None` for a value that is not an offered name.
    pub fn from_raw(raw: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|tf| tf.raw() == raw)
    }

    /// The name IPC clients see, the protocol's own (`srgb`, `gamma22`, `gamma28`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::Gamma22 => "gamma22",
            Self::Gamma28 => "gamma28",
            Self::St2084Pq => "st2084_pq",
            Self::Hlg => "hlg",
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Gamma22 => 2,
            Self::Gamma28 => 3,
            Self::Srgb => 9,
            Self::St2084Pq => 11,
            Self::Hlg => 13,
        }
    }
}

/// Transfer functions clients are told they can name.
pub const SUPPORTED_TFS: &[NamedTf] = &NamedTf::ALL;

/// How electrical values relate to light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransferFunction {
    Named(NamedTf),
    /// A pure power curve, the exponent times [`EXPONENT_SCALE`].
    Power(u32),
}

/// The range of light a description covers and what its reference white is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Luminance {
    /// cd/m² times [`MIN_LUM_SCALE`].
    pub min: u32,
    /// cd/m².
    pub max: u32,
    /// cd/m².
    pub reference: u32,
}

impl Luminance {
    /// What a PQ description has when it says nothing: 0.005 to 10000 cd/m², white at 203.
    pub const PQ: Self = Self {
        min: 50,
        max: 10_000,
        reference: 203,
    };

    /// What an HLG description has when it says nothing: 0.005 to 1000 cd/m², white at 203.
    pub const HLG: Self = Self {
        min: 50,
        max: 1_000,
        reference: 203,
    };

    /// What an SDR description has when it says nothing: 0.2 to 80 cd/m², white at 80.
    pub const SDR: Self = Self {
        min: 2_000,
        max: 80,
        reference: 80,
    };
}

/// Colors as a surface or an output has them: primaries, transfer function and luminance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageDescription {
    pub primaries: Chromaticities,
    pub tf: TransferFunction,
    pub luminance: Luminance,
    /// Maximum content light level, cd/m².
    pub max_cll: Option<u32>,
    /// Maximum frame-average light level, cd/m².
    pub max_fall: Option<u32>,
}

impl ImageDescription {
    /// What a surface with no description is taken to be, and what every output is described
    /// as until the output pipeline knows better.
    pub const fn srgb() -> Self {
        Self {
            primaries: Chromaticities {
                red: [640_000, 330_000],
                green: [300_000, 600_000],
                blue: [150_000, 60_000],
                white: [312_700, 329_000],
            },
            tf: TransferFunction::Named(NamedTf::Srgb),
            luminance: Luminance::SDR,
            max_cll: None,
            max_fall: None,
        }
    }

    /// The name of the primaries, if they are exactly an offered set.
    pub fn named_primaries(&self) -> Option<NamedPrimaries> {
        NamedPrimaries::detect(&self.primaries)
    }

    /// What `wp_image_description_info_v1` tells about this description, in the order it is
    /// sent. There is no mastering display, so the target volume is the primary one.
    pub fn info(&self) -> Vec<InfoEvent> {
        let mut events = vec![InfoEvent::Primaries(self.primaries.to_array())];
        if let Some(named) = self.named_primaries() {
            events.push(InfoEvent::PrimariesNamed(named.raw()));
        }
        events.push(match self.tf {
            TransferFunction::Named(tf) => InfoEvent::TfNamed(tf.raw()),
            TransferFunction::Power(exponent) => InfoEvent::TfPower(exponent),
        });
        let Luminance {
            min,
            max,
            reference,
        } = self.luminance;
        events.push(InfoEvent::Luminances {
            min,
            max,
            reference,
        });
        events.push(InfoEvent::TargetPrimaries(self.primaries.to_array()));
        events.push(InfoEvent::TargetLuminance { min, max });
        if let Some(cll) = self.max_cll {
            events.push(InfoEvent::TargetMaxCll(cll));
        }
        if let Some(fall) = self.max_fall {
            events.push(InfoEvent::TargetMaxFall(fall));
        }
        events
    }
}

/// One event of `wp_image_description_info_v1` (the final `done` is not listed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoEvent {
    Primaries([i32; 8]),
    /// The raw protocol value of the name.
    PrimariesNamed(u32),
    TfPower(u32),
    /// The raw protocol value of the name.
    TfNamed(u32),
    Luminances {
        min: u32,
        max: u32,
        reference: u32,
    },
    TargetPrimaries([i32; 8]),
    TargetLuminance {
        min: u32,
        max: u32,
    },
    TargetMaxCll(u32),
    TargetMaxFall(u32),
}

/// Errors of `wp_image_description_creator_params_v1`, with the protocol's values. Newer
/// versions of the protocol call 1 `already_set`, older ones `inconsistent_set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatorError {
    IncompleteSet,
    /// A property was set twice.
    AlreadySet,
    UnsupportedFeature,
    InvalidTf,
    InvalidPrimariesNamed,
    InvalidLuminance,
}

impl CreatorError {
    pub fn code(self) -> u32 {
        match self {
            Self::IncompleteSet => 0,
            Self::AlreadySet => 1,
            Self::UnsupportedFeature => 2,
            Self::InvalidTf => 3,
            Self::InvalidPrimariesNamed => 4,
            Self::InvalidLuminance => 5,
        }
    }
}

/// A request that breaks a rule of the protocol: the client is disconnected with `error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation {
    pub error: CreatorError,
    pub message: &'static str,
}

fn violation<T>(error: CreatorError, message: &'static str) -> Result<T, Violation> {
    Err(Violation { error, message })
}

/// Why `create` gave no description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateError {
    /// A rule of the protocol was broken.
    Protocol(Violation),
    /// The parameters are fine but jff cannot draw them: the description fails gracefully
    /// (`failed` with cause `unsupported`).
    Unsupported(&'static str),
}

/// The parameters a client has set so far on a `wp_image_description_creator_params_v1`.
#[derive(Clone, Debug, Default)]
pub struct ParamsBuilder {
    tf: Option<TransferFunction>,
    primaries: Option<Chromaticities>,
    luminance: Option<Luminance>,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
}

impl ParamsBuilder {
    pub fn set_tf_named(&mut self, raw: u32) -> Result<(), Violation> {
        if self.tf.is_some() {
            return violation(
                CreatorError::AlreadySet,
                "the transfer function is set already",
            );
        }
        match NamedTf::from_raw(raw) {
            Some(tf) => self.tf = Some(TransferFunction::Named(tf)),
            None => return violation(CreatorError::InvalidTf, "unknown transfer function"),
        }
        Ok(())
    }

    /// `exponent` is the curve exponent times [`EXPONENT_SCALE`].
    pub fn set_tf_power(&mut self, exponent: u32) -> Result<(), Violation> {
        if !supports(Feature::SetTfPower) {
            return violation(
                CreatorError::UnsupportedFeature,
                "set_tf_power is not offered",
            );
        }
        if self.tf.is_some() {
            return violation(
                CreatorError::AlreadySet,
                "the transfer function is set already",
            );
        }
        if !(EXPONENT_SCALE..=10 * EXPONENT_SCALE).contains(&exponent) {
            return violation(
                CreatorError::InvalidTf,
                "the exponent is outside 1.0 to 10.0",
            );
        }
        self.tf = Some(TransferFunction::Power(exponent));
        Ok(())
    }

    pub fn set_primaries_named(&mut self, raw: u32) -> Result<(), Violation> {
        if self.primaries.is_some() {
            return violation(CreatorError::AlreadySet, "the primaries are set already");
        }
        match NamedPrimaries::from_raw(raw) {
            Some(named) => self.primaries = Some(named.chromaticities()),
            None => {
                return violation(
                    CreatorError::InvalidPrimariesNamed,
                    "unknown named primaries",
                )
            }
        }
        Ok(())
    }

    /// Whether the numbers can be drawn is decided by `build`, they are not an error here.
    pub fn set_primaries(&mut self, values: [i32; 8]) -> Result<(), Violation> {
        if !supports(Feature::SetPrimaries) {
            return violation(
                CreatorError::UnsupportedFeature,
                "set_primaries is not offered",
            );
        }
        if self.primaries.is_some() {
            return violation(CreatorError::AlreadySet, "the primaries are set already");
        }
        self.primaries = Some(Chromaticities::from_array(values));
        Ok(())
    }

    /// `min` is cd/m² times [`MIN_LUM_SCALE`], `max` and `reference` are cd/m².
    pub fn set_luminances(&mut self, min: u32, max: u32, reference: u32) -> Result<(), Violation> {
        if !supports(Feature::SetLuminances) {
            return violation(
                CreatorError::UnsupportedFeature,
                "set_luminances is not offered",
            );
        }
        if self.luminance.is_some() {
            return violation(CreatorError::AlreadySet, "the luminances are set already");
        }
        if !above_min(min, max) || !above_min(min, reference) {
            return violation(
                CreatorError::InvalidLuminance,
                "the maximum and reference luminance must be above the minimum",
            );
        }
        self.luminance = Some(Luminance {
            min,
            max,
            reference,
        });
        Ok(())
    }

    /// Mastering display primaries are not offered (`set_mastering_display_primaries`).
    pub fn set_mastering_display_primaries(&mut self) -> Result<(), Violation> {
        violation(
            CreatorError::UnsupportedFeature,
            "set_mastering_display_primaries is not offered",
        )
    }

    /// The mastering luminance range needs the same feature as the primaries.
    pub fn set_mastering_luminance(&mut self) -> Result<(), Violation> {
        violation(
            CreatorError::UnsupportedFeature,
            "set_mastering_display_primaries is not offered",
        )
    }

    /// cd/m². The value is checked by `build`. Setting it again replaces it.
    pub fn set_max_cll(&mut self, max_cll: u32) {
        self.max_cll = Some(max_cll);
    }

    /// cd/m². The value is checked by `build`. Setting it again replaces it.
    pub fn set_max_fall(&mut self, max_fall: u32) {
        self.max_fall = Some(max_fall);
    }

    /// Check the set and make the description.
    ///
    /// Version 1 of the protocol wants a set light level to lie above the minimum and at or
    /// below the maximum luminance. A level of 0 is how CTA-861 says "unknown", so it counts
    /// as not set instead of being an error.
    pub fn build(self) -> Result<ImageDescription, CreateError> {
        let (Some(tf), Some(primaries)) = (self.tf, self.primaries) else {
            return Err(CreateError::Protocol(Violation {
                error: CreatorError::IncompleteSet,
                message: "the transfer function and the primaries must both be set",
            }));
        };
        let luminance = self.luminance.unwrap_or(match tf {
            TransferFunction::Named(NamedTf::St2084Pq) => Luminance::PQ,
            TransferFunction::Named(NamedTf::Hlg) => Luminance::HLG,
            _ => Luminance::SDR,
        });
        let max_cll = self.max_cll.filter(|&v| v != 0);
        let max_fall = self.max_fall.filter(|&v| v != 0);
        if let (Some(cll), Some(fall)) = (max_cll, max_fall) {
            if fall > cll {
                return Err(CreateError::Protocol(Violation {
                    error: CreatorError::InvalidLuminance,
                    message: "max_fall is above max_cll",
                }));
            }
        }
        for level in [max_cll, max_fall].into_iter().flatten() {
            if !above_min(luminance.min, level) || level > luminance.max {
                return Err(CreateError::Protocol(Violation {
                    error: CreatorError::InvalidLuminance,
                    message: "a light level is outside the luminance range",
                }));
            }
        }
        if let Err(reason) = primaries.check() {
            return Err(CreateError::Unsupported(reason));
        }
        Ok(ImageDescription {
            primaries,
            tf,
            luminance,
            max_cll,
            max_fall,
        })
    }
}

/// Whether `level` (cd/m²) is above `min` (cd/m² times [`MIN_LUM_SCALE`]).
fn above_min(min: u32, level: u32) -> bool {
    u64::from(level) * u64::from(MIN_LUM_SCALE) > u64::from(min)
}

/// Hands out the numbers that tell image descriptions apart. Zero is never used, and a number
/// is never given twice.
#[derive(Clone, Debug)]
pub struct Identities {
    next: Option<u32>,
}

impl Identities {
    pub fn new() -> Self {
        Self::starting_at(1)
    }

    /// For tests of the end of the range.
    pub fn starting_at(next: u32) -> Self {
        Self {
            next: Some(next.max(1)),
        }
    }

    /// `None` when all 32 bits are used.
    pub fn next(&mut self) -> Option<u32> {
        let id = self.next?;
        self.next = id.checked_add(1);
        Some(id)
    }
}

impl Default for Identities {
    fn default() -> Self {
        Self::new()
    }
}
