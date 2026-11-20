//! TTY backend: jff as the session compositor on bare DRM/KMS. Session and VT switching go
//! through libseat (needs seatd or logind), input through libinput, hotplug through udev.
//!
//! Every GPU is opened. One (`render-device`, else the boot GPU) draws everything with its
//! renderer, and clients, linux-dmabuf and the wl_display binding stay on it. Another GPU only
//! scans out: the primary draws the frame of its monitors into a dmabuf and the other GPU shows
//! it as one texture (`cross_gpu`), so those monitors have a software cursor. Those frames go
//! through a copy, so a monitor on another GPU has no VRR and no content type. HDR works there
//! too: the primary codes the frame as PQ and it crosses in a 10-bit format.
//! A GPU plugged in later is opened and lit like the others, one that is unplugged takes its
//! monitors with it. The primary cannot go away: if it is unplugged jff stops. A connector
//! name that two GPUs share (`DP-1`) is kept by the first one (the primary's) and the other is
//! called `cardN-DP-1`.

mod cross_gpu;

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
    Colorspace, ConnectorColorState, DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent,
    DrmEventMetadata,
    DrmEventTime, DrmNode, Eotf, HdrOutputMetadata, VrrSupport,
};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::utils::select_dmabuf_feedback;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    default_primary_scanout_output_compare, Id, Kind,
    RenderElementStates,
};
use smithay::backend::renderer::utils::CommitCounter;
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
use smithay::output::{
    Mode, Output, OutputModeSource, PhysicalProperties, Scale as OutputScale, Subpixel,
};
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
use smithay::utils::{
    DeviceFd, Logical, Monotonic, Physical, Point, Rectangle, Scale, Size, Transform,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    with_states, with_surface_tree_downward, SurfaceData, TraversalAction,
};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder};
use smithay::wayland::drm_lease::{DrmLease, DrmLeaseBuilder, DrmLeaseState, LeaseRejected};
use smithay::wayland::presentation::Refresh;
use smithay::reexports::wayland_server::DisplayHandle;

use cross_gpu::{Cross, CrossElement};

use crate::app::pacing::advance_frame_sequence;
use crate::app::{Backend, BACKGROUND};
use crate::config::{Config, ModeRequest, OutputTransform, Vrr};
use crate::desk::Desk;
use crate::feature::outputs::{output_rect, wanted_scale, Monitor, MonitorMode};
use crate::feature::overview_mode::{
    overview_drag_elements, overview_elements, overview_ripple_elements,
};
use crate::feature::screenshot::screenshot_elements;
use crate::primitives::color::ImageDescription;
use crate::primitives::edid::{self, Edid, Identity};
use crate::primitives::frame_clock::{next_presentation_time, wait_until, FALLBACK_REFRESH};
use crate::primitives::hdr::{self, KernelSupport, OutputEotf, Status, Unavailable};
use crate::primitives::hdr_output::{self, FinalPass};
use crate::primitives::hdr_scanout::{self, Decline, Facts, Over, Tried};
use crate::primitives::layout;
use crate::primitives::stats as nerd;
use crate::protocol::color_management::WorkingSpace;
use crate::protocol::content_type::Content;
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
pub(crate) const CURSOR_MARGIN: i32 = 256;

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
    /// A scanout tranche (what the primary plane takes directly), a second one for what only an
    /// overlay plane takes, then the render formats.
    /// For surfaces being scanned out, so the client allocates buffers that need no copy.
    scanout: DmabufFeedback,
}

/// The primary plane's formats come first, so a fullscreen client allocates what that plane
/// takes without a copy. The formats only an overlay plane takes follow in a tranche of their
/// own, for windows that are not fullscreen. None without a render node.
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
    // What an overlay plane takes and the primary plane does not. Planes often share formats,
    // so the set drops the repeats.
    let overlay_set: FormatSet = compositor
        .surface()
        .planes()
        .overlay
        .iter()
        .flat_map(|plane| plane.formats.iter())
        .filter(|format| texture_formats.contains(format) && !plane_formats.contains(format))
        .copied()
        .collect();
    let overlay_formats: Vec<_> = overlay_set.iter().copied().collect();

    tracing::info!(
        "dmabuf feedback: {} primary-plane formats, {} renderer formats, {} usable for scanout, {} only on overlay planes",
        plane_formats.iter().count(),
        texture_formats.iter().count(),
        scanout_formats.len(),
        overlay_formats.len(),
    );
    let render = DmabufFeedbackBuilder::new(main_device, texture_formats.iter().copied())
        .build()
        .ok()?;
    let scanout = if scanout_formats.is_empty() && overlay_formats.is_empty() {
        render.clone()
    } else {
        let mut builder = DmabufFeedbackBuilder::new(main_device, texture_formats.iter().copied());
        if !scanout_formats.is_empty() {
            builder = builder.add_preference_tranche(
                scanout_device,
                TrancheFlags::Scanout,
                scanout_formats,
                0..=u32::MAX,
            );
        }
        if !overlay_formats.is_empty() {
            builder = builder.add_preference_tranche(
                scanout_device,
                TrancheFlags::Scanout,
                overlay_formats,
                0..=u32::MAX,
            );
        }
        builder.build().ok()?
    };
    Some(SurfaceDmabufFeedback { render, scanout })
}

/// The feedback for the monitors of a secondary GPU: the formats the primary's renderer imports,
/// and never a scanout tranche, since those monitors show a copy. It is also what takes a window
/// back to the render tranche when it moves there from a monitor it was scanned out on.
fn render_only_feedback(
    renderer: &GlesRenderer,
    render_node: Option<DrmNode>,
) -> Option<SurfaceDmabufFeedback> {
    let main_device = render_node?.dev_id();
    let formats: FormatSet = renderer.dmabuf_formats().into_iter().collect();
    let render = DmabufFeedbackBuilder::new(main_device, formats.iter().copied())
        .build()
        .ok()?;
    Some(SurfaceDmabufFeedback {
        render: render.clone(),
        scanout: render,
    })
}

/// After a frame: note which surfaces ended up on a plane and send each the matching feedback.
/// A scanned-out fullscreen client gets the scanout tranche, and drops back to the render one
/// once something covers it. Without `feedback` only the monitor a surface is shown on is noted.
fn send_surface_feedback(
    desk: &Desk<Window>,
    output: &Output,
    states: &RenderElementStates,
    feedback: Option<&SurfaceDmabufFeedback>,
    only_topmost: bool,
) {
    let rect = output_rect(output);
    let select = feedback.map(|feedback| {
        move |surface: &WlSurface, _: &SurfaceData| {
            select_dmabuf_feedback(surface, states, &feedback.render, &feedback.scanout)
        }
    });
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
        if let Some(select) = &select {
            entry
                .window
                .send_dmabuf_feedback(output, surface_primary_scanout_output, select);
        }
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
        if let Some(select) = &select {
            layer.send_dmabuf_feedback(output, surface_primary_scanout_output, select);
        }
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

/// What the stats overlay and `jff msg stats` report about the window `surface` on the monitor
/// `lit`. `path` is how its frame reached the screen, the rest is what the monitor does with it.
fn stats_snapshot(
    surface: &WlSurface,
    win: &crate::feature::stats::WindowFacts,
    lit: &TtyOutput,
    config: &Config,
    path: nerd::Path,
    scanout_without_overlay: Option<bool>,
    async_flip: bool,
) -> nerd::Snapshot {
    let (color, format) = surface_scanout_facts(surface);
    let buffer = match format {
        Some(code) => nerd::BufferKind::Dmabuf(
            code,
            surface_buffer_modifier(surface).unwrap_or(nerd::MODIFIER_INVALID),
        ),
        None if crate::protocol::layer_shell::is_mapped(surface) => nerd::BufferKind::Shm,
        None => nerd::BufferKind::None,
    };
    // a client-drawn shadow makes the sizes differ, so only a fullscreen window is compared
    let buffer_size = if win.fullscreen {
        smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
            state.buffer_size()
        })
        .flatten()
        .map(|size| (size.w, size.h))
    } else {
        None
    };
    nerd::Snapshot {
        app_id: win.app_id.clone(),
        output: lit.output.name(),
        mode: lit
            .output
            .current_mode()
            .map(|mode| (mode.size.w, mode.size.h, mode.refresh)),
        client: None,
        shown: None,
        display_hz: 0.0,
        cost: None,
        sync: None,
        latency: None,
        tearing: None,
        fullscreen: win.fullscreen,
        wants_tearing: win.wants_tearing,
        says_content: win.content.map(Content::name),
        client_sync: crate::protocol::presentation::surface_sync_use(surface),
        buffer,
        buffer_size,
        window_size: win.window_size,
        color,
        allow_tearing: config.allow_tearing(),
        vrr_policy: match lit.vrr_mode {
            Vrr::Off => "off",
            Vrr::Fullscreen => "fullscreen",
            Vrr::Always => "always",
        },
        path,
        scanout_without_overlay,
        vrr: match lit.vrr_support {
            VrrSupport::NotSupported => nerd::Vrr::Unsupported,
            _ if lit.vrr_on => nerd::Vrr::On,
            _ => nerd::Vrr::Off,
        },
        hdr_output: lit.hdr.is_some(),
        content_told: lit.content_told.map(Content::name),
        async_flip,
    }
}

