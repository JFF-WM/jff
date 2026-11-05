//! Monitors in the shared desktop space: where each one sits, and what that means for windows
//! and the pointer. Backend-agnostic, the TTY backend calls in when monitors come and go.

use std::time::{Duration, Instant};

use smithay::backend::input::InputTime;
use smithay::desktop::space::SpaceElement;
use smithay::input::pointer::MotionEvent;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER};
use smithay::wayland::fractional_scale::with_fractional_scale;

use crate::config::ModeRequest;
use crate::primitives::layout::{self, Rect};
use crate::state::JffState;

/// Area used when there is no monitor at all.
const NO_OUTPUT: Rect = (0, 0, 800, 600);

/// How long an unplugged monitor's global stays around for clients to react first.
const GLOBAL_GRACE: Duration = Duration::from_secs(10);

/// One video mode a connected monitor offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorMode {
    pub width: i32,
    pub height: i32,
    /// Refresh in mHz, as `wl_output` reports it.
    pub refresh: i32,
    pub preferred: bool,
}

/// A connected monitor, lit or not, and its modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// Connector name, like `DP-1`.
    pub name: String,
    /// Physical size in mm, `(0, 0)` when unknown.
    pub size_mm: (i32, i32),
    pub modes: Vec<MonitorMode>,
}

impl Monitor {
    /// Whether the monitor offers the requested size (and refresh, when one is given).
    pub fn has_mode(&self, request: &ModeRequest) -> bool {
        self.modes.iter().any(|m| {
            (m.width, m.height) == (request.width, request.height)
                && request
                    .refresh
                    .map_or(true, |hz| mhz_to_hz(m.refresh) == hz)
        })
    }
}

/// `wl_output` refresh (mHz) to whole Hz, as the config uses.
pub fn mhz_to_hz(mhz: i32) -> i32 {
    (mhz + 500) / 1000
}

/// The area `output` covers in the shared space.
pub fn output_rect(output: &Output) -> Rect {
    let loc = output.current_location();
    let size = output
        .current_mode()
        .map(|m| (m.size.w, m.size.h))
        .unwrap_or((0, 0));
    (loc.x, loc.y, size.0, size.1)
}

impl JffState {
    /// Area of every monitor, in the order they were added.
    pub fn output_rects(&self) -> Vec<Rect> {
        self.outputs.iter().map(output_rect).collect()
    }

    /// Monitor the pointer is on, or the closest one.
    pub fn rect_at_pointer(&self) -> Rect {
        let rects = self.output_rects();
        let p = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        layout::nearest((p.x, p.y), &rects)
            .map(|i| rects[i])
            .unwrap_or(NO_OUTPUT)
    }

    /// Monitor a window of `size` at `pos` belongs to: the one under its center.
    pub fn rect_for_window(&self, pos: (i32, i32), size: (i32, i32)) -> Rect {
        let rects = self.output_rects();
        layout::nearest(layout::center(pos, size), &rects)
            .map(|i| rects[i])
            .unwrap_or(NO_OUTPUT)
    }

    /// Start using a monitor: advertise it, lay it out with the others.
    pub fn add_output(&mut self, output: Output) {
        let global = output.create_global::<Self>(&self.display_handle);
        self.output_globals.insert(output.name(), global);
        self.outputs.push(output);
        self.relayout_outputs();
    }

    /// Stop using a monitor: windows move to the remaining ones, clients are told it's gone.
    pub fn remove_output(&mut self, output: &Output) {
        self.outputs.retain(|o| o != output);
        // Panels on a monitor that's gone have nowhere to be: tell them to quit (or move).
        for layer in smithay::desktop::layer_map_for_output(output).layers() {
            layer.layer_surface().send_close();
        }
        if self.overview.as_ref().is_some_and(|ov| ov.shows(output)) {
            self.abort_overview();
        }
        if self.screenshot.as_ref().is_some_and(|ui| ui.shows(output)) {
            self.cancel_screenshot();
        }
        if let Some(global) = self.output_globals.remove(&output.name()) {
            self.display_handle.disable_global::<Self>(global.clone());
            self.retiring_globals.push((Instant::now(), global));
        }
        // Every window, including ones on workspaces that aren't shown.
        for entry in self.desk.all_top_down() {
            entry.window.output_leave(output);
        }
        self.relayout_outputs();
    }

    /// Destroy globals of monitors unplugged a while ago.
    pub fn reap_globals(&mut self) {
        if self.retiring_globals.is_empty() {
            return;
        }
        let now = Instant::now();
        let (expired, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retiring_globals)
            .into_iter()
            .partition(|(since, _)| now.duration_since(*since) >= GLOBAL_GRACE);
        for (_, global) in expired {
            self.display_handle.remove_global::<Self>(global);
        }
        self.retiring_globals = pending;
    }

    /// Apply a changed config to the monitors that are already connected.
    pub fn reconcile_outputs(&mut self) {
        if self.backend.is_tty() {
            self.scan_tty_outputs();
        }
        self.relayout_outputs();
    }

