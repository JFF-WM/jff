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
//! The global is version 3 of the protocol. What version 2 and 3 change is decided here, so it
//! is tested without a display: the transfer function `compound_power_2_4` replaces the
//! deprecated `srgb` for version 2 clients ([`NamedTf::from_raw_in`], [`NamedTf::wire_raw`]),
//! the creator drops the light level range check ([`ParamsBuilder::for_version`]), the
//! Windows-BT.2100 feature is for version 3 clients ([`supported_features`]), and identities are
//! 64 bits ([`LongIdentities`]), of which a version 1 client sees the low 32
//! ([`short_identity`]).
//!
//! Chromaticities are CIE 1931 xy times 1,000,000, minimum luminance is cd/m² times 10,000 and
//! the other luminances are whole cd/m², all as on the wire, so descriptions compare exactly.
//!
//! A PQ description may also say how bright its content gets: `max_cll`, or else the maximum of
//! the mastering luminance. [`ImageDescription::content_light`] turns that into the number the
//! HDR final pass fits its tone mapping to (`primitives/hdr_output.rs`). The mastering
//! primaries are only checked, and the frame-average level is only passed on.

/// Scale of a chromaticity coordinate on the wire.
pub const CHROMA_SCALE: i32 = 1_000_000;

/// Scale of a minimum luminance on the wire.
pub const MIN_LUM_SCALE: u32 = 10_000;

/// Exponent of a power transfer function is sent times this.
pub const EXPONENT_SCALE: u32 = 10_000;

/// How a surface wants its colors brought to the output (see `primitives/gamut.rs`).
///
/// * perceptual desaturates a color the output cannot show toward the grey of the same luminance until it fits,
/// * saturation keeps the primaries pure: red, green and blue of the surface are the output's own, whatever the gamuts, and what is left outside goes the perceptual way,
/// * relative colorimetric moves the white point over (Bradford) and clips what does not fit,
/// * relative with black point compensation does the same after the black of the surface is moved onto the output's,
/// * absolute colorimetric is the same as relative here, as it is for every description made of primaries and a curve, whose media white is the white point they state,
/// * absolute without adaptation keeps the color itself (CIE XYZ) and moves no white point, so a surface with another white than the output's shows it tinted.
///
/// Colors that fit are the same for the perceptual intents and the colorimetric ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderIntent {
    /// 0. Mandatory.
    Perceptual,
    /// 1.
    Relative,
    /// 2.
    Saturation,
    /// 3.
    Absolute,
    /// 4.
    RelativeBpc,
    /// 5, from version 2 of the protocol.
    AbsoluteNoAdaptation,
}

impl RenderIntent {
    /// `None` for a number that is not an intent.
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Perceptual),
            1 => Some(Self::Relative),
            2 => Some(Self::Saturation),
            3 => Some(Self::Absolute),
            4 => Some(Self::RelativeBpc),
            5 => Some(Self::AbsoluteNoAdaptation),
            _ => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Perceptual => 0,
            Self::Relative => 1,
            Self::Saturation => 2,
            Self::Absolute => 3,
            Self::RelativeBpc => 4,
            Self::AbsoluteNoAdaptation => 5,
        }
    }

    /// The version of the protocol this intent came with.
    pub fn since(self) -> u32 {
        match self {
            Self::AbsoluteNoAdaptation => 2,
            _ => 1,
        }
    }
}

/// Rendering intents clients are told they can use.
pub const SUPPORTED_INTENTS: &[RenderIntent] = &[
    RenderIntent::Perceptual,
    RenderIntent::Relative,
    RenderIntent::Saturation,
    RenderIntent::Absolute,
    RenderIntent::RelativeBpc,
    RenderIntent::AbsoluteNoAdaptation,
];

/// Intents a client that bound `version` of the protocol is told it can use.
pub fn supported_intents(version: u32) -> Vec<RenderIntent> {
    SUPPORTED_INTENTS
        .iter()
        .copied()
        .filter(|intent| intent.since() <= version)
        .collect()
}

