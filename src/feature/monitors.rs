//! Several monitors: go to one, send a window to one, trade a workspace with one.
//!
//! The monitor in use is the one the pointer is on (see `workspaces.rs`: switching a workspace,
//! the overview and new windows all go by it), so everything here that changes the monitor
//! moves the pointer too. Which monitor a target means (`left`, `next`, `"DP-2"`) is worked
//! out by the pure [`MonitorTarget`], the desk bookkeeping of a swap is
//! [`Desk::swap_monitors`](crate::desk::Desk::swap_monitors), what is here is the Smithay
//! side: positions, configures, focus and the pointer.
//!
//! The public methods give `Err` with a sentence for a toast when there is nothing to do (the
//! edge of the desktop, a name that is not connected). The `*_bind` ones are what key bindings
//! run, they say so with a toast only where it is a mistake and not just the edge.

use smithay::desktop::Window;

use crate::desk::Entry;
use crate::primitives::geom::{Pos, Size};
use crate::primitives::layout::{self, MonitorTarget, Rect};
use crate::state::JffState;

/// A window that changes monitor with a workspace swap, and where it was.
struct Carried {
    window: Window,
    pos: Pos,
    size: Size,
    /// Where it floats back to, `Some` for a window that is maximized, snapped or fullscreen.
    restore: Option<(Pos, Size)>,
    from: Rect,
    to: Rect,
}

impl JffState {
    /// The monitor in use (the pointer's, else the closest), as an index into `outputs`.
    fn active_monitor(&self) -> Option<usize> {
        let at = self.seat.get_pointer()?.current_location();
        layout::nearest((at.x, at.y), &self.output_rects())
    }

    /// The names of the monitors, in the order of `outputs`.
    fn monitor_names(&self) -> Vec<String> {
        self.outputs.iter().map(|output| output.name()).collect()
    }

    /// The monitor `target` means seen from the monitor `from`.
    fn resolve_monitor(&self, target: &MonitorTarget, from: usize) -> Result<usize, String> {
        target.resolve(&self.output_rects(), &self.monitor_names(), from)
    }

    /// The monitor a window is on, as an index into `outputs`: the one it lives on with
    /// per-output workspaces (which is also where a hidden window is), otherwise the one under
    /// its center.
    fn monitor_of(&self, entry: &Entry<Window>, rects: &[Rect]) -> Option<usize> {
        if self.desk.per_output() {
            if let Some(i) = self
                .outputs
                .iter()
                .position(|output| output.name() == entry.output)
            {
                return Some(i);
            }
        }
        let size = entry.window.geometry().size;
        layout::nearest(layout::center(entry.pos, (size.w, size.h)), rects)
    }

    /// Say why a key binding of a monitor action did nothing. The edge of the desktop is
    /// quiet, a monitor named in the config that is not there is not.
    fn monitor_action_failed(&mut self, target: &MonitorTarget, why: &str) {
        tracing::debug!("monitor action: {why}");
        if matches!(target, MonitorTarget::Named(_)) {
            self.show_error("No such monitor", why);
        }
    }

    /// Put the pointer and the keyboard on monitor `to` (an index into `outputs`): on `prefer`
    /// when it is shown there, else the topmost window shown there, with the pointer at its
    /// center. Without a window the pointer goes to the middle of the monitor and the keyboard
    /// stays where it is (there is nothing on `to` to give it to).
    fn focus_monitor_index(&mut self, to: usize, prefer: Option<Window>) {
        let rects = self.output_rects();
        let Some(&monitor) = rects.get(to) else {
            return;
        };
        let here = |entry: &Entry<Window>| self.monitor_of(entry, &rects) == Some(to);
        let window = prefer
            .filter(|w| self.desk.top_down().any(|e| e.window == *w && here(e)))
            .or_else(|| {
                self.desk
                    .top_down()
                    .find(|e| here(*e))
                    .map(|e| e.window.clone())
            });
        let at = window
            .as_ref()
            .and_then(|w| self.desk.find_by(|x| x == w))
            .map(|entry| {
                let size = entry.window.geometry().size;
                (entry.pos, (size.w, size.h))
            })
            // A window that has no size yet has no center.
            .filter(|(_, size)| size.0 > 0 && size.1 > 0)
            .map_or_else(
                || layout::center((monitor.0, monitor.1), (monitor.2, monitor.3)),
                |(pos, size)| layout::center(pos, size),
            );
        if let Some(window) = &window {
            self.focus_window(window);
        }
        self.move_cursor(at);
    }

