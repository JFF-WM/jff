//! TTY backend: jff as the session compositor on bare DRM/KMS. Session and VT switching go
//! through libseat (needs seatd or logind), input through libinput, hotplug through udev.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use smithay::reexports::wayland_server::Resource;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::renderer::PresentationMode;
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::{
    Colorspace, ConnectorColorState, DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata,
    DrmEventTime, DrmNode, Eotf, HdrOutputMetadata, VrrSupport,
};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::utils::select_dmabuf_feedback;
use smithay::backend::renderer::element::{
    default_primary_scanout_output_compare, Kind,
    RenderElementStates,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::ImportDma;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev;
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::{
    surface_primary_scanout_output, update_surface_primary_scanout_output,
    OutputPresentationFeedback,
};
use smithay::desktop::Window;
use smithay::output::{Mode, Output, OutputModeSource, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{
    connector, crtc, property, Device as ControlDevice, Mode as DrmMode, ModeTypeFlags,
    ResourceHandles,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{DeviceFd, Logical, Monotonic, Physical, Point, Scale, Transform};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    with_states, with_surface_tree_downward, SurfaceData, TraversalAction,
};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder};
use smithay::wayland::drm_lease::{DrmLease, DrmLeaseBuilder, DrmLeaseState, LeaseRejected};
use smithay::wayland::presentation::Refresh;
use smithay::reexports::wayland_server::DisplayHandle;

use crate::app::pacing::advance_frame_sequence;
use crate::app::{Backend, BACKGROUND};
use crate::config::{Config, ModeRequest, Vrr};
use crate::desk::Desk;
use crate::feature::outputs::{output_rect, Monitor, MonitorMode};
use crate::feature::overview_mode::{overview_drag_elements, overview_elements};
use crate::feature::screenshot::screenshot_elements;
use crate::primitives::color::ImageDescription;
use crate::primitives::edid::{self, Edid};
use crate::primitives::frame_clock::{next_presentation_time, wait_until, FALLBACK_REFRESH};
use crate::primitives::hdr::{self, KernelSupport, OutputEotf, Status, Unavailable};
use crate::primitives::hdr_output::{self, FinalPass};
use crate::primitives::hdr_scanout::{self, Decline, Facts, Over, Tried};
use crate::primitives::layout;
use crate::protocol::color_management::WorkingSpace;
use crate::protocol::layer_shell::{
    fullscreen_covers, fullscreen_surface, layer_body_elements, layer_elements,
    layer_popup_elements, layers_above, layers_shown, BELOW_WINDOWS,
};
use crate::protocol::session_lock::LOCKED_BACKGROUND;
use crate::render::cursor::RenderCursor;
use crate::render::effects::background_effect::window_elements;
use crate::render::effects::border::BorderPaint;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::effects::hdr::{self as hdr_frame, HdrFrame};
use crate::render::elements::JffElement;
use crate::state::JffState;

const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Abgr8888];

/// For a monitor that is asked to be HDR: 10 bits per color first, so PQ does not band.
const COLOR_FORMATS_HDR: [Fourcc; 4] = [
    Fourcc::Argb2101010,
    Fourcc::Abgr2101010,
    Fourcc::Argb8888,
    Fourcc::Abgr8888,
];

/// How far outside a monitor a cursor can still be partly visible on it.
const CURSOR_MARGIN: i32 = 256;

/// The per-frame user data is the presentation feedback, answered at vblank.
type GbmDrmCompositor = DrmCompositor<
    GbmAllocator<DeviceFd>,
    GbmFramebufferExporter<DeviceFd>,
    OutputPresentationFeedback,
    DeviceFd,
>;

/// The two dmabuf feedbacks a monitor's surfaces can get.
struct SurfaceDmabufFeedback {
    /// What the renderer can import. For surfaces that get composited.
    render: DmabufFeedback,
    /// A scanout tranche (what the primary plane takes directly) followed by the render formats.
    /// For surfaces being scanned out, so the client allocates buffers that need no copy.
    scanout: DmabufFeedback,
}

/// Only the primary plane counts, since `render_crtc` never puts a client buffer on an overlay.
/// Advertising overlay-only formats would steer clients into buffers that can't be scanned out.
/// None without a render node.
fn build_dmabuf_feedback(
    compositor: &GbmDrmCompositor,
    renderer: &GlesRenderer,
    drm: &DrmDevice,
    render_node: Option<DrmNode>,
) -> Option<SurfaceDmabufFeedback> {
    let main_device = render_node?.dev_id();
    let scanout_device = drm.device_fd().dev_id().ok()?;
    let texture_formats: FormatSet = renderer.dmabuf_formats().into_iter().collect();

    let plane_formats: FormatSet = compositor
        .surface()
        .plane_info()
        .formats
        .iter()
        .copied()
        .collect();
    let scanout_formats: Vec<_> = plane_formats
        .intersection(&texture_formats)
        .copied()
        .collect();

    tracing::info!(
        "dmabuf feedback: {} primary-plane formats, {} renderer formats, {} usable for scanout",
        plane_formats.iter().count(),
        texture_formats.iter().count(),
        scanout_formats.len(),
    );
    let render = DmabufFeedbackBuilder::new(main_device, texture_formats.iter().copied())
        .build()
        .ok()?;
    let scanout = if scanout_formats.is_empty() {
        render.clone()
    } else {
        DmabufFeedbackBuilder::new(main_device, texture_formats.iter().copied())
            .add_preference_tranche(
                scanout_device,
                TrancheFlags::Scanout,
                scanout_formats,
                0..=u32::MAX,
            )
            .build()
            .ok()?
    };
    Some(SurfaceDmabufFeedback { render, scanout })
}

/// After a frame: note which surfaces ended up on a plane and send each the matching feedback.
/// A scanned-out fullscreen client gets the scanout tranche, and drops back to the render one
/// once something covers it.
fn send_surface_feedback(
    desk: &Desk<Window>,
    output: &Output,
    states: &RenderElementStates,
    feedback: &SurfaceDmabufFeedback,
    only_topmost: bool,
) {
    let rect = output_rect(output);
    let select = |surface: &WlSurface, _: &SurfaceData| {
        select_dmabuf_feedback(surface, states, &feedback.render, &feedback.scanout)
    };
    for entry in desk.top_down() {
        let size = entry.window.geometry().size;
        let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
        if !layout::intersects(area, rect) {
            continue;
        }
        entry.window.with_surfaces(|surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                None,
                states,
                default_primary_scanout_output_compare,
            );
        });
        entry
            .window
            .send_dmabuf_feedback(output, surface_primary_scanout_output, select);
        // Windows behind a covering fullscreen window were not in this frame.
        if only_topmost {
            break;
        }
    }
    for layer in layer_map_for_output(output).layers() {
        layer.with_surfaces(|surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                None,
                states,
                default_primary_scanout_output_compare,
            );
        });
        layer.send_dmabuf_feedback(output, surface_primary_scanout_output, select);
    }
}

/// A failure in the HDR frame path, remembered per output so each kind is logged once.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HdrFault {
    Target,
    Draw,
}

/// Record `fault` and say whether it is new, that is, whether it should be logged.
fn note_hdr_fault(last: &mut Option<HdrFault>, fault: HdrFault) -> bool {
    if *last == Some(fault) {
        return false;
    }
    *last = Some(fault);
    true
}

/// What a monitor in HDR mode remembers about putting a window straight on the primary plane.
#[derive(Default)]
struct HdrScanout {
    /// The window and buffer format the compositor found not to reach the plane, so it is not tried again until one of them changes.
    declined: Option<Tried>,
    /// The reason logged last, so a reason is logged once.
    reason: Option<Decline>,
}

/// How many surfaces the window's tree holds: the window and its subsurfaces.
fn surface_count(surface: &WlSurface) -> usize {
    let mut count = 0;
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, _, _| count += 1,
        |_, _, _| true,
    );
    count
}

/// The colors and the buffer format of a window's surface, for [`hdr_scanout::decide`].
fn surface_scanout_facts(surface: &WlSurface) -> (Option<ImageDescription>, Option<u32>) {
    let tag = with_states(surface, crate::protocol::color_management::surface_color_in)
        .map(|color| color.description);
    let format = smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
        state
            .buffer()
            .and_then(|buffer| get_dmabuf(buffer).ok())
            .map(|dmabuf| dmabuf.format().code as u32)
    })
    .flatten();
    (tag, format)
}

// The decision compares the fourccs it knows by number.
const _: () = {
    assert!(Fourcc::Argb2101010 as u32 == hdr_scanout::ARGB2101010);
    assert!(Fourcc::Abgr2101010 as u32 == hdr_scanout::ABGR2101010);
};

/// How a monitor in HDR mode is drawn.
struct HdrOutput {
    space: WorkingSpace,
    /// The settings of the output. The content peak is not part of them (it is `None` here), see `content_peak`.
    pass: FinalPass,
    /// `hdr-content-metadata`: fit the tone mapping to the peak the visible content states.
    content_metadata: bool,
    /// The content peak the last frame was shown through, `None` for the configured tone curve. A screenshot is made with it, so it has the tone mapping that is on screen.
    content_peak: Option<f32>,
    /// The offscreen target, made on the first frame and again when the mode or the pass changes.
    frame: Option<HdrFrame>,
}

/// A lit monitor and its frame loop state.
struct TtyOutput {
    compositor: GbmDrmCompositor,
    output: Output,
    connector: connector::Handle,
    mode: DrmMode,
    /// Where the output is in its draw cycle, see [`RedrawState`].
    redraw: RedrawState,
    /// CLOCK_MONOTONIC time of the last vblank. The frame clock counts on from it.
    last_vblank: Option<Duration>,
    /// What the monitor and driver support for VRR.
    vrr_support: VrrSupport,
    /// From the config (`adaptive-sync`), refreshed on reload.
    vrr_mode: Vrr,
    /// VRR is on right now.
    vrr_on: bool,
    /// From the config (`content-type-hint`), refreshed on reload.
    content_hint: bool,
    /// The connector's `content type` is Game right now.
    content_game: bool,
    /// Curves from the ICC profile named in the config, if any.
    gamma: Option<crate::feature::icc::GammaTable>,
    /// Curves a client set with wlr-gamma-control, they win over `gamma`.
    client_gamma: Option<crate::feature::icc::GammaTable>,
    /// `gamma` hasn't reached the display yet (see [`Tty::apply_gamma`]).
    gamma_pending: bool,
    /// The display's gamma table was written, so clear it if the profile goes away.
    gamma_applied: bool,
    /// Color facts from the monitor's EDID, read again whenever the config is synced.
    edid: Option<Edid>,
    /// What the connector should signal: HDR10 when `hdr` is configured and possible, else the default.
    color_target: ConnectorColorState,
    /// `color_target` has not been staged on the compositor yet (see [`JffState::render_crtc`]).
    color_dirty: bool,
    /// The frame is drawn through the HDR path while this is set.
    hdr: Option<HdrOutput>,
    /// The plan the connector's HDR signal comes from, `None` while the output is not HDR.
    hdr_plan: Option<hdr::Plan>,
    /// The content light levels (max_cll, max_fall) of the window on the primary plane, `(None, None)` when the frame is composited.
    hdr_content: (Option<u32>, Option<u32>),
    /// What the last sync decided about HDR, kept to log only when it changes.
    hdr_status: Status,
    /// The HDR failure logged last, cleared when a frame succeeds or the HDR settings are synced.
    hdr_fault: Option<HdrFault>,
    /// Direct scanout of a fullscreen window on this monitor while it is HDR.
    hdr_scanout: HdrScanout,
    /// The swapchain was made with the 10-bit formats first, which only a monitor that asked for `hdr` when it was lit gets.
    hdr_formats: bool,
    /// Feedback carrying this monitor's scanout tranche (see [`build_dmabuf_feedback`]).
    dmabuf_feedback: Option<SurfaceDmabufFeedback>,
    /// Last frame had a client buffer on the primary plane (shown by `jff msg watch`).
    scanning_out: bool,
    /// A fullscreen window covered the monitor last frame. Only used to log changes.
    covering: bool,
    /// Switched off by a client. We must not draw, a frame would light it again.
    powered_off: bool,
}

