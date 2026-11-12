//! wp-color-management-v1: clients describe the colors of a surface, and ask what the output
//! (and so their surface) prefers.
//!
//! This module keeps the books: a surface's image description and rendering intent are
//! double-buffered like the rest of its state. Requests change the pending state, a
//! `wl_surface.commit` makes it current, and [`surface_color`] reads the current one. Only
//! parametric descriptions exist: ICC profiles, mastering display metadata and the Windows
//! formats are refused with `unsupported_feature`. The rules for building a description, and
//! what is offered, are in `primitives/color.rs`.
//!
//! The renderer reads the books through [`surface_color_in`] and draws a surface whose
//! description is not its monitor's through the color conversion of `primitives/gamut.rs` (see
//! `render/effects/gamut.rs`). [`output_color`] says what a monitor is converted to, or that it
//! is not managed (`color-management` in the config). On an SDR monitor a surface with no
//! description, or plain sRGB, is not touched unless `convert-srgb` is set.
//!
//! A monitor in HDR mode (`hdr` in its `output`, see [`hdr_color`]) is different: every surface
//! is converted into a linear working space ([`WorkingSpace`], BT.2020 with 1.0 at the monitor's
//! `sdr-white`), and the final pass of `render/effects/hdr.rs` maps it to the monitor's peak and
//! codes it as PQ. Nothing is left as the client drew it, so `fullscreen-passthrough` does not
//! apply there and such a monitor is never scanned out directly.
//!
//! The global is version 1 of the protocol: descriptions are announced with `ready` and a 32
//! bit identity. Moving to version 2 or 3 means `ready2`, `preferred_changed2` and the
//! reference objects, and is not needed for anything here.
//!
//! An output is described as sRGB, unless the config gives a monitor an ICC profile: then it is
//! described as that profile says (its primaries, tone curve and white, see
//! `feature::icc::description`), and [`sync_output_color`] keeps it up to date. What clients are
//! told is one description for all outputs, the profile of the first monitor that has one, so
//! the preferred description of any surface is that one, except on a monitor in HDR mode: that
//! one is described as what it is (PQ in BT.2020, its peak, which is the EDID's or `hdr-peak`, and
//! its SDR white), and the surfaces on it are told so. What the renderer converts to is the
//! description of the monitor itself, so a monitor with no profile is still shown as sRGB. When
//! the description clients are told changes, `image_description_changed` and
//! `preferred_changed` go to the objects that asked, which is why those are kept in
//! [`ColorManagement`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::{
    self, WpColorManagementOutputV1,
};
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::{
    self, WpColorManagementSurfaceFeedbackV1,
};
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::{
    self, WpColorManagementSurfaceV1,
};
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::{
    self, WpColorManagerV1,
};
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::{
    self, WpImageDescriptionCreatorParamsV1,
};
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::{
    self, WpImageDescriptionV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::desktop::utils::surface_primary_scanout_output;
use smithay::wayland::compositor::{with_states, Cacheable, SurfaceData};

use crate::config::Config;
use crate::primitives::color::{
    CreateError, Identities, ImageDescription, InfoEvent, ParamsBuilder, RenderIntent,
    SUPPORTED_FEATURES, SUPPORTED_INTENTS, SUPPORTED_PRIMARIES, SUPPORTED_TFS,
};
use crate::state::JffState;

/// An image description and the number that identifies it. Clients compare the numbers to
/// tell whether two descriptions are the same.
#[derive(Debug)]
pub struct Described {
    pub description: ImageDescription,
    pub identity: u32,
}

/// What a surface set as of its last commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceColor {
    pub description: ImageDescription,
    pub intent: RenderIntent,
}