    /// `focus-monitor`: go to the monitor `target` means from the one in use. Returns its name.
    pub fn focus_monitor(&mut self, target: &MonitorTarget) -> Result<String, String> {
        let from = self.active_monitor().ok_or("There are no monitors.")?;
        let to = self.resolve_monitor(target, from)?;
        self.let_go_of_windows();
        self.focus_monitor_index(to, None);
        let name = self.monitor_names()[to].clone();
        tracing::info!("focus-monitor: {name}");
        Ok(name)
    }

    /// What the key binding `focus-monitor` runs.
    pub fn focus_monitor_bind(&mut self, target: &MonitorTarget) {
        if let Err(why) = self.focus_monitor(target) {
            self.monitor_action_failed(target, &why);
        }
    }

    /// Send `window` to the monitor `target` means from the one the window is on, and follow it
    /// with the keyboard and the pointer. It keeps its offset from the monitor's corner (moved
    /// into the new monitor when that is smaller), a maximized, snapped or fullscreen window
    /// is fitted to the new monitor and goes back to the same place there. With per-output
    /// workspaces it lands on the workspace the new monitor shows. A window of a tiling
    /// container takes its container along, as with a workspace. Returns the name of the
    /// monitor.
    pub fn move_to_monitor(
        &mut self,
        window: &Window,
        target: &MonitorTarget,
    ) -> Result<String, String> {
        let rects = self.output_rects();
        let from = self
            .desk
            .find_by(|w| w == window)
            .and_then(|entry| self.monitor_of(entry, &rects))
            .ok_or("That window is on no monitor.")?;
        let to = self.resolve_monitor(target, from)?;
        self.let_go_of_windows();
        if from != to && !self.send_to_monitor(window, to) {
            return Err("The window could not be moved.".to_string());
        }
        self.focus_monitor_index(to, Some(window.clone()));
        self.refresh_window_outputs();
        let name = self.monitor_names()[to].clone();
        tracing::info!("move-to-monitor: {name}");
        Ok(name)
    }

    /// What the key binding `move-to-monitor` runs: the focused window goes.
    pub fn move_to_monitor_bind(&mut self, target: &MonitorTarget) {
        let Some(window) = self.focused_window() else {
            return;
        };
        if let Err(why) = self.move_to_monitor(&window, target) {
            self.monitor_action_failed(target, &why);
        }
    }

    /// Put `window` on monitor `to` (an index into `outputs`), see
    /// [`JffState::move_to_monitor`] for how. Does not touch the focus or the pointer. Returns
    /// false when the window or the monitor is not there, or the window is on `to` already.
    fn send_to_monitor(&mut self, window: &Window, to: usize) -> bool {
        let rects = self.output_rects();
        let (Some(&target), Some(name)) = (
            rects.get(to),
            self.outputs.get(to).map(|output| output.name()),
        ) else {
            return false;
        };
        let Some(entry) = self.desk.find_by(|w| w == window) else {
            return false;
        };
        let Some(from) = self.monitor_of(entry, &rects) else {
            return false;
        };
        if from == to {
            return false;
        }
        let source = rects[from];
        let size = entry.window.geometry().size;
        let size = (size.w, size.h);
        let (pos, restore, container) = (entry.pos, entry.restore, entry.container);

        // A window of a container goes with all of it, laid out to fill the other monitor.
        if let Some(id) = container {
            return self.drop_container(id, target);
        }

        // One that fills its monitor only has to end up on the right one, `relayout_outputs`
        // below fits it there. A floating one keeps its place on the monitor.
        let moved = if restore.is_some() {
            layout::center_in(size, target)
        } else {
            layout::carry_pos(pos, size, source, target)
        };
        self.desk.set_pos_where(|w| w == window, moved);
        self.desk.set_output_where(|w| w == window, &name);
        self.desk.raise_where(|w| w == window);
        match restore {
            Some((restore_pos, restore_size)) => {
                self.desk.set_restore_pos_where(
                    |w| w == window,
                    layout::carry_pos(restore_pos, restore_size, source, target),
                );
                self.relayout_outputs();
            }
            // A floating member of a zone takes its zone along.
            None => self.zones_carry(window, pos),
        }
        true
    }

