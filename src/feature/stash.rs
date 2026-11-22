//! Minimize and the scratchpad: windows put out of sight on purpose.
//!
//! A hidden window keeps its workspace, monitor, place and size, it just isn't shown, drawn or
//! reachable, the same as a window on a workspace nobody is looking at (see
//! [`Desk::is_visible`](crate::desk::Desk::is_visible)). The bookkeeping is the desk's, what is
//! here is the Smithay side: handing the keyboard on and bringing a window back.
//!
//! Two kinds go together:
//! - Minimized: `minimize` puts the focused window away, `unminimize` brings the one that went
//!   last back to the workspace it came from. A taskbar can do both too, and activating a
//!   minimized window (a taskbar click, an activation request) brings it back as well.
//! - The scratchpad: `move-to-scratchpad` puts the focused window in it, `scratchpad` shows one
//!   on the monitor in use, centered and on top, and puts it away again when it has the
//!   keyboard. With several the key steps through them, each time the one that has been away the
//!   longest. Sending a scratchpad window to a workspace takes it out of the scratchpad.

use smithay::desktop::Window;
use smithay::utils::SERIAL_COUNTER;

use crate::primitives::layout;
use crate::state::JffState;

impl JffState {
    /// Put `window` out of sight, into the scratchpad when `scratch`. The keyboard moves on to
    /// what is on top now when the window had it. A member of a container leaves it first (the
    /// others take its room). Returns false, having changed nothing, when it could not be
    /// hidden: it is gone, hidden already, or a fullscreen member of a container.
    fn hide_window(&mut self, window: &Window, scratch: bool) -> bool {
        if self.desk.container_of(|w| w == window).is_some() {
            let size = window.geometry().size;
            let pos = self.desk.pos_of(|w| w == window).unwrap_or_default();
            let middle = (
                f64::from(pos.0) + f64::from(size.w) / 2.0,
                f64::from(pos.1) + f64::from(size.h) / 2.0,
            );
            self.eject_from_container(window, middle.into());
        }
        let had_keyboard = self.focused_window().is_some_and(|w| w == *window);
        if !self.desk.hide_where(|w| w == window, scratch) {
            return false;
        }
        // A move or resize of this window must not carry on out of sight.
        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.is_grabbed() && !self.overview_active() {
                pointer.unset_grab(
                    self,
                    SERIAL_COUNTER.next_serial(),
                    smithay::backend::input::InputTime::now(),
                );
            }
        }
        if had_keyboard {
            self.refocus_topmost();
        }
        self.refresh_pointer_focus();
        true
    }

    /// Minimize `window`: out of sight until [`JffState::unminimize_window`] or an activation.
    ///
    /// When the taskbar that asked told where its button is (`wlr-foreign-toplevel`
    /// `set_rectangle`), the window flies into the button while it fades.
    pub fn minimize_window(&mut self, window: &Window) {
        // The picture has to be taken while the window is shown, and a ghost is only kept when
        // the window did go away.
        let ghost = self
            .foreign_toplevel_rectangle(window)
            .and_then(|button| self.minimize_ghost(window, button));
        if self.hide_window(window, false) {
            if let Some(ghost) = ghost {
                self.closing.push(ghost);
            }
            tracing::info!("window minimized");
        }
    }

    /// `minimize`: the focused window goes.
    pub fn minimize_focused(&mut self) {
        if let Some(window) = self.focused_window() {
            self.minimize_window(&window);
        }
    }

    /// Bring a minimized `window` back, on the workspace it came from, and give it the
    /// keyboard. Does nothing for one that is not hidden.
    pub fn unminimize_window(&mut self, window: &Window) {
        let minimized = self
            .desk
            .find_by(|w| w == window)
            .filter(|e| e.hidden.is_some())
            .map(|e| !e.scratch);
        let Some(minimized) = minimized else {
            return;
        };
        self.show_workspace_of(window);
        self.focus_window(window);
        self.refresh_pointer_focus();
        // A window of the scratchpad did not go into a button.
        if minimized {
            if let Some(button) = self.foreign_toplevel_rectangle(window) {
                self.start_restore_fly(window, button);
            }
        }
    }

    /// `unminimize`: the window that was minimized last comes back.
    pub fn unminimize_last(&mut self) {
        let window = self
            .desk
            .newest_hidden(false)
            .map(|entry| entry.window.clone());
        match window {
            Some(window) => self.unminimize_window(&window),
            None => self.show_error("Nothing minimized", "There is no minimized window."),
        }
    }

    /// `move-to-scratchpad`: the focused window goes into the scratchpad, out of sight.
    pub fn move_to_scratchpad_focused(&mut self) {
        if let Some(window) = self.focused_window() {
            self.move_to_scratchpad_window(&window);
        }
    }

    /// Put `window` into the scratchpad, out of sight. Returns false, having changed nothing,
    /// when it could not be hidden (see [`JffState::hide_window`]).
    pub fn move_to_scratchpad_window(&mut self, window: &Window) -> bool {
        let done = self.hide_window(window, true);
        if done {
            tracing::info!("window put in the scratchpad");
        }
        done
    }

    /// Whether any window belongs to the scratchpad, shown or away.
    pub fn scratchpad_has_windows(&self) -> bool {
        self.desk.all_top_down().any(|entry| entry.scratch)
    }

    /// `scratchpad` for one window: an away window of the scratchpad comes to the monitor in
    /// use, a shown one goes away. Returns false, having changed nothing, when `window` is not
    /// in the scratchpad.
    pub fn toggle_scratchpad_window(&mut self, window: &Window) -> bool {
        let away = self
            .desk
            .find_by(|w| w == window)
            .filter(|entry| entry.scratch)
            .map(|entry| entry.hidden.is_some());
        match away {
            Some(true) => self.summon_to_active_monitor(window),
            Some(false) => {
                self.hide_window(window, true);
            }
            None => return false,
        }
        true
    }

    /// `scratchpad`: a scratchpad window that has the keyboard goes away. Otherwise the one
    /// that has been away the longest comes to the monitor in use, and when none is away, a
    /// scratchpad window that is shown goes away.
    pub fn toggle_scratchpad(&mut self) {
        let focused_scratch = self.focused_window().filter(|window| {
            self.desk
                .find_by(|w| w == window)
                .is_some_and(|e| e.scratch)
        });
        if let Some(window) = focused_scratch {
            self.hide_window(&window, true);
            return;
        }
        let away = self
            .desk
            .oldest_hidden(true)
            .map(|entry| entry.window.clone());
        if let Some(window) = away {
            self.summon_to_active_monitor(&window);
            return;
        }
        let shown = self
            .desk
            .top_down()
            .find(|entry| entry.scratch)
            .map(|entry| entry.window.clone());
        match shown {
            Some(window) => {
                self.hide_window(&window, true);
            }
            None => self.show_error(
                "Scratchpad is empty",
                "Put a window in it with move-to-scratchpad.",
            ),
        }
    }

    /// Show the hidden `window` on the workspace the monitor in use shows, centered on that
    /// monitor and on top, and give it the keyboard. A maximized or snapped one is fitted to
    /// the monitor and goes back to the same place there.
    fn summon_to_active_monitor(&mut self, window: &Window) {
        let output = self.active_output_name();
        let rects = self.output_rects();
        let Some(&target) = self
            .outputs
            .iter()
            .position(|o| o.name() == output)
            .and_then(|i| rects.get(i))
        else {
            return;
        };
        let Some(entry) = self.desk.find_by(|w| w == window) else {
            return;
        };
        let size = window.geometry().size;
        let size = (size.w, size.h);
        let (pos, restore) = (entry.pos, entry.restore);
        let source = self.rect_for_window(pos, size);

        self.let_go_of_windows();
        let workspace = self.current_workspace();
        self.desk.unhide_where(|w| w == window);
        self.desk.set_output_where(|w| w == window, &output);
        self.desk.move_to_workspace(|w| w == window, workspace);
        self.desk
            .set_pos_where(|w| w == window, layout::center_in(size, target));
        if let Some((restore_pos, restore_size)) = restore {
            self.desk.set_restore_pos_where(
                |w| w == window,
                layout::carry_pos(restore_pos, restore_size, source, target),
            );
            self.relayout_outputs();
        }
        self.focus_window(window);
        self.refresh_window_outputs();
        self.refresh_pointer_focus();
        tracing::info!("scratchpad window shown on {output}");
    }
}
