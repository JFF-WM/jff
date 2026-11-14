//! winit backend, only used when running nested during development.

use std::time::Duration;

use anyhow::anyhow;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Scale, Transform};
use smithay::wayland::compositor::SurfaceData;

use crate::feature::outputs::output_rect;
use crate::feature::overview_mode::overview_elements;
use crate::primitives::layout;
use crate::protocol::layer_shell::{
    layer_body_elements, layer_elements, layer_popup_elements, layers_above, ABOVE_WINDOWS,
    BELOW_WINDOWS,
};
use crate::render::effects::background_effect::window_elements;
use crate::render::effects::border::BorderPaint;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::elements::JffElement;
use crate::state::JffState;

use crate::config::Config;
use crate::primitives::hdr::OutputEotf;
use crate::primitives::hdr_output::{self, FinalPass};
use crate::protocol::color_management::WorkingSpace;
use crate::render::effects::hdr::{self as hdr_frame, HdrFrame};

use super::pacing::{advance_frame_sequence, frame_sequence, LastCallback};
use super::{Backend, BACKGROUND};

/// How often a window hidden behind a covering fullscreen window gets a frame callback.
const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_secs(1);

/// Peak luminance the simulated HDR output has when `hdr-peak` is not set, in cd/m².
const SIMULATED_PEAK: u32 = 1000;

/// Bits per color of the host window, what the simulation dithers for.
const WINDOW_BITS: u32 = 8;

/// What the nested HDR simulation keeps between frames.
#[derive(Default)]
pub struct WinitHdr {
    frame: Option<HdrFrame>,
    /// The content peak the last frame went out through, for a screenshot of it.
    content_peak: Option<f32>,
}

/// The HDR settings of the winit output from the config: `hdr` in `output "jff-winit"`, with the same `sdr-white`, `hdr-peak`, `hdr-tone-map` and `hdr-content-metadata` as a monitor. The panel's gamut is not known, so nothing is mapped to it. The window is an SDR surface of the host: it shows the PQ code values as they are, which is what the signal of a real HDR monitor holds.
fn hdr_settings(config: &Config, name: &str) -> Option<(WorkingSpace, FinalPass, bool)> {
    let output = config.output(name).filter(|c| c.hdr)?;
    let sdr_white = output.sdr_white.unwrap_or(hdr_output::DEFAULT_SDR_WHITE);
    let peak = output.hdr_peak.unwrap_or(SIMULATED_PEAK);
    let pass = FinalPass::new(sdr_white as f32, peak as f32, None)
        .with_tone_map(output.hdr_tone_map.unwrap_or_default())
        .with_eotf(OutputEotf::Pq)
        .with_dither(Some(WINDOW_BITS));
    Some((
        WorkingSpace { sdr_white },
        pass,
        output.hdr_content_metadata.unwrap_or(true),
    ))
}

pub fn init_winit() -> anyhow::Result<(Backend, WinitEventLoop, Output)> {
    let (backend, winit_loop) =
        winit::init::<GlesRenderer>().map_err(|e| anyhow!("failed to start winit: {e:?}"))?;

    let output = Output::new(
        "jff-winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "jff".into(),
            model: "winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    // winit's framebuffer comes out upside down.
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, None);
    output.set_preferred(mode);

    Ok((Backend::Winit(backend), winit_loop, output))
}

impl JffState {
    /// The simulated HDR settings of the winit output, `None` when it is SDR.
    fn winit_hdr_settings(&self) -> Option<(WorkingSpace, FinalPass, bool)> {
        let output = self.outputs.first()?;
        hdr_settings(&self.config, &output.name())
    }

    /// What clients are told about the winit output when it simulates HDR.
    pub fn winit_hdr_descriptions(
        &self,
    ) -> Vec<(String, crate::primitives::color::ImageDescription)> {
        let Some(output) = self.outputs.first() else {
            return Vec::new();
        };
        hdr_settings(&self.config, &output.name())
            .map(|(space, pass, _)| {
                (
                    output.name(),
                    crate::protocol::color_management::hdr_output_description(&pass, space),
                )
            })
            .into_iter()
            .collect()
    }

    /// The working space and final pass of the winit output for a screenshot, with the content peak of the last frame.
    pub fn winit_hdr_capture(&self, output: &Output) -> Option<(WorkingSpace, FinalPass)> {
        if self.outputs.first() != Some(output) {
            return None;
        }
        let (space, pass, _) = self.winit_hdr_settings()?;
        Some((space, pass.with_content_peak(self.winit_hdr.content_peak)))
    }

