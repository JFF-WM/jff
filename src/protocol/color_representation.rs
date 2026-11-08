//! wp-color-representation-v1: a client says how its buffer's values are encoded (alpha mode,
//! YCbCr matrix and range, chroma location).
//!
//! Stage 1 accepts and keeps the values. Stage 2 (config `color-representation { yuv }`) draws
//! YUV buffers with the matrix and range the client declared: see `primitives/yuv.rs` for the
//! arithmetic and `render/effects/yuv.rs` for where it is applied.
//!
//! Only what the renderer does correctly is advertised. Without `yuv`, that is premultiplied
//! electrical alpha and RGB with full range (`identity`), which is what every surface is drawn
//! as. A client that needs more, such as BT.709 limited range video, sees that the compositor
//! has no such support and converts for itself. With `yuv`, BT.601, BT.709, BT.2020, FCC and
//! SMPTE 240M in limited and full range are added, see [`supported_coefficients`]. Straight
//! and optical alpha stay unadvertised.
//!
//! The values are double-buffered like the rest of a surface's state: requests change the
//! pending state and a `wl_surface.commit` makes it current. [`color_representation`] reads
//! the current one. Destroying the extension object unsets everything, also on the next commit.
//!
//! Not checked: the `pixel_format` error, for a committed buffer whose format does not fit what
//! was set. A matrix set on an RGB buffer is ignored when drawing (the shader only changes
//! external, i.e. YUV, textures), and a YUV buffer marked `identity` is drawn as it would be
//! without the protocol.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::reexports::wayland_protocols::wp::color_representation::v1::server::wp_color_representation_manager_v1::{
    self, WpColorRepresentationManagerV1,
};
use smithay::reexports::wayland_protocols::wp::color_representation::v1::server::wp_color_representation_surface_v1::{
    self, WpColorRepresentationSurfaceV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::{with_states, Cacheable, SurfaceData};

use crate::state::JffState;

/// How a pixel's alpha relates to its color. Alpha itself is always linear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    /// Color was multiplied by alpha after the transfer function. The default.
    #[default]
    PremultipliedElectrical,
    /// Color was multiplied by alpha before the transfer function.
    PremultipliedOptical,
    /// Color was not multiplied by alpha.
    Straight,
}

impl AlphaMode {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::PremultipliedElectrical),
            1 => Some(Self::PremultipliedOptical),
            2 => Some(Self::Straight),
            _ => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::PremultipliedElectrical => 0,
            Self::PremultipliedOptical => 1,
            Self::Straight => 2,
        }
    }
}

/// Matrix coefficients (H.273 MatrixCoefficients in the comments).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coefficients {
    /// RGB, 0.
    Identity,
    /// 1.
    Bt709,
    /// 4.
    Fcc,
    /// 5 and 6.
    Bt601,
    /// 7.
    Smpte240,
    /// 9.
    Bt2020,
    /// 10.
    Bt2020Cl,
    /// 14.
    Ictcp,
}

impl Coefficients {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Identity),
            2 => Some(Self::Bt709),
            3 => Some(Self::Fcc),
            4 => Some(Self::Bt601),
            5 => Some(Self::Smpte240),
            6 => Some(Self::Bt2020),
            7 => Some(Self::Bt2020Cl),
            8 => Some(Self::Ictcp),
            _ => None,
        }
    }

    /// The luma weights (Kr, Kb) of the standard, `None` for the ones that have no such pair
    /// (identity is RGB, and the constant luminance ones are not YCbCr in the usual sense).
    pub fn luma_weights(self) -> Option<(f64, f64)> {
        match self {
            Self::Bt709 => Some((0.2126, 0.0722)),
            Self::Fcc => Some((0.30, 0.11)),
            Self::Bt601 => Some((0.299, 0.114)),
            Self::Smpte240 => Some((0.212, 0.087)),
            Self::Bt2020 => Some((0.2627, 0.0593)),
            Self::Identity | Self::Bt2020Cl | Self::Ictcp => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Identity => 1,
            Self::Bt709 => 2,
            Self::Fcc => 3,
            Self::Bt601 => 4,
            Self::Smpte240 => 5,
            Self::Bt2020 => 6,
            Self::Bt2020Cl => 7,
            Self::Ictcp => 8,
        }
    }
}

