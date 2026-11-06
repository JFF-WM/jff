//! TTY backend: jff as the session compositor on bare DRM/KMS. Session and VT switching go
//! through libseat (needs seatd or logind), input through libinput, hotplug through udev.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, VrrSupport,
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
use smithay::reexports::calloop::LoopHandle;
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
use smithay::wayland::compositor::SurfaceData;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder};
use smithay::wayland::presentation::Refresh;

use crate::app::{Backend, BACKGROUND};
use crate::config::{Config, ModeRequest, Vrr};
use crate::desk::Desk;
use crate::feature::outputs::{output_rect, Monitor, MonitorMode};
use crate::feature::overview_mode::{overview_drag_elements, overview_elements};
use crate::feature::screenshot::screenshot_elements;
use crate::primitives::layout;
use crate::protocol::layer_shell::{
    fullscreen_covers, layer_body_elements, layer_elements, layer_popup_elements, layers_above,
    BELOW_WINDOWS,
};
use crate::protocol::session_lock::LOCKED_BACKGROUND;
use crate::render::cursor::RenderCursor;
use crate::render::effects::background_effect::window_elements;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::elements::JffElement;
use crate::state::JffState;

const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Abgr8888];

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

/// A lit monitor and its frame loop state.
struct TtyOutput {
    compositor: GbmDrmCompositor,
    output: Output,
    connector: connector::Handle,
    mode: DrmMode,
    /// A frame is queued and its vblank hasn't arrived yet.
    waiting_for_vblank: bool,
    /// A retry timer is already scheduled.
    retry_pending: bool,
    /// CLOCK_MONOTONIC time of the last vblank. The idle timer follows this grid.
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
    /// `gamma` hasn't reached the display yet (see [`Tty::apply_gamma`]).
    gamma_pending: bool,
    /// The display's gamma table was written, so clear it if the profile goes away.
    gamma_applied: bool,
    /// Feedback carrying this monitor's scanout tranche (see [`build_dmabuf_feedback`]).
    dmabuf_feedback: Option<SurfaceDmabufFeedback>,
    /// Last frame had a client buffer on the primary plane (shown by `jff msg watch`).
    scanning_out: bool,
    /// A fullscreen window covered the monitor last frame. Only used to log changes.
    covering: bool,
}

/// Time until the next expected vblank, counting on from the last one seen.
///
/// A fixed delay isn't a multiple of the refresh interval, so after a vblank with no client
/// frame the next check lands at a random phase and frames arrive unevenly (micro stutter).
fn next_vblank_delay(last: Option<Duration>, refresh: Duration) -> Duration {
    let refresh_ns = refresh.as_nanos().max(1);
    let Some(last) = last else {
        return refresh;
    };
    let now_ns = crate::protocol::presentation::monotonic_now().as_nanos();
    let last_ns = last.as_nanos();
    let periods = now_ns.saturating_sub(last_ns) / refresh_ns + 1;
    let next_ns = last_ns + periods * refresh_ns;
    let delay_ns = next_ns.saturating_sub(now_ns).max(500_000);
    Duration::from_nanos(delay_ns as u64)
}