    pub fn start_rendering(&mut self) {
        match &self.backend {
            Backend::Winit(winit) => winit.window().request_redraw(),
            Backend::Tty(_) => {
                // the event loop has not run yet, so nothing else would draw the first frame
                self.queue_redraw_all();
                self.redraw_queued_outputs();
            }
        }
    }

    /// Ask for a frame on every monitor, because something on screen may have changed. On the
    /// TTY backend it is drawn at the end of the event loop pass, once per vblank at most, a
    /// monitor that is never asked draws nothing.
    pub fn queue_redraw_all(&mut self) {
        if self.backend.is_tty() {
            self.queue_redraw_tty();
        } else if let Backend::Winit(winit) = &self.backend {
            winit.window().request_redraw();
        }
    }

    pub fn on_winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, .. } => {
                // a frozen screenshot would no longer match the window
                self.cancel_screenshot();
                if let Some(output) = self.outputs.first() {
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        None,
                        None,
                    );
                }
                self.relayout_outputs();
            }
            WinitEvent::Redraw => self.render(),
            WinitEvent::CloseRequested => self.loop_signal.stop(),
            WinitEvent::Input(event) => self.process_input_event(event),
            _ => {}
        }
    }

    /// Draw now (winit) or ask for a frame (TTY, where drawing follows the vblank).
    pub fn render(&mut self) {
        if self.backend.is_tty() {
            self.queue_redraw_tty();
        } else {
            self.render_winit();
        }
    }

    fn render_winit(&mut self) {
        // No vblank times from the host, so assume the next refresh is one interval away.
        if let Some(output) = self.outputs.first().cloned() {
            let refresh = crate::protocol::presentation::refresh_interval(&output)
                .unwrap_or(Duration::from_nanos(16_666_667));
            self.release_commit_timers(
                &output,
                crate::protocol::presentation::monotonic_now() + refresh,
            );
        }
        // Clients of zones hear about moved members and a resized zone.
        self.zones_sync();
        self.refresh_window_outputs();
        // Needs &mut self, so it has to happen before the backend is borrowed. May end the overview.
        let overview = self.overview_plan();
        let origin: Point<i32, Logical> = self
            .outputs
            .first()
            .map(|o| {
                let rect = output_rect(o);
                (rect.0, rect.1).into()
            })
            .unwrap_or_default();
        let layer_output = self.outputs.first().cloned();
        let fullscreen = self.fullscreen_rects();
        let above = layer_output
            .as_ref()
            .map(|o| layers_above(&fullscreen, output_rect(o), overview.is_some()))
            .unwrap_or(&ABOVE_WINDOWS);
        let locked = self.session_lock.is_locked();
        // Blur is opt-in: `None` keeps every blur call below out of the frame.
        let blur = (!self.config.blur().off).then(|| self.config.blur());
        // What the monitor shows: every surface drawn below is converted to it, not just windows.
        let frame_color = layer_output.as_ref().and_then(|output| {
            crate::protocol::color_management::output_color(
                &self.config,
                &self.color_management,
                &output.name(),
            )
        });
        // A simulated HDR output composites in the working space, like a monitor in HDR mode.
        let hdr = self
            .winit_hdr_settings()
            .filter(|_| hdr_frame::supported(self.backend.renderer()));
        let frame_color = match hdr {
            Some((space, _, _)) => Some(crate::protocol::color_management::hdr_color(
                frame_color,
                space,
            )),
            None => frame_color,
        };
        let _color_scope = crate::render::effects::gamut::enter(frame_color);
        let _light_scope = crate::render::effects::gamut::collect_content_light(
            hdr.is_some_and(|(_, _, content_metadata)| content_metadata),
        );
        let Backend::Winit(winit) = &mut self.backend else {
            return;
        };
        let scale = Scale::from(1.0f64);

        // front to back: topmost first
        let mut elements: Vec<JffElement> = Vec::new();
        // Blur is of the wallpaper alone unless `behind-windows` is set: a copy of the frame
        // is taken once the background layers are drawn, and the window blurs read that.
        let snapshot = blur
            .filter(|b| !b.behind_windows)
            .zip(layer_output.as_ref())
            .map(|(_, output)| WallpaperSnapshot::for_output(&output.name()));
        {
            let renderer = winit.renderer();
            // dnd icon goes above everything
            if let Some(pointer) = self.seat.get_pointer() {
                let loc: Point<i32, Physical> = pointer
                    .current_location()
                    .to_i32_round::<i32>()
                    .to_physical_precise_round(scale);
                elements.extend(crate::feature::dnd::icon_elements(
                    self.dnd_icon.as_ref(),
                    &mut *renderer,
                    loc,
                    scale,
                ));
            }
            // input method candidates sit above windows and panels
            let input_popups = if locked {
                Vec::new()
            } else {
                crate::protocol::text_input::place_input_popups(
                    &self.text_input,
                    &self.desk,
                    &self.outputs,
                )
            };
            if let Some(rect) = layer_output.as_ref().map(output_rect) {
                elements.extend(crate::protocol::text_input::input_popup_elements(
                    &mut *renderer,
                    &input_popups,
                    rect,
                    scale,
                ));
            }
            // toasts go in here, above windows and panels
            let osd_at = elements.len();
            // while selecting a screenshot area the frozen frame replaces the live scene
            let frozen = layer_output
                .as_ref()
                .and_then(|output| self.screenshot.as_ref().filter(|ui| ui.shows(output)));
            if locked {
                // locked: lock surface only
                if let Some(output) = &layer_output {
                    elements.extend(crate::protocol::session_lock::lock_surface_elements(
                        &mut *renderer,
                        &self.session_lock,
                        output,
                        scale,
                    ));
                }
            } else if let Some(ui) = frozen {
                elements.extend(crate::feature::screenshot::screenshot_elements(
                    &mut *renderer,
                    ui,
                ));
            } else {
                if let Some(output) = &layer_output {
                    elements.extend(layer_elements(
                        &mut *renderer,
                        output,
                        above,
                        blur,
                        self.config.blur_ignore_layers(),
                        self.config.popup_radius(),
                    ));
                    // popups of layers below the windows still draw above them
                    elements.extend(layer_popup_elements(
                        &mut *renderer,
                        output,
                        &BELOW_WINDOWS,
                        blur,
                        self.config.blur_ignore_layers(),
                        self.config.popup_radius(),
                    ));
                }
                if let Some(plan) = &overview {
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
                    let color = frame_color;
                    let border = self.config.border();
                    let animate = self.config.window_animations();
                    let (open_time, close_time) = self.config.window_animation_times();
                    let open_style = self.config.window_open_style();
                    let now = std::time::Instant::now();
                    // windows that were just closed, fading out, are on top
                    crate::feature::window_anim::ghost_elements(
                        &mut *renderer,
                        &mut self.closing,
                        self.desk.current_on(
                            &self.outputs.first().map(Output::name).unwrap_or_default(),
                        ),
                        origin,
                        close_time,
                        now,
                        &mut elements,
                    );
                    for entry in self.desk.top_down() {
                        let geo = entry.window.geometry();
                        let pos: Point<i32, Logical> = Point::from(entry.pos) - geo.loc;
                        // Popups and the window, each with its blur right behind it.
                        window_elements(
                            &mut *renderer,
                            &entry.window,
                            pos,
                            self.config.blur_for_window(
                                blur,
                                &crate::feature::overview_mode::app_id_of(&entry.window),
                            ),
                            snapshot.as_ref(),
                            radius,
                            popup_radius,
                            color,
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
                    }
                }
                // Fills the wallpaper copy: drawn right after the background layers.
                if let (Some(snapshot), Some(output)) = (&snapshot, &layer_output) {
                    let rect = output_rect(output);
                    elements.push(JffElement::from(snapshot.element((rect.2, rect.3).into())));
                }
                if let Some(output) = &layer_output {
                    elements.extend(layer_body_elements(
                        &mut *renderer,
                        output,
                        &BELOW_WINDOWS,
                        blur,
                        self.config.blur_ignore_layers(),
                    ));
                }
            }
            if !locked && frozen.is_none() && !self.osd.is_empty() {
                if let Some(output) = &layer_output {
                    let mut toasts = Vec::new();
                    let pointer = self
                        .seat
                        .get_pointer()
                        .map(|p| p.current_location())
                        .unwrap_or_default();
                    self.osd
                        .elements(&mut *renderer, output, pointer, &mut toasts);
                    elements.splice(osd_at..osd_at, toasts);
                }
            }
        }

        // HDR: composite in the working space, then draw the one element that codes the result.
        let mut hdr_states = None;
        if let (Some((space, pass, _)), Some(output)) = (hdr, layer_output.as_ref()) {
            let renderer = winit.renderer();
            let linear = hdr_frame::linearize(&mut *renderer, std::mem::take(&mut elements), space);
            let lights = crate::render::effects::gamut::take_content_light();
            let state = &mut self.winit_hdr;
            if !state.frame.as_ref().is_some_and(|f| f.fits(output, &pass)) {
                state.frame = HdrFrame::new(&mut *renderer, output, pass)
                    .map_err(|err| tracing::warn!("cannot make the HDR target: {err}"))
                    .ok();
            }
            let background = if locked {
                crate::protocol::session_lock::LOCKED_BACKGROUND
            } else {
                BACKGROUND
            };
            let clear = hdr_frame::clear_color(space, background);
            match state
                .frame
                .as_mut()
                .map(|f| f.render(&mut *renderer, &linear, clear, &lights))
            {
                Some(Ok((element, states))) => {
                    state.content_peak = state.frame.as_ref().and_then(HdrFrame::content_peak);
                    elements.push(JffElement::Hdr(element));
                    hdr_states = Some(states);
                }
                Some(Err(err)) => {
                    tracing::warn!("error drawing the HDR frame: {err}");
                    state.frame = None;
                    state.content_peak = None;
                }
                None => {}
            }
        } else {
            self.winit_hdr = WinitHdr::default();
        }

        let result = {
            let (renderer, mut framebuffer) = match winit.bind() {
                Ok(bound) => bound,
                Err(err) => {
                    tracing::warn!("error binding backend: {err:?}");
                    return;
                }
            };
            self.damage_tracker.render_output(
                renderer,
                &mut framebuffer,
                0,
                &elements,
                if locked {
                    crate::protocol::session_lock::LOCKED_BACKGROUND
                } else {
                    BACKGROUND
                },
            )
        };

        match result {
            Ok(res) => {
                if let Some(damage) = res.damage {
                    if let Err(err) = winit.submit(Some(damage)) {
                        tracing::warn!("error submitting frame: {err:?}");
                    }
                    // Host gives no presentation timestamps, so report the submit time.
                    if let Some(output) = self.outputs.first() {
                        let mut feedback = crate::protocol::presentation::take_feedback(
                            &self.desk,
                            output,
                            hdr_states.as_ref().unwrap_or(&res.states),
                        );
                        feedback.presented::<_, smithay::utils::Monotonic>(
                            crate::protocol::presentation::monotonic_now(),
                            smithay::wayland::presentation::Refresh::Unknown,
                            0,
                            smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::empty(),
                        );
                    }
                }
            }
            Err(err) => tracing::warn!("error rendering output: {err:?}"),
        }

        // redraws non-stop: the winit backend has no vblank to follow
        winit.window().request_redraw();

        if let Some(output) = self.outputs.first().cloned() {
            advance_frame_sequence(&output);
            self.send_frame_callbacks(&output);
            self.release_fifo_barriers(&output);
        }
    }

    /// Frame callbacks for everything on `output`, so clients go on to draw their next frame.
    /// Each surface gets at most one per frame cycle of the output (see `pacing.rs`), however
    /// often this is called.
    pub fn send_frame_callbacks(&self, output: &Output) {
        let rect = output_rect(output);
        let time = self.start_time.elapsed();
        let sequence = frame_sequence(output);
        let name = output.name();
        let should_send = |_: &WlSurface, states: &SurfaceData| -> Option<Output> {
            states
                .data_map
                .get_or_insert_threadsafe(LastCallback::default)
                .first_in_cycle(&name, sequence)
                .then(|| output.clone())
        };
        // Windows behind a fullscreen window that covers the monitor can't be seen: they get a
        // callback once per `HIDDEN_FRAME_INTERVAL` instead of one per refresh, so they stay
        // alive without competing with the game for the GPU.
        let covered = crate::protocol::layer_shell::fullscreen_covers(&self.desk, rect);
        let mut behind = false;
        for entry in self.desk.top_down() {
            let size = entry.window.geometry().size;
            let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
            if !layout::intersects(area, rect) {
                continue;
            }
            let throttle = if covered && behind {
                HIDDEN_FRAME_INTERVAL
            } else {
                Duration::ZERO
            };
            behind = true;
            entry
                .window
                .send_frame(output, time, Some(throttle), should_send);
        }
        // layers get callbacks regardless of windows
        for layer in smithay::desktop::layer_map_for_output(output).layers() {
            layer.send_frame(output, time, Some(Duration::ZERO), should_send);
        }
    }
}
