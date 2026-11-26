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
mod device;
mod events;
mod render;
mod startup;

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
use crate::primitives::zoom::PointerStyle;
use crate::protocol::color_management::WorkingSpace;
use crate::protocol::content_type::Content;
use crate::protocol::layer_shell::{
    below_windows_elements, flush_covers, fullscreen_covers, fullscreen_surface, layer_elements,
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

pub use self::startup::init;
use self::startup::*;

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
    cursor: Option<nerd::Cursor>,
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
        cursor,
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
    /// The cursor pictures coded for this output, they go to the cursor plane as they are.
    cursor: hdr_frame::CursorCache,
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
    /// The frame in flight was queued as an async (tearing) flip, so its presentation is not
    /// vsync'd. Set when the frame is queued, read at its vblank.
    async_flip: bool,
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