/// Optional parts of the protocol jff offers. ICC profiles and extended target volumes are not.
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
    /// 5. A target color volume inside the primary one. Its primaries are checked and not drawn
    /// differently, its luminance is what an HDR monitor fits the tone mapping to when the
    /// description has no `max_cll` (see [`ImageDescription::content_light`]).
    SetMasteringDisplayPrimaries,
    /// 7. `create_windows_scrgb`: the sRGB-primaries, extended-linear, 80 cd/m² reference
    /// description that Mesa's Vulkan WSI and Wine/Proton use for fp16 HDR swapchains.
    WindowsScrgb,
    /// 8. `create_windows_bt2100` (version 3): BT.2020 primaries and PQ, as Windows 10 drives an
    /// HDR screen in BT.2100 mode. Its reference white and target volume are unknown.
    WindowsBt2100,
}

impl Feature {
    pub fn raw(self) -> u32 {
        match self {
            Self::Parametric => 1,
            Self::SetPrimaries => 2,
            Self::SetTfPower => 3,
            Self::SetLuminances => 4,
            Self::SetMasteringDisplayPrimaries => 5,
            Self::WindowsScrgb => 7,
            Self::WindowsBt2100 => 8,
        }
    }

    /// The first version of the protocol a client can use the feature in.
    pub fn since(self) -> u32 {
        match self {
            Self::WindowsBt2100 => 3,
            _ => 1,
        }
    }
}

/// Features clients of version 1 are told they can use. See [`supported_features`].
pub const SUPPORTED_FEATURES: &[Feature] = &[
    Feature::Parametric,
    Feature::SetPrimaries,
    Feature::SetTfPower,
    Feature::SetLuminances,
    Feature::SetMasteringDisplayPrimaries,
    Feature::WindowsScrgb,
];

/// Features a client that bound `version` of the protocol is told it can use.
pub fn supported_features(version: u32) -> Vec<Feature> {
    SUPPORTED_FEATURES
        .iter()
        .copied()
        .chain([Feature::WindowsBt2100])
        .filter(|feature| feature.since() <= version)
        .collect()
}

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

/// The protocol's `compound_power_2_4` (version 2), the piecewise sRGB curve under a name that
/// says which curve it is. It replaces `srgb` (9), which is deprecated.
const COMPOUND_POWER_2_4: u32 = 14;

/// Named transfer functions jff offers, plus [`NamedTf::ExtLinear`], which only the
/// Windows-scRGB description has (it is not in [`NamedTf::ALL`], so a client cannot name it in
/// a parametric creator).
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
    /// 5. Extended linear: light is the encoded value, which may be below 0 or above 1. Only
    /// Windows-scRGB (`create_windows_scrgb`) uses it.
    ExtLinear,
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

    /// [`NamedTf::from_raw`] as a client of protocol `version` can name it. Version 2 deprecates
    /// `srgb` (9) for `compound_power_2_4` (14), the same piecewise curve: a version 2 client
    /// that names 9 has named something that was not advertised, and 14 is [`NamedTf::Srgb`].
    pub fn from_raw_in(raw: u32, version: u32) -> Option<Self> {
        if version >= 2 {
            match raw {
                COMPOUND_POWER_2_4 => Some(Self::Srgb),
                9 => None,
                _ => Self::from_raw(raw),
            }
        } else {
            Self::from_raw(raw)
        }
    }

    /// The number a client of protocol `version` is sent for this curve: [`NamedTf::raw`], and
    /// for the piecewise sRGB curve `compound_power_2_4` from version 2 on.
    pub fn wire_raw(self, version: u32) -> u32 {
        match self {
            Self::Srgb if version >= 2 => COMPOUND_POWER_2_4,
            _ => self.raw(),
        }
    }

    /// The name IPC clients see, the protocol's own (`srgb`, `gamma22`, `gamma28`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::Gamma22 => "gamma22",
            Self::Gamma28 => "gamma28",
            Self::St2084Pq => "st2084_pq",
            Self::Hlg => "hlg",
            Self::ExtLinear => "ext_linear",
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Gamma22 => 2,
            Self::Gamma28 => 3,
            Self::ExtLinear => 5,
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

    /// What Windows-scRGB has: 1.0 is 80 cd/m², and that is also its reference white. Values
    /// above 1.0 are the highlights, so `max` is only the nominal range (see
    /// [`ImageDescription::windows_scrgb`]), only `reference` is used to place the light.
    pub const SCRGB: Self = Self {
        min: 2_000,
        max: 80,
        reference: 80,
    };
}

/// The luminance range of the display the content was mastered on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MasteringLuminance {
    /// cd/m² times [`MIN_LUM_SCALE`].
    pub min: u32,
    /// cd/m².
    pub max: u32,
}