/// What drawing the surfaces of one monitor needs to know about color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputColor {
    /// The colors the monitor shows: what tagged surfaces are converted to.
    pub target: ImageDescription,
    /// A fullscreen window is shown as its client drew it, so it stays eligible for direct
    /// scanout (`color-management { fullscreen-passthrough }`).
    pub fullscreen_passthrough: bool,
    /// Surfaces with no description (plain sRGB) are converted too
    /// (`color-management { convert-srgb }`).
    pub convert_srgb: bool,
    /// The monitor is shown as HDR: every surface is drawn into a linear working space instead, see [`WorkingSpace`].
    pub hdr: Option<WorkingSpace>,
}

/// The linear space an HDR monitor's frame is composited in: BT.2020 primaries, 1.0 is SDR white, and a final pass turns it into the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkingSpace {
    /// Luminance of 1.0, in cd/m².
    pub sdr_white: u32,
}

/// What to convert the surfaces of the monitor `output` to, `None` when it is not managed: the
/// config has `color-management { off }`, or `color-management "off"` for that monitor. Cheap
/// enough to ask every frame, so a change of the config applies at once.
pub fn output_color(config: &Config, color: &ColorManagement, output: &str) -> Option<OutputColor> {
    let settings = config.color_management();
    if settings.off
        || config
            .output(output)
            .is_some_and(|c| c.color_management_off)
    {
        return None;
    }
    Some(OutputColor {
        target: color.monitor_description(output),
        fullscreen_passthrough: settings.fullscreen_passthrough,
        convert_srgb: settings.convert_srgb,
        hdr: None,
    })
}

/// What an HDR monitor's frame is drawn as: `base`, or plain sRGB when the monitor is not color managed, with every surface going to the working space of `space`.
pub fn hdr_color(base: Option<OutputColor>, space: WorkingSpace) -> OutputColor {
    let base = base.unwrap_or(OutputColor {
        target: ImageDescription::srgb(),
        fullscreen_passthrough: false,
        convert_srgb: false,
        hdr: None,
    });
    OutputColor {
        // a fullscreen window is not shown as its client drew it: it is encoded for the signal like the rest
        fullscreen_passthrough: false,
        hdr: Some(space),
        ..base
    }
}

/// What to convert to for a picture that is saved to a file (a screenshot): sRGB, which is what
/// image files are taken to be. `None` when color management is off.
pub fn capture_color(config: &Config) -> Option<OutputColor> {
    (!config.color_management().off).then(|| OutputColor {
        target: ImageDescription::srgb(),
        fullscreen_passthrough: false,
        // The target is sRGB: plain sRGB surfaces are already what the picture should hold.
        convert_srgb: false,
        hdr: None,
    })
}

/// The `wp_color_manager_v1` global and the numbering of descriptions.
pub struct ColorManagement {
    /// `None` when the config turns the protocol off.
    _global: Option<GlobalId>,
    identities: Identities,
    output: Arc<Described>,
    /// The colors of each monitor that has an ICC profile, by connector name. A monitor that
    /// is not in it is sRGB.
    monitors: HashMap<String, ImageDescription>,
    /// What clients are told about each monitor in HDR mode, by connector name.
    hdr: HashMap<String, Arc<Described>>,
    /// Information objects waiting for their events. `done` is a destructor event: sent while
    /// the `get_information` request that made the object is still being dispatched, it
    /// destroys the object before the backend has attached its data to it (a use-after-free).
    /// So the events go out after the dispatch, from [`flush_pending_info`].
    pending_info: Vec<(WpImageDescriptionInfoV1, ImageDescription)>,
    /// Clients' `wp_color_management_output_v1` objects, told when the output's description
    /// changes.
    outputs: Vec<WpColorManagementOutputV1>,
    /// Clients' `wp_color_management_surface_feedback_v1` objects, told when the preferred
    /// description changes.
    feedbacks: Vec<WpColorManagementSurfaceFeedbackV1>,
}

