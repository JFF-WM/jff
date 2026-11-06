//! winit backend, only used when running nested during development.

use std::time::Duration;

use anyhow::anyhow;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::utils::{Logical, Physical, Point, Scale, Transform};

use crate::feature::outputs::output_rect;
use crate::feature::overview_mode::overview_elements;
use crate::primitives::layout;
use crate::protocol::layer_shell::{
    layer_body_elements, layer_elements, layer_popup_elements, layers_above, ABOVE_WINDOWS,
    BELOW_WINDOWS,
};
use crate::render::effects::background_effect::window_elements;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::elements::JffElement;
use crate::state::JffState;

use super::{Backend, BACKGROUND};

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
    pub fn start_rendering(&mut self) {
        match &self.backend {
            Backend::Winit(winit) => winit.window().request_redraw(),
            Backend::Tty(_) => self.render(),
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

    pub fn render(&mut self) {
        if self.backend.is_tty() {
            self.render_tty();
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
                    elements.extend(layer_elements(&mut *renderer, output, above, blur));
                    // popups of layers below the windows still draw above them
                    elements.extend(layer_popup_elements(
                        &mut *renderer,
                        output,
                        &BELOW_WINDOWS,
                        blur,
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
                    for entry in self.desk.top_down() {
                        let geo = entry.window.geometry();
                        let pos: Point<i32, Logical> = Point::from(entry.pos) - geo.loc;
                        // Popups and the window, each with its blur right behind it.
                        window_elements(
                            &mut *renderer,
                            &entry.window,
                            pos,
                            blur,
                            snapshot.as_ref(),
                            radius,
                            popup_radius,
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
                    ));
                }
            }
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
                            &res.states,
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

        // redraws non-stop for now, pace it later
        winit.window().request_redraw();

        if let Some(output) = self.outputs.first().cloned() {
            self.send_frame_callbacks(&output);
            self.release_fifo_barriers(&output);
        }
    }

    /// Frame callbacks for everything on `output`, so clients go on to draw their next frame.
    pub fn send_frame_callbacks(&self, output: &Output) {
        let rect = output_rect(output);
        let time = self.start_time.elapsed();
        for entry in self.desk.top_down() {
            let size = entry.window.geometry().size;
            let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
            if !layout::intersects(area, rect) {
                continue;
            }
            entry
                .window
                .send_frame(output, time, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
        }
        // layers get callbacks regardless of windows
        for layer in smithay::desktop::layer_map_for_output(output).layers() {
            layer.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }
}