/// How bright the content of a surface gets, as far as its description says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ContentLight {
    /// Bounded by its own white (sRGB, gamma, power curves): nothing to fit.
    Sdr,
    /// An absolute-light curve (PQ) that says its peak, in cd/m².
    Peak(f32),
    /// Light that can be as bright as the curve allows and says nothing about it: PQ without
    /// metadata, HLG and Windows-scRGB.
    Unknown,
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
    /// Luminance range of the mastering display. Its primaries are not kept: they are only checked.
    pub mastering_luminance: Option<MasteringLuminance>,
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
            mastering_luminance: None,
        }
    }

    /// Windows-scRGB (`create_windows_scrgb`): sRGB primaries and white, an extended linear
    /// curve in which 1.0 is 80 cd/m², so a value above 1.0 is brighter than SDR white and one
    /// below 0 is a color outside the sRGB gamut. Games reach it through fp16 swapchains
    /// (`VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT`).
    pub const fn windows_scrgb() -> Self {
        let srgb = Self::srgb();
        Self {
            primaries: srgb.primaries,
            tf: TransferFunction::Named(NamedTf::ExtLinear),
            luminance: Luminance::SCRGB,
            max_cll: None,
            max_fall: None,
            mastering_luminance: None,
        }
    }

    /// Windows-BT.2100 (`create_windows_bt2100`, version 3): BT.2020 primaries and PQ, with the
    /// luminances PQ has when a description says nothing (reference white 203 cd/m², what the
    /// protocol says to assume). Its target volume is unknown, so nothing more is claimed.
    pub fn windows_bt2100() -> Self {
        Self {
            primaries: NamedPrimaries::Bt2020.chromaticities(),
            tf: TransferFunction::Named(NamedTf::St2084Pq),
            luminance: Luminance::PQ,
            ..Self::srgb()
        }
    }

    /// The name of the primaries, if they are exactly an offered set.
    pub fn named_primaries(&self) -> Option<NamedPrimaries> {
        NamedPrimaries::detect(&self.primaries)
    }

    /// How bright this description's content gets, for fitting an HDR monitor's tone mapping to it.
    ///
    /// Only absolute-light curves have a peak to speak of: PQ gives `max_cll` if it is set and
    /// not 0 (`build` already turns a 0 into "not set"), else the maximum of the mastering
    /// luminance, clamped to what PQ can carry. A PQ description with neither, and HLG and
    /// Windows-scRGB (whose highlights are not bounded by a level they state), are
    /// [`ContentLight::Unknown`]. Everything else is [`ContentLight::Sdr`].
    pub fn content_light(&self) -> ContentLight {
        match self.tf {
            TransferFunction::Named(NamedTf::St2084Pq) => {
                let level = self
                    .max_cll
                    .filter(|&v| v != 0)
                    .or(self.mastering_luminance.map(|m| m.max).filter(|&v| v != 0));
                match level {
                    Some(level) => ContentLight::Peak(level.min(Luminance::PQ.max) as f32),
                    None => ContentLight::Unknown,
                }
            }
            TransferFunction::Named(NamedTf::Hlg | NamedTf::ExtLinear) => ContentLight::Unknown,
            _ => ContentLight::Sdr,
        }
    }

    /// The peak of the content in cd/m² if the description states one: `max_cll`, else the
    /// maximum mastering luminance, for PQ only (see [`ImageDescription::content_light`]).
    pub fn content_peak(&self) -> Option<f32> {
        match self.content_light() {
            ContentLight::Peak(peak) => Some(peak),
            _ => None,
        }
    }

    /// What `wp_image_description_info_v1` tells about this description, in the order it is
    /// sent. The target volume is the mastering display's luminance when the description has
    /// one, else the primary volume's (and the primaries are always the primary ones, the
    /// mastering primaries are not kept).
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
        events.push(match self.mastering_luminance {
            Some(mastering) => InfoEvent::TargetLuminance {
                min: mastering.min,
                max: mastering.max,
            },
            None => InfoEvent::TargetLuminance { min, max },
        });
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
    /// The version of the protocol the client bound. 0 (the default) is taken as 1.
    version: u32,
    tf: Option<TransferFunction>,
    primaries: Option<Chromaticities>,
    luminance: Option<Luminance>,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
    mastering_primaries: Option<Chromaticities>,
    mastering_luminance: Option<(u32, u32)>,
}