/// Quantization range of the values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    /// 0 to 255 for 8 bits.
    Full,
    /// 16 to 235 for 8 bits (luma).
    Limited,
}

impl Range {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Full),
            2 => Some(Self::Limited),
            _ => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Limited => 2,
        }
    }
}

/// Where the chroma samples of 4:2:0 YCbCr sit (H.273 Chroma420SampleLocType 0 to 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaLocation {
    Type0,
    Type1,
    Type2,
    Type3,
    Type4,
    Type5,
}

impl ChromaLocation {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Type0),
            2 => Some(Self::Type1),
            3 => Some(Self::Type2),
            4 => Some(Self::Type3),
            5 => Some(Self::Type4),
            6 => Some(Self::Type5),
            _ => None,
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Type0 => 1,
            Self::Type1 => 2,
            Self::Type2 => 3,
            Self::Type3 => 4,
            Self::Type4 => 5,
            Self::Type5 => 6,
        }
    }
}

/// Alpha modes clients are told they can use: what the renderer draws correctly.
pub const SUPPORTED_ALPHA_MODES: &[AlphaMode] = &[AlphaMode::PremultipliedElectrical];

/// What a driver turns a YUV buffer into RGB with when it is told nothing: BT.601, limited
/// range. The shader corrects from this to what the client declared.
pub const DRIVER_COEFFICIENTS: (Coefficients, Range) = (Coefficients::Bt601, Range::Limited);

/// Matrix and range pairs clients are told they can use. `yuv` is the config switch
/// `color-representation { yuv }`: without it only RGB (`identity`, full range).
pub fn supported_coefficients(yuv: bool) -> Vec<(Coefficients, Range)> {
    let mut pairs = vec![(Coefficients::Identity, Range::Full)];
    if yuv {
        for coefficients in [
            Coefficients::Bt709,
            Coefficients::Bt601,
            Coefficients::Bt2020,
            Coefficients::Fcc,
            Coefficients::Smpte240,
        ] {
            pairs.push((coefficients, Range::Limited));
            pairs.push((coefficients, Range::Full));
        }
    }
    pairs
}

/// What a surface has set, as of its last commit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorRepresentation {
    /// Premultiplied electrical when the client set none.
    pub alpha_mode: AlphaMode,
    /// `None`: the client set none, the compositor decides.
    pub coefficients: Option<(Coefficients, Range)>,
    /// `None`: the client set none.
    pub chroma_location: Option<ChromaLocation>,
}

/// The state Smithay double-buffers for the surface. Raw protocol values, checked when set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorRepresentationState {
    alpha_mode: Option<u32>,
    coefficients: Option<(u32, u32)>,
    chroma_location: Option<u32>,
}

impl Cacheable for ColorRepresentationState {
    /// What was set stays set until a request changes it: the pending state carries on.
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// What `surface` set as of its last commit. All unset for a surface that never used the
/// protocol.
pub fn color_representation(surface: &WlSurface) -> ColorRepresentation {
    with_states(surface, color_representation_in)
}

/// [`color_representation`] for code that already has the surface's data (a surface tree walk).
pub fn color_representation_in(states: &SurfaceData) -> ColorRepresentation {
    if !states.cached_state.has::<ColorRepresentationState>() {
        return ColorRepresentation::default();
    }
    let mut cached = states.cached_state.get::<ColorRepresentationState>();
    let current = *cached.current();
    ColorRepresentation {
        alpha_mode: current
            .alpha_mode
            .and_then(AlphaMode::from_raw)
            .unwrap_or_default(),
        coefficients: current.coefficients.and_then(|(coefficients, range)| {
            Some((
                Coefficients::from_raw(coefficients)?,
                Range::from_raw(range)?,
            ))
        }),
        chroma_location: current.chroma_location.and_then(ChromaLocation::from_raw),
    }
}

/// Whether a surface has its extension object already, kept in its data map.
#[derive(Default)]
struct HasExtension(AtomicBool);

pub struct ColorRepresentationManager {
    _global: GlobalId,
}

impl ColorRepresentationManager {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, WpColorRepresentationManagerV1, ()>(1, ()),
        }
    }
}

/// The surface behind one extension object.
pub struct ColorRepresentationSurface {
    surface: WlSurface,
}