impl ColorManagement {
    /// `enabled` is false for `color-management { off }`: the global is then not created, and
    /// clients never see the protocol.
    pub fn new(display: &DisplayHandle, enabled: bool) -> Self {
        let mut identities = Identities::new();
        let output = Arc::new(Described {
            description: ImageDescription::srgb(),
            identity: identities.next().unwrap_or(1),
        });
        Self {
            _global: enabled
                .then(|| display.create_global::<JffState, WpColorManagerV1, ()>(1, ())),
            identities,
            output,
            monitors: HashMap::new(),
            hdr: HashMap::new(),
            pending_info: Vec::new(),
            outputs: Vec::new(),
            feedbacks: Vec::new(),
        }
    }

    /// The description of the output `name`, which is also the preferred one of the surfaces on it.
    pub fn output_description(&self, name: &str) -> Arc<Described> {
        self.hdr.get(name).unwrap_or(&self.output).clone()
    }

    /// The colors of the monitor `name`: what its ICC profile says, sRGB without one.
    pub fn monitor_description(&self, name: &str) -> ImageDescription {
        self.monitors
            .get(name)
            .copied()
            .unwrap_or_else(ImageDescription::srgb)
    }

    /// Whether the monitor `name` has an ICC profile that was read, so that
    /// [`monitor_description`](Self::monitor_description) is what the profile says and not the
    /// sRGB it falls back to.
    pub fn monitor_has_profile(&self, name: &str) -> bool {
        self.monitors.contains_key(name)
    }

    /// Give the outputs a new description. Returns its identity, `None` when it is the one they
    /// have already (or the numbers ran out, and what they have stays).
    fn set_output_description(&mut self, description: ImageDescription) -> Option<u32> {
        if self.output.description == description {
            return None;
        }
        let identity = self.identities.next()?;
        self.output = Arc::new(Described {
            description,
            identity,
        });
        Some(identity)
    }

    /// A client's description with a number of its own. `None` when the numbers ran out.
    fn describe(&mut self, description: ImageDescription) -> Option<Arc<Described>> {
        let identity = self.identities.next()?;
        Some(Arc::new(Described {
            description,
            identity,
        }))
    }
}

/// The state Smithay double-buffers for the surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceColorState {
    color: Option<SurfaceColor>,
}

impl Cacheable for SurfaceColorState {
    /// What was set stays set until a request changes it: the pending state carries on.
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// What `surface` set as of its last commit, `None` for a surface with no description (which is
/// taken to be sRGB).
pub fn surface_color(surface: &WlSurface) -> Option<SurfaceColor> {
    with_states(surface, surface_color_in)
}

/// [`surface_color`] for code that already has the surface's data (a surface tree walk).
pub fn surface_color_in(states: &SurfaceData) -> Option<SurfaceColor> {
    if !states.cached_state.has::<SurfaceColorState>() {
        return None;
    }
    let mut cached = states.cached_state.get::<SurfaceColorState>();
    cached.current().color
}

/// Whether a surface has its extension object already, kept in its data map.
#[derive(Default)]
struct HasColorObject(AtomicBool);

fn set_pending(surface: &WlSurface, color: Option<SurfaceColor>) {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<SurfaceColorState>();
        cached.pending().color = color;
    });
}

// ---- user data -------------------------------------------------------------------------------

/// The surface behind one `wp_color_management_surface_v1`.
pub struct ColorSurface {
    surface: WlSurface,
    /// False for the extra object of a surface that already had one (a protocol error): it
    /// mustn't touch what the first one set.
    owner: bool,
}

/// The surface behind one `wp_color_management_surface_feedback_v1`.
pub struct ColorFeedback {
    surface: WlSurface,
}

/// The output behind one `wp_color_management_output_v1`, `None` if the `wl_output` was not
/// one of ours.
pub struct ColorOutput {
    output: Option<Output>,
}

/// One `wp_image_description_v1`. Without a description it failed, and can only be destroyed.
pub struct ImageDescriptionData {
    described: Option<Arc<Described>>,
    /// `get_information` is allowed only on what the compositor made (an output's or a
    /// surface's preferred description), not on what a client created.
    allows_info: bool,
}