/// The format modifier of the surface's dmabuf, `None` when it is not one.
fn surface_buffer_modifier(surface: &WlSurface) -> Option<u64> {
    smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
        state
            .buffer()
            .and_then(|buffer| get_dmabuf(buffer).ok())
            .map(|dmabuf| u64::from(dmabuf.format().modifier))
    })
    .flatten()
}

// The decision compares the fourccs it knows by number.
const _: () = {
    assert!(Fourcc::Argb2101010 as u32 == hdr_scanout::ARGB2101010);
    assert!(Fourcc::Abgr2101010 as u32 == hdr_scanout::ABGR2101010);
    assert!(Fourcc::Xrgb2101010 as u32 == hdr_scanout::XRGB2101010);
    assert!(Fourcc::Xbgr2101010 as u32 == hdr_scanout::XBGR2101010);
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

/// What a monitor on a secondary GPU shows until the first frame of the primary reaches it.
const SECONDARY_PLACEHOLDER: [f32; 4] = [0.0, 0.35, 0.45, 1.0];

/// A lit monitor and its frame loop state.
struct TtyOutput {
    compositor: GbmDrmCompositor,
    output: Output,
    connector: connector::Handle,
    mode: DrmMode,
    /// The monitor is on a secondary GPU: the primary draws its frames and the secondary only
    /// scans them out.
    via_primary: bool,
    /// How the frames of a monitor on a secondary GPU get there, `None` before the first one.
    cross: Option<Cross>,
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
    /// What the connector's `content type` says right now (`None` is no data).
    content_told: Option<Content>,
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
    /// Gain of the light from the curves a client set (night light), applied by the HDR final pass because the display's own table cannot take them in HDR.
    night_gain: [f32; 3],
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
    /// The frame in flight was drawn while the session was locked.
    locked_frame: bool,
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

/// Which GPU a CRTC (or anything else numbered by the kernel) belongs to. Those handles are
/// only unique per device, so they are never used alone as a key. A number is never given to
/// another GPU, so a late event of a GPU that is gone finds nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DeviceId(usize);

impl DeviceId {
    /// The GPU that composes everything.
    const PRIMARY: DeviceId = DeviceId(0);
}

/// A lit monitor is found by its device and its CRTC.
type OutputKey = (DeviceId, crtc::Handle);

/// One opened GPU.
struct GpuDevice {
    // Field order is drop order: the renderer goes before the DRM and GBM devices it draws on,
    // and the DRM device before the GBM device it works with.
    /// A renderer on this GPU, only for secondary ones (the primary's is `Tty::renderer`). It
    /// imports the frames the primary draws for the monitors of this GPU.
    renderer: Option<GlesRenderer>,
    render_node: Option<DrmNode>,
    drm: DrmDevice,
    gbm: GbmDevice<DeviceFd>,
    path: std::path::PathBuf,
    /// The source of its DRM events, taken out of the loop when the GPU goes away.
    events: Option<RegistrationToken>,
}

/// [`pick_device`] for a caller that needs `&mut`: creating a DRM surface does.
fn pick_device_mut<'a>(
    primary: &'a mut GpuDevice,
    secondary: &'a mut [Option<GpuDevice>],
    id: DeviceId,
) -> Option<&'a mut GpuDevice> {
    match id.0 {
        0 => Some(primary),
        n => secondary.get_mut(n - 1)?.as_mut(),
    }
}

/// The renderer of secondary device `id` (the primary's is `Tty::renderer`).
fn secondary_renderer(
    secondary: &mut [Option<GpuDevice>],
    id: DeviceId,
) -> Option<&mut GlesRenderer> {
    secondary
        .get_mut(id.0.checked_sub(1)?)?
        .as_mut()?
        .renderer
        .as_mut()
}

/// The device behind `id`: 0 is the primary, `n` is `secondary[n - 1]`, `None` once it is gone.
/// Takes the two fields instead of `&Tty` so it works while other fields of `Tty` are borrowed.
fn pick_device<'a>(
    primary: &'a GpuDevice,
    secondary: &'a [Option<GpuDevice>],
    id: DeviceId,
) -> Option<&'a GpuDevice> {
    match id.0 {
        0 => Some(primary),
        n => secondary.get(n - 1)?.as_ref(),
    }
}

pub struct Tty {
    // Field order is drop order: everything that uses a GPU goes before the devices.
    pub renderer: GlesRenderer,
    outputs: HashMap<OutputKey, TtyOutput>,
    /// Leases clients hold right now. They go before `drm` so they are revoked first.
    active_leases: Vec<DrmLease>,
    /// wp_drm_lease_device_v1, `None` until [`Tty::init_leasing`] or if it could not be set up.
    lease_state: Option<DrmLeaseState>,
    /// Connected non-desktop connectors (VR headsets) with the CRTC kept for each. They are
    /// offered as leases and never become outputs.
    /// Leasing stays on the primary GPU.
    non_desktop: HashMap<connector::Handle, crtc::Handle>,
    /// All connected monitors, lit or not.
    monitors: Vec<Monitor>,
    libinput: Libinput,
    session: LibSeatSession,
    loop_handle: LoopHandle<'static, JffState>,
    /// GPUs that only scan out, `DeviceId(n)` is `secondary[n - 1]`. One that was unplugged
    /// leaves `None`, so the others keep their number.
    secondary: Vec<Option<GpuDevice>>,
    /// The GPU `renderer` belongs to.
    primary: GpuDevice,
}

/// The name a monitor goes by, taken into `names`. Connector names count per GPU, so `DP-1`
/// exists on each of them: the first GPU to ask keeps it, the others get `card1-DP-1`. `None`
/// when that is taken too.
fn unique_name(names: &mut HashSet<String>, name: &str, card: &str) -> Option<String> {
    if names.insert(name.to_string()) {
        return Some(name.to_string());
    }
    let renamed = format!("{card}-{name}");
    names.insert(renamed.clone()).then_some(renamed)
}