/// Where a monitor is in its draw cycle. A frame is drawn only when something asked for one
/// ([`JffState::queue_redraw_all`]), never on a timer.
enum RedrawState {
    /// Nothing to draw and nothing in flight.
    Idle,
    /// A frame was asked for. It is drawn at the end of the current event loop pass.
    Queued,
    /// A frame is queued on the display and its vblank hasn't arrived. `redraw_needed` is set
    /// by a commit or an animation in the meantime: the next frame follows at the vblank.
    WaitingForVBlank { redraw_needed: bool },
    /// The last redraw had nothing new to show, so no vblank will come. A timer stands in for
    /// it: clients keep their frame callbacks at the refresh rate while the monitor idles.
    WaitingForEstimatedVBlank { token: RegistrationToken },
    /// Same, and a frame was asked for in the meantime. It is drawn at the end of the current
    /// event loop pass, not when the timer goes off: a frame that waits for the timer is
    /// committed around the real vblank and lands on either side of it at random. A frame that
    /// goes to the display removes the timer.
    WaitingForEstimatedVBlankAndQueued { token: RegistrationToken },
}

impl TtyOutput {
    /// A frame is queued on the display and its vblank hasn't arrived.
    fn waiting_for_vblank(&self) -> bool {
        matches!(self.redraw, RedrawState::WaitingForVBlank { .. })
    }

    /// Ask for a frame: now when idle, after the vblank or the estimated vblank when waiting.
    fn queue_redraw(&mut self) {
        self.redraw = match std::mem::replace(&mut self.redraw, RedrawState::Idle) {
            RedrawState::Idle | RedrawState::Queued => RedrawState::Queued,
            RedrawState::WaitingForVBlank { .. } => RedrawState::WaitingForVBlank {
                redraw_needed: true,
            },
            RedrawState::WaitingForEstimatedVBlank { token }
            | RedrawState::WaitingForEstimatedVBlankAndQueued { token } => {
                RedrawState::WaitingForEstimatedVBlankAndQueued { token }
            }
        };
    }

    /// A frame is asked for and is drawn at the end of this event loop pass.
    fn redraw_is_queued(&self) -> bool {
        matches!(
            self.redraw,
            RedrawState::Queued | RedrawState::WaitingForEstimatedVBlankAndQueued { .. }
        )
    }

    /// Forget the draw cycle (monitor off, session paused): nothing is in flight any more.
    fn reset_redraw(&mut self, handle: &LoopHandle<'static, JffState>) {
        if let RedrawState::WaitingForEstimatedVBlank { token }
        | RedrawState::WaitingForEstimatedVBlankAndQueued { token } = self.redraw
        {
            handle.remove(token);
        }
        self.redraw = RedrawState::Idle;
    }

    /// Refresh interval of the monitor, 60 Hz when it has no mode.
    fn refresh(&self) -> Duration {
        crate::protocol::presentation::refresh_interval(&self.output).unwrap_or(FALLBACK_REFRESH)
    }

    /// When a frame drawn now reaches the screen (CLOCK_MONOTONIC).
    fn next_presentation_time(&self) -> Duration {
        next_presentation_time(
            self.last_vblank,
            self.refresh(),
            crate::protocol::presentation::monotonic_now(),
        )
    }
}

pub struct Tty {
    // Field order is drop order: everything that uses the GPU goes before the devices.
    pub renderer: GlesRenderer,
    outputs: HashMap<crtc::Handle, TtyOutput>,
    /// Leases clients hold right now. They go before `drm` so they are revoked first.
    active_leases: Vec<DrmLease>,
    /// wp_drm_lease_device_v1, `None` until [`Tty::init_leasing`] or if it could not be set up.
    lease_state: Option<DrmLeaseState>,
    /// Connected non-desktop connectors (VR headsets) with the CRTC kept for each. They are
    /// offered as leases and never become outputs.
    non_desktop: HashMap<connector::Handle, crtc::Handle>,
    /// All connected monitors, lit or not.
    monitors: Vec<Monitor>,
    libinput: Libinput,
    session: LibSeatSession,
    loop_handle: LoopHandle<'static, JffState>,
    render_node: Option<DrmNode>,
    drm: DrmDevice,
    gbm: GbmDevice<DeviceFd>,
}

/// Connector name as clients see it, e.g. `DP-1`.
fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// The requested mode if the monitor has it, else its preferred one.
fn pick_mode(info: &connector::Info, request: Option<ModeRequest>) -> Option<DrmMode> {
    let modes = info.modes();
    if let Some(req) = request {
        let best = modes
            .iter()
            .filter(|m| (m.size().0 as i32, m.size().1 as i32) == (req.width, req.height))
            // closest refresh to the request, or the highest when none was given
            .min_by_key(|m| match req.refresh {
                Some(hz) => (m.vrefresh() as i32 - hz).abs(),
                None => -(m.vrefresh() as i32),
            });
        if let Some(mode) = best {
            return Some(mode.clone());
        }
        tracing::warn!(
            "{} has no {}x{} mode, using its preferred one",
            connector_name(info),
            req.width,
            req.height
        );
    }
    modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .cloned()
}

/// Mode list for output management, deduplicated (DRM can list a size and refresh twice).
fn monitor_of(info: &connector::Info, name: String) -> Monitor {
    let mut modes: Vec<MonitorMode> = Vec::new();
    for m in info.modes() {
        let refresh = Mode::from(m.clone()).refresh;
        let (width, height) = (m.size().0 as i32, m.size().1 as i32);
        let preferred = m.mode_type().contains(ModeTypeFlags::PREFERRED);
        match modes
            .iter_mut()
            .find(|x| (x.width, x.height, x.refresh) == (width, height, refresh))
        {
            Some(existing) => existing.preferred |= preferred,
            None => modes.push(MonitorMode {
                width,
                height,
                refresh,
                preferred,
            }),
        }
    }
    let (pw, ph) = info.size().unwrap_or((0, 0));
    Monitor {
        name,
        size_mm: (pw as i32, ph as i32),
        modes,
    }
}

/// First free CRTC that can drive `info`, preferring the one the firmware already uses.
fn pick_crtc(
    drm: &DrmDevice,
    resources: &ResourceHandles,
    info: &connector::Info,
    used: &HashSet<crtc::Handle>,
) -> Option<crtc::Handle> {
    info.current_encoder()
        .and_then(|enc| drm.get_encoder(enc).ok())
        .and_then(|enc| enc.crtc())
        .filter(|crtc| !used.contains(crtc))
        .or_else(|| {
            info.encoders()
                .iter()
                .filter_map(|&enc| drm.get_encoder(enc).ok())
                .flat_map(|enc| resources.filter_crtcs(enc.possible_crtcs()))
                .find(|crtc| !used.contains(crtc))
        })
}

/// One `GAMMA_LUT` entry, same layout as the kernel's `struct drm_color_lut`.
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmColorLut {
    red: u16,
    green: u16,
    blue: u16,
    reserved: u16,
}

/// The CRTC's `GAMMA_LUT` property and how many entries the table has, or `None` when the GPU
/// has no usable table.
fn crtc_gamma_property(drm: &DrmDevice, crtc: crtc::Handle) -> Option<(property::Handle, usize)> {
    let props = drm.get_properties(crtc).ok()?;
    let (handles, values) = props.as_props_and_values();
    let mut lut_property = None;
    let mut lut_size = 0usize;
    for (&handle, &value) in handles.iter().zip(values.iter()) {
        let Ok(info) = drm.get_property(handle) else {
            continue;
        };
        match info.name().to_bytes() {
            b"GAMMA_LUT" => lut_property = Some(handle),
            b"GAMMA_LUT_SIZE" => lut_size = value as usize,
            _ => {}
        }
    }
    lut_property.filter(|_| lut_size > 1).map(|p| (p, lut_size))
}

/// Set the CRTC gamma table, or clear it with `None`. The hardware applies it at scanout, so
/// it affects everything on the monitor.
fn set_gamma(
    drm: &DrmDevice,
    crtc: crtc::Handle,
    table: Option<&crate::feature::icc::GammaTable>,
) -> anyhow::Result<()> {
    let Some((lut_property, lut_size)) = crtc_gamma_property(drm, crtc) else {
        bail!("this GPU has no gamma table for the monitor");
    };

    let blob = match table {
        Some(table) => {
            let entries: Vec<DrmColorLut> = table
                .resampled(lut_size)
                .into_iter()
                .map(|[red, green, blue]| DrmColorLut {
                    red,
                    green,
                    blue,
                    reserved: 0,
                })
                .collect();
            match create_lut_blob(drm, entries)? {
                property::Value::Blob(id) => id,
                _ => bail!("the kernel did not return a blob"),
            }
        }
        None => 0,
    };
    let result = drm
        .set_property(crtc, lut_property, blob)
        .context("error setting GAMMA_LUT");
    // the kernel keeps its own reference while the CRTC uses the blob
    if blob != 0 {
        let _ = drm.destroy_property_blob(blob);
    }
    result
}

/// Fixed values of the connector's `content type` enum (`DRM_MODE_CONTENT_TYPE_*`).
const CONTENT_TYPE_NO_DATA: u64 = 0;
const CONTENT_TYPE_GAME: u64 = 4;

fn lease_description(name: &str) -> String {
    format!("Non-desktop display on {name}")
}