impl ParamsBuilder {
    /// A builder for a client that bound `version` of the protocol. The rules that changed in
    /// version 2 apply from there on: which transfer functions can be named, and that the light
    /// levels are not held to a range.
    pub fn for_version(version: u32) -> Self {
        Self {
            version,
            ..Self::default()
        }
    }

    pub fn set_tf_named(&mut self, raw: u32) -> Result<(), Violation> {
        if self.tf.is_some() {
            return violation(
                CreatorError::AlreadySet,
                "the transfer function is set already",
            );
        }
        match NamedTf::from_raw_in(raw, self.version) {
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

    /// Whether the numbers can be drawn is decided by `build`, they are not an error here.
    pub fn set_mastering_display_primaries(&mut self, values: [i32; 8]) -> Result<(), Violation> {
        if !supports(Feature::SetMasteringDisplayPrimaries) {
            return violation(
                CreatorError::UnsupportedFeature,
                "set_mastering_display_primaries is not offered",
            );
        }
        if self.mastering_primaries.is_some() {
            return violation(
                CreatorError::AlreadySet,
                "the mastering display primaries are set already",
            );
        }
        self.mastering_primaries = Some(Chromaticities::from_array(values));
        Ok(())
    }

    /// `min` is cd/m² times [`MIN_LUM_SCALE`], `max` is cd/m².
    pub fn set_mastering_luminance(&mut self, min: u32, max: u32) -> Result<(), Violation> {
        if !supports(Feature::SetMasteringDisplayPrimaries) {
            return violation(
                CreatorError::UnsupportedFeature,
                "set_mastering_display_primaries is not offered",
            );
        }
        if self.mastering_luminance.is_some() {
            return violation(
                CreatorError::AlreadySet,
                "the mastering luminance is set already",
            );
        }
        if !above_min(min, max) {
            return violation(
                CreatorError::InvalidLuminance,
                "the mastering maximum luminance must be above the minimum",
            );
        }
        self.mastering_luminance = Some((min, max));
        Ok(())
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
    /// below the maximum luminance. Version 2 and later do not: only `max_fall` above `max_cll`
    /// is an error, and a level beyond the range is taken as it is (the tone mapping clamps it
    /// to what PQ carries). A level of 0 is how CTA-861 says "unknown", so it counts as not set
    /// instead of being an error.
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
        for level in [max_cll, max_fall]
            .into_iter()
            .flatten()
            .filter(|_| self.version < 2)
        {
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
        // The target primaries are a hint: they are checked so a nonsense set fails, and the
        // picture is converted from the container volume. The luminance was checked when it was
        // set, and is kept: it is what the tone mapping fits to when there is no `max_cll`.
        if let Some(Err(reason)) = self.mastering_primaries.map(|m| m.check()) {
            return Err(CreateError::Unsupported(reason));
        }
        Ok(ImageDescription {
            primaries,
            tf,
            luminance,
            max_cll,
            max_fall,
            mastering_luminance: self
                .mastering_luminance
                .map(|(min, max)| MasteringLuminance { min, max }),
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

/// [`Identities`] for 64-bit numbers (`ready2`, version 2): zero is never used, a number is never
/// given twice, and none is recycled, so a client that holds on to a number can trust it.
#[derive(Clone, Debug)]
pub struct LongIdentities {
    next: Option<u64>,
}

impl LongIdentities {
    pub fn new() -> Self {
        Self::starting_at(1)
    }

    /// For tests of the end of the range.
    pub fn starting_at(next: u64) -> Self {
        Self {
            next: Some(next.max(1)),
        }
    }

    /// `None` when all 64 bits are used.
    pub fn next(&mut self) -> Option<u64> {
        let id = self.next?;
        self.next = id.checked_add(1);
        Some(id)
    }
}

impl Default for LongIdentities {
    fn default() -> Self {
        Self::new()
    }
}

/// The high and the low 32 bits of an identity, as `ready2` and `preferred_changed2` carry it.
pub fn split_identity(identity: u64) -> (u32, u32) {
    ((identity >> 32) as u32, identity as u32)
}

/// What a version 1 client is sent for an identity: the number itself, which is all of it while it fits in 32 bits. `None` when it does not, there is no number a version 1 client could tell it by.
pub fn short_identity(identity: u64) -> Option<u32> {
    u32::try_from(identity).ok().filter(|&id| id != 0)
}