pub struct Tty {
    // Field order is drop order: everything that uses the GPU goes before the devices.
    pub renderer: GlesRenderer,
    outputs: HashMap<crtc::Handle, TtyOutput>,
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

/// Set the CRTC gamma table, or clear it with `None`. The hardware applies it at scanout, so
/// it affects everything on the monitor.
fn set_gamma(
    drm: &DrmDevice,
    crtc: crtc::Handle,
    table: Option<&crate::feature::icc::GammaTable>,
) -> anyhow::Result<()> {
    let props = drm
        .get_properties(crtc)
        .context("error reading the CRTC properties")?;
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
    let Some(lut_property) = lut_property.filter(|_| lut_size > 1) else {
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

    /// Connected monitors as of the last scan, lit or not.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
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
        for &conn in resources.connectors() {
            let Ok(info) = self.drm.get_connector(conn, true) else {
                continue;
            };
            if info.state() != connector::State::Connected || info.modes().is_empty() {
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
            match self.outputs.iter().find(|(_, o)| o.connector == conn) {
                Some((&crtc, lit)) if lit.mode == mode => {
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

        let mut added = Vec::new();
        for (info, name, mode) in candidates {
            let Some(crtc) = pick_crtc(&self.drm, &resources, &info, &used) else {
                tracing::warn!("no free CRTC for monitor {name}");
                continue;
            };
            match self.light_up(&info, &name, crtc, mode) {
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
            GbmFramebufferExporter::new(self.gbm.clone(), self.render_node.into()),
            COLOR_FORMATS.iter().copied(),
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
            content_hint: false,
            content_game: false,
            compositor,
            output,
            connector: info.handle(),
            mode: drm_mode,
            waiting_for_vblank: false,
            retry_pending: false,
            last_vblank: None,
            gamma: None,
            gamma_pending: false,
            gamma_applied: false,
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

    /// Apply pending curves, only on monitors that have shown a frame and have no flip in
    /// flight. The first modeset resets the display's colour state, so this has to come after it.
    fn apply_gamma(&mut self) {
        let Tty { drm, outputs, .. } = &mut *self;
        for (&crtc, lit) in outputs.iter_mut() {
            if !lit.gamma_pending || lit.waiting_for_vblank || lit.last_vblank.is_none() {
                continue;
            }
            lit.gamma_pending = false;
            if lit.gamma.is_none() && !lit.gamma_applied {
                continue;
            }
            match set_gamma(drm, crtc, lit.gamma.as_ref()) {
                Ok(()) => lit.gamma_applied = lit.gamma.is_some(),
                Err(err) => tracing::warn!(
                    "monitor {}: cannot set the calibration curves: {err:#}",
                    lit.output.name()
                ),
            }
        }
    }

    /// Render `crtc` again after `delay`, unless a frame or retry is already pending.
    fn schedule_retry(&mut self, crtc: crtc::Handle, delay: Duration) {
        let Some(lit) = self.outputs.get_mut(&crtc) else {
            return;
        };
        if lit.retry_pending || lit.waiting_for_vblank {
            return;
        }
        lit.retry_pending = true;
        let timer = Timer::from_duration(delay);
        let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
            if let Backend::Tty(tty) = &mut state.backend {
                if let Some(lit) = tty.outputs.get_mut(&crtc) {
                    lit.retry_pending = false;
                }
            }
            state.render_crtc(crtc);
            TimeoutAction::Drop
        });
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
    Ok((Backend::Tty(tty), outputs))
}

impl JffState {
    /// Draw a frame on every monitor that is ready for one.
    pub fn render_tty(&mut self) {
        let crtcs: Vec<crtc::Handle> = match &self.backend {
            Backend::Tty(tty) => tty.outputs.keys().copied().collect(),
            _ => return,
        };
        for crtc in crtcs {
            self.render_crtc(crtc);
        }
    }

    /// Draw one frame to the monitor on `crtc` and queue it for the next vblank.
    fn render_crtc(&mut self, crtc: crtc::Handle) {
        self.refresh_window_outputs();
        // Needs &mut self, so it has to happen before the backend is borrowed. May end the overview.
        let overview = self.overview_plan();
        let fullscreen = self.fullscreen_rects();

        // Timed commits due by the next vblank (when this frame shows up) belong in this frame.
        if let Backend::Tty(tty) = &self.backend {
            if tty.drm.is_active() {
                if let Some(lit) = tty.outputs.get(&crtc).filter(|l| !l.waiting_for_vblank) {
                    let refresh = crate::protocol::presentation::refresh_interval(&lit.output)
                        .unwrap_or(Duration::from_nanos(16_666_667));
                    let target = crate::protocol::presentation::monotonic_now()
                        + next_vblank_delay(lit.last_vblank, refresh);
                    let output = lit.output.clone();
                    self.release_commit_timers(&output, target);
                }
            }
        }

        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        // on another VT, nothing to draw
        if !tty.drm.is_active() {
            return;
        }
        let Tty {
            renderer,
            outputs,
            drm,
            ..
        } = &mut *tty;
        let Some(lit) = outputs.get_mut(&crtc) else {
            return;
        };
        if lit.waiting_for_vblank {
            return; // the vblank handler will draw the next one
        }

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
        // while an area screenshot is open here, the frozen frame replaces layers and windows
        // (cursor stays on top)
        let frozen = self.screenshot.as_ref().filter(|ui| ui.shows(&lit.output));
        // A fullscreen window exactly the size of the monitor hides everything behind it. The
        // frame then holds only that window (plus cursor and overlay layer), so DrmCompositor
        // can show the client's buffer on the primary plane without a copy.
        let mut direct_scanout = false;
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
            direct_scanout = overview_here.is_none() && fullscreen_covers(&self.desk, rect);
            // Blur is opt-in, and never with a window going straight to the primary plane: the
            // blur needs the frame composited.
            let blur = (!direct_scanout && !self.config.blur().off).then(|| self.config.blur());
            let above = layers_above(&fullscreen, rect, overview_here.is_some());
            elements.extend(layer_elements(&mut *renderer, &lit.output, above, blur));
            // popups of layers below the windows draw above them, except over a fullscreen
            // window: only the overlay layer is in front of that
            if !direct_scanout {
                elements.extend(layer_popup_elements(
                    &mut *renderer,
                    &lit.output,
                    &BELOW_WINDOWS,
                    blur,
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
                    // Popups and the window, each with its blur right behind it.
                    window_elements(
                        &mut *renderer,
                        &entry.window,
                        window_origin - origin,
                        blur,
                        snapshot.as_ref(),
                        radius,
                        &mut elements,
                    );
                    // nothing below the covering window goes in the frame
                    let size = entry.window.geometry().size;
                    if direct_scanout && (entry.pos.0, entry.pos.1, size.w, size.h) == rect {
                        break;
                    }
                }
            }
            // Wallpapers and desktop widgets are behind everything.
            if !direct_scanout {
                // Fills the wallpaper copy: drawn right after the background layers.
                if let Some(snapshot) = &snapshot {
                    elements.push(JffElement::from(snapshot.element((rect.2, rect.3).into())));
                }
                elements.extend(layer_body_elements(
                    &mut *renderer,
                    &lit.output,
                    &BELOW_WINDOWS,
                    blur,
                ));
            }
        }

        // Plane scanout is off by default. When a fullscreen window covers the monitor, allow
        // its buffer on the primary plane whatever the element kind, and move the cursor to its
        // own plane, or it would sit on top of the window and block that.
        if direct_scanout != lit.covering {
            lit.covering = direct_scanout;
            tracing::debug!(
                "monitor {}: a fullscreen window {} the monitor",
                lit.output.name(),
                if direct_scanout {
                    "now covers"
                } else {
                    "no longer covers"
                },
            );
        }
        let frame_flags = if direct_scanout {
            FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
        } else {
            FrameFlags::empty()
        };

        let mut queued = false;
        match lit.compositor.render_frame(
            &mut *renderer,
            &elements,
            if locked {
                LOCKED_BACKGROUND
            } else {
                BACKGROUND
            },
            frame_flags,
        ) {
            Ok(res) => {
                let is_empty = res.is_empty;
                // debug log for direct scanout changes
                let on_plane = matches!(res.primary_element, PrimaryPlaneElement::Element(_));
                if on_plane != lit.scanning_out {
                    lit.scanning_out = on_plane;
                    tracing::debug!(
                        "monitor {}: primary plane now shows {} (fullscreen cover: {direct_scanout})",
                        lit.output.name(),
                        if on_plane {
                            "the client's buffer (direct scanout)"
                        } else {
                            "the composited frame"
                        },
                    );
                }
                if res.needs_sync() {
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
                            &res.states,
                            dmabuf_feedback,
                            direct_scanout,
                        );
                    }
                    let feedback = crate::protocol::presentation::take_feedback(
                        &self.desk,
                        &lit.output,
                        &res.states,
                    );
                    match lit.compositor.queue_frame(feedback) {
                        Ok(()) => queued = true,
                        Err(err) => tracing::warn!("error queueing frame: {err}"),
                    }
                }
            }
            Err(err) => tracing::warn!("error rendering frame: {err}"),
        }

        if queued {
            lit.waiting_for_vblank = true;
        }
        let retry_delay = next_vblank_delay(
            lit.last_vblank,
            crate::protocol::presentation::refresh_interval(&lit.output)
                .unwrap_or(Duration::from_nanos(16_666_667)),
        );
        let output = lit.output.clone();
        if !queued {
            // no frame queued means no vblank event, so poll on the refresh grid instead
            tty.schedule_retry(crtc, retry_delay);
        }

        self.send_frame_callbacks(&output);
        if !queued {
            // no vblank is coming to clear the barriers, and what clients committed is already
            // on screen
            self.release_fifo_barriers(&output);
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
        lit.waiting_for_vblank = false;
        // kernel flip timestamp, if it gave one on the monotonic clock
        let hw_time = match meta.map(|m| m.time) {
            Some(DrmEventTime::Monotonic(time)) if !time.is_zero() => Some(time),
            _ => None,
        };
        lit.last_vblank =
            Some(hw_time.unwrap_or_else(crate::protocol::presentation::monotonic_now));
        match lit.compositor.frame_submitted() {
            Ok(Some(mut feedback)) => {
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
            Ok(None) => {}
            Err(err) => tracing::warn!("error marking frame as submitted: {err}"),
        }
        let output = lit.output.clone();
        tty.apply_gamma();
        // the frame is on screen, so clients waiting on a fifo barrier may commit again
        self.release_fifo_barriers(&output);
        self.render_crtc(crtc);
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
        tty.apply_gamma();
        let monitors_changed = tty.monitors() != known.as_slice();
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
                for lit in tty.outputs.values_mut() {
                    lit.waiting_for_vblank = false;
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
                for lit in tty.outputs.values_mut() {
                    if let Err(err) = lit.compositor.reset_state() {
                        tracing::warn!("error resetting the DRM compositor: {err:?}");
                    }
                    lit.compositor.reset_buffers();
                    lit.waiting_for_vblank = false;
                    lit.last_vblank = None;
                    // another VT may have left its own curves: set ours (or clear them)
                    // after the first frame
                    lit.gamma_pending = true;
                    lit.gamma_applied = true;
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
