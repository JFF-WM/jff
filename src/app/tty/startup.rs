//! Starting the TTY backend: picking and opening the GPUs, wiring up their events.

use super::*;

/// The primary GPU, the one that composes everything: the one `render-device` names, else the
/// primary one (the one the machine booted on), else the first there is.
pub(super) fn choose_gpu(
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
pub(super) fn other_gpus(
    all: &[std::path::PathBuf],
    primary: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    all.iter()
        .filter(|path| path.as_path() != primary)
        .cloned()
        .collect()
}

/// Open a GPU: the DRM and GBM devices and a renderer on them, plus the source of its DRM
/// events. The renderer is returned apart: the primary's lives in `Tty::renderer`, a
/// secondary's goes into its `GpuDevice`.
pub(super) fn open_gpu(
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
pub(super) fn on_drm_event(
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