/// The connector's `non-desktop` property is set: a VR headset or similar, not a monitor.
fn is_non_desktop(drm: &DrmDevice, connector: connector::Handle) -> bool {
    let Ok(props) = drm.get_properties(connector) else {
        return false;
    };
    let (handles, values) = props.as_props_and_values();
    handles.iter().zip(values).any(|(&handle, &value)| {
        drm.get_property(handle)
            .is_ok_and(|info| info.name().to_bytes() == b"non-desktop")
            && value != 0
    })
}

/// The connector's EDID, parsed. `None` when it has none or it is not an EDID.
fn read_edid(drm: &DrmDevice, connector: connector::Handle) -> Option<Edid> {
    let props = drm.get_properties(connector).ok()?;
    let (handles, values) = props.as_props_and_values();
    let blob = handles.iter().zip(values).find_map(|(&handle, &value)| {
        let info = drm.get_property(handle).ok()?;
        (info.name().to_bytes() == b"EDID").then_some(value)
    })?;
    if blob == 0 {
        return None;
    }
    let bytes = drm.get_property_blob(blob).ok()?;
    edid::parse(&bytes).ok()
}

/// The connector state that signals HDR: BT.2020 RGB, PQ (or HLG), and the plan's metadata.
fn hdr_color_state(plan: &hdr::Plan) -> ConnectorColorState {
    let metadata = HdrOutputMetadata::pq_bt2020(
        plan.max_luminance,
        plan.min_luminance,
        plan.max_cll,
        plan.max_fall,
    );
    ConnectorColorState {
        colorspace: Colorspace::Bt2020Rgb,
        hdr_metadata: Some(match plan.eotf {
            OutputEotf::Pq => metadata,
            OutputEotf::Hlg => HdrOutputMetadata {
                eotf: Eotf::Hlg,
                ..metadata
            },
        }),
        max_bpc: plan.max_bpc,
    }
}

/// Log a change of what an output does about HDR: once per change, never per frame.
fn log_hdr_status(name: &str, status: &Status, swapchain: Fourcc) {
    match status {
        Status::Off => tracing::info!("monitor {name}: {status}"),
        Status::Sdr(_) => tracing::warn!("monitor {name}: {status}"),
        Status::Hdr10 { .. } => {
            tracing::info!(
                "monitor {name}: {status}, swapchain {swapchain:?}, working space {:?}",
                hdr_frame::WORKING_FORMAT
            );
            if status.banding() {
                tracing::warn!(
                    "monitor {name}: the link cannot carry 10 bits per color, the picture is dithered to hide banding"
                );
            }
            if status.low_headroom() {
                tracing::warn!(
                    "monitor {name}: SDR white leaves little headroom under this peak, lower `sdr-white` or raise `hdr-peak` for more"
                );
            }
        }
    }
}

/// Bits per color of a swapchain format, 16 for the formats it is not worth dithering.
fn link_bits(format: Fourcc) -> u32 {
    match format {
        Fourcc::Argb2101010 | Fourcc::Abgr2101010 | Fourcc::Xrgb2101010 | Fourcc::Xbgr2101010 => 10,
        Fourcc::Argb8888 | Fourcc::Abgr8888 | Fourcc::Xrgb8888 | Fourcc::Xbgr8888 => 8,
        _ => 16,
    }
}

/// Check the monitor and the connector for HDR10 and plan its metadata.
fn hdr_target(
    compositor: &GbmDrmCompositor,
    connector: connector::Handle,
    edid: Option<&Edid>,
    peak_override: Option<u32>,
) -> Result<hdr::Plan, Unavailable> {
    let kernel = KernelSupport {
        hdr_metadata: compositor
            .hdr_metadata_supported(connector)
            .unwrap_or(false),
        bt2020_rgb: compositor
            .supported_colorspaces(connector)
            .is_ok_and(|all| all.contains(&Colorspace::Bt2020Rgb)),
        max_bpc: compositor.max_bpc_range(connector).ok().flatten(),
    };
    hdr::plan(edid, &kernel, peak_override)
}

/// Set the connector's `content type` property to Game, or back to no data.
fn set_connector_content_type(
    drm: &DrmDevice,
    connector: connector::Handle,
    game: bool,
) -> anyhow::Result<()> {
    let props = drm
        .get_properties(connector)
        .context("error reading the connector properties")?;
    let (handles, _) = props.as_props_and_values();
    let property = handles
        .iter()
        .copied()
        .find(|&handle| {
            drm.get_property(handle)
                .is_ok_and(|info| info.name().to_bytes() == b"content type")
        })
        .context("this connector has no content type property")?;
    let value = if game {
        CONTENT_TYPE_GAME
    } else {
        CONTENT_TYPE_NO_DATA
    };
    drm.set_property(connector, property, value)
        .context("error setting the content type")
}

const DPMS_ON: u64 = 0;
const DPMS_OFF: u64 = 3;

fn set_connector_dpms(
    drm: &DrmDevice,
    connector: connector::Handle,
    on: bool,
) -> anyhow::Result<()> {
    let props = drm
        .get_properties(connector)
        .context("error reading the connector properties")?;
    let (handles, _) = props.as_props_and_values();
    let property = handles
        .iter()
        .copied()
        .find(|&handle| {
            drm.get_property(handle)
                .is_ok_and(|info| info.name().to_bytes() == b"DPMS")
        })
        .context("this connector has no DPMS property")?;
    drm.set_property(connector, property, if on { DPMS_ON } else { DPMS_OFF })
        .context("error setting DPMS")
}

/// `create_property_blob` wants a fixed-size array, hence the list of known table sizes.
fn create_lut_blob(
    drm: &DrmDevice,
    entries: Vec<DrmColorLut>,
) -> anyhow::Result<property::Value<'static>> {
    macro_rules! by_size {
        ($($n:literal),*) => {
            match entries.len() {
                $($n => {
                    let lut: Box<[DrmColorLut; $n]> = entries
                        .into_boxed_slice()
                        .try_into()
                        .map_err(|_| anyhow!("wrong gamma table size"))?;
                    drm.create_property_blob(&*lut)
                        .context("error creating the gamma table blob")
                })*
                other => bail!("this GPU's gamma table has {other} entries, which jff cannot fill"),
            }
        };
    }
    by_size!(256, 512, 1024, 2048, 4096, 8192, 16384)
}

impl Tty {
    /// DRM device that client sync objects are imported on (wp_linux_drm_syncobj_v1).
    pub fn drm_device_fd(&self) -> &DrmDeviceFd {
        self.drm.device_fd()
    }

    /// Whether the last frame on `output` was direct scanout. False if it isn't lit.
    pub fn scanning_out(&self, output: &Output) -> bool {
        self.outputs
            .values()
            .any(|lit| lit.output == *output && lit.scanning_out)
    }

    /// Whether variable refresh rate is on for `output` right now. False if it isn't lit.
    pub fn adaptive_sync(&self, output: &Output) -> bool {
        self.outputs
            .values()
            .any(|lit| lit.output == *output && lit.vrr_on)
    }