    /// `move-workspace-to-monitor`: the monitor in use and the monitor `target` means trade what
    /// they show. The windows of the workspace shown here go there and the other way round,
    /// each monitor keeps showing the workspace number it showed, nothing is merged into a
    /// workspace that was hidden (see [`Desk::swap_monitors`](crate::desk::Desk::swap_monitors)).
    /// Maximized, snapped and fullscreen windows are fitted to their new monitor, containers
    /// fill it, floating windows keep their offset. The pointer and the keyboard follow the
    /// workspace that was in use, to the other monitor. Needs per-output workspaces. Returns the
    /// name of the monitor the workspace went to.
    pub fn move_workspace_to_monitor(&mut self, target: &MonitorTarget) -> Result<String, String> {
        if !self.desk.per_output() {
            return Err(
                "Workspaces are shared by all monitors. Turn on workspaces { per-output } \
                 to move one."
                    .to_string(),
            );
        }
        let from = self.active_monitor().ok_or("There are no monitors.")?;
        let to = self.resolve_monitor(target, from)?;
        let names = self.monitor_names();
        if from == to {
            return Ok(names[to].clone());
        }
        let rects = self.output_rects();
        let (src, dst) = (names[from].clone(), names[to].clone());

        // What is shown on either monitor changes monitor. A container is moved by the desk
        // and laid out again by `relayout_outputs`, only its loose windows need a new place.
        let carried: Vec<Carried> = self
            .desk
            .top_down()
            .filter(|entry| entry.container.is_none())
            .filter_map(|entry| {
                let at = self.monitor_of(entry, &rects)?;
                let (origin, destination) = if at == from {
                    (rects[from], rects[to])
                } else if at == to {
                    (rects[to], rects[from])
                } else {
                    return None;
                };
                let size = entry.window.geometry().size;
                Some(Carried {
                    window: entry.window.clone(),
                    pos: entry.pos,
                    size: (size.w, size.h),
                    restore: entry.restore,
                    from: origin,
                    to: destination,
                })
            })
            .collect();

        self.let_go_of_windows();
        let focused = self.focused_window();
        self.desk.swap_monitors(&src, &dst);
        for c in &carried {
            let pos = if c.restore.is_some() {
                layout::center_in(c.size, c.to)
            } else {
                layout::carry_pos(c.pos, c.size, c.from, c.to)
            };
            self.desk.set_pos_where(|w| *w == c.window, pos);
            match c.restore {
                Some((restore_pos, restore_size)) => self.desk.set_restore_pos_where(
                    |w| *w == c.window,
                    layout::carry_pos(restore_pos, restore_size, c.from, c.to),
                ),
                None => self.zones_carry(&c.window, c.pos),
            }
        }
        self.relayout_outputs();
        self.focus_monitor_index(to, focused);
        tracing::info!("move-workspace-to-monitor: {src} <-> {dst}");
        Ok(dst)
    }

    /// What the key binding `move-workspace-to-monitor` runs.
    pub fn move_workspace_to_monitor_bind(&mut self, target: &MonitorTarget) {
        if let Err(why) = self.move_workspace_to_monitor(target) {
            // Without per-output workspaces it can never work, which is worth saying.
            if self.desk.per_output() {
                self.monitor_action_failed(target, &why);
            } else {
                self.show_error("Workspaces are shared", &why);
            }
        }
    }
}