/// Connector name as clients see it, e.g. `DP-1`.
fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// How the config wants the picture of monitor `name` turned onto its panel.
fn output_transform(config: &Config, name: &str) -> Transform {
    config
        .output(name)
        .map_or(Transform::Normal, |c| c.transform.to_smithay())
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
fn monitor_of(info: &connector::Info, name: String, identity: Identity) -> Monitor {
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
        identity,
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
const CONTENT_TYPE_PHOTO: u64 = 2;
const CONTENT_TYPE_CINEMA: u64 = 3;
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

/// The connector's EDID as the kernel hands it over. `None` when the connector has none.
fn read_edid_bytes(drm: &DrmDevice, connector: connector::Handle) -> Option<Vec<u8>> {
    let props = drm.get_properties(connector).ok()?;
    let (handles, values) = props.as_props_and_values();
    let blob = handles.iter().zip(values).find_map(|(&handle, &value)| {
        let info = drm.get_property(handle).ok()?;
        (info.name().to_bytes() == b"EDID").then_some(value)
    })?;
    if blob == 0 {
        return None;
    }
    drm.get_property_blob(blob).ok()
}

/// The connector's EDID, parsed. `None` when it has none or it is not an EDID.
fn read_edid(drm: &DrmDevice, connector: connector::Handle) -> Option<Edid> {
    edid::parse(&read_edid_bytes(drm, connector)?).ok()
}

/// Who made the monitor and which one it is. Empty when there is no usable EDID.
fn read_identity(drm: &DrmDevice, connector: connector::Handle) -> Identity {
    read_edid_bytes(drm, connector).map_or_else(Identity::default, |bytes| edid::identity(&bytes))
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
    min_override: Option<u32>,
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
    hdr::plan(edid, &kernel, peak_override).map(|plan| plan.with_min_luminance(min_override))
}

/// Set the connector's `content type` property to Photo, Cinema (video) or Game, or back to no
/// data.
fn set_connector_content_type(
    drm: &DrmDevice,
    connector: connector::Handle,
    content: Option<Content>,
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
    let value = match content {
        Some(Content::Photo) => CONTENT_TYPE_PHOTO,
        Some(Content::Video) => CONTENT_TYPE_CINEMA,
        Some(Content::Game) => CONTENT_TYPE_GAME,
        None => CONTENT_TYPE_NO_DATA,
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
    /// The render node of the GPU jff draws with, `None` when the driver has none.
    pub fn render_node(&self) -> Option<DrmNode> {
        self.primary.render_node
    }

    pub fn drm_device_fd(&self) -> &DrmDeviceFd {
        self.primary.drm.device_fd()
    }

    /// The device behind `id`, `None` if it is gone.
    fn device(&self, id: DeviceId) -> Option<&GpuDevice> {
        pick_device(&self.primary, &self.secondary, id)
    }

    /// Whether the last frame on `output` was direct scanout. False if it isn't lit.
    pub fn scanning_out(&self, output: &Output) -> bool {
        self.outputs
            .values()
            .any(|lit| lit.output == *output && lit.scanning_out)
    }

    /// The HDR state of `output` for `outputs` and `watch`: what the config asks for, and what the monitor does with it.
    pub fn output_hdr(&self, output: &Output, config: &Config) -> crate::ipc::events::OutputHdr {
        use crate::ipc::events::OutputHdr;
        let name = output.name();
        let output_config = config.output(&name);
        let requested = output_config.is_some_and(|c| c.hdr);
        let sdr_white = output_config
            .and_then(|c| c.sdr_white)
            .unwrap_or(hdr_output::DEFAULT_SDR_WHITE);
        let base = OutputHdr {
            requested,
            sdr_white,
            ..OutputHdr::default()
        };
        let Some((_, lit)) = self.outputs.iter().find(|(_, lit)| lit.output == *output) else {
            return OutputHdr {
                reason: requested.then(|| "the monitor is not lit".to_string()),
                ..base
            };
        };
        let scanout = lit.scanning_out;
        let (Some(_), Some(plan)) = (&lit.hdr, lit.hdr_plan) else {
            let reason = if !requested {
                None
            } else if let Status::Sdr(reason) = &lit.hdr_status {
                Some(reason.to_string())
            } else {
                Some("HDR is not set up yet".to_string())
            };
            return OutputHdr {
                reason,
                scanout,
                ..base
            };
        };
        let scanout_reason = if scanout {
            None
        } else if let Some(reason) = lit.hdr_scanout.reason {
            Some(reason.to_string())
        } else if lit.hdr_scanout.declined.is_some() {
            Some("the buffer did not reach the primary plane".to_string())
        } else {
            Some(hdr_scanout::Decline::NotCovering.to_string())
        };
        OutputHdr {
            active: true,
            eotf: Some(
                match plan.eotf {
                    OutputEotf::Pq => "PQ",
                    OutputEotf::Hlg => "HLG",
                }
                .to_string(),
            ),
            peak: Some(u32::from(plan.max_luminance)),
            peak_source: Some(
                match plan.peak_source {
                    hdr::PeakSource::Edid => "edid",
                    hdr::PeakSource::Override => "hdr-peak",
                    hdr::PeakSource::Assumed => "assumed",
                }
                .to_string(),
            ),
            bits: Some(link_bits(lit.compositor.format()).min(plan.max_bpc.unwrap_or(u32::MAX))),
            night_light: lit.night_gain != [1.0; 3],
            max_cll: lit.hdr_content.0,
            max_fall: lit.hdr_content.1,
            scanout,
            scanout_reason,
            ..base
        }
    }

    /// Whether variable refresh rate is on for `output` right now. False if it isn't lit.
    pub fn adaptive_sync(&self, output: &Output) -> bool {
        self.outputs
            .values()
            .any(|lit| lit.output == *output && lit.vrr_on)
    }

    /// Whether the monitor and driver can do variable refresh rate on `output`. `None` if it
    /// isn't lit: that is only known once there is a compositor for it.
    pub fn vrr_capable(&self, output: &Output) -> Option<bool> {
        self.outputs
            .values()
            .find(|lit| lit.output == *output)
            .map(|lit| !matches!(lit.vrr_support, VrrSupport::NotSupported))
    }

    /// Connected monitors as of the last scan, lit or not.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// Offer the non-desktop connectors as leases. Runs once the Wayland display exists.
    pub fn init_leasing(&mut self, display: &DisplayHandle) {
        let node = match DrmNode::from_file(self.primary.drm.device_fd()) {
            Ok(node) => node,
            Err(err) => {
                tracing::warn!("no DRM leasing, cannot identify the GPU: {err}");
                return;
            }
        };
        // A lease is direct access to the display: not for sandboxed apps.
        let lease_state = DrmLeaseState::new_with_filter::<JffState, _>(display, &node, |client| {
            !crate::state::is_sandboxed(client)
        });
        let mut state = match lease_state {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!("no DRM leasing: {err}");
                return;
            }
        };
        // headsets found by the first scan, before there was a global to tell
        for &conn in self.non_desktop.keys() {
            if let Ok(info) = self.primary.drm.get_connector(conn, false) {
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
        if !self.primary.drm.is_active() {
            return Err(LeaseRejected::default());
        }
        let mut builder = DrmLeaseBuilder::new(&self.primary.drm);
        for &conn in connectors {
            let Some(&crtc) = self.non_desktop.get(&conn) else {
                tracing::warn!("lease request for {conn:?}, which is not a non-desktop connector");
                return Err(LeaseRejected::default());
            };
            builder.add_connector(conn);
            builder.add_crtc(crtc);
            let planes = self
                .primary
                .drm
                .planes(&crtc)
                .map_err(LeaseRejected::with_cause)?;
            let (primary, claim) = planes
                .primary
                .iter()
                .find_map(|plane| {
                    self.primary
                        .drm
                        .claim_plane(plane.handle, crtc)
                        .map(|claim| (plane, claim))
                })
                .ok_or_else(LeaseRejected::default)?;
            builder.add_plane(primary.handle, claim);
            if let Some((cursor, claim)) = planes.cursor.iter().find_map(|plane| {
                self.primary
                    .drm
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
            let Ok(info) = self.primary.drm.get_connector(conn, false) else {
                continue;
            };
            let Some(crtc) = pick_crtc(&self.primary.drm, resources, &info, used) else {
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
    /// unplugged or disabled ones, redo those whose mode changed. Goes over every GPU. Returns
    /// (removed, added).
    fn reconcile(&mut self, config: &Config) -> anyhow::Result<(Vec<Output>, Vec<Output>)> {
        let devices: Vec<DeviceId> = std::iter::once(DeviceId::PRIMARY)
            .chain(
                (1..=self.secondary.len())
                    .filter(|&n| self.secondary[n - 1].is_some())
                    .map(DeviceId),
            )
            .collect();
        let mut monitors = Vec::new();
        // Connector names are numbered per GPU, so two GPUs can both have an `HDMI-A-1`.
        let mut names = HashSet::new();
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for dev in devices {
            match self.reconcile_device(dev, config, &mut monitors, &mut names) {
                Ok((gone, new)) => {
                    removed.extend(gone);
                    added.extend(new);
                }
                // without the primary there is no session, a secondary only loses its monitors
                Err(err) if dev == DeviceId::PRIMARY => return Err(err),
                Err(err) => tracing::warn!("GPU {}: error scanning monitors: {err:#}", dev.0),
            }
        }
        self.monitors = monitors;
        Ok((removed, added))
    }

    /// [`Tty::reconcile`] for the monitors of GPU `dev`. `monitors` and `names` collect what all
    /// the GPUs have.
    fn reconcile_device(
        &mut self,
        dev: DeviceId,
        config: &Config,
        monitors: &mut Vec<Monitor>,
        names: &mut HashSet<String>,
    ) -> anyhow::Result<(Vec<Output>, Vec<Output>)> {
        let mut used: HashSet<crtc::Handle> = HashSet::new();
        let mut candidates = Vec::new();
        let mut leasable: Vec<(connector::Handle, String)> = Vec::new();
        let device = self.device(dev).context("the GPU is gone")?;
        let resources = device
            .drm
            .resource_handles()
            .context("error reading DRM resources")?;
        for &conn in resources.connectors() {
            let Ok(info) = device.drm.get_connector(conn, true) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            // A VR headset is no monitor: it is offered as a lease instead, on the primary
            // only. On another GPU it is left alone.
            if is_non_desktop(&device.drm, conn) {
                if dev == DeviceId::PRIMARY {
                    leasable.push((conn, connector_name(&info)));
                }
                continue;
            }
            if info.modes().is_empty() {
                continue;
            }
            let card = device
                .path
                .file_name()
                .and_then(|file| file.to_str())
                .unwrap_or("gpu");
            let plain = connector_name(&info);
            let Some(name) = unique_name(names, &plain, card) else {
                tracing::warn!(
                    "GPU {}: monitor {plain} is left dark, its name is taken and so is {card}-{plain}",
                    dev.0
                );
                continue;
            };
            if name != plain {
                tracing::info!("GPU {}: monitor {plain} is called {name}", dev.0);
            }
            let identity = read_identity(&device.drm, conn);
            monitors.push(monitor_of(&info, name.clone(), identity));
            let wanted = config.output(&name);
            if wanted.is_some_and(|c| c.off) {
                continue;
            }
            let Some(mode) = pick_mode(&info, wanted.and_then(|c| c.mode)) else {
                continue;
            };
            let hdr_wanted = wanted.is_some_and(|c| c.hdr);
            match self
                .outputs
                .iter()
                .find(|(key, o)| key.0 == dev && o.connector == conn)
            {
                // a swapchain made without the 10-bit formats would band under PQ, so `hdr` turned on by a reload lights the monitor again
                // a monitor whose name changed (another GPU took it, or let it go) is lit again
                Some((&(_, crtc), lit))
                    if lit.mode == mode
                        && (lit.hdr_formats || !hdr_wanted)
                        && lit.output.name() == name =>
                {
                    used.insert(crtc);
                }
                _ => candidates.push((info, name, mode)),
            }
        }

        // A new `transform` needs no new mode: the monitor keeps showing, turned the other
        // way from the next frame on. The output's compositor takes the turn from the output,
        // and starts its buffers over, since what was drawn is the wrong way round now.
        for (_, lit) in self.outputs.iter_mut().filter(|(key, _)| key.0 == dev) {
            let wanted = output_transform(config, &lit.output.name());
            if lit.output.current_transform() != wanted {
                tracing::info!(
                    "monitor {}: transform {}",
                    lit.output.name(),
                    OutputTransform::from_smithay(wanted).name()
                );
                lit.output
                    .change_current_state(None, Some(wanted), None, None);
                lit.compositor.reset_buffers();
                lit.queue_redraw();
            }
            // A new `scale` needs no new mode either: the picture is drawn again at the new
            // scale, and `relayout_outputs` (after this) fits windows and panels to the new size.
            let scale = wanted_scale(config, &lit.output.name());
            if lit.output.current_scale().fractional_scale() != scale {
                tracing::info!("monitor {}: scale {scale}", lit.output.name());
                lit.output.change_current_state(
                    None,
                    None,
                    Some(OutputScale::Fractional(scale)),
                    None,
                );
                lit.queue_redraw();
            }
        }

        let stale: Vec<OutputKey> = self
            .outputs
            .keys()
            .filter(|(d, crtc)| *d == dev && !used.contains(crtc))
            .copied()
            .collect();
        let removed: Vec<Output> = stale
            .into_iter()
            .filter_map(|key| self.outputs.remove(&key))
            .map(|lit| lit.output)
            .collect();

        // Before lighting new monitors, so a headset's CRTC isn't taken from under it. Leasing
        // is the primary's: this would withdraw its headsets when run for another GPU.
        if dev == DeviceId::PRIMARY {
            self.sync_leasable(&resources, leasable, &mut used);
        }

        let mut added = Vec::new();
        for (info, name, mode) in candidates {
            let crtc = self
                .device(dev)
                .and_then(|device| pick_crtc(&device.drm, &resources, &info, &used));
            let Some(crtc) = crtc else {
                tracing::warn!("no free CRTC for monitor {name}");
                continue;
            };
            let hdr_wanted = config.output(&name).is_some_and(|c| c.hdr);
            let transform = output_transform(config, &name);
            let scale = wanted_scale(config, &name);
            match self.light_up(dev, &info, &name, crtc, mode, transform, scale, hdr_wanted) {
                Ok(lit) => {
                    used.insert(crtc);
                    added.push(lit.output.clone());
                    self.outputs.insert((dev, crtc), lit);
                }
                Err(err) => tracing::warn!("could not use monitor {name}: {err:#}"),
            }
        }
        Ok((removed, added))
    }

    /// Create the output and scanout pipeline for a monitor.
    fn light_up(
        &mut self,
        dev: DeviceId,
        info: &connector::Info,
        name: &str,
        crtc: crtc::Handle,
        drm_mode: DrmMode,
        transform: Transform,
        scale: f64,
        hdr_wanted: bool,
    ) -> anyhow::Result<TtyOutput> {
        let primary_node = self.primary.render_node;
        let Tty {
            renderer,
            primary,
            secondary,
            ..
        } = &mut *self;
        let device = pick_device_mut(primary, secondary, dev).context("the GPU is gone")?;
        let via_primary = dev != DeviceId::PRIMARY;
        tracing::info!(
            "monitor {name}: {}x{}@{}",
            drm_mode.size().0,
            drm_mode.size().1,
            drm_mode.vrefresh()
        );
        if via_primary {
            tracing::info!("monitor {name}: on a secondary GPU, the primary draws its frames");
        }

        let (pw, ph) = info.size().unwrap_or((0, 0));
        // What clients and output profiles (kanshi) match a monitor on.
        let identity = read_identity(&device.drm, info.handle());
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (pw as i32, ph as i32).into(),
                subpixel: Subpixel::Unknown,
                make: identity.make().into(),
                model: identity.model().into(),
                serial_number: identity.serial().into(),
            },
        );
        let wl_mode = Mode::from(drm_mode.clone());
        output.change_current_state(
            Some(wl_mode),
            Some(transform),
            Some(OutputScale::Fractional(scale)),
            Some((0, 0).into()),
        );
        output.set_preferred(wl_mode);

        let surface = device
            .drm
            .create_surface(crtc, drm_mode.clone(), &[info.handle()])
            .context("error creating the DRM surface")?;
        let allocator = GbmAllocator::new(
            device.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        // What the DrmCompositor may render into is what the renderer that draws for it can do:
        // the secondary's own for its monitors.
        let formats_of: &GlesRenderer = if via_primary {
            device
                .renderer
                .as_ref()
                .context("the secondary GPU has no renderer")?
        } else {
            &*renderer
        };
        let render_formats = formats_of
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
            GbmFramebufferExporter::new(device.gbm.clone(), NodeFilter::All),
            if hdr_wanted {
                COLOR_FORMATS_HDR.iter().copied()
            } else {
                COLOR_FORMATS.iter().copied()
            },
            render_formats,
            device.drm.cursor_size(),
            Some(device.gbm.clone()),
        )
        .context("error creating the DRM compositor")?;

        // Only asks what's supported, `render_crtc` decides every frame whether VRR is on. A
        // monitor on a secondary GPU shows a copy of the frame and `render_secondary_crtc` never
        // switches VRR or the content type, so `adaptive-sync` and `content-type-hint` do nothing
        // there and it says it can't do VRR.
        let vrr_support = if via_primary {
            VrrSupport::NotSupported
        } else {
            compositor
                .vrr_supported(info.handle())
                .unwrap_or(VrrSupport::NotSupported)
        };
        tracing::info!("monitor {name}: variable refresh rate {vrr_support:?}");

        // Clients draw for the primary, a scanout tranche of the secondary's would be wrong.
        let dmabuf_feedback = if via_primary {
            render_only_feedback(renderer, primary_node)
        } else {
            build_dmabuf_feedback(&compositor, renderer, &device.drm, device.render_node)
        };
        if dmabuf_feedback.is_none() && !via_primary {
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
            night_gain: [1.0; 3],
            hdr_fault: None,
            hdr_scanout: HdrScanout::default(),
            hdr_formats: hdr_wanted,
            content_hint: false,
            content_told: None,
            compositor,
            output,
            connector: info.handle(),
            mode: drm_mode,
            via_primary,
            cross: None,
            redraw: RedrawState::Idle,
            last_vblank: None,
            gamma: None,
            client_gamma: None,
            gamma_pending: false,
            gamma_applied: false,
            powered_off: false,
            locked_frame: false,
        })
    }

    /// Open the GPUs on the seat that are not in use yet: one that was plugged in, or could not
    /// be opened before. Their monitors are lit by the next [`Tty::reconcile`]. Says whether
    /// there is a new one.
    fn open_new_gpus(&mut self) -> bool {
        let Ok(all) = udev::all_gpus(self.session.seat()) else {
            return false;
        };
        let mut added = false;
        for path in all {
            if self.primary.path == path || self.secondary.iter().flatten().any(|d| d.path == path)
            {
                continue;
            }
            let (mut device, notifier, renderer) = match open_gpu(&mut self.session, &path) {
                Ok(opened) => opened,
                Err(err) => {
                    tracing::warn!("GPU {path:?} is not used, it could not be opened: {err:#}");
                    continue;
                }
            };
            let dev = DeviceId(self.secondary.len() + 1);
            match self
                .loop_handle
                .insert_source(notifier, move |event, meta, state| {
                    on_drm_event(state, dev, event, meta)
                }) {
                Ok(token) => {
                    device.renderer = Some(renderer);
                    device.events = Some(token);
                    self.secondary.push(Some(device));
                    tracing::info!("GPU {path:?} added");
                    added = true;
                }
                Err(err) => tracing::warn!(
                    "GPU {path:?} is not used, no DRM event source: {}",
                    err.error
                ),
            }
        }
        added
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
            primary,
            secondary,
            outputs,
            renderer,
            ..
        } = &mut *self;
        for (&(dev, _), lit) in outputs.iter_mut() {
            let Some(device) = pick_device(primary, secondary, dev) else {
                continue;
            };
            let name = lit.output.name();
            lit.hdr_fault = None;
            let edid = read_edid(&device.drm, lit.connector);
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
            let sdr_transfer = output_config
                .and_then(|c| c.sdr_transfer)
                .unwrap_or_default();
            let black_level = output_config
                .and_then(|c| c.hdr_black_level)
                .map_or(hdr_output::DEFAULT_BLACK_LEVEL, |m| m as f32 / 1000.0);
            let saturation = output_config
                .and_then(|c| c.hdr_saturation)
                .map_or(hdr_output::DEFAULT_SATURATION, |m| m as f32 / 1000.0);
            let white_balance = output_config
                .and_then(|c| c.hdr_white_balance)
                .map_or(hdr_output::DEFAULT_WHITE_BALANCE, |m| {
                    m.map(|g| g as f32 / 1000.0)
                });
            let contrast = output_config
                .and_then(|c| c.hdr_contrast)
                .map_or(hdr_output::DEFAULT_CONTRAST, |m| m as f32 / 1000.0);
            let content_metadata = output_config
                .and_then(|c| c.hdr_content_metadata)
                .unwrap_or(true);
            let gamut_map = output_config
                .and_then(|c| c.hdr_gamut_map)
                .unwrap_or(hdr_output::DEFAULT_GAMUT_MAP);
            let min_override = output_config.and_then(|c| c.hdr_min_luminance);
            let mut hdr = None;
            let mut hdr_plan = None;
            let mut target = ConnectorColorState::default();
            let status = if !output_config.is_some_and(|c| c.hdr) {
                Status::Off
            } else {
                // The frame of a monitor on another GPU crosses in a 10-bit format, which both GPUs have to take.
                let crossable = dev == DeviceId::PRIMARY
                    || secondary_renderer(secondary, dev)
                        .is_some_and(|other| cross_gpu::hdr_crossable(renderer, other));
                match hdr_target(
                    &lit.compositor,
                    lit.connector,
                    lit.edid.as_ref(),
                    peak_override,
                    min_override,
                ) {
                    Ok(_) if !hdr_frame::supported(renderer) => {
                        Status::Sdr(Unavailable::NoFloatTarget)
                    }
                    Ok(_) if !crossable => Status::Sdr(Unavailable::NoCrossFormat),
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
                        .with_black_level(black_level)
                        .with_gamut_map(gamut_map)
                        .with_min_luminance(f32::from(plan.min_luminance) / 10_000.0)
                        .with_white_balance(white_balance)
                        .with_night(lit.night_gain)
                        .with_saturation(saturation)
                        .with_contrast(contrast)
                        .with_tone_map(tone)
                        .with_eotf(plan.eotf)
                        .with_dither(Some(
                            link_bits(lit.compositor.format())
                                .min(plan.max_bpc.unwrap_or(u32::MAX)),
                        ));
                        hdr = Some((
                            WorkingSpace {
                                sdr_white,
                                sdr_transfer,
                            },
                            pass,
                            content_metadata,
                        ));
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
        // the night light tints the screen, not what is saved
        Some((
            hdr.space,
            hdr.pass
                .with_night([1.0; 3])
                .with_content_peak(hdr.content_peak),
        ))
    }

    /// What the frame of `output` is drawn with while it is HDR: the working space, the final pass (without the content peak, that comes from the frame) and whether the pass is fitted to the content. For a monitor on another GPU, whose frame the primary draws. `None` while it is SDR.
    pub fn hdr_settings(&self, output: &Output) -> Option<(WorkingSpace, FinalPass, bool)> {
        let lit = self.outputs.values().find(|lit| lit.output == *output)?;
        let hdr = lit.hdr.as_ref()?;
        Some((hdr.space, hdr.pass, hdr.content_metadata))
    }

    /// Remember the content peak the last frame of `output` was fitted to, where screenshots and `jff msg` read it.
    pub fn set_hdr_content_peak(&mut self, output: &Output, peak: Option<f32>) {
        let hdr = self
            .outputs
            .values_mut()
            .find(|lit| lit.output == *output)
            .and_then(|lit| lit.hdr.as_mut());
        if let Some(hdr) = hdr {
            hdr.content_peak = peak;
        }
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
        let Tty {
            primary,
            secondary,
            outputs,
            ..
        } = &mut *self;
        for (&(dev, crtc), lit) in outputs.iter_mut() {
            let Some(device) = pick_device(primary, secondary, dev) else {
                continue;
            };
            if !lit.gamma_pending || lit.waiting_for_vblank() || lit.last_vblank.is_none() {
                continue;
            }
            lit.gamma_pending = false;
            let curves = lit.client_gamma.as_ref().or(lit.gamma.as_ref());
            // The table works on the signal the CRTC scans out. An HDR output sends PQ (or HLG), where curves made for sRGB would bend the whole picture, so none is set and one that is there is cleared. A client's curves (night light) are made light gains of the final pass instead, see `set_client_gamma`.
            if lit.hdr.is_some() && lit.client_gamma.is_none() && curves.is_some() {
                tracing::warn!(
                    "monitor {}: calibration curves are not applied while it is in HDR mode",
                    lit.output.name()
                );
            }
            let wanted = curves.filter(|_| lit.hdr.is_none());
            if wanted.is_none() && !lit.gamma_applied {
                continue;
            }
            match set_gamma(&device.drm, crtc, wanted) {
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
    fn queue_estimated_vblank(&mut self, dev: DeviceId, crtc: crtc::Handle) {
        let Some(lit) = self.outputs.get_mut(&(dev, crtc)) else {
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
            state.on_estimated_vblank(dev, crtc);
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

/// The primary GPU, the one that composes everything: the one `render-device` names, else the
/// primary one (the one the machine booted on), else the first there is.
fn choose_gpu(
    all: &[std::path::PathBuf],
    seat: &str,
    wanted: Option<&str>,
) -> anyhow::Result<std::path::PathBuf> {
    let mut chosen = None;
    if let Some(wanted) = wanted {
        let wanted_real = std::fs::canonicalize(wanted).ok();
        chosen = all
            .iter()
            .find(|path| {
                path.as_path() == std::path::Path::new(wanted)
                    || (wanted_real.is_some() && std::fs::canonicalize(path).ok() == wanted_real)
            })
            .cloned();
        if chosen.is_none() {
            tracing::warn!(
                "render-device {wanted:?} is not one of the GPUs on this seat ({all:?}), using the default"
            );
        }
    }
    chosen
        .or_else(|| udev::primary_gpu(seat).ok().flatten())
        .or_else(|| all.first().cloned())
        .context("no GPU found")
}

/// The GPUs on the seat other than the primary. They only scan out for their own monitors.
fn other_gpus(all: &[std::path::PathBuf], primary: &std::path::Path) -> Vec<std::path::PathBuf> {
    all.iter()
        .filter(|path| path.as_path() != primary)
        .cloned()
        .collect()
}

/// Open a GPU: the DRM and GBM devices and a renderer on them, plus the source of its DRM
/// events. The renderer is returned apart: the primary's lives in `Tty::renderer`, a
/// secondary's goes into its `GpuDevice`.
fn open_gpu(
    session: &mut LibSeatSession,
    path: &std::path::Path,
) -> anyhow::Result<(GpuDevice, DrmDeviceNotifier, GlesRenderer)> {
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| anyhow!("could not open {path:?}: {e:?}"))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, notifier) =
        DrmDevice::new(device_fd.clone(), false).context("error opening the DRM device")?;
    let gbm = GbmDevice::new(device_fd.device_fd()).context("error creating the GBM device")?;

    let display = unsafe { EGLDisplay::new(gbm.clone()) }.context("error creating EGL display")?;
    let egl_device = EGLDevice::device_for_display(&display).context("no EGL device")?;
    let render_node = egl_device.try_get_render_node().ok().flatten();
    let context = EGLContext::new(&display).context("error creating the EGL context")?;
    let renderer = unsafe { GlesRenderer::new(context) }.context("error creating the renderer")?;
    let device = GpuDevice {
        renderer: None,
        render_node,
        drm,
        gbm,
        path: path.to_path_buf(),
        events: None,
    };
    Ok((device, notifier, renderer))
}

/// One DRM event of the device `dev`.
fn on_drm_event(
    state: &mut JffState,
    dev: DeviceId,
    event: DrmEvent,
    meta: &mut Option<DrmEventMetadata>,
) {
    match event {
        DrmEvent::VBlank(crtc) => state.on_vblank(dev, crtc, meta.take()),
        DrmEvent::Error(err) => tracing::warn!("DRM error: {err}"),
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

    // ---- GPUs ----
    // The primary composes everything. The others are opened to scan out their monitors, and
    // one that fails to open is left out: it must not take the session down.
    let all = udev::all_gpus(&seat).context("error listing the GPUs")?;
    let gpu_path = choose_gpu(&all, &seat, config.render_device())?;
    let (primary, drm_notifier, renderer) = open_gpu(&mut session, &gpu_path)?;
    let mut opened = Vec::new();
    for path in other_gpus(&all, &gpu_path) {
        match open_gpu(&mut session, &path) {
            Ok((mut device, notifier, renderer)) => {
                device.renderer = Some(renderer);
                opened.push((path, device, notifier));
            }
            Err(err) => {
                tracing::warn!("GPU {path:?} is not used, it could not be opened: {err:#}")
            }
        }
    }

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
        .insert_source(input_backend, |event, _, state| {
            // Pointer devices get their `input` settings when they appear.
            match &event {
                InputEvent::DeviceAdded { device } => state.input_device_added(device.clone()),
                InputEvent::DeviceRemoved { device } => state.input_device_removed(device),
                _ => {}
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
        .insert_source(drm_notifier, |event, meta, state| {
            on_drm_event(state, DeviceId::PRIMARY, event, meta)
        })
        .map_err(|e| anyhow!("failed to insert the DRM source: {}", e.error))?;
    // One source per device: a CRTC is only known by its device's events.
    let mut secondary = Vec::new();
    for (path, mut device, notifier) in opened {
        let dev = DeviceId(secondary.len() + 1);
        match handle.insert_source(notifier, move |event, meta, state| {
            on_drm_event(state, dev, event, meta)
        }) {
            Ok(token) => {
                device.events = Some(token);
                secondary.push((path, device));
            }
            Err(err) => tracing::warn!(
                "GPU {path:?} is not used, no DRM event source: {}",
                err.error
            ),
        }
    }
    if secondary.is_empty() {
        tracing::info!("using GPU {gpu_path:?}");
    } else {
        let names: Vec<&std::path::Path> =
            secondary.iter().map(|(path, _)| path.as_path()).collect();
        tracing::info!("using GPU {gpu_path:?} (primary), also open: {names:?}");
    }
    let secondary: Vec<Option<GpuDevice>> = secondary
        .into_iter()
        .map(|(_, device)| Some(device))
        .collect();
    let udev_backend = udev::UdevBackend::new(&seat).context("error creating the udev backend")?;
    handle
        .insert_source(udev_backend, |event, _, state| match event {
            udev::UdevEvent::Added { .. } => state.on_gpu_added(),
            udev::UdevEvent::Removed { device_id } => state.on_gpu_removed(device_id),
            _ => state.scan_tty_outputs(),
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
        secondary,
        primary,
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
        let keys: Vec<OutputKey> = match &self.backend {
            Backend::Tty(tty) => tty
                .outputs
                .iter()
                .filter(|(_, lit)| lit.redraw_is_queued())
                .map(|(&key, _)| key)
                .collect(),
            _ => return,
        };
        for (dev, crtc) in keys {
            self.render_crtc(dev, crtc);
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
        let (&(dev, crtc), _) = tty.outputs.iter().find(|(_, lit)| lit.output == *output)?;
        crtc_gamma_property(&tty.device(dev)?.drm, crtc).map(|(_, size)| size as u32)
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
        // the same curves on an HDR output are light gains of its final pass
        lit.night_gain = table.as_ref().map_or([1.0; 3], |t| {
            let top = |curve: &[u16]| curve.last().copied().unwrap_or(u16::MAX);
            hdr_output::night_gains([top(&t.red), top(&t.green), top(&t.blue)])
        });
        let night = lit.night_gain;
        if let Some(hdr) = lit.hdr.as_mut() {
            hdr.pass = hdr.pass.with_night(night);
            lit.queue_redraw();
        }
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
        let Tty {
            outputs,
            primary,
            secondary,
            loop_handle,
            ..
        } = &mut *tty;
        let Some((&(dev, _), lit)) = outputs.iter_mut().find(|(_, lit)| lit.output == *output)
        else {
            return false;
        };
        let Some(drm) = pick_device(primary, secondary, dev).map(|d| &d.drm) else {
            return false;
        };
        if !drm.is_active() {
            return false;
        }
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
    fn render_crtc(&mut self, dev: DeviceId, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let active = tty.device(dev).is_some_and(|d| d.drm.is_active());
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
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
        if lit.via_primary {
            self.render_secondary_crtc(dev, crtc);
            return;
        }
        // Where the cost of this frame is counted from (stats for nerds).
        let cost_from = std::time::Instant::now();
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
            primary,
            secondary,
            loop_handle,
            ..
        } = &mut *tty;
        let Some(drm) = pick_device(primary, secondary, dev).map(|d| &d.drm) else {
            return;
        };
        let Some(lit) = outputs.get_mut(&(dev, crtc)) else {
            return;
        };

        // VRR follows the config, for `fullscreen`, whether a fullscreen window is on top. A
        // switch that needs a modeset blanks the screen, so only `always` does one.
        // wp_content_type_v1: what does the fullscreen window on top say it shows? Only checked
        // when the config uses it.
        let says_content = if lit.content_hint || lit.content_told.is_some() {
            crate::protocol::layer_shell::fullscreen_content(&self.desk, output_rect(&lit.output))
        } else {
            None
        };
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

        // connector `content type` follows what the fullscreen window on top shows: Photo,
        // Cinema or Game. A failure is logged once, not retried every frame.
        let want_content = if lit.content_hint { says_content } else { None };
        if want_content != lit.content_told {
            lit.content_told = want_content;
            match set_connector_content_type(drm, lit.connector, want_content) {
                Ok(()) => tracing::info!(
                    "monitor {}: content type {}",
                    lit.output.name(),
                    want_content.map_or("no data", Content::name)
                ),
                Err(err) => tracing::warn!(
                    "monitor {}: cannot set the content type: {err:#}",
                    lit.output.name()
                ),
            }
        }

        // Everything is placed in logical pixels, the output's tracker scales it to the panel.
        let output_scale = lit.output.current_scale().fractional_scale();
        let scale = Scale::from(output_scale);
        let _frame_scale = crate::render::effects::frame_scale::enter(output_scale);
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
        // The stats overlay follows the window the user works in, on the monitor that has it.
        let nerd_window = if self.stats.active() {
            crate::feature::stats::followed_window(self.focused_window.as_ref(), &self.desk, rect)
        } else {
            None
        };
        if let Some((surface, _)) = &nerd_window {
            self.stats
                .follow(surface.id().protocol_id(), &lit.output.name());
        }
        // Drawn over the window, so the frame has to be composited: no direct scanout while it
        // is on screen. The overlay says so.
        let nerd_overlay = nerd_window.is_some()
            && self.stats.drawn()
            && !locked
            && frozen.is_none()
            && !overview
                .as_ref()
                .is_some_and(|plan| plan.output == lit.output);
        let mut nerd_covering = false;
        let mut nerd_overview = false;
        let mut nerd_decline: Option<String> = None;
        let mut nerd_alt: Option<bool> = None;

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
            nerd_covering = covering;
            nerd_overview = overview_here.is_some();
            direct_scanout = hdr_space.is_none() && covering && !nerd_overlay;
            if nerd_overlay && hdr_space.is_none() {
                // without the overlay the frame would be tried on the plane
                nerd_alt = Some(covering);
            }
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
                        } else if nerd_overlay {
                            Some(Over::StatsOverlay)
                        } else {
                            None
                        },
                        // A night light tint is drawn by the final pass, which the plane skips.
                        effects: !crate::render::effects::yuv::corrections(&surface).is_empty()
                            || !crate::render::effects::pixel_alpha::modes(&surface).is_empty()
                            || lit.night_gain != [1.0; 3],
                        tag,
                        buffer_format,
                        swapchain_format: lit.compositor.format() as u32,
                        signalled: current == lit.compositor.pending_color_state()
                            && current == lit.color_target,
                    };
                    if nerd_overlay {
                        let without = Facts {
                            over: facts.over.filter(|over| *over != Over::StatsOverlay),
                            ..facts
                        };
                        nerd_alt = Some(hdr_scanout::decide(&without).is_ok());
                    }
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
                if nerd_window.is_some() {
                    nerd_decline = Some(reason.to_string());
                }
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
            if let Some(plan) = overview_here {
                elements.extend(overview_ripple_elements(&mut *renderer, plan));
            }
            let above = layers_above(&fullscreen, rect, overview_here.is_some());
            elements.extend(layer_elements(
                &mut *renderer,
                &lit.output,
                above,
                blur,
                self.config.blur_layers(),
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
                    self.config.blur_layers(),
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
                        (if animate {
                            crate::feature::window_anim::open_frame(
                                open_style,
                                entry.shown,
                                now,
                                open_time,
                            )
                        } else {
                            crate::feature::window_anim::OpenFrame::DONE
                        })
                        .with_opacity(entry.opacity),
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
                    self.config.blur_layers(),
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
        // the stats overlay, above the toasts
        if nerd_overlay {
            let inset = match &nerd_window {
                Some((_, win)) => crate::feature::stats::overlay_inset(&lit.output, win, rect),
                None => (0, 0, rect.2),
            };
            let fonts = self.osd.fonts();
            if let Some(element) = self.stats.element(&mut *renderer, fonts, inset) {
                elements.splice(osd_at..osd_at, [element]);
            }
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
        } else if hdr_space.is_none() {
            // A composited frame on an SDR monitor. Elements may go to the primary, overlay
            // and cursor planes. The cursor on its own plane means moving the pointer neither
            // redraws the desktop nor puts the cursor in the frame. The cursor elements are
            // plain surface or memory elements drawn outside the colour-conversion scope, so
            // the plane shows what the renderer would have, and the CRTC gamma (night light)
            // applies to every plane. Screencopy and screenshots build their own frame and are
            // not affected. If a plane cannot take an element, the compositor draws it into the
            // frame as before.
            FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT
                | FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT
                | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
        } else {
            // An HDR monitor: the frame is composited into its own texture and encoded by the
            // final pass, which a cursor on a plane would skip.
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

        let nerd_async = matches!(presentation, PresentationMode::Async);

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
        // How long the CPU waited for the GPU to finish the frame, if it did.
        let mut gpu_wait = None;
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
                            let waited = std::time::Instant::now();
                            if let Err(err) = element.sync.wait() {
                                tracing::warn!("error waiting for frame completion: {err:?}");
                            }
                            gpu_wait = Some(waited.elapsed());
                        }
                    }
                    // Nothing new to show while locked: the screen already is the lock.
                    if is_empty && locked && !declined {
                        self.session_lock.presented_locked(&lit.output);
                    }
                    if !is_empty {
                        if let Some(dmabuf_feedback) = &lit.dmabuf_feedback {
                            send_surface_feedback(
                                &self.desk,
                                &lit.output,
                                hdr_states.as_ref().unwrap_or(&res.states),
                                Some(dmabuf_feedback),
                                scanout,
                            );
                        }
                        let feedback = crate::protocol::presentation::take_feedback(
                            &self.desk,
                            &lit.output,
                            hdr_states.as_ref().unwrap_or(&res.states),
                        );
                        match lit.compositor.queue_frame(feedback) {
                            Ok(()) => {
                                queued = true;
                                lit.locked_frame = locked;
                            }
                            Err(err) => tracing::warn!("error queueing frame: {err}"),
                        }
                    }
                }
                Err(err) => tracing::warn!("error rendering frame: {err}"),
            }
        }

        // Stats for nerds: what this frame did, as the overlay and `jff msg stats` show it. The
        // numbers are rebuilt a few times a second, not per frame.
        if let Some((surface, win)) = &nerd_window {
            if queued {
                let name = lit.output.name();
                // The vblank this frame was drawn for. Under VRR or tearing the flip does not
                // wait for one, and after an idle spell the frame clock is not to be trusted.
                let refresh = lit.refresh();
                let predicted = if lit.vrr_on || nerd_async {
                    None
                } else {
                    nerd::countable_prediction(lit.last_vblank, presented_at, refresh)
                };
                self.stats.note_queued(&name, predicted, refresh);
                let spent = cost_from.elapsed();
                self.stats.note_cost(
                    &name,
                    crate::protocol::presentation::monotonic_now(),
                    nerd::FrameCost {
                        cpu: spent.saturating_sub(gpu_wait.unwrap_or_default()),
                        gpu_wait,
                    },
                );
            }
            if self.stats.due(std::time::Instant::now()) {
                let path = if lit.scanning_out {
                    nerd::Path::Direct
                } else {
                    nerd::Path::Composited(if locked {
                        nerd::Why::Locked
                    } else if frozen.is_some() {
                        nerd::Why::Screenshot
                    } else if nerd_overview {
                        nerd::Why::Overview
                    } else if !win.fullscreen {
                        nerd::Why::NotFullscreen
                    } else if !nerd_covering {
                        nerd::Why::NotCovering
                    } else if nerd_overlay {
                        nerd::Why::StatsOverlay
                    } else if let Some(text) = nerd_decline.take() {
                        nerd::Why::Hdr(text)
                    } else if layers_shown(&lit.output, layers_above(&fullscreen, rect, false)) {
                        nerd::Why::LayerOver
                    } else if !input_popups.is_empty() {
                        nerd::Why::PopupOver
                    } else {
                        nerd::Why::PlaneRefused
                    })
                };
                let snapshot =
                    stats_snapshot(surface, win, lit, &self.config, path, nerd_alt, nerd_async);
                self.stats.store(
                    snapshot,
                    std::time::Instant::now(),
                    crate::protocol::presentation::monotonic_now(),
                );
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
            tty.queue_estimated_vblank(dev, crtc);
            if retry {
                if let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) {
                    lit.queue_redraw();
                }
            }
        }

        if queued {
            // The clients' buffers were latched by this frame, so the callbacks can go out now.
            // A new cycle starts: each surface gets one callback per cycle, which keeps a client
            // that commits without damage from spinning.
            advance_frame_sequence(&output);
            self.image_copy.output_drawn(&output);
            self.wlr_screencopy.output_drawn(&output);
            self.send_frame_callbacks(&output);
        } else {
            self.release_fifo_barriers(&output);
        }
    }

    /// Draw one frame to a monitor on a secondary GPU: the primary draws it, the secondary shows it.
    fn render_secondary_crtc(&mut self, dev: DeviceId, crtc: crtc::Handle) {
        // Where the cost of this frame is counted from (stats for nerds).
        let cost_from = std::time::Instant::now();
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
            return;
        };
        let presented_at = lit.next_presentation_time();
        let output = lit.output.clone();
        self.release_commit_timers(&output, presented_at);
        // the primary draws the monitor's frame into a buffer the secondary can read
        self.update_cross_frame(dev, crtc, &output);
        let stats_window = if self.stats.active() {
            crate::feature::stats::followed_window(
                self.focused_window.as_ref(),
                &self.desk,
                output_rect(&output),
            )
        } else {
            None
        };

        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        tty.apply_gamma();
        let Tty {
            outputs,
            secondary,
            loop_handle,
            ..
        } = &mut *tty;
        let Some(lit) = outputs.get_mut(&(dev, crtc)) else {
            return;
        };
        let Some(renderer) = secondary_renderer(secondary, dev) else {
            tracing::warn!("monitor {}: its GPU has no renderer", output.name());
            lit.reset_redraw(loop_handle);
            return;
        };

        // The signalling goes out with the next frame, as on the primary: HDR when the monitor
        // is, and after a VT switch `reset_state` read back what the other VT left on the
        // connector, so SDR again.
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
                lit.hdr_status = Status::Sdr(Unavailable::SignalRejected);
                let _ = lit.compositor.use_color_state(lit.color_target);
            }
        }

        // The frame that came across, or a plain color until the first one does. An unchanged
        // frame is the same texture, so the compositor finds nothing new to show.
        let element: CrossElement = match lit.cross.as_ref().and_then(|c| c.shown.as_ref()) {
            Some(buffer) => TextureRenderElement::from_texture_render_buffer(
                (0.0, 0.0),
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .into(),
            None => {
                let (width, height) = lit.mode.size();
                let size = output
                    .current_transform()
                    .transform_size(Size::<i32, Physical>::from((width as i32, height as i32)));
                SolidColorRenderElement::new(
                    Id::new(),
                    Rectangle::<i32, Physical>::new((0, 0).into(), size),
                    CommitCounter::default(),
                    SECONDARY_PLACEHOLDER,
                    Kind::Unspecified,
                )
                .into()
            }
        };

        let locked = self.session_lock.is_locked();
        let mut queued = false;
        match lit.compositor.render_frame(
            &mut *renderer,
            &[element],
            SECONDARY_PLACEHOLDER,
            FrameFlags::empty(),
            PresentationMode::VSync,
        ) {
            Ok(res) => {
                if res.needs_sync() {
                    if let PrimaryPlaneElement::Swapchain(element) = res.primary_element {
                        if let Err(err) = element.sync.wait() {
                            tracing::warn!("error waiting for frame completion: {err:?}");
                        }
                    }
                }
                if res.is_empty {
                    // nothing of the desktop is on this monitor yet: a lock is already in place
                    if locked {
                        self.session_lock.presented_locked(&lit.output);
                    }
                } else {
                    // what the primary drew says which surfaces were shown, the secondary only has the one texture
                    let states = lit
                        .cross
                        .as_ref()
                        .and_then(|c| c.states.as_ref())
                        .unwrap_or(&res.states);
                    // the monitor a surface is shown on, which color management and callbacks go by
                    send_surface_feedback(
                        &self.desk,
                        &lit.output,
                        states,
                        lit.dmabuf_feedback.as_ref(),
                        false,
                    );
                    let feedback = crate::protocol::presentation::take_feedback(
                        &self.desk,
                        &lit.output,
                        states,
                    );
                    match lit.compositor.queue_frame(feedback) {
                        Ok(()) => {
                            queued = true;
                            lit.locked_frame = locked;
                        }
                        Err(err) => tracing::warn!("error queueing frame: {err}"),
                    }
                }
            }
            Err(err) => tracing::warn!("error rendering frame: {err}"),
        }

        // Stats for nerds, as `render_crtc` keeps them. A monitor on another GPU is always
        // composited, and the overlay says why.
        if let Some((surface, win)) = &stats_window {
            if queued {
                let name = output.name();
                let refresh = lit.refresh();
                let predicted = nerd::countable_prediction(lit.last_vblank, presented_at, refresh);
                self.stats.note_queued(&name, predicted, refresh);
                self.stats.note_cost(
                    &name,
                    crate::protocol::presentation::monotonic_now(),
                    nerd::FrameCost {
                        cpu: cost_from.elapsed(),
                        gpu_wait: None,
                    },
                );
            }
            if self.stats.due(std::time::Instant::now()) {
                let snapshot = stats_snapshot(
                    surface,
                    win,
                    lit,
                    &self.config,
                    nerd::Path::Composited(nerd::Why::OtherGpu),
                    None,
                    false,
                );
                self.stats.store(
                    snapshot,
                    std::time::Instant::now(),
                    crate::protocol::presentation::monotonic_now(),
                );
            }
        }

        if queued {
            if let RedrawState::WaitingForEstimatedVBlankAndQueued { token } = lit.redraw {
                loop_handle.remove(token);
            }
            lit.redraw = RedrawState::WaitingForVBlank {
                redraw_needed: false,
            };
            advance_frame_sequence(&output);
            self.image_copy.output_drawn(&output);
            self.wlr_screencopy.output_drawn(&output);
            self.send_frame_callbacks(&output);
        } else {
            // no vblank is coming: the timer stands in for it, as on the primary
            tty.queue_estimated_vblank(dev, crtc);
            self.release_fifo_barriers(&output);
        }
    }

    /// The timer that stands in for a vblank went off (see [`RedrawState::WaitingForEstimatedVBlank`]).
    fn on_estimated_vblank(&mut self, dev: DeviceId, crtc: crtc::Handle) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
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
            if let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) {
                if ongoing {
                    lit.queue_redraw();
                }
            }
        }
    }

    /// The display showed the frame queued on `crtc`.
    fn on_vblank(&mut self, dev: DeviceId, crtc: crtc::Handle, meta: Option<DrmEventMetadata>) {
        // The frame that just reached the screen says which monitor each surface is on: a window
        // that moved to a monitor with other colors learns its new preferred description.
        crate::protocol::color_management::refresh_feedbacks(self);
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
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
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
            return;
        };
        // kernel flip timestamp, if it gave one on the monotonic clock
        let hw_time = match meta.map(|m| m.time) {
            Some(DrmEventTime::Monotonic(time)) if !time.is_zero() => Some(time),
            _ => None,
        };
        // Whether this flip came off the vblank grid, which a tearing flip does. Told from the
        // kernel's stamps of this flip and the one before (`nerd::flip_off_grid`), so not with
        // VRR, whose flips come when the frames do, and not without a stamp.
        let off_grid = match (hw_time, lit.last_vblank) {
            (Some(now), Some(previous)) if !lit.vrr_on => {
                crate::protocol::presentation::refresh_interval(&lit.output)
                    .and_then(|refresh| nerd::flip_off_grid(previous, now, refresh))
            }
            _ => None,
        };
        lit.last_vblank =
            Some(hw_time.unwrap_or_else(crate::protocol::presentation::monotonic_now));
        if self.stats.active() {
            self.stats.note_flip(
                &lit.output.name(),
                lit.last_vblank.unwrap_or_default(),
                off_grid,
            );
        }
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
        if std::mem::take(&mut lit.locked_frame) {
            self.session_lock.presented_locked(&output);
        }
        // the frame is on screen, so clients waiting on a fifo barrier may commit again
        self.release_fifo_barriers(&output);

        // Draw again right away when a commit or an animation asked for it. Otherwise the
        // monitor goes idle: nothing is drawn until something asks for it. The drawing itself
        // happens at the end of this event loop pass, see `redraw_queued_outputs`.
        let ongoing = self.animations_ongoing();
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) else {
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

    /// A DRM node appeared: open the GPU if it is one, and light its monitors.
    fn on_gpu_added(&mut self) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        if tty.open_new_gpus() {
            self.scan_tty_outputs();
        }
    }

    /// A DRM node went away. If it was a secondary GPU, its monitors go first (clients are told
    /// they are gone), then the GPU is closed. The primary cannot be replaced.
    fn on_gpu_removed(&mut self, device_id: libc::dev_t) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        let is = |device: &GpuDevice| device.drm.device_fd().dev_id().ok() == Some(device_id);
        let Some(slot) = tty
            .secondary
            .iter()
            .position(|device| device.as_ref().is_some_and(is))
        else {
            if is(&tty.primary) {
                // nothing can be drawn without it, and every frame would only fail
                tracing::error!("the primary GPU was removed, jff cannot draw any more: stopping");
                self.loop_signal.stop();
            }
            return;
        };
        let dev = DeviceId(slot + 1);
        let keys: Vec<OutputKey> = tty
            .outputs
            .keys()
            .filter(|key| key.0 == dev)
            .copied()
            .collect();
        let mut lits: Vec<TtyOutput> = keys
            .iter()
            .filter_map(|key| tty.outputs.remove(key))
            .collect();
        for lit in &mut lits {
            lit.reset_redraw(&tty.loop_handle);
        }
        let outputs: Vec<Output> = lits.iter().map(|lit| lit.output.clone()).collect();
        for output in outputs {
            tracing::info!("monitor {} removed", output.name());
            self.remove_output(&output);
        }
        // the compositors hold the GPU's buffers, they go before it
        drop(lits);
        if let Backend::Tty(tty) = &mut self.backend {
            if let Some(token) = tty.secondary[slot].as_ref().and_then(|d| d.events) {
                tty.loop_handle.remove(token);
            }
            tty.secondary[slot] = None;
            tracing::info!("GPU removed");
        }
        // forgets the monitors of the GPU and tells output management
        self.scan_tty_outputs();
    }

    /// Rescan for plugged, unplugged or reconfigured monitors and update the outputs.
    pub fn scan_tty_outputs(&mut self) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        // on another VT, the scan happens when the session comes back
        if !tty.primary.drm.is_active() {
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
                tty.primary.drm.pause();
                for device in tty.secondary.iter_mut().flatten() {
                    device.drm.pause();
                }
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
                if let Err(err) = tty.primary.drm.activate(false) {
                    tracing::warn!("error activating the DRM device: {err:?}");
                }
                for device in tty.secondary.iter_mut().flatten() {
                    if let Err(err) = device.drm.activate(false) {
                        tracing::warn!("error activating a secondary DRM device: {err:?}");
                    }
                }
                // a GPU that appeared while another VT was in front could not be opened then
                tty.open_new_gpus();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_gpu_keeps_the_connector_name() {
        let mut names = HashSet::new();
        assert_eq!(
            unique_name(&mut names, "DP-1", "card0").as_deref(),
            Some("DP-1")
        );
        assert_eq!(
            unique_name(&mut names, "DP-1", "card1").as_deref(),
            Some("card1-DP-1")
        );
    }

    #[test]
    fn a_name_that_is_taken_twice_is_given_up() {
        let mut names = HashSet::new();
        names.insert("DP-1".to_string());
        names.insert("card1-DP-1".to_string());
        assert_eq!(unique_name(&mut names, "DP-1", "card1"), None);
    }

    #[test]
    fn different_connectors_do_not_clash() {
        let mut names = HashSet::new();
        assert!(unique_name(&mut names, "DP-1", "card0").is_some());
        assert_eq!(
            unique_name(&mut names, "HDMI-A-1", "card1").as_deref(),
            Some("HDMI-A-1")
        );
    }
}