    /// Connected monitors as of the last scan, lit or not.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// Offer the non-desktop connectors as leases. Runs once the Wayland display exists.
    pub fn init_leasing(&mut self, display: &DisplayHandle) {
        let node = match DrmNode::from_file(self.drm.device_fd()) {
            Ok(node) => node,
            Err(err) => {
                tracing::warn!("no DRM leasing, cannot identify the GPU: {err}");
                return;
            }
        };
        let mut state = match DrmLeaseState::new::<JffState>(display, &node) {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!("no DRM leasing: {err}");
                return;
            }
        };
        // headsets found by the first scan, before there was a global to tell
        for &conn in self.non_desktop.keys() {
            if let Ok(info) = self.drm.get_connector(conn, false) {
                let name = connector_name(&info);
                state.add_connector::<JffState>(conn, name.clone(), lease_description(&name));
            }
        }
        self.lease_state = Some(state);
    }

    pub fn lease_state_mut(&mut self) -> Option<&mut DrmLeaseState> {
        self.lease_state.as_mut()
    }

    /// Claim what a lease on `connectors` needs: the connector, the CRTC kept for it and its
    /// primary (and cursor) plane.
    pub fn build_lease(
        &mut self,
        connectors: &[connector::Handle],
    ) -> Result<DrmLeaseBuilder, LeaseRejected> {
        if !self.drm.is_active() {
            return Err(LeaseRejected::default());
        }
        let mut builder = DrmLeaseBuilder::new(&self.drm);
        for &conn in connectors {
            let Some(&crtc) = self.non_desktop.get(&conn) else {
                tracing::warn!("lease request for {conn:?}, which is not a non-desktop connector");
                return Err(LeaseRejected::default());
            };
            builder.add_connector(conn);
            builder.add_crtc(crtc);
            let planes = self.drm.planes(&crtc).map_err(LeaseRejected::with_cause)?;
            let (primary, claim) = planes
                .primary
                .iter()
                .find_map(|plane| {
                    self.drm
                        .claim_plane(plane.handle, crtc)
                        .map(|claim| (plane, claim))
                })
                .ok_or_else(LeaseRejected::default)?;
            builder.add_plane(primary.handle, claim);
            if let Some((cursor, claim)) = planes.cursor.iter().find_map(|plane| {
                self.drm
                    .claim_plane(plane.handle, crtc)
                    .map(|claim| (plane, claim))
            }) {
                builder.add_plane(cursor.handle, claim);
            }
        }
        Ok(builder)
    }

    pub fn add_active_lease(&mut self, lease: DrmLease) {
        self.active_leases.push(lease);
    }

    pub fn remove_active_lease(&mut self, id: u32) {
        self.active_leases.retain(|lease| lease.id() != id);
    }

    /// Bring the leasable connectors in line with the ones just found: withdraw unplugged
    /// ones, keep a CRTC for new ones and announce them. Their CRTCs go into `used`.
    fn sync_leasable(
        &mut self,
        resources: &ResourceHandles,
        found: Vec<(connector::Handle, String)>,
        used: &mut HashSet<crtc::Handle>,
    ) {
        let gone: Vec<connector::Handle> = self
            .non_desktop
            .keys()
            .filter(|conn| !found.iter().any(|(f, _)| f == *conn))
            .copied()
            .collect();
        for conn in gone {
            self.non_desktop.remove(&conn);
            if let Some(state) = self.lease_state.as_mut() {
                state.withdraw_connector(conn);
            }
            tracing::info!("non-desktop connector {conn:?} removed");
        }
        for (conn, name) in found {
            if let Some(&crtc) = self.non_desktop.get(&conn) {
                used.insert(crtc);
                continue;
            }
            let Ok(info) = self.drm.get_connector(conn, false) else {
                continue;
            };
            let Some(crtc) = pick_crtc(&self.drm, resources, &info, used) else {
                tracing::warn!("no free CRTC to lease {name}");
                continue;
            };
            used.insert(crtc);
            self.non_desktop.insert(conn, crtc);
            tracing::info!("non-desktop connector {name} offered for leasing");
            if let Some(state) = self.lease_state.as_mut() {
                state.add_connector::<JffState>(conn, name.clone(), lease_description(&name));
            }
        }
    }

    /// Bring lit monitors in line with what's connected and the config: light new ones, drop
    /// unplugged or disabled ones, redo those whose mode changed. Returns (removed, added).
    fn reconcile(&mut self, config: &Config) -> anyhow::Result<(Vec<Output>, Vec<Output>)> {
        let resources = self
            .drm
            .resource_handles()
            .context("error reading DRM resources")?;

        let mut used: HashSet<crtc::Handle> = HashSet::new();
        let mut candidates = Vec::new();
        let mut monitors = Vec::new();
        let mut leasable: Vec<(connector::Handle, String)> = Vec::new();
        for &conn in resources.connectors() {
            let Ok(info) = self.drm.get_connector(conn, true) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            // A VR headset is no monitor: it is offered as a lease instead.
            if is_non_desktop(&self.drm, conn) {
                leasable.push((conn, connector_name(&info)));
                continue;
            }
            if info.modes().is_empty() {
                continue;
            }
            let name = connector_name(&info);
            monitors.push(monitor_of(&info, name.clone()));
            let wanted = config.output(&name);
            if wanted.is_some_and(|c| c.off) {
                continue;
            }
            let Some(mode) = pick_mode(&info, wanted.and_then(|c| c.mode)) else {
                continue;
            };
            let hdr_wanted = wanted.is_some_and(|c| c.hdr);
            match self.outputs.iter().find(|(_, o)| o.connector == conn) {
                // a swapchain made without the 10-bit formats would band under PQ, so `hdr` turned on by a reload lights the monitor again
                Some((&crtc, lit)) if lit.mode == mode && (lit.hdr_formats || !hdr_wanted) => {
                    used.insert(crtc);
                }
                _ => candidates.push((info, name, mode)),
            }
        }

        self.monitors = monitors;

        let stale: Vec<crtc::Handle> = self
            .outputs
            .keys()
            .filter(|crtc| !used.contains(*crtc))
            .copied()
            .collect();
        let removed: Vec<Output> = stale
            .into_iter()
            .filter_map(|crtc| self.outputs.remove(&crtc))
            .map(|lit| lit.output)
            .collect();

        // Before lighting new monitors, so a headset's CRTC isn't taken from under it.
        self.sync_leasable(&resources, leasable, &mut used);

        let mut added = Vec::new();
        for (info, name, mode) in candidates {
            let Some(crtc) = pick_crtc(&self.drm, &resources, &info, &used) else {
                tracing::warn!("no free CRTC for monitor {name}");
                continue;
            };
            let hdr_wanted = config.output(&name).is_some_and(|c| c.hdr);
            match self.light_up(&info, &name, crtc, mode, hdr_wanted) {
                Ok(lit) => {
                    used.insert(crtc);
                    added.push(lit.output.clone());
                    self.outputs.insert(crtc, lit);
                }
                Err(err) => tracing::warn!("could not use monitor {name}: {err:#}"),
            }
        }
        Ok((removed, added))
    }

    /// Create the output and scanout pipeline for a monitor.
    fn light_up(
        &mut self,
        info: &connector::Info,
        name: &str,
        crtc: crtc::Handle,
        drm_mode: DrmMode,
        hdr_wanted: bool,
    ) -> anyhow::Result<TtyOutput> {
        tracing::info!(
            "monitor {name}: {}x{}@{}",
            drm_mode.size().0,
            drm_mode.size().1,
            drm_mode.vrefresh()
        );

        let (pw, ph) = info.size().unwrap_or((0, 0));
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (pw as i32, ph as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "jff".into(),
                model: name.to_string(),
                serial_number: "Unknown".into(),
            },
        );
        let wl_mode = Mode::from(drm_mode.clone());
        output.change_current_state(
            Some(wl_mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(wl_mode);

        let surface = self
            .drm
            .create_surface(crtc, drm_mode.clone(), &[info.handle()])
            .context("error creating the DRM surface")?;
        let allocator = GbmAllocator::new(
            self.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let render_formats = self
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect::<FormatSet>();
        let compositor = DrmCompositor::new(
            OutputModeSource::Auto(output.downgrade()),
            surface,
            None,
            allocator,
            GbmFramebufferExporter::new(self.gbm.clone(), NodeFilter::All),
            if hdr_wanted {
                COLOR_FORMATS_HDR.iter().copied()
            } else {
                COLOR_FORMATS.iter().copied()
            },
            render_formats,
            self.drm.cursor_size(),
            Some(self.gbm.clone()),
        )
        .context("error creating the DRM compositor")?;

        // only asks what's supported, `render_crtc` decides every frame whether VRR is on
        let vrr_support = compositor
            .vrr_supported(info.handle())
            .unwrap_or(VrrSupport::NotSupported);
        tracing::info!("monitor {name}: variable refresh rate {vrr_support:?}");

        let dmabuf_feedback =
            build_dmabuf_feedback(&compositor, &self.renderer, &self.drm, self.render_node);
        if dmabuf_feedback.is_none() {
            tracing::warn!(
                "monitor {name}: no scanout dmabuf feedback, clients get the render formats"
            );
        }

        Ok(TtyOutput {
            dmabuf_feedback,
            scanning_out: false,
            covering: false,
            vrr_support,
            vrr_mode: Vrr::default(),
            vrr_on: false,
            edid: None,
            color_target: ConnectorColorState::default(),
            color_dirty: false,
            hdr_plan: None,
            hdr_content: (None, None),
            hdr: None,
            hdr_status: Status::Off,
            hdr_fault: None,
            hdr_scanout: HdrScanout::default(),
            hdr_formats: hdr_wanted,
            content_hint: false,
            content_game: false,
            compositor,
            output,
            connector: info.handle(),
            mode: drm_mode,
            redraw: RedrawState::Idle,
            last_vblank: None,
            gamma: None,
            client_gamma: None,
            gamma_pending: false,
            gamma_applied: false,
            powered_off: false,
        })
    }

    /// Re-read each lit monitor's config (VRR, content hint, ICC profile) so `reload-config`
    /// picks up edits. Curves reach the display in [`Tty::apply_gamma`].
    fn sync_gamma(&mut self, config: &Config) {
        for lit in self.outputs.values_mut() {
            let name = lit.output.name();
            lit.vrr_mode = config.output(&name).map(|c| c.vrr).unwrap_or_default();
            lit.content_hint = config.output(&name).is_some_and(|c| c.content_type_hint);
            let wanted = config.output(&name).and_then(|c| c.icc_profile.as_deref());
            lit.gamma = wanted.and_then(|path| match crate::feature::icc::load(path) {
                Ok(table) => {
                    tracing::info!(
                        "monitor {name}: {} from {}",
                        table.describe(),
                        path.display()
                    );
                    Some(table)
                }
                Err(err) => {
                    tracing::warn!("monitor {name}: cannot use {}: {err:#}", path.display());
                    None
                }
            });
            lit.gamma_pending = true;
        }
    }

    /// Re-read each lit monitor's EDID and HDR settings and work out what its connector should signal. Staged in [`JffState::render_crtc`].
    fn sync_hdr(&mut self, config: &Config) {
        let Tty {
            drm,
            outputs,
            renderer,
            ..
        } = &mut *self;
        for lit in outputs.values_mut() {
            let name = lit.output.name();
            lit.hdr_fault = None;
            let edid = read_edid(drm, lit.connector);
            if edid != lit.edid {
                if let Some(info) = &edid {
                    tracing::debug!(
                        "monitor {name}: EDID primaries {:?}, colorimetry {:?}, HDR block {:?}",
                        info.primaries,
                        info.colorimetry,
                        info.hdr
                    );
                }
                lit.edid = edid;
            }
            let output_config = config.output(&name);
            let sdr_white = output_config
                .and_then(|c| c.sdr_white)
                .unwrap_or(hdr_output::DEFAULT_SDR_WHITE);
            let peak_override = output_config.and_then(|c| c.hdr_peak);
            let tone = output_config
                .and_then(|c| c.hdr_tone_map)
                .unwrap_or_default();
            let content_metadata = output_config
                .and_then(|c| c.hdr_content_metadata)
                .unwrap_or(true);
            let mut hdr = None;
            let mut hdr_plan = None;
            let mut target = ConnectorColorState::default();
            let status = if !output_config.is_some_and(|c| c.hdr) {
                Status::Off
            } else {
                match hdr_target(
                    &lit.compositor,
                    lit.connector,
                    lit.edid.as_ref(),
                    peak_override,
                ) {
                    Ok(_) if !hdr_frame::supported(renderer) => {
                        Status::Sdr(Unavailable::NoFloatTarget)
                    }
                    Ok(plan) => {
                        let panel = lit
                            .edid
                            .as_ref()
                            .and_then(|e| e.primaries.as_ref())
                            .map(hdr_output::chromaticities_of);
                        let pass = FinalPass::new(
                            sdr_white as f32,
                            f32::from(plan.max_luminance),
                            panel.as_ref(),
                        )
                        .with_tone_map(tone)
                        .with_eotf(plan.eotf)
                        .with_dither(Some(
                            link_bits(lit.compositor.format())
                                .min(plan.max_bpc.unwrap_or(u32::MAX)),
                        ));
                        hdr = Some((WorkingSpace { sdr_white }, pass, content_metadata));
                        target = hdr_color_state(
                            &plan.with_content(lit.hdr_content.0, lit.hdr_content.1),
                        );
                        hdr_plan = Some(plan);
                        Status::hdr10(&plan, sdr_white)
                    }
                    Err(reason) => Status::Sdr(reason),
                }
            };
            if status != lit.hdr_status {
                log_hdr_status(&name, &status, lit.compositor.format());
                lit.hdr_status = status;
            }
            let unchanged = match (&lit.hdr, &hdr) {
                (None, None) => true,
                (Some(old), Some((space, pass, content_metadata))) => {
                    old.space == *space
                        && old.pass == *pass
                        && old.content_metadata == *content_metadata
                }
                _ => false,
            };
            if !unchanged {
                // Entering or leaving HDR changes whether the calibration curves may be set.
                lit.gamma_pending = true;
                lit.hdr = hdr.map(|(space, pass, content_metadata)| HdrOutput {
                    space,
                    pass,
                    content_metadata,
                    content_peak: None,
                    frame: None,
                });
                lit.queue_redraw();
            }
            lit.hdr_plan = hdr_plan;
            if hdr_plan.is_none() {
                lit.hdr_content = (None, None);
            }
            if target != lit.color_target {
                lit.color_target = target;
                lit.color_dirty = true;
                lit.queue_redraw();
            }
        }
    }

    /// The working space and final pass of `output` while it is drawn as HDR, `None` when it is
    /// SDR, not lit, or not one of ours. A screenshot is made with the same pass the monitor
    /// shows, so it has the same tone curve and gamut, and the content peak the last frame was
    /// fitted to.
    pub fn hdr_capture(&self, output: &Output) -> Option<(WorkingSpace, FinalPass)> {
        let lit = self.outputs.values().find(|lit| lit.output == *output)?;
        let hdr = lit.hdr.as_ref()?;
        Some((hdr.space, hdr.pass.with_content_peak(hdr.content_peak)))
    }

    /// What clients are told about each monitor drawn in HDR: PQ in BT.2020 up to its peak (the EDID's or `hdr-peak`), with SDR white at its configured level.
    pub fn hdr_descriptions(&self) -> Vec<(String, ImageDescription)> {
        self.outputs
            .values()
            .filter_map(|lit| {
                let hdr = lit.hdr.as_ref()?;
                let description =
                    crate::protocol::color_management::hdr_output_description(&hdr.pass, hdr.space);
                Some((lit.output.name(), description))
            })
            .collect()
    }

    /// Apply pending curves, only on monitors that have shown a frame and have no flip in
    /// flight. The first modeset resets the display's colour state, so this has to come after it.
    fn apply_gamma(&mut self) {
        let Tty { drm, outputs, .. } = &mut *self;
        for (&crtc, lit) in outputs.iter_mut() {
            if !lit.gamma_pending || lit.waiting_for_vblank() || lit.last_vblank.is_none() {
                continue;
            }
            lit.gamma_pending = false;
            let curves = lit.client_gamma.as_ref().or(lit.gamma.as_ref());
            // The table works on the signal the CRTC scans out. An HDR output sends PQ (or HLG), where curves made for sRGB would bend the whole picture, so none is set and one that is there is cleared.
            if lit.hdr.is_some() && curves.is_some() {
                tracing::warn!(
                    "monitor {}: calibration curves are not applied while it is in HDR mode",
                    lit.output.name()
                );
            }
            let wanted = curves.filter(|_| lit.hdr.is_none());
            if wanted.is_none() && !lit.gamma_applied {
                continue;
            }
            match set_gamma(drm, crtc, wanted) {
                Ok(()) => lit.gamma_applied = wanted.is_some(),
                Err(err) => tracing::warn!(
                    "monitor {}: cannot set the calibration curves: {err:#}",
                    lit.output.name()
                ),
            }
        }
    }

    /// The redraw of `crtc` had nothing to queue, so no vblank is coming: wait for the one it
    /// would have been (see [`RedrawState::WaitingForEstimatedVBlank`]).
    fn queue_estimated_vblank(&mut self, crtc: crtc::Handle) {
        let Some(lit) = self.outputs.get_mut(&crtc) else {
            return;
        };
        // The redraw that just had nothing to show was asked for while a timer was already
        // running: that timer stands, no second one.
        if let RedrawState::WaitingForEstimatedVBlank { token }
        | RedrawState::WaitingForEstimatedVBlankAndQueued { token } = lit.redraw
        {
            lit.redraw = RedrawState::WaitingForEstimatedVBlank { token };
            return;
        }
        let now = crate::protocol::presentation::monotonic_now();
        let delay = wait_until(lit.next_presentation_time(), now);
        let timer = Timer::from_duration(delay);
        match self.loop_handle.insert_source(timer, move |_, _, state| {
            state.on_estimated_vblank(crtc);
            TimeoutAction::Drop
        }) {
            Ok(token) => lit.redraw = RedrawState::WaitingForEstimatedVBlank { token },
            Err(err) => {
                tracing::warn!(
                    "monitor {}: no estimated vblank timer: {err}",
                    lit.output.name()
                );
                lit.redraw = RedrawState::Idle;
            }
        }
    }
}

/// Open the session, the GPU, every connected monitor and the input devices.
pub fn init(
    handle: &LoopHandle<'static, JffState>,
    config: &Config,
) -> anyhow::Result<(Backend, Vec<Output>)> {
    let (mut session, notifier) = LibSeatSession::new().context(
        "could not open a session (is seatd or logind running, and is this a real TTY?)",
    )?;
    let seat = session.seat();

    // ---- GPU ----
    let gpu_path = udev::primary_gpu(&seat)
        .context("error looking for the primary GPU")?
        .or_else(|| {
            udev::all_gpus(&seat)
                .ok()
                .and_then(|gpus| gpus.into_iter().next())
        })
        .context("no GPU found")?;
    tracing::info!("using GPU {gpu_path:?}");

    let fd = session
        .open(
            &gpu_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| anyhow!("could not open {gpu_path:?}: {e:?}"))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) =
        DrmDevice::new(device_fd.clone(), false).context("error opening the DRM device")?;
    let gbm = GbmDevice::new(device_fd.device_fd()).context("error creating the GBM device")?;

    let display = unsafe { EGLDisplay::new(gbm.clone()) }.context("error creating EGL display")?;
    let egl_device = EGLDevice::device_for_display(&display).context("no EGL device")?;
    let render_node = egl_device.try_get_render_node().ok().flatten();
    let context = EGLContext::new(&display).context("error creating the EGL context")?;
    let renderer = unsafe { GlesRenderer::new(context) }.context("error creating the renderer")?;

    // ---- input ----
    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| anyhow!("error assigning the seat to libinput"))?;
    // Started on an inactive VT: begin suspended so ActivateSession fully re-enumerates the
    // input devices.
    if !session.is_active() {
        libinput.suspend();
    }
    let input_backend = LibinputInputBackend::new(libinput.clone());
    handle
        .insert_source(input_backend, |mut event, _, state| {
            // Tap-to-click on touchpads.
            if let InputEvent::DeviceAdded { device } = &mut event {
                if device.config_tap_finger_count() > 0 {
                    let _ = device.config_tap_set_enabled(true);
                }
            }
            state.process_input_event(event);
            // the pointer moved, a key changed something...
            state.queue_redraw_all();
        })
        .map_err(|e| anyhow!("failed to insert the libinput source: {}", e.error))?;

    // ---- events from the session, the display and monitor hotplug ----
    handle
        .insert_source(notifier, |event, _, state| state.on_session_event(event))
        .map_err(|e| anyhow!("failed to insert the session source: {}", e.error))?;
    handle
        .insert_source(drm_notifier, |event, meta, state| match event {
            DrmEvent::VBlank(crtc) => state.on_vblank(crtc, meta.take()),
            DrmEvent::Error(err) => tracing::warn!("DRM error: {err}"),
        })
        .map_err(|e| anyhow!("failed to insert the DRM source: {}", e.error))?;
    let udev_backend = udev::UdevBackend::new(&seat).context("error creating the udev backend")?;
    handle
        .insert_source(udev_backend, |event, _, state| {
            if let udev::UdevEvent::Changed { .. } = event {
                state.scan_tty_outputs();
            }
        })
        .map_err(|e| anyhow!("failed to insert the udev source: {}", e.error))?;

    // ---- monitors ----
    let mut tty = Tty {
        renderer,
        outputs: HashMap::new(),
        active_leases: Vec::new(),
        lease_state: None,
        non_desktop: HashMap::new(),
        monitors: Vec::new(),
        libinput,
        session,
        loop_handle: handle.clone(),
        render_node,
        drm,
        gbm,
    };
    let (_, outputs) = tty.reconcile(config)?;
    if outputs.is_empty() {
        bail!("no connected monitor found");
    }
    // curves can only be set after the first frame, see `apply_gamma`
    tty.sync_gamma(config);
    tty.sync_hdr(config);
    Ok((Backend::Tty(tty), outputs))
}

