//! Workspaces: several sets of windows, one shown at a time.
//!
//! Bookkeeping lives in `desk.rs` (pure, unit-tested): each window knows its workspace, the desk
//! knows the current one. Everything that draws, hit-tests, or sends frame callbacks walks
//! [`Desk::top_down`](crate::desk::Desk::top_down), which only yields windows of the current
//! workspace, so a hidden window is simply not there: no pixels, no pointer, no frame
//! callbacks (clients stop animating), and `refresh_window_outputs` tells it it left every
//! monitor. Position and size are kept while hidden.
//!
//! By default workspaces are shared by all monitors: switching changes the windows everywhere
//! at once. With `workspaces { per-output }` every monitor shows a workspace of its own: a window
//! lives on the monitor its center is on (the desk keeps that up to date, see
//! [`JffState::reconcile_window_workspaces`]) and is shown while that monitor shows its
//! workspace, switching acts on the monitor the pointer is on, and a window dragged to another
//! monitor lands on the workspace that monitor shows. Panels and wallpapers (layer surfaces)
//! belong to a monitor and are always shown.
//!
//! This file is the Smithay side: switching, sending windows around, and handing the keyboard
//! to whatever is on top afterwards.

use smithay::backend::input::InputTime;
use smithay::desktop::Window;
use smithay::utils::SERIAL_COUNTER;

use crate::desk::{self, Goto};
use crate::feature::outputs::output_rect;
use crate::primitives::layout::{self, Rect};
use crate::state::JffState;

impl JffState {
    /// How many workspaces there are.
    pub fn workspace_count(&self) -> usize {
        self.config.workspaces()
    }

    /// The monitor the user is working on: the one the pointer is on. Empty without monitors.
    pub fn active_output_name(&self) -> String {
        self.output_at_pointer()
            .map(|output| output.name())
            .unwrap_or_default()
    }

    /// The workspace shown on the monitor in use (0-based). The shared one, unless every
    /// monitor has its own workspace.
    pub fn current_workspace(&self) -> usize {
        if self.desk.per_output() {
            self.desk.current_on(&self.active_output_name())
        } else {
            self.desk.current()
        }
    }

    /// With per-output workspaces: tell the desk which monitor every window is on, from where
    /// its center is. A window that was shown and moved to another monitor goes on the
    /// workspace that monitor shows, so it follows the drag instead of disappearing. Called
    /// before every frame and when monitors change. Does nothing otherwise.
    pub fn reconcile_window_workspaces(&mut self) {
        if !self.desk.per_output() {
            return;
        }
        let monitors: Vec<(String, Rect)> = self
            .outputs
            .iter()
            .map(|output| (output.name(), output_rect(output)))
            .collect();
        if monitors.is_empty() {
            return;
        }
        let rects: Vec<Rect> = monitors.iter().map(|(_, rect)| *rect).collect();
        self.desk.assign_outputs(|entry| {
            let size = entry.window.geometry().size;
            let at = layout::center(entry.pos, (size.w, size.h));
            let i = layout::nearest(at, &rects)?;
            Some(monitors[i].0.clone())
        });
    }

    /// Show the workspace `goto` leads to on the monitor in use.
    pub fn goto_workspace(&mut self, goto: Goto) {
        let target = desk::resolve(goto, self.current_workspace(), self.workspace_count());
        self.switch_workspace(target);
    }

    /// Show workspace `target` (0-based, clamped) on the monitor in use. Keyboard goes to the
    /// topmost window there, or to nobody when it's empty.
    pub fn switch_workspace(&mut self, target: usize) {
        let output = self.active_output_name();
        self.switch_workspace_on(&output, target);
    }

    /// [`JffState::switch_workspace`] for the monitor `output`. Without per-output workspaces
    /// the monitor makes no difference.
    pub fn switch_workspace_on(&mut self, output: &str, target: usize) {
        let target = target.min(self.workspace_count().max(1) - 1);
        let before = self.desk.current_on(output);
        if target == before {
            return;
        }

        // The overview, a move/resize, and an open menu all hold windows of the workspace
        // that's about to disappear, let go first. Panel menus go too, they'd otherwise stay
        // open over a workspace they weren't opened on.
        self.dismiss_popups();
        self.abort_overview();
        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.is_grabbed() {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }

        if self.desk.per_output() {
            tracing::info!("workspace {} -> {} on {output}", before + 1, target + 1);
        } else {
            tracing::info!("workspace {} -> {}", before + 1, target + 1);
        }
        self.desk.set_current_on(output, target);
        self.refocus_topmost();
        self.refresh_pointer_focus();
    }

    /// Show the workspace `window` lives on (it may be hidden right now), on its monitor.
    pub fn show_workspace_of(&mut self, window: &Window) {
        if let Some((output, workspace)) = self.desk.home_of(|w| w == window) {
            self.switch_workspace_on(&output, workspace);
        }
    }

    /// Send the focused window to workspace `target` (0-based).
    pub fn move_focused_to_workspace(&mut self, target: usize) {
        if let Some(window) = self.focused_window() {
            self.send_to_workspace(&window, target);
        }
    }

    /// Send `window` to workspace `target` (0-based, clamped). One that leaves the shown
    /// workspace disappears, keyboard moves to what's on top now. One that arrives on the shown
    /// workspace appears on top and takes the keyboard.
    pub fn send_to_workspace(&mut self, window: &Window, target: usize) {
        let target = target.min(self.workspace_count().max(1) - 1);
        let Some((output, from)) = self.desk.home_of(|w| w == window) else {
            return;
        };
        if from == target {
            return;
        }
        // The window stays on its monitor, so it is that monitor's workspace that counts.
        let shown = self.desk.current_on(&output);
        self.desk.move_to_workspace(|w| w == window, target);

        if from == shown {
            // A move or resize on this window must not carry on out of sight.
            if let Some(pointer) = self.seat.get_pointer() {
                if pointer.is_grabbed() && !self.overview_active() {
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
                }
            }
            self.refocus_topmost();
            self.refresh_pointer_focus();
        } else if target == shown {
            self.focus_window(window);
            self.refresh_pointer_focus();
        }
    }

    /// The config may have changed the workspace count or `per-output`: fold windows that no longer have one
    /// into the last, and fix the focus if the shown workspace went away.
    pub fn apply_workspace_count(&mut self) {
        // `per-output` may have been turned on or off.
        let per_output = self.config.workspaces_per_output();
        let mode_changed = per_output != self.desk.per_output();
        if mode_changed {
            let active = self.active_output_name();
            self.desk.set_per_output(per_output, &active);
            self.reconcile_window_workspaces();
        }
        let before = self.current_workspace();
        self.desk.clamp_workspaces(self.workspace_count());
        if mode_changed || self.current_workspace() != before {
            self.refocus_topmost();
            self.refresh_pointer_focus();
        }
    }
}
