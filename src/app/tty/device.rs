//! The `Tty` backend's state and what it does with its GPUs, connectors, leases and HDR.

use super::*;

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
    pub(super) fn device(&self, id: DeviceId) -> Option<&GpuDevice> {
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
    pub(super) fn sync_leasable(
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
    pub(super) fn reconcile(
        &mut self,
        config: &Config,
    ) -> anyhow::Result<(Vec<Output>, Vec<Output>)> {
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
    pub(super) fn reconcile_device(
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
    pub(super) fn light_up(
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
    pub(super) fn open_new_gpus(&mut self) -> bool {
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
    pub(super) fn sync_gamma(&mut self, config: &Config) {
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
    pub(super) fn sync_hdr(&mut self, config: &Config) {
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
    pub(super) fn apply_gamma(&mut self) {
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
    pub(super) fn queue_estimated_vblank(&mut self, dev: DeviceId, crtc: crtc::Handle) {
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