impl JffState {
    /// Ask for a frame on every monitor. It is drawn at the end of the current event loop pass
    /// (or right after the vblank when one is in flight), see [`Self::redraw_queued_outputs`].
    pub fn queue_redraw_tty(&mut self) {
        if let Backend::Tty(tty) = &mut self.backend {
            for lit in tty.outputs.values_mut() {
                lit.queue_redraw();
            }
        }
    }

    /// Draw a frame on every monitor that asked for one. The event loop calls it after each
    /// pass, once everything the pass did (commits, input, vblanks) has been applied.
    pub fn redraw_queued_outputs(&mut self) {
        let crtcs: Vec<crtc::Handle> = match &self.backend {
            Backend::Tty(tty) => tty
                .outputs
                .iter()
                .filter(|(_, lit)| lit.redraw_is_queued())
                .map(|(&crtc, _)| crtc)
                .collect(),
            _ => return,
        };
        for crtc in crtcs {
            self.render_crtc(crtc);
        }
    }

    pub fn output_powered(&self, output: &Output) -> bool {
        match &self.backend {
            Backend::Tty(tty) => tty
                .outputs
                .values()
                .find(|lit| lit.output == *output)
                .is_none_or(|lit| !lit.powered_off),
            _ => true,
        }
    }

    /// Entries per channel of `output`'s gamma table, `None` when it has none (winit, not lit,
    /// no `GAMMA_LUT`).
    pub fn output_gamma_size(&self, output: &Output) -> Option<u32> {
        let Backend::Tty(tty) = &self.backend else {
            return None;
        };
        let (&crtc, _) = tty.outputs.iter().find(|(_, lit)| lit.output == *output)?;
        crtc_gamma_property(&tty.drm, crtc).map(|(_, size)| size as u32)
    }

    /// Use `table` on `output` instead of the config's ICC profile, or go back to the profile
    /// with `None`. Reaches the display after the next frame. False when the monitor is gone.
    pub fn set_client_gamma(
        &mut self,
        output: &Output,
        table: Option<crate::feature::icc::GammaTable>,
    ) -> bool {
        let Backend::Tty(tty) = &mut self.backend else {
            return false;
        };
        let Some(lit) = tty.outputs.values_mut().find(|lit| lit.output == *output) else {
            return false;
        };
        lit.client_gamma = table;
        lit.gamma_pending = true;
        tty.apply_gamma();
        true
    }