impl GlobalDispatch<WpColorRepresentationManagerV1, ()> for JffState {
    fn bind(
        state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorRepresentationManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        for mode in SUPPORTED_ALPHA_MODES {
            if let Ok(mode) = wp_color_representation_surface_v1::AlphaMode::try_from(mode.raw()) {
                manager.supported_alpha_mode(mode);
            }
        }
        for (coefficients, range) in supported_coefficients(state.config.color_representation_yuv())
        {
            if let (Ok(coefficients), Ok(range)) = (
                wp_color_representation_surface_v1::Coefficients::try_from(coefficients.raw()),
                wp_color_representation_surface_v1::Range::try_from(range.raw()),
            ) {
                manager.supported_coefficients_and_ranges(coefficients, range);
            }
        }
        manager.done();
    }
}

impl Dispatch<WpColorRepresentationManagerV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        resource: &WpColorRepresentationManagerV1,
        request: wp_color_representation_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            wp_color_representation_manager_v1::Request::GetSurface { id, surface } => {
                let taken = with_states(&surface, |states| {
                    states
                        .data_map
                        .get_or_insert_threadsafe(HasExtension::default)
                        .0
                        .swap(true, Ordering::SeqCst)
                });
                if taken {
                    resource.post_error(
                        wp_color_representation_manager_v1::Error::SurfaceExists,
                        "the surface already has a color representation object",
                    );
                    return;
                }
                data_init.init(id, ColorRepresentationSurface { surface });
            }
            wp_color_representation_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpColorRepresentationSurfaceV1, ColorRepresentationSurface> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &WpColorRepresentationSurfaceV1,
        request: wp_color_representation_surface_v1::Request,
        data: &ColorRepresentationSurface,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_color_representation_surface_v1::{Error, Request};

        if matches!(request, Request::Destroy) {
            return; // `destroyed` unsets what was set
        }
        if !data.surface.is_alive() {
            resource.post_error(Error::Inert, "the wl_surface is gone");
            return;
        }
        match request {
            Request::SetAlphaMode { alpha_mode } => {
                let raw = u32::from(alpha_mode);
                let supported = AlphaMode::from_raw(raw)
                    .is_some_and(|mode| SUPPORTED_ALPHA_MODES.contains(&mode));
                if !supported {
                    resource.post_error(Error::AlphaMode, "alpha mode is not supported");
                    return;
                }
                set_pending(&data.surface, |state| state.alpha_mode = Some(raw));
            }
            Request::SetCoefficientsAndRange {
                coefficients,
                range,
            } => {
                let (coefficients, range) = (u32::from(coefficients), u32::from(range));
                let supported = Coefficients::from_raw(coefficients)
                    .zip(Range::from_raw(range))
                    .is_some_and(|pair| {
                        supported_coefficients(state.config.color_representation_yuv())
                            .contains(&pair)
                    });
                if !supported {
                    resource.post_error(
                        Error::Coefficients,
                        "matrix coefficients and range are not supported",
                    );
                    return;
                }
                set_pending(&data.surface, |state| {
                    state.coefficients = Some((coefficients, range));
                });
            }
            Request::SetChromaLocation { chroma_location } => {
                let raw = u32::from(chroma_location);
                if ChromaLocation::from_raw(raw).is_none() {
                    resource.post_error(Error::ChromaLocation, "invalid chroma location");
                    return;
                }
                set_pending(&data.surface, |state| state.chroma_location = Some(raw));
            }
            _ => {}
        }
    }

    /// The object is gone, by `destroy` or because its client left: the surface can have a new
    /// one, and what this one set is unset on the next commit.
    fn destroyed(
        _state: &mut JffState,
        _client: ClientId,
        _resource: &WpColorRepresentationSurfaceV1,
        data: &ColorRepresentationSurface,
    ) {
        if !data.surface.is_alive() {
            return;
        }
        with_states(&data.surface, |states| {
            if let Some(has) = states.data_map.get::<HasExtension>() {
                has.0.store(false, Ordering::SeqCst);
            }
            if states.cached_state.has::<ColorRepresentationState>() {
                let mut cached = states.cached_state.get::<ColorRepresentationState>();
                *cached.pending() = ColorRepresentationState::default();
            }
        });
    }
}

fn set_pending(surface: &WlSurface, change: impl FnOnce(&mut ColorRepresentationState)) {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ColorRepresentationState>();
        change(cached.pending());
    });
}
