//! Things that happen to the session: vblanks, GPUs coming and going, VT switches.

use super::*;

impl JffState {
    /// The timer that stands in for a vblank went off (see [`RedrawState::WaitingForEstimatedVBlank`]).
    pub(super) fn on_estimated_vblank(&mut self, dev: DeviceId, crtc: crtc::Handle) {
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
    pub(super) fn on_vblank(
        &mut self,
        dev: DeviceId,
        crtc: crtc::Handle,
        meta: Option<DrmEventMetadata>,
    ) {
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
    pub(super) fn on_gpu_added(&mut self) {
        let Backend::Tty(tty) = &mut self.backend else {
            return;
        };
        if tty.open_new_gpus() {
            self.scan_tty_outputs();
        }
    }

    /// A DRM node went away. If it was a secondary GPU, its monitors go first (clients are told
    /// they are gone), then the GPU is closed. The primary cannot be replaced.
    pub(super) fn on_gpu_removed(&mut self, device_id: libc::dev_t) {
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
    pub(super) fn on_session_event(&mut self, event: SessionEvent) {
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