    /// False when it can't be done (winit, another VT in front, no DPMS property).
    pub fn set_output_power(&mut self, output: &Output, on: bool) -> bool {
        let Backend::Tty(tty) = &mut self.backend else {
            return false;
        };
        if !tty.drm.is_active() {
            return false;
        }
        let Tty {
            outputs,
            drm,
            loop_handle,
            ..
        } = &mut *tty;
        let Some((_, lit)) = outputs.iter_mut().find(|(_, lit)| lit.output == *output) else {
            return false;
        };
        if lit.powered_off == !on {
            return true;
        }
        if let Err(err) = set_connector_dpms(drm, lit.connector, on) {
            tracing::warn!(
                "monitor {}: cannot switch it {}: {err:#}",
                lit.output.name(),
                if on { "on" } else { "off" }
            );
            return false;
        }
        tracing::info!(
            "monitor {} switched {}",
            lit.output.name(),
            if on { "on" } else { "off" }
        );
        lit.powered_off = !on;
        lit.reset_redraw(loop_handle);
        if on {
            // same recovery as after a VT switch
            if let Err(err) = lit.compositor.reset_state() {
                tracing::warn!("error resetting the DRM compositor: {err:?}");
            }
            lit.compositor.reset_buffers();
            lit.last_vblank = None;
            lit.queue_redraw();
        }
        true
    }