impl ImageDescriptionData {
    fn ready(described: Arc<Described>, allows_info: bool) -> Self {
        Self {
            described: Some(described),
            allows_info,
        }
    }

    fn failed() -> Self {
        Self {
            described: None,
            allows_info: false,
        }
    }
}

/// The parameters one `wp_image_description_creator_params_v1` has so far.
#[derive(Default)]
pub struct ParamsCreator {
    builder: Mutex<ParamsBuilder>,
}

/// A new image description that failed: `failed` says why, and the object only waits to be
/// destroyed.
fn init_failed(
    data_init: &mut DataInit<'_, JffState>,
    id: New<WpImageDescriptionV1>,
    cause: wp_image_description_v1::Cause,
    message: &str,
) {
    let object = data_init.init(id, ImageDescriptionData::failed());
    object.failed(cause, message.to_string());
}

/// A new image description for a client that is disconnected right after (a protocol error):
/// the id still has to be given an object.
fn init_dead(data_init: &mut DataInit<'_, JffState>, id: New<WpImageDescriptionV1>) {
    data_init.init(id, ImageDescriptionData::failed());
}

/// A new image description that is ready, and says so.
fn init_ready(
    data_init: &mut DataInit<'_, JffState>,
    id: New<WpImageDescriptionV1>,
    described: Arc<Described>,
    allows_info: bool,
) {
    let identity = described.identity;
    let object = data_init.init(id, ImageDescriptionData::ready(described, allows_info));
    object.ready(identity);
}

// ---- the global ------------------------------------------------------------------------------

impl GlobalDispatch<WpColorManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let manager = data_init.init(resource, ());
        for intent in SUPPORTED_INTENTS {
            if let Ok(intent) = wp_color_manager_v1::RenderIntent::try_from(intent.raw()) {
                manager.supported_intent(intent);
            }
        }
        for feature in SUPPORTED_FEATURES {
            if let Ok(feature) = wp_color_manager_v1::Feature::try_from(feature.raw()) {
                manager.supported_feature(feature);
            }
        }
        for tf in SUPPORTED_TFS {
            if let Ok(tf) = wp_color_manager_v1::TransferFunction::try_from(tf.raw()) {
                manager.supported_tf_named(tf);
            }
        }
        for primaries in SUPPORTED_PRIMARIES {
            if let Ok(primaries) = wp_color_manager_v1::Primaries::try_from(primaries.raw()) {
                manager.supported_primaries_named(primaries);
            }
        }
        manager.done();
    }
}

impl Dispatch<WpColorManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_color_manager_v1::{Error, Request};

        match request {
            Request::Destroy => {}
            Request::GetOutput { id, output } => {
                let object = data_init.init(
                    id,
                    ColorOutput {
                        output: Output::from_resource(&output),
                    },
                );
                state.color_management.outputs.retain(|o| o.is_alive());
                state.color_management.outputs.push(object);
            }
            Request::GetSurface { id, surface } => {
                let taken = with_states(&surface, |states| {
                    states
                        .data_map
                        .get_or_insert_threadsafe(HasColorObject::default)
                        .0
                        .swap(true, Ordering::SeqCst)
                });
                // The new id has to be initialised even when the request is an error.
                data_init.init(
                    id,
                    ColorSurface {
                        surface,
                        owner: !taken,
                    },
                );
                if taken {
                    resource.post_error(
                        Error::SurfaceExists,
                        "the surface already has a color management object",
                    );
                }
            }
            Request::GetSurfaceFeedback { id, surface } => {
                let object = data_init.init(id, ColorFeedback { surface });
                state.color_management.feedbacks.retain(|f| f.is_alive());
                state.color_management.feedbacks.push(object);
            }
            Request::CreateIccCreator { obj } => {
                data_init.init(obj, ());
                resource.post_error(Error::UnsupportedFeature, "ICC profiles are not supported");
            }
            Request::CreateParametricCreator { obj } => {
                data_init.init(obj, ParamsCreator::default());
            }
            Request::CreateWindowsScrgb { image_description } => {
                init_dead(data_init, image_description);
                resource.post_error(Error::UnsupportedFeature, "windows_scrgb is not supported");
            }
            _ => {}
        }
    }
}

