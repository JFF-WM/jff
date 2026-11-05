//! Workspaces: several sets of windows, one shown at a time.
//!
//! Bookkeeping lives in `desk.rs` (pure, unit-tested): each window knows its workspace, the desk
//! knows the current one. Everything that draws, hit-tests, or sends frame callbacks walks
//! [`Desk::top_down`](crate::desk::Desk::top_down), which only yields windows of the current
//! workspace, so a hidden window is simply not there: no pixels, no pointer, no frame
//! callbacks (clients stop animating), and `refresh_window_outputs` tells it it left every
//! monitor. Position and size are kept while hidden.
//!
//! Workspaces are shared by all monitors: switching changes the windows everywhere at once.
//! Panels and wallpapers (layer surfaces) belong to a monitor and are always shown.
//!
//! This file is the Smithay side: switching, sending windows around, and handing the keyboard
//! to whatever is on top afterwards.

use smithay::backend::input::InputTime;
use smithay::desktop::Window;
use smithay::utils::SERIAL_COUNTER;

use crate::desk::{self, Goto};
use crate::state::JffState;

impl JffState {
    /// How many workspaces there are.
    pub fn workspace_count(&self) -> usize {
        self.config.workspaces()
    }

    /// Show the workspace `goto` leads to.
    pub fn goto_workspace(&mut self, goto: Goto) {
        let target = desk::resolve(goto, self.desk.current(), self.workspace_count());
        self.switch_workspace(target);
    }

    /// Show workspace `target` (0-based, clamped). Keyboard goes to the topmost window there,
    /// or to nobody when it's empty.
    pub fn switch_workspace(&mut self, target: usize) {
        let target = target.min(self.workspace_count().max(1) - 1);
        if target == self.desk.current() {
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

        tracing::info!("workspace {} -> {}", self.desk.current() + 1, target + 1);
        self.desk.set_current(target);
        self.refocus_topmost();
        self.refresh_pointer_focus();
    }

    /// Show the workspace `window` lives on (it may be hidden right now).
    pub fn show_workspace_of(&mut self, window: &Window) {
        if let Some(workspace) = self.desk.workspace_of(|w| w == window) {
            self.switch_workspace(workspace);
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
        let Some(from) = self.desk.workspace_of(|w| w == window) else {
            return;
        };
        if from == target {
            return;
        }
        let shown = self.desk.current();
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

    /// The config may have changed the workspace count: fold windows that no longer have one
    /// into the last, and fix the focus if the shown workspace went away.
    pub fn apply_workspace_count(&mut self) {
        let before = self.desk.current();
        self.desk.clamp_workspaces(self.workspace_count());
        if self.desk.current() != before {
            self.refocus_topmost();
            self.refresh_pointer_focus();
        }
    }
}