    /// Draw one frame to the monitor on `crtc` and queue it for the next vblank. Does nothing
    /// unless a frame was asked for (`Queued`, or `WaitingForEstimatedVBlankAndQueued`).
    fn render_crtc(&mut self, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let active = tty.drm.is_active();
        let Some(lit) = tty.outputs.get_mut(&crtc) else {
            return;
        };
        if !lit.redraw_is_queued() {
            return;
        }
        // On another VT or switched off there is nothing to draw: the monitor is asked again
        // when it comes back. This also stops the estimated vblank timer, if one is running.
        if !active || lit.powered_off {
            lit.reset_redraw(&tty.loop_handle);
            return;
        }
        // When this frame reaches the screen. Timed commits due by then belong in it, and
        // animations are computed for that moment (the clock stands still there for the frame).
        let presented_at = lit.next_presentation_time();
        let output = lit.output.clone();
        let anim_now = std::time::Instant::now()
            + presented_at.saturating_sub(crate::protocol::presentation::monotonic_now());

        self.reconcile_window_workspaces();
        self.refresh_window_outputs();
        // Needs &mut self, so it has to happen before the backend is borrowed. May end the overview.
        let overview = self.overview_plan();
        let fullscreen = self.fullscreen_rects();
        self.release_commit_timers(&output, presented_at);

        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        tty.apply_gamma();
        let Tty {
            renderer,
            outputs,
            drm,
            loop_handle,
            ..
        } = &mut *tty;
        let Some(lit) = outputs.get_mut(&crtc) else {
            return;
        };

        // VRR follows the config, for `fullscreen`, whether a fullscreen window is on top. A
        // switch that needs a modeset blanks the screen, so only `always` does one.
        // wp_content_type_v1: does the fullscreen window on top say it's a game? Only checked
        // when the config uses it.
        let says_game = (lit.content_hint || lit.content_game)
            && crate::protocol::layer_shell::fullscreen_says_game(
                &self.desk,
                output_rect(&lit.output),
            );
        let want_vrr = match lit.vrr_mode {
            Vrr::Off => false,
            Vrr::Always => !matches!(lit.vrr_support, VrrSupport::NotSupported),
            Vrr::Fullscreen => {
                matches!(lit.vrr_support, VrrSupport::Supported)
                    && fullscreen.contains(&output_rect(&lit.output))
            }
        };
        if want_vrr != lit.vrr_on {
            match lit.compositor.use_vrr(want_vrr) {
                Ok(()) => {
                    lit.vrr_on = want_vrr;
                    tracing::info!(
                        "monitor {}: variable refresh rate {}",
                        lit.output.name(),
                        if want_vrr { "on" } else { "off" }
                    );
                }
                Err(err) => {
                    // don't retry every frame
                    tracing::warn!(
                        "monitor {}: cannot switch variable refresh rate: {err}",
                        lit.output.name()
                    );
                    lit.vrr_support = VrrSupport::NotSupported;
                }
            }
        }

        // The HDR signalling goes out with the next frame, as part of its modeset commit.
        if lit.color_dirty {
            lit.color_dirty = false;
            if let Err(err) = lit.compositor.use_color_state(lit.color_target) {
                tracing::warn!(
                    "monitor {}: cannot signal HDR, back to SDR: {err}",
                    lit.output.name()
                );
                lit.color_target = ConnectorColorState::default();
                lit.hdr = None;
                lit.hdr_plan = None;
                lit.hdr_content = (None, None);
                lit.hdr_scanout = HdrScanout::default();
                lit.hdr_status = Status::Sdr(Unavailable::SignalRejected);
                let _ = lit.compositor.use_color_state(lit.color_target);
            }
        }

        // connector `content type` is Game while a fullscreen game is on top. A failure is
        // logged once, not retried every frame.
        let want_game = lit.content_hint && says_game;
        if want_game != lit.content_game {
            lit.content_game = want_game;
            match set_connector_content_type(drm, lit.connector, want_game) {
                Ok(()) => tracing::info!(
                    "monitor {}: content type {}",
                    lit.output.name(),
                    if want_game { "game" } else { "no data" }
                ),
                Err(err) => tracing::warn!(
                    "monitor {}: cannot set the content type: {err:#}",
                    lit.output.name()
                ),
            }
        }

        let scale = Scale::from(1.0f64);
        let rect = output_rect(&lit.output);
        let origin: Point<i32, Logical> = (rect.0, rect.1).into();
        // What this monitor shows. Every surface drawn below (layers, popups, thumbnails, the
        // lock screen, windows) is converted to it, not only the windows.
        let color = crate::protocol::color_management::output_color(
            &self.config,
            &self.color_management,
            &lit.output.name(),
        );
        let hdr_space = lit.hdr.as_ref().map(|hdr| hdr.space);
        // a window goes to the plane as the client coded it, which only matches a PQ signal
        let hdr_pq = lit
            .hdr
            .as_ref()
            .is_none_or(|hdr| hdr.pass.eotf == OutputEotf::Pq);
        let color = match hdr_space {
            Some(space) => Some(crate::protocol::color_management::hdr_color(color, space)),
            None => color,
        };
        let _color_scope = crate::render::effects::gamut::enter(color);
        // The surfaces drawn below say how bright their content gets, for the final pass of an HDR monitor.
        let _light_scope = crate::render::effects::gamut::collect_content_light(
            lit.hdr.as_ref().is_some_and(|hdr| hdr.content_metadata),
        );
        let pointer: Point<f64, Logical> = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();

        // front to back: cursor, then windows topmost first
        let mut elements: Vec<JffElement> = Vec::new();
        let cursor_near =
            layout::contains(layout::grow(rect, CURSOR_MARGIN), (pointer.x, pointer.y));
        match self.cursor.render_cursor() {
            RenderCursor::Hidden => {}
            _ if !cursor_near => {}
            // client-drawn cursor, placed at the pointer minus hotspot
            RenderCursor::Surface { hotspot, surface } => {
                let loc: Point<i32, Physical> =
                    (pointer.to_i32_round() - hotspot - origin).to_physical_precise_round(scale);
                let cursor_elements: Vec<JffElement> = render_elements_from_surface_tree(
                    &mut *renderer,
                    &surface,
                    loc,
                    scale,
                    1.0,
                    Kind::Cursor,
                );
                elements.extend(cursor_elements);
            }
            // themed cursor, frame picked by time (animated ones loop)
            RenderCursor::Named(cursor) => {
                let millis = self.start_time.elapsed().as_millis() as u32;
                let frame = cursor.frame(millis);
                let loc: Point<f64, Physical> =
                    (pointer - origin.to_f64() - frame.hotspot.to_f64()).to_physical(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    &mut *renderer,
                    loc,
                    &frame.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => elements.push(element.into()),
                    Err(err) => tracing::warn!("error importing the cursor image: {err:?}"),
                }
            }
        }
        // dnd icon: under the cursor, above everything else
        if cursor_near {
            let loc: Point<i32, Physical> =
                (pointer.to_i32_round() - origin).to_physical_precise_round(scale);
            elements.extend(crate::feature::dnd::icon_elements(
                self.dnd_icon.as_ref(),
                &mut *renderer,
                loc,
                scale,
            ));
        }
        // a window of a container on its way to another place: a small picture of it at the
        // pointer, under the cursor and above everything else
        elements.extend(crate::feature::containers::tile_carry_elements(
            self.tile_carry.as_ref(),
            pointer,
            self.config.radius(),
            &mut *renderer,
            origin,
            rect,
        ));
        // input method candidates sit above windows and panels
        let locked = self.session_lock.is_locked();
        let input_popups = if locked {
            Vec::new()
        } else {
            crate::protocol::text_input::place_input_popups(
                &self.text_input,
                &self.desk,
                &self.outputs,
            )
        };
        elements.extend(crate::protocol::text_input::input_popup_elements(
            &mut *renderer,
            &input_popups,
            rect,
            scale,
        ));
        // toasts go in here, above windows and panels
        let osd_at = elements.len();
        // while an area screenshot is open here, the frozen frame replaces layers and windows
        // (cursor stays on top)
        let frozen = self.screenshot.as_ref().filter(|ui| ui.shows(&lit.output));
        // A fullscreen window exactly the size of the monitor hides everything behind it. The
        // frame then holds only that window (plus cursor and overlay layer), so DrmCompositor
        // can show the client's buffer on the primary plane without a copy.
        let mut direct_scanout = false;
        // An HDR output whose fullscreen window is tried on the primary plane this frame.
        let mut hdr_scanout_try: Option<Tried> = None;
        // Blur is of the wallpaper alone unless `behind-windows` is set: a copy of the frame
        // is taken once the background layers are drawn, and the window blurs read that.
        let blur_config = (!self.config.blur().off).then(|| self.config.blur());
        let snapshot = blur_config
            .filter(|b| !b.behind_windows)
            .map(|_| WallpaperSnapshot::for_output(&lit.output.name()));
        if locked {
            // locked: lock surface only
            elements.extend(crate::protocol::session_lock::lock_surface_elements(
                &mut *renderer,
                &self.session_lock,
                &lit.output,
                scale,
            ));
        } else if let Some(ui) = frozen {
            elements.extend(screenshot_elements(&mut *renderer, ui));
        } else {
            // the monitor showing the overview draws thumbnails instead of windows
            let overview_here = overview.as_ref().filter(|plan| plan.output == lit.output);
            // a thumbnail dragged onto another monitor stays on top there
            if overview_here.is_none() {
                if let Some(plan) = &overview {
                    elements.extend(overview_drag_elements(
                        &mut *renderer,
                        plan,
                        origin,
                        rect,
                        blur_config,
                        snapshot.as_ref(),
                    ));
                }
            }
            // With a fullscreen window on top only the overlay layer stays in front of it.
            // An HDR output composites into its own texture, so a window goes to the plane only
            // when `hdr_scanout::decide` says its buffer can reach the panel as it is.
            let covering = overview_here.is_none() && fullscreen_covers(&self.desk, rect);
            direct_scanout = hdr_space.is_none() && covering;
            let mut scanout_blocked = None;
            // The light levels the connector should state: the client's while its buffer goes to the plane.
            let mut content = (None, None);
            if hdr_space.is_some() && hdr_pq && covering {
                if let Some(surface) = fullscreen_surface(&self.desk, rect) {
                    let (tag, buffer_format) = surface_scanout_facts(&surface);
                    let current = lit.compositor.current_color_state();
                    let facts = Facts {
                        covering,
                        over: if layers_shown(&lit.output, layers_above(&fullscreen, rect, false)) {
                            Some(Over::Layer)
                        } else if !input_popups.is_empty() {
                            Some(Over::Popup)
                        } else if !elements.is_empty() {
                            Some(Over::Cursor)
                        } else if !self.osd.is_empty() {
                            Some(Over::Toast)
                        } else {
                            None
                        },
                        effects: !crate::render::effects::yuv::corrections(&surface).is_empty(),
                        tag,
                        buffer_format,
                        swapchain_format: lit.compositor.format() as u32,
                        signalled: current == lit.compositor.pending_color_state()
                            && current == lit.color_target,
                    };
                    let key = Tried {
                        surface: surface.id().protocol_id(),
                        format: buffer_format,
                        subsurfaces: surface_count(&surface),
                    };
                    match hdr_scanout::decide(&facts) {
                        Ok(()) => {
                            lit.hdr_scanout.reason = None;
                            if key.was_declined(lit.hdr_scanout.declined) {
                                scanout_blocked = Some(None);
                            } else {
                                hdr_scanout_try = Some(key);
                                content = facts
                                    .tag
                                    .map_or((None, None), |tag| (tag.max_cll, tag.max_fall));
                            }
                        }
                        // The levels are on their way to the connector, keep them until they land.
                        Err(Decline::SignalPending) => {
                            content = lit.hdr_content;
                            scanout_blocked = Some(Some(Decline::SignalPending));
                        }
                        Err(decline) => scanout_blocked = Some(Some(decline)),
                    }
                }
            }
            if hdr_space.is_some() && !covering {
                lit.hdr_scanout = HdrScanout::default();
            }
            if content != lit.hdr_content {
                lit.hdr_content = content;
                if let Some(plan) = lit.hdr_plan {
                    lit.color_target = hdr_color_state(&plan.with_content(content.0, content.1));
                    lit.color_dirty = true;
                    lit.queue_redraw();
                }
            }
            if let Some(reason) = scanout_blocked.flatten() {
                if lit.hdr_scanout.reason != Some(reason) {
                    lit.hdr_scanout.reason = Some(reason);
                    if reason.is_notable() {
                        tracing::info!(
                            "monitor {}: a PQ window is not scanned out while HDR: {reason}",
                            lit.output.name()
                        );
                    } else {
                        tracing::debug!(
                            "monitor {}: no direct scanout while HDR: {reason}",
                            lit.output.name()
                        );
                    }
                }
            }
            // Blur is opt-in, and never with a window going straight to the primary plane: the
            // blur needs the frame composited.
            let blur = (!direct_scanout && hdr_scanout_try.is_none() && !self.config.blur().off)
                .then(|| self.config.blur());
            let above = layers_above(&fullscreen, rect, overview_here.is_some());
            elements.extend(layer_elements(
                &mut *renderer,
                &lit.output,
                above,
                blur,
                self.config.blur_ignore_layers(),
                self.config.popup_radius(),
            ));
            // popups of layers below the windows draw above them, except over a fullscreen
            // window: only the overlay layer is in front of that
            if !(direct_scanout || hdr_scanout_try.is_some()) {
                elements.extend(layer_popup_elements(
                    &mut *renderer,
                    &lit.output,
                    &BELOW_WINDOWS,
                    blur,
                    self.config.blur_ignore_layers(),
                    self.config.popup_radius(),
                ));
            }
            if let Some(plan) = overview_here {
                elements.extend(overview_elements(
                    &mut *renderer,
                    plan,
                    origin,
                    blur,
                    snapshot.as_ref(),
                ));
            } else {
                let radius = self.config.radius();
                let popup_radius = self.config.popup_radius();
                let border = self.config.border();
                let animate = self.config.window_animations();
                let (open_time, close_time) = self.config.window_animation_times();
                let open_style = self.config.window_open_style();
                let now = anim_now;
                // windows that were just closed, fading out, are on top
                crate::feature::window_anim::ghost_elements(
                    &mut *renderer,
                    &mut self.closing,
                    self.desk.current_on(&lit.output.name()),
                    origin,
                    close_time,
                    now,
                    &mut elements,
                );
                for entry in self.desk.top_down() {
                    let geo = entry.window.geometry();
                    let window_origin: Point<i32, Logical> = Point::from(entry.pos) - geo.loc;
                    let bbox = entry.window.bbox();
                    let area = (
                        window_origin.x + bbox.loc.x,
                        window_origin.y + bbox.loc.y,
                        bbox.size.w,
                        bbox.size.h,
                    );
                    // on another monitor
                    if !layout::intersects(area, rect) {
                        continue;
                    }
                    let size = entry.window.geometry().size;
                    let is_covering = (entry.pos.0, entry.pos.1, size.w, size.h) == rect;
                    // Popups and the window, each with its blur right behind it. A window tried on
                    // the primary plane is not converted: its buffer goes to the panel as it is.
                    window_elements(
                        &mut *renderer,
                        &entry.window,
                        window_origin - origin,
                        self.config.blur_for_window(
                            blur,
                            &crate::feature::overview_mode::app_id_of(&entry.window),
                        ),
                        snapshot.as_ref(),
                        radius,
                        popup_radius,
                        if hdr_scanout_try.is_some() && is_covering {
                            None
                        } else {
                            color
                        },
                        BorderPaint::of(border, &entry.window, self.focused_window.as_ref()),
                        if animate {
                            crate::feature::window_anim::open_frame(
                                open_style,
                                entry.shown,
                                now,
                                open_time,
                            )
                        } else {
                            crate::feature::window_anim::OpenFrame::DONE
                        },
                        &mut elements,
                    );
                    // nothing below the covering window goes in the frame
                    if is_covering {
                        if direct_scanout || hdr_scanout_try.is_some() {
                            break;
                        }
                    }
                }
            }
            // Wallpapers and desktop widgets are behind everything.
            if !(direct_scanout || hdr_scanout_try.is_some()) {
                // Fills the wallpaper copy: drawn right after the background layers.
                if let Some(snapshot) = &snapshot {
                    elements.push(JffElement::from(snapshot.element((rect.2, rect.3).into())));
                }
                elements.extend(layer_body_elements(
                    &mut *renderer,
                    &lit.output,
                    &BELOW_WINDOWS,
                    blur,
                    self.config.blur_ignore_layers(),
                ));
            }
        }

        // no toasts over direct scanout, the lock screen or the frozen screenshot
        let scanout = direct_scanout || hdr_scanout_try.is_some();
        if !scanout && !locked && frozen.is_none() && !self.osd.is_empty() {
            let mut toasts = Vec::new();
            self.osd
                .elements(&mut *renderer, &lit.output, pointer, &mut toasts);
            elements.splice(osd_at..osd_at, toasts);
        }

        // Plane scanout is off by default. When a fullscreen window covers the monitor, allow
        // its buffer on the primary plane whatever the element kind, and move the cursor to its
        // own plane, or it would sit on top of the window and block that.
        if scanout != lit.covering {
            lit.covering = scanout;
            tracing::debug!(
                "monitor {}: a fullscreen window {} the monitor",
                lit.output.name(),
                if scanout {
                    "now covers"
                } else {
                    "no longer covers"
                },
            );
        }
        let frame_flags = if direct_scanout {
            FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
        } else if hdr_scanout_try.is_some() {
            // Only a buffer in the swapchain's format, and no plane for the cursor: its pixels
            // would not be taken as PQ.
            FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT
        } else {
            FrameFlags::empty()
        };

        // Tearing: a fullscreen window covering the monitor that asked for it. The compositor
        // falls back to a vsync flip when the frame can't be flipped async.
        let presentation = if scanout
            && self.config.allow_tearing()
            && crate::protocol::layer_shell::fullscreen_wants_tearing(&self.desk, rect)
        {
            PresentationMode::Async
        } else {
            PresentationMode::VSync
        };

        // HDR: composite in the working space, then hand the output's compositor the one
        // element that encodes the result for the signal.
        let mut hdr_states = None;
        let mut hdr_failed = false;
        if let Some(hdr) = lit.hdr.as_mut().filter(|_| hdr_scanout_try.is_none()) {
            let space = hdr.space;
            let pass = hdr.pass;
            let linear = hdr_frame::linearize(&mut *renderer, std::mem::take(&mut elements), space);
            let lights = crate::render::effects::gamut::take_content_light();
            if !hdr
                .frame
                .as_ref()
                .is_some_and(|f| f.fits(&lit.output, &pass))
            {
                hdr.frame = match HdrFrame::new(&mut *renderer, &lit.output, pass) {
                    Ok(frame) => Some(frame),
                    Err(err) => {
                        if note_hdr_fault(&mut lit.hdr_fault, HdrFault::Target) {
                            tracing::warn!(
                                "monitor {}: cannot make the HDR target: {err}",
                                lit.output.name()
                            );
                        }
                        None
                    }
                };
            }
            let background = if locked {
                LOCKED_BACKGROUND
            } else {
                BACKGROUND
            };
            let clear = hdr_frame::clear_color(space, background);
            match hdr
                .frame
                .as_mut()
                .map(|f| f.render(&mut *renderer, &linear, clear, &lights))
            {
                Some(Ok((element, states))) => {
                    hdr.content_peak = hdr.frame.as_ref().and_then(HdrFrame::content_peak);
                    if lit.hdr_fault.take().is_some() {
                        tracing::info!("monitor {}: the HDR frame draws again", lit.output.name());
                    }
                    elements.push(JffElement::Hdr(element));
                    hdr_states = Some(states);
                }
                Some(Err(err)) => {
                    if note_hdr_fault(&mut lit.hdr_fault, HdrFault::Draw) {
                        tracing::warn!(
                            "monitor {}: error drawing the HDR frame: {err}",
                            lit.output.name()
                        );
                    }
                    hdr.frame = None;
                    hdr.content_peak = None;
                    hdr_failed = true;
                }
                None => hdr_failed = true,
            }
        }

        // The frame is not drawn into the texture while the window is on the plane, so the
        // texture is repainted whole when it is needed again.
        if hdr_scanout_try.is_some() {
            if let Some(hdr) = lit.hdr.as_mut() {
                hdr.frame = None;
                hdr.content_peak = None;
            }
        }

        let mut queued = false;
        // The window did not reach the plane: draw the frame again through the HDR path.
        let mut retry = false;
        if !hdr_failed {
            match lit.compositor.render_frame(
                &mut *renderer,
                &elements,
                if locked || scanout {
                    LOCKED_BACKGROUND
                } else {
                    BACKGROUND
                },
                frame_flags,
                presentation,
            ) {
                Ok(res) => {
                    // The window was tried on the plane and the compositor put it in a copy: that
                    // copy is not PQ-encoded for the panel, so it is dropped and the frame is
                    // drawn through the HDR path.
                    let declined = hdr_scanout_try.is_some()
                        && !matches!(res.primary_element, PrimaryPlaneElement::Element(_));
                    let is_empty = res.is_empty || declined;
                    if declined {
                        retry = true;
                        if let Some(key) = hdr_scanout_try {
                            if lit.hdr_scanout.declined != Some(key) {
                                lit.hdr_scanout.declined = Some(key);
                                tracing::info!(
                                    "monitor {}: a PQ window is not scanned out while HDR: the buffer did not reach the primary plane",
                                    lit.output.name()
                                );
                            }
                        }
                    }
                    // debug log for direct scanout changes
                    let on_plane = matches!(res.primary_element, PrimaryPlaneElement::Element(_));
                    if on_plane != lit.scanning_out {
                        lit.scanning_out = on_plane;
                        tracing::debug!(
                            "monitor {}: primary plane now shows {} (fullscreen cover: {scanout})",
                            lit.output.name(),
                            if on_plane {
                                "the client's buffer (direct scanout)"
                            } else {
                                "the composited frame"
                            },
                        );
                    }
                    if !declined && res.needs_sync() {
                        if let PrimaryPlaneElement::Swapchain(element) = res.primary_element {
                            if let Err(err) = element.sync.wait() {
                                tracing::warn!("error waiting for frame completion: {err:?}");
                            }
                        }
                    }
                    if !is_empty {
                        if let Some(dmabuf_feedback) = &lit.dmabuf_feedback {
                            send_surface_feedback(
                                &self.desk,
                                &lit.output,
                                hdr_states.as_ref().unwrap_or(&res.states),
                                dmabuf_feedback,
                                scanout,
                            );
                        }
                        let feedback = crate::protocol::presentation::take_feedback(
                            &self.desk,
                            &lit.output,
                            hdr_states.as_ref().unwrap_or(&res.states),
                        );
                        match lit.compositor.queue_frame(feedback) {
                            Ok(()) => queued = true,
                            Err(err) => tracing::warn!("error queueing frame: {err}"),
                        }
                    }
                }
                Err(err) => tracing::warn!("error rendering frame: {err}"),
            }
        }

        if queued {
            // The frame goes out with the next vblank, so the timer that stood in for it is
            // not needed any more.
            if let RedrawState::WaitingForEstimatedVBlankAndQueued { token } = lit.redraw {
                loop_handle.remove(token);
            }
            lit.redraw = RedrawState::WaitingForVBlank {
                redraw_needed: false,
            };
        } else {
            // Nothing went to the display, so no vblank is coming. What clients committed is
            // already on screen: their barriers go, their callbacks wait for the estimated vblank.
            tty.queue_estimated_vblank(crtc);
            if retry {
                if let Some(lit) = tty.outputs.get_mut(&crtc) {
                    lit.queue_redraw();
                }
            }
        }

        if queued {
            // The clients' buffers were latched by this frame, so the callbacks can go out now.
            // A new cycle starts: each surface gets one callback per cycle, which keeps a client
            // that commits without damage from spinning.
            advance_frame_sequence(&output);
            self.send_frame_callbacks(&output);
        } else {
            self.release_fifo_barriers(&output);
        }
    }