// ---- outputs and feedback ----------------------------------------------------------------------

impl Dispatch<WpColorManagementOutputV1, ColorOutput> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        data: &ColorOutput,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        if let wp_color_management_output_v1::Request::GetImageDescription { image_description } =
            request
        {
            let present = data
                .output
                .as_ref()
                .is_some_and(|output| state.outputs.contains(output));
            if !present {
                init_failed(
                    data_init,
                    image_description,
                    wp_image_description_v1::Cause::NoOutput,
                    "the output is gone",
                );
                return;
            }
            let name = data.output.as_ref().map(|o| o.name()).unwrap_or_default();
            let described = state.color_management.output_description(&name);
            init_ready(data_init, image_description, described, true);
        }
    }
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, ColorFeedback> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        data: &ColorFeedback,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_color_management_surface_feedback_v1::{Error, Request};

        // Every description jff has is parametric, so both requests answer the same.
        let (Request::GetPreferred { image_description }
        | Request::GetPreferredParametric { image_description }) = request
        else {
            return;
        };
        if !data.surface.is_alive() {
            init_dead(data_init, image_description);
            resource.post_error(Error::Inert, "the wl_surface is gone");
            return;
        }
        // The monitor the surface is shown on, the first one before it has been drawn.
        let name = with_states(&data.surface, |states| {
            surface_primary_scanout_output(&data.surface, states)
        })
        .or_else(|| state.outputs.first().cloned())
        .map(|output| output.name())
        .unwrap_or_default();
        let described = state.color_management.output_description(&name);
        init_ready(data_init, image_description, described, true);
    }
}

// ---- the surface -------------------------------------------------------------------------------

impl Dispatch<WpColorManagementSurfaceV1, ColorSurface> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        resource: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &ColorSurface,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_color_management_surface_v1::{Error, Request};

        if matches!(request, Request::Destroy) || !data.owner {
            return; // `destroyed` unsets what was set
        }
        if !data.surface.is_alive() {
            resource.post_error(Error::Inert, "the wl_surface is gone");
            return;
        }
        match request {
            Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let described = image_description
                    .data::<ImageDescriptionData>()
                    .and_then(|data| data.described.as_ref());
                let Some(described) = described else {
                    resource.post_error(
                        Error::ImageDescription,
                        "the image description is not ready",
                    );
                    return;
                };
                let Some(intent) = RenderIntent::from_raw(u32::from(render_intent)) else {
                    resource
                        .post_error(Error::RenderIntent, "the rendering intent is not supported");
                    return;
                };
                set_pending(
                    &data.surface,
                    Some(SurfaceColor {
                        description: described.description,
                        intent,
                    }),
                );
            }
            Request::UnsetImageDescription => set_pending(&data.surface, None),
            _ => {}
        }
    }

    /// The object is gone, by `destroy` or because its client left: the surface can have a new
    /// one, and what this one set is unset on the next commit.
    fn destroyed(
        _state: &mut JffState,
        _client: ClientId,
        _resource: &WpColorManagementSurfaceV1,
        data: &ColorSurface,
    ) {
        if !data.owner || !data.surface.is_alive() {
            return;
        }
        with_states(&data.surface, |states| {
            if let Some(has) = states.data_map.get::<HasColorObject>() {
                has.0.store(false, Ordering::SeqCst);
            }
            if states.cached_state.has::<SurfaceColorState>() {
                let mut cached = states.cached_state.get::<SurfaceColorState>();
                cached.pending().color = None;
            }
        });
    }
}

