//! Drawing one frame for one CRTC: on the primary GPU, or on a secondary one the primary
//! renders for.

use super::*;

impl JffState {
    /// Draw one frame to the monitor on `crtc` and queue it for the next vblank. Does nothing
    /// unless a frame was asked for (`Queued`, or `WaitingForEstimatedVBlankAndQueued`).
    pub(super) fn render_crtc(&mut self, dev: DeviceId, crtc: crtc::Handle) {
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
        // Needs &mut self too: the part of the monitor the zoom shows follows the pointer.
        let zoom = self.zoom_plan(&output, anim_now);

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
        // While zoomed, a pointer kept at its size is drawn where the zoom puts it on the screen,
        // on top of the zoomed frame. Otherwise it is drawn with the rest of the scene.
        let cursor_zoomed = zoom
            .as_ref()
            .filter(|plan| plan.pointer == PointerStyle::Keep && lit.hdr.is_none());
        let cursor_at: Point<f64, Logical> = match cursor_zoomed {
            Some(plan) => {
                let (x, y) = plan
                    .view
                    .to_screen((pointer.x - f64::from(rect.0), pointer.y - f64::from(rect.1)));
                (x + f64::from(rect.0), y + f64::from(rect.1)).into()
            }
            None => pointer,
        };
        // The themed cursor of this frame, kept for an HDR monitor, see `hdr_cursor_plane`.
        let mut named_cursor: Option<(
            std::rc::Rc<crate::render::cursor::Cursor>,
            u32,
            Point<f64, Physical>,
        )> = None;
        match self.cursor.render_cursor() {
            RenderCursor::Hidden => {}
            _ if !cursor_near => {}
            _ if zoom
                .as_ref()
                .is_some_and(|plan| plan.pointer == PointerStyle::Hide) => {}
            // client-drawn cursor, placed at the pointer minus hotspot
            RenderCursor::Surface { hotspot, surface } => {
                let loc: Point<i32, Physical> =
                    (cursor_at.to_i32_round() - hotspot - origin).to_physical_precise_round(scale);
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
                    (cursor_at - origin.to_f64() - frame.hotspot.to_f64()).to_physical(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    &mut *renderer,
                    loc,
                    &frame.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => {
                        elements.push(element.into());
                        named_cursor = Some((cursor.clone(), millis, loc));
                    }
                    Err(err) => tracing::warn!("error importing the cursor image: {err:?}"),
                }
            }
        }
        // Those are the cursor's elements, kept out of the zoomed scene when it is drawn on top.
        let cursor_len = elements.len();
        // On an HDR monitor the scene is coded for the signal by a final pass, which a plane
        // can't be put through. A themed cursor is coded on its own instead (`encode_cursor`)
        // and goes in front of the coded frame, so it can take the cursor plane: moving the
        // pointer then redraws nothing. Not while zoomed, the cursor is part of the enlarged
        // picture then. Where the plane can't take it the compositor draws it into the frame
        // as before, and that is right too: the frame it draws onto is coded already.
        let hdr_cursor_plane =
            lit.hdr.is_some() && zoom.is_none() && cursor_len == 1 && named_cursor.is_some();
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
        // A maximized window drawn flush that fills the monitor: its buffer may take the primary
        // plane, but the frame is built as usual (toasts, panels and popups stay).
        let mut flush_scanout = false;
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
            nerd_overview = overview_here.is_some();
            // A zoomed frame is drawn from a texture of its own, so a window can't go to the plane.
            direct_scanout = hdr_space.is_none() && covering && !nerd_overlay && zoom.is_none();
            // `ALLOW_PRIMARY_PLANE_SCANOUT` takes only a buffer in the swapchain's format, and
            // an opaque client buffer is exported as XRGB, which never matches the ARGB
            // swapchain. A maximized window that fills the monitor needs `_ANY` like a
            // fullscreen one, or it is never scanned out.
            let flush_cover = overview_here.is_none() && flush_covers(&self.desk, rect);
            flush_scanout = hdr_space.is_none()
                && !direct_scanout
                && flush_cover
                && !nerd_overlay
                && zoom.is_none();
            // The overlay's verdict counts a maximized window that fills the monitor too.
            nerd_covering = covering || flush_cover;
            if nerd_overlay && hdr_space.is_none() {
                // without the overlay the frame would be tried on the plane
                nerd_alt = Some(nerd_covering);
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
                            || lit.night_gain != [1.0; 3]
                            || zoom.is_some(),
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
                let mut backdrop = crate::feature::containers::Backdrop::of_focused(
                    &self.config,
                    &self.desk,
                    self.focused_window.as_ref(),
                );
                for entry in self.desk.top_down() {
                    let geo = entry.window.geometry();
                    let drawn = crate::feature::containers::drawn_pos(&self.desk, entry);
                    if let Some(backdrop) = backdrop.as_mut() {
                        if backdrop.hides(entry, drawn, (geo.size.w, geo.size.h)) {
                            continue;
                        }
                    }
                    let window_origin: Point<i32, Logical> = Point::from(drawn) - geo.loc;
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
                    let is_covering = (drawn.0, drawn.1, size.w, size.h) == rect;
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
                // The wallpaper copy sits between the bottom and background layers.
                elements.extend(below_windows_elements(
                    &mut *renderer,
                    &lit.output,
                    blur,
                    self.config.blur_layers(),
                    snapshot.as_ref().map(|s| (s, (rect.2, rect.3).into())),
                    !(blur.is_some() && overview_here.is_some_and(|p| p.draws_bottom_layer())),
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
        } else if flush_scanout {
            // A maximized window filling an SDR monitor: its buffer may go to the primary plane
            // in any format, a toast or panel above it may take an overlay plane, and the
            // cursor its own. When one of them cannot, the compositor draws the frame.
            FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY
                | FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT
                | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
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
            // final pass, which a cursor on a plane would skip. A cursor that is coded on its
            // own (`hdr_cursor_plane`) may take the plane, nothing else may.
            if hdr_cursor_plane {
                FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
            } else {
                FrameFlags::empty()
            }
        };

        let cursor_plane_allowed = frame_flags.contains(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT);

        // Tearing: a fullscreen window covering the monitor that asked for it. The compositor
        // falls back to a vsync flip when the frame can't be flipped async.
        let presentation = if scanout
            && self.config.allow_tearing()
            && crate::protocol::layer_shell::fullscreen_wants_tearing(
                &self.desk,
                rect,
                self.config.force_tearing(),
            )
        {
            PresentationMode::Async
        } else {
            PresentationMode::VSync
        };

        let nerd_async = matches!(presentation, PresentationMode::Async);

        // HDR: composite in the working space, then hand the output's compositor the one
        // element that encodes the result for the signal. The states of what was composited in
        // a texture first (this, or the zoomed frame below) are the ones clients hear about.
        let mut hdr_states = None;
        let mut hdr_failed = false;
        if let Some(hdr) = lit.hdr.as_mut().filter(|_| hdr_scanout_try.is_none()) {
            let space = hdr.space;
            let pass = hdr.pass;
            // The cursor leaves the scene, it is added again below, coded.
            let plane_cursor = if hdr_cursor_plane && !elements.is_empty() {
                elements.remove(0);
                named_cursor.take()
            } else {
                None
            };
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
            // zoomed: the final pass draws the part around the pointer over the whole monitor
            if let Some(frame) = hdr.frame.as_mut() {
                frame.set_view(zoom.as_ref().map(|plan| plan.src(output_scale)));
            }
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
                    if let Some((cursor, millis, loc)) = plane_cursor {
                        let picture = cursor.frame(millis);
                        let buffer = hdr.cursor.buffer(
                            picture.serial,
                            &picture.pixels,
                            picture.size,
                            space,
                            &pass,
                        );
                        match MemoryRenderBufferRenderElement::from_buffer(
                            &mut *renderer,
                            loc,
                            buffer,
                            None,
                            None,
                            None,
                            Kind::Cursor,
                        ) {
                            // in front of the frame
                            Ok(element) => elements.insert(0, element.into()),
                            Err(err) => {
                                tracing::warn!("error importing the HDR cursor image: {err:?}")
                            }
                        }
                    }
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

        // Zoomed, on a monitor that is not HDR: the scene goes into a texture of its own and the
        // part around the pointer is drawn enlarged over the whole monitor. A pointer kept at its
        // size stays out of the texture and goes on top.
        if let (Some(plan), None) = (zoom.as_ref(), lit.hdr.as_ref()) {
            let on_top = if cursor_zoomed.is_some() {
                cursor_len
            } else {
                0
            };
            let scene = elements.split_off(on_top);
            let clear = if locked {
                LOCKED_BACKGROUND
            } else {
                BACKGROUND
            };
            match self
                .zoom
                .magnify(&mut *renderer, &lit.output, &scene, clear, plan)
            {
                Some((element, states)) => {
                    elements.push(JffElement::Magnified(element));
                    hdr_states = Some(states);
                }
                None => elements.extend(scene),
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
        // Whether the cursor went to the cursor plane in the frame just built, `None` when no
        // frame was built.
        let mut cursor_on_plane: Option<bool> = None;
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
                    cursor_on_plane = Some(res.cursor_element.is_some());
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
                                lit.async_flip = nerd_async;
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
                // fullscreen, or maximized and drawn flush: either may go to the plane
                let nerd_flush = self
                    .focused_window
                    .as_ref()
                    .is_some_and(crate::render::effects::rounded::is_flush);
                let path = if lit.scanning_out {
                    nerd::Path::Direct
                } else {
                    nerd::Path::Composited(if locked {
                        nerd::Why::Locked
                    } else if frozen.is_some() {
                        nerd::Why::Screenshot
                    } else if nerd_overview {
                        nerd::Why::Overview
                    } else if !(win.fullscreen || nerd_flush) {
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
                let cursor = cursor_on_plane.map(|on_plane| {
                    if !cursor_near {
                        nerd::Cursor::Elsewhere
                    } else if cursor_len == 0 {
                        nerd::Cursor::Hidden
                    } else if on_plane {
                        nerd::Cursor::Hardware
                    } else {
                        nerd::Cursor::Software(if lit.hdr.is_some() && !cursor_plane_allowed {
                            nerd::CursorWhy::Hdr
                        } else if zoom.is_some() && cursor_zoomed.is_none() {
                            nerd::CursorWhy::Zoomed
                        } else {
                            nerd::CursorWhy::PlaneRefused
                        })
                    }
                });
                let snapshot = stats_snapshot(
                    surface,
                    win,
                    lit,
                    &self.config,
                    path,
                    nerd_alt,
                    nerd_async,
                    cursor,
                );
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
    pub(super) fn render_secondary_crtc(&mut self, dev: DeviceId, crtc: crtc::Handle) {
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
        // The primary draws the cursor into the frame that crosses over, when it is on this monitor.
        let cursor_near = self.seat.get_pointer().is_some_and(|pointer| {
            let at = pointer.current_location();
            layout::contains(
                layout::grow(output_rect(&output), CURSOR_MARGIN),
                (at.x, at.y),
            )
        });
        let cursor_shown =
            cursor_near && !matches!(self.cursor.render_cursor(), RenderCursor::Hidden);

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
                            lit.async_flip = false;
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
                    Some(if cursor_shown {
                        nerd::Cursor::Software(nerd::CursorWhy::OtherGpu)
                    } else if cursor_near {
                        nerd::Cursor::Hidden
                    } else {
                        nerd::Cursor::Elsewhere
                    }),
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
}