    /// The timer that stands in for a vblank went off (see [`RedrawState::WaitingForEstimatedVBlank`]).
    fn on_estimated_vblank(&mut self, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&crtc) else {
            return;
        };
        match lit.redraw {
            RedrawState::WaitingForEstimatedVBlank { .. } => {}
            // The timer fired just in front of a redraw that was asked for: draw it now.
            RedrawState::WaitingForEstimatedVBlankAndQueued { .. } => {
                lit.redraw = RedrawState::Queued;
                advance_frame_sequence(&lit.output);
                return;
            }
            _ => return, // a timer that was overtaken (monitor off, session paused)
        }
        lit.redraw = RedrawState::Idle;
        let output = lit.output.clone();

        // The cycle is over: clients that committed without damage get their callback now.
        advance_frame_sequence(&output);
        self.send_frame_callbacks(&output);

        let ongoing = self.animations_ongoing();
        if let Backend::Tty(tty) = &mut self.backend {
            if let Some(lit) = tty.outputs.get_mut(&crtc) {
                if ongoing {
                    lit.queue_redraw();
                }
            }
        }
    }

    /// The display showed the frame queued on `crtc`.
    fn on_vblank(&mut self, crtc: crtc::Handle, meta: Option<DrmEventMetadata>) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&crtc) else {
            return;
        };
        // Out of the waiting state for the rest of this handler. A commit that comes in while
        // barriers are released below queues a redraw on the idle monitor.
        let previous = std::mem::replace(&mut lit.redraw, RedrawState::Idle);
        if let RedrawState::WaitingForEstimatedVBlank { token }
        | RedrawState::WaitingForEstimatedVBlankAndQueued { token } = previous
        {
            tty.loop_handle.remove(token);
        }
        let redraw_needed = matches!(
            previous,
            RedrawState::WaitingForVBlank {
                redraw_needed: true
            }
        );
        let Some(lit) = tty.outputs.get_mut(&crtc) else {
            return;
        };
        // kernel flip timestamp, if it gave one on the monotonic clock
        let hw_time = match meta.map(|m| m.time) {
            Some(DrmEventTime::Monotonic(time)) if !time.is_zero() => Some(time),
            _ => None,
        };
        lit.last_vblank =
            Some(hw_time.unwrap_or_else(crate::protocol::presentation::monotonic_now));
        match lit.compositor.frame_submitted() {
            Ok(mut feedback) => {
                let refresh = crate::protocol::presentation::refresh_interval(&lit.output)
                    .map_or(Refresh::Unknown, Refresh::Fixed);
                let mut flags = wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwCompletion;
                if hw_time.is_some() {
                    flags.insert(wp_presentation_feedback::Kind::HwClock);
                }
                feedback.presented::<_, Monotonic>(
                    hw_time.unwrap_or_else(crate::protocol::presentation::monotonic_now),
                    refresh,
                    meta.map_or(0, |m| u64::from(m.sequence)),
                    flags,
                );
            }
            Err(err) => tracing::warn!("error marking frame as submitted: {err}"),
        }
        let output = lit.output.clone();
        // the frame is on screen, so clients waiting on a fifo barrier may commit again
        self.release_fifo_barriers(&output);

        // Draw again right away when a commit or an animation asked for it. Otherwise the
        // monitor goes idle: nothing is drawn until something asks for it. The drawing itself
        // happens at the end of this event loop pass, see `redraw_queued_outputs`.
        let ongoing = self.animations_ongoing();
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&crtc) else {
            return;
        };
        if redraw_needed || ongoing {
            lit.queue_redraw();
        }
        if matches!(lit.redraw, RedrawState::Idle) {
            // Same cycle as the last frame, so only surfaces that have not had a callback yet get one.
            self.send_frame_callbacks(&output);
        }
    }

    /// Rescan for plugged, unplugged or reconfigured monitors and update the outputs.
    pub fn scan_tty_outputs(&mut self) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        // on another VT, the scan happens when the session comes back
        if !tty.drm.is_active() {
            return;
        }
        let known = tty.monitors().to_vec();
        let (removed, added) = match tty.reconcile(&self.config) {
            Ok(changes) => changes,
            Err(err) => {
                tracing::warn!("error scanning monitors: {err:#}");
                return;
            }
        };
        // also runs on reload-config, where a profile may have changed
        tty.sync_gamma(&self.config);
        tty.sync_hdr(&self.config);
        let monitors_changed = tty.monitors() != known.as_slice();
        crate::protocol::color_management::sync_output_color(self);
        if removed.is_empty() && added.is_empty() {
            // a disabled monitor can still have been plugged or unplugged
            if monitors_changed {
                self.output_management_changed();
            }
            return;
        }
        for output in removed {
            tracing::info!("monitor {} removed", output.name());
            self.remove_output(&output);
        }
        for output in added {
            tracing::info!("monitor {} added", output.name());
            self.add_output(output);
        }
        crate::protocol::color_management::sync_output_color(self);
        self.render();
    }

    /// VT switches: let go of the GPU and input when the session is lost, take them back on return.
    fn on_session_event(&mut self, event: SessionEvent) {
        // The overview holds the pointer and the keyboard, drop it before the session goes.
        if matches!(event, SessionEvent::PauseSession) {
            self.abort_overview();
            self.cancel_screenshot();
            // Releases that happen while another VT has the keyboard never arrive.
            self.suppressed_keys.clear();
            self.forwarded_keys.clear();
            self.swallowed_buttons.clear();
        }
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                tty.libinput.suspend();
                tty.drm.pause();
                // the kernel took the leases back with the device
                tty.active_leases.clear();
                if let Some(state) = tty.lease_state.as_mut() {
                    state.suspend();
                }
                for lit in tty.outputs.values_mut() {
                    lit.reset_redraw(&tty.loop_handle);
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                if tty.libinput.resume().is_err() {
                    tracing::warn!("error resuming libinput");
                }
                if let Err(err) = tty.drm.activate(false) {
                    tracing::warn!("error activating the DRM device: {err:?}");
                }
                if let Some(state) = tty.lease_state.as_mut() {
                    state.resume::<JffState>();
                }
                for lit in tty.outputs.values_mut() {
                    if let Err(err) = lit.compositor.reset_state() {
                        tracing::warn!("error resetting the DRM compositor: {err:?}");
                    }
                    lit.compositor.reset_buffers();
                    lit.reset_redraw(&tty.loop_handle);
                    lit.last_vblank = None;
                    lit.powered_off = false;
                    // another VT may have left its own curves: set ours (or clear them)
                    // after the first frame
                    lit.gamma_pending = true;
                    lit.gamma_applied = true;
                    // reset_state read back whatever the other VT left on the connector
                    lit.color_dirty = true;
                }
                // Monitors may have been swapped while another VT was in front.
                self.scan_tty_outputs();
                self.render();
            }
        }
    }

    /// Ctrl+Alt+F1..F12.
    pub fn change_vt(&mut self, vt: i32) {
        if let Backend::Tty(tty) = &mut self.backend {
            if let Err(err) = tty.session.change_vt(vt) {
                tracing::warn!("error changing VT: {err}");
            }
        }
    }
}