// ---- image descriptions ------------------------------------------------------------------------

impl Dispatch<WpImageDescriptionV1, ImageDescriptionData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &ImageDescriptionData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_image_description_v1::{Error, Request};

        if let Request::GetInformation { information } = request {
            // The new id has to be initialised even when the request is an error.
            let info = data_init.init(information, ());
            let Some(described) = data.described.as_ref() else {
                resource.post_error(Error::NotReady, "the image description is not ready");
                return;
            };
            if !data.allows_info {
                resource.post_error(
                    Error::NoInformation,
                    "this image description does not give information",
                );
                return;
            }
            // Not `send_info` here: its `done` destroys `info` while this request is still
            // being dispatched. See `flush_pending_info`.
            state
                .color_management
                .pending_info
                .push((info, described.description));
        }
    }
}

/// Bring what clients are told in line with the config and the monitors: ICC profiles, the first profile as the shared description, and the HDR descriptions. Runs at startup, on reload and when monitors come and go.
pub fn sync_output_color(state: &mut JffState) {
    let mut monitors = HashMap::new();
    for output in &state.outputs {
        let name = output.name();
        let Some(path) = state
            .config
            .output(&name)
            .and_then(|config| config.icc_profile.as_deref())
        else {
            continue;
        };
        match crate::feature::icc::load_description(path) {
            Ok(description) => {
                tracing::debug!("monitor {name}: color description from {}", path.display());
                monitors.insert(name, description);
            }
            Err(err) => tracing::warn!(
                "monitor {name}: cannot describe {}: {err:#}",
                path.display()
            ),
        }
    }
    let told = state
        .outputs
        .iter()
        .find_map(|output| monitors.get(&output.name()).copied())
        .unwrap_or_else(ImageDescription::srgb);
    state.color_management.monitors = monitors;
    let hdr_changed = sync_hdr_descriptions(state);
    if !set_output_color(state, told) && hdr_changed {
        notify_preferred_changed(state);
    }
}

/// Take the HDR descriptions from the backend and say whether what clients are told about them changed.
fn sync_hdr_descriptions(state: &mut JffState) -> bool {
    let wanted = match &state.backend {
        crate::app::Backend::Tty(tty) => tty.hdr_descriptions(),
        _ => Vec::new(),
    };
    let manager = &mut state.color_management;
    let same = wanted.len() == manager.hdr.len()
        && wanted.iter().all(|(name, description)| {
            manager
                .hdr
                .get(name)
                .is_some_and(|told| told.description == *description)
        });
    if same {
        return false;
    }
    let mut hdr = HashMap::new();
    for (name, description) in wanted {
        let kept = manager
            .hdr
            .get(&name)
            .filter(|told| told.description == description)
            .cloned();
        let Some(described) = kept.or_else(|| manager.describe(description)) else {
            continue;
        };
        hdr.insert(name, described);
    }
    manager.hdr = hdr;
    true
}

/// Tell every client that follows the outputs' description that it may have changed. They ask
/// again, and get the one of the monitor the surface is on.
fn notify_preferred_changed(state: &mut JffState) {
    let manager = &mut state.color_management;
    manager.outputs.retain(|o| o.is_alive());
    manager.feedbacks.retain(|f| f.is_alive());
    for output in &manager.outputs {
        output.image_description_changed();
    }
    let identity = manager.output.identity;
    for feedback in &manager.feedbacks {
        feedback.preferred_changed(identity);
    }
}

/// Make `description` the outputs' and tell the clients that follow it, if it is a change.
fn set_output_color(state: &mut JffState, description: ImageDescription) -> bool {
    if state
        .color_management
        .set_output_description(description)
        .is_none()
    {
        return false;
    }
    tracing::info!("the outputs' color description changed");
    notify_preferred_changed(state);
    true
}