    /// Place every monitor in the shared space, then bring windows and the pointer along:
    /// maximized windows follow their monitor, anything left out of reach is moved back.
    pub fn relayout_outputs(&mut self) {
        let sizes: Vec<(i32, i32)> = self.output_rects().iter().map(|r| (r.2, r.3)).collect();
        let fixed: Vec<Option<(i32, i32)>> = self
            .outputs
            .iter()
            .map(|o| self.config.output(&o.name()).and_then(|c| c.position))
            .collect();
        for (output, pos) in self.outputs.iter().zip(layout::arrange(&sizes, &fixed)) {
            let loc = output.current_location();
            if (loc.x, loc.y) != pos {
                output.change_current_state(None, None, None, Some(pos.into()));
            }
        }

        // Monitors may have changed size: lay panels out again before windows look at their room.
        self.arrange_layers();

        let rects = self.output_rects();
        let usable: Vec<Rect> = rects.iter().map(|r| self.usable_rect(*r)).collect();
        if !rects.is_empty() {
            for entry in self.desk.iter_mut() {
                let size = entry.window.geometry().size;
                let size = (size.w, size.h);
                if entry.restore.is_some() {
                    let Some(i) = layout::nearest(layout::center(entry.pos, size), &rects) else {
                        continue;
                    };
                    // Maximized windows stop at panels, fullscreen ones fill the monitor.
                    let fullscreen = entry.window.toplevel().is_some_and(|t| {
                        t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen))
                    });
                    let rect = if fullscreen { rects[i] } else { usable[i] };
                    entry.pos = (rect.0, rect.1);
                    if let Some(toplevel) = entry.window.toplevel() {
                        toplevel.with_pending_state(|s| s.size = Some((rect.2, rect.3).into()));
                        toplevel.send_pending_configure();
                    }
                } else if !layout::reachable(entry.pos, size, &rects) {
                    let Some(i) = layout::nearest(layout::grab_point(entry.pos, size), &rects)
                    else {
                        continue;
                    };
                    entry.pos = layout::fit_into(entry.pos, size, rects[i]);
                }
            }
        }

        self.clamp_pointer();
        self.refresh_window_outputs();
        // wlr-output-management clients (wlr-randr, kanshi) want to know about any change.
        self.output_management_changed();
    }

    /// Pull the pointer back onto a monitor if its previous one is gone or moved.
    fn clamp_pointer(&mut self) {
        let rects = self.output_rects();
        if rects.is_empty() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let loc = pointer.current_location();
        let (x, y) = layout::clamp_to_rects((loc.x, loc.y), &rects);
        if (x, y) == (loc.x, loc.y) {
            return;
        }
        let pos: Point<f64, Logical> = (x, y).into();
        let under = self.pointer_focus_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    /// Tell every window which monitors it's on (clients use it for scale, and to pause
    /// animations on windows nobody can see). A window on a hidden workspace is on no monitor.
    pub fn refresh_window_outputs(&self) {
        let outputs: Vec<(&Output, Rect)> =
            self.outputs.iter().map(|o| (o, output_rect(o))).collect();
        let overlap = Rectangle::<i32, Logical>::from_size(Size::from((i32::MAX, i32::MAX)));
        for entry in self.desk.all_top_down() {
            let size = entry.window.geometry().size;
            let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
            let visible = self.desk.is_visible(entry);
            for (output, rect) in &outputs {
                if visible && layout::intersects(area, *rect) {
                    entry.window.output_enter(output, overlap);
                    // wp_fractional_scale_v1: the window learns this monitor's scale.
                    let scale = output.current_scale().fractional_scale();
                    entry.window.with_surfaces(|_, states| {
                        with_fractional_scale(states, |fractional| {
                            fractional.set_preferred_scale(scale)
                        });
                    });
                } else {
                    entry.window.output_leave(output);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> Monitor {
        Monitor {
            name: "DP-1".into(),
            size_mm: (600, 340),
            modes: vec![
                MonitorMode {
                    width: 2560,
                    height: 1440,
                    refresh: 143_912,
                    preferred: true,
                },
                MonitorMode {
                    width: 1920,
                    height: 1080,
                    refresh: 60_000,
                    preferred: false,
                },
            ],
        }
    }

    #[test]
    fn has_mode_compares_size_and_whole_hz() {
        let m = monitor();
        let req = |width, height, refresh| ModeRequest {
            width,
            height,
            refresh,
        };
        assert!(m.has_mode(&req(2560, 1440, Some(144))));
        assert!(m.has_mode(&req(1920, 1080, None)));
        assert!(!m.has_mode(&req(2560, 1440, Some(60))));
        assert!(!m.has_mode(&req(1280, 720, None)));
    }

    #[test]
    fn mhz_rounds_to_nearest_hz() {
        assert_eq!(mhz_to_hz(59_940), 60);
        assert_eq!(mhz_to_hz(143_912), 144);
    }
}