/// Send the events of the information objects queued by `get_information`. Call it right after
/// the clients' requests were dispatched, when no object is half-created any more.
pub fn flush_pending_info(state: &mut JffState) {
    for (info, description) in std::mem::take(&mut state.color_management.pending_info) {
        if info.is_alive() {
            send_info(&info, &description);
        }
    }
}

/// Tell an information object everything about a description, then `done`.
fn send_info(info: &WpImageDescriptionInfoV1, description: &ImageDescription) {
    for event in description.info() {
        match event {
            InfoEvent::Primaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]) => {
                info.primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
            }
            InfoEvent::PrimariesNamed(raw) => {
                if let Ok(named) = wp_color_manager_v1::Primaries::try_from(raw) {
                    info.primaries_named(named);
                }
            }
            InfoEvent::TfPower(exponent) => info.tf_power(exponent),
            InfoEvent::TfNamed(raw) => {
                if let Ok(tf) = wp_color_manager_v1::TransferFunction::try_from(raw) {
                    info.tf_named(tf);
                }
            }
            InfoEvent::Luminances {
                min,
                max,
                reference,
            } => info.luminances(min, max, reference),
            InfoEvent::TargetPrimaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]) => {
                info.target_primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
            }
            InfoEvent::TargetLuminance { min, max } => info.target_luminance(min, max),
            InfoEvent::TargetMaxCll(cll) => info.target_max_cll(cll),
            InfoEvent::TargetMaxFall(fall) => info.target_max_fall(fall),
        }
    }
    info.done();
}

/// An information object has no requests: it only receives events.
impl Dispatch<WpImageDescriptionInfoV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        _request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
    }
}

/// The ICC creator is only ever created to be refused, the client is disconnected with it.
impl Dispatch<WpImageDescriptionCreatorIccV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorIccV1,
        _request: <WpImageDescriptionCreatorIccV1 as Resource>::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionCreatorParamsV1, ParamsCreator> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &ParamsCreator,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use wp_image_description_creator_params_v1::Request;

        let mut builder = data.builder.lock().unwrap_or_else(PoisonError::into_inner);
        let result = match request {
            Request::SetTfNamed { tf } => builder.set_tf_named(u32::from(tf)),
            Request::SetTfPower { eexp } => builder.set_tf_power(eexp),
            Request::SetPrimariesNamed { primaries } => {
                builder.set_primaries_named(u32::from(primaries))
            }
            Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => builder.set_primaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
            Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => builder.set_luminances(min_lum, max_lum, reference_lum),
            Request::SetMasteringDisplayPrimaries { .. } => {
                builder.set_mastering_display_primaries()
            }
            Request::SetMasteringLuminance { .. } => builder.set_mastering_luminance(),
            Request::SetMaxCll { max_cll } => {
                builder.set_max_cll(max_cll);
                Ok(())
            }
            Request::SetMaxFall { max_fall } => {
                builder.set_max_fall(max_fall);
                Ok(())
            }
            Request::Create { image_description } => {
                let built = std::mem::take(&mut *builder).build();
                drop(builder);
                match built {
                    Ok(description) => match state.color_management.describe(description) {
                        Some(described) => {
                            init_ready(data_init, image_description, described, false)
                        }
                        None => init_failed(
                            data_init,
                            image_description,
                            wp_image_description_v1::Cause::OperatingSystem,
                            "out of image description numbers",
                        ),
                    },
                    Err(CreateError::Unsupported(reason)) => {
                        tracing::debug!("color management: description not supported: {reason}");
                        init_failed(
                            data_init,
                            image_description,
                            wp_image_description_v1::Cause::Unsupported,
                            reason,
                        );
                    }
                    Err(CreateError::Protocol(violation)) => {
                        init_dead(data_init, image_description);
                        resource.post_error(violation.error.code(), violation.message);
                    }
                }
                return;
            }
            _ => Ok(()),
        };
        if let Err(violation) = result {
            resource.post_error(violation.error.code(), violation.message);
        }
    }
}
