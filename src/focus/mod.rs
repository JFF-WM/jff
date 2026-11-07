//! Keyboard focus.
//!
//! Who has the keyboard is derived, not pushed around by whoever acts last. The inputs are
//! plain state: the window the user picked (`focused_window`), the panel they clicked
//! (`layer_on_demand_focus`), launchers that asked for every key (exclusive layer surfaces), and
//! whether the overview is open. `JffState::update_keyboard_focus` turns that into one answer
//! and hands it to the seat, only when the answer changed.
//!
//! Runs once per event-loop iteration (see `main.rs`) and right after the discrete actions that
//! change the inputs (a click, a new window, a workspace switch). So a window that went away, a
//! launcher that closed, or an overview that finished can never leave the keyboard pointing at
//! nobody or at a dead surface: the next pass puts it where it belongs.
//!
//! The same pass keeps the `Activated` state of toplevels in step with the focus, and ends a
//! menu grab when the window that owns the menu stops having the keyboard.

use smithay::backend::input::{InputTime, KeyState};
use smithay::desktop::{
    layer_map_for_output, PopupGrab, PopupKind, PopupManager, PopupUngrabStrategy, Window,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{IsAlive, Serial, SERIAL_COUNTER};
use smithay::wayland::shell::wlr_layer::KeyboardInteractivity;

use crate::protocol::layer_shell::is_mapped;
use crate::state::JffState;

/// Who the seat's keyboard belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyboardFocus {
    /// Nothing that takes keys is open.
    Nobody,
    /// Overview is open: it reads the keys itself, no client gets any.
    Overview,
    /// Area screenshot selection is open: it reads the keys itself, no client gets any.
    Screenshot,
    /// A window (its toplevel surface), including while one of its menus has a grab.
    Window(WlSurface),
    /// A panel or launcher (its layer surface).
    Layer(WlSurface),
}

impl KeyboardFocus {
    /// The surface that gets the keys, if any.
    pub fn surface(&self) -> Option<&WlSurface> {
        match self {
            KeyboardFocus::Window(surface) | KeyboardFocus::Layer(surface) => Some(surface),
            KeyboardFocus::Nobody | KeyboardFocus::Overview | KeyboardFocus::Screenshot => None,
        }
    }

    fn into_surface(self) -> Option<WlSurface> {
        match self {
            KeyboardFocus::Window(surface) | KeyboardFocus::Layer(surface) => Some(surface),
            KeyboardFocus::Nobody | KeyboardFocus::Overview | KeyboardFocus::Screenshot => None,
        }
    }
}

/// A menu (`xdg_popup.grab`) of a window that takes keyboard and pointer right now.
pub struct PopupGrabState {
    /// Toplevel at the root of the menu.
    pub root: WlSurface,
    pub grab: PopupGrab<JffState>,
    /// Serial the seat grabs were taken with, so we can recognise them.
    pub serial: Serial,
    /// Root is a window, so the grab is tied to that window keeping the keyboard. (Panel menus
    /// are left to the popup grab itself, a panel often can't take the keyboard at all.)
    pub window_root: bool,
}

impl JffState {
    /// Raise a window and make it the one the user is working in. The keyboard follows, unless a
    /// launcher that asked for every key, or the overview, has it.
    pub fn focus_window(&mut self, window: &Window) {
        let target = window.clone();
        self.desk.raise_where(|w| *w == target);
        self.focused_window = Some(window.clone());
        // Clicking a window takes the keyboard back from a panel that had it.
        self.layer_on_demand_focus = None;
        self.update_keyboard_focus();
    }

    /// Hand the keyboard to the topmost window of the shown workspace, or to nobody when there
    /// is none.
    pub fn refocus_topmost(&mut self) {
        self.focused_window = None;
        self.layer_on_demand_focus = None;
        self.update_keyboard_focus();
    }

    /// A panel was clicked: give it the keyboard if it takes keys.
    pub fn focus_layer_on_demand(&mut self, surface: &WlSurface) {
        self.layer_on_demand_focus = Some(surface.clone());
        self.update_keyboard_focus();
    }

    /// Work out who should have the keyboard and make it so. Cheap when nothing changed.
    pub fn update_keyboard_focus(&mut self) {
        self.refresh_popup_grab();
        self.forget_stale_on_demand_focus();

        let layout = self.layout_focus();
        self.focused_window.clone_from(&layout);

        let focus = self.compute_keyboard_focus(layout.as_ref());
        // While the overview owns the keyboard, no window is "active": a window must not claim
        // activation without the keyboard.
        let shown_active = if matches!(focus, KeyboardFocus::Overview) || self.session_locked() {
            None
        } else {
            layout.as_ref()
        };
        self.refresh_window_states(shown_active);

        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };

        if self.keyboard_focus != focus {
            tracing::debug!("keyboard focus: {:?} -> {:?}", self.keyboard_focus, focus);
        }

        // A menu belongs to its window: when the window loses the keyboard, the menu closes and
        // lets go of the seat, or it would keep input nobody can see.
        //
        // A panel's menu only closes when the keyboard goes somewhere it can't follow: to the
        // overview (which reads every key itself), or, for a panel that takes keys, to another
        // surface than the panel. A panel that refuses the keyboard never has it, so a focus
        // change says nothing about its menu. That one ends with an outside click.
        let grab_outlived = self.popup_grab.as_ref().is_some_and(|grab| {
            let elsewhere = Some(&grab.root) != focus.surface();
            if grab.window_root {
                elsewhere
            } else {
                matches!(focus, KeyboardFocus::Overview | KeyboardFocus::Screenshot)
                    || (elsewhere && self.layer_takes_keys(&grab.root))
            }
        });
        if grab_outlived {
            self.end_popup_grab();
        }

        let to_client = focus.surface().is_some();
        // A client that is left while a key is down never sees that key come up: the overview
        // and the screenshot selection keep every release from reaching anyone, and the key
        // that opened them is often still down (`jff msg toggle-overview` typed in a terminal:
        // the terminal got the Enter press, so it repeats Enter until told otherwise, and not
        // every client stops repeating on a leave). Tell it the keys came up, before it
        // loses the keyboard. Keys whose press it never saw (a binding took them) stay out.
        if self.keyboard_focus != focus && !to_client && self.keyboard_focus.surface().is_some() {
            let held: Vec<_> = keyboard
                .pressed_keys()
                .into_iter()
                .filter(|key| !self.suppressed_keys.contains(key))
                .collect();
            for key in held {
                keyboard.input_forward(
                    self,
                    key,
                    KeyState::Released,
                    SERIAL_COUNTER.next_serial(),
                    InputTime::now(),
                    false,
                );
            }
        }
        self.keyboard_focus.clone_from(&focus);
        keyboard.set_focus(self, focus.into_surface(), SERIAL_COUNTER.next_serial());
        // Modifiers can change while no client has the keyboard: the Alt release that opened
        // the overview is kept from everybody, and the window that was left never saw it. Its
        // idea of the modifiers would stay "Alt held" and every key typed in it would arrive as
        // Alt+key. Whoever gets the keyboard next is told the modifiers as they are now.
        if to_client {
            keyboard.advertise_modifier_state(self);
        }
    }

    /// The window the user is working in: the one they picked while it's still on the shown
    /// workspace, otherwise whatever is on top there.
    fn layout_focus(&self) -> Option<Window> {
        if let Some(window) = &self.focused_window {
            if self.desk.top_down().any(|entry| entry.window == *window) {
                return Some(window.clone());
            }
        }
        // A workspace per monitor: the topmost window on the monitor in use comes first.
        if self.desk.per_output() {
            let active = self.active_output_name();
            if let Some(entry) = self.desk.top_down().find(|entry| entry.output == active) {
                return Some(entry.window.clone());
            }
        }
        self.desk
            .top_down()
            .next()
            .map(|entry| entry.window.clone())
    }

    /// The decision, in priority order: overview, launcher that wants every key, panel the user
    /// clicked, the window they work in.
    fn compute_keyboard_focus(&self, layout: Option<&Window>) -> KeyboardFocus {
        // Locked session: the screen locker has the keyboard, nobody else.
        if self.session_locked() {
            return self
                .lock_keyboard_surface()
                .map_or(KeyboardFocus::Nobody, KeyboardFocus::Layer);
        }
        if self.overview_active() {
            return KeyboardFocus::Overview;
        }
        if self.screenshot.is_some() {
            return KeyboardFocus::Screenshot;
        }
        if let Some(surface) = self.exclusive_layer_surface() {
            return KeyboardFocus::Layer(surface);
        }
        if let Some(surface) = &self.layer_on_demand_focus {
            return KeyboardFocus::Layer(surface.clone());
        }
        layout
            .and_then(|window| window.toplevel())
            .map(|toplevel| KeyboardFocus::Window(toplevel.wl_surface().clone()))
            .unwrap_or(KeyboardFocus::Nobody)
    }

    /// The panel the user clicked stops being the keyboard owner when it went away, hid itself,
    /// or no longer takes keys.
    fn forget_stale_on_demand_focus(&mut self) {
        let Some(surface) = &self.layer_on_demand_focus else {
            return;
        };
        let good = surface.alive() && is_mapped(surface) && self.layer_takes_keys(surface);
        if !good {
            self.layer_on_demand_focus = None;
        }
    }

    /// Whether `surface` is a layer surface in a layer map that doesn't refuse the keyboard.
    fn layer_takes_keys(&self, surface: &WlSurface) -> bool {
        self.outputs.iter().any(|output| {
            layer_map_for_output(output).layers().any(|layer| {
                layer.wl_surface() == surface
                    && layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None
            })
        })
    }

    /// Only the window the user works in is shown as active, on every workspace. A configure
    /// only goes out for a window whose state really changes.
    fn refresh_window_states(&mut self, layout: Option<&Window>) {
        for entry in self.desk.all_top_down() {
            let Some(toplevel) = entry.window.toplevel() else {
                continue;
            };
            let active = layout == Some(&entry.window);
            let changed = toplevel.with_pending_state(|state| {
                let was = state.states.contains(xdg_toplevel::State::Activated);
                if active {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
                was != active
            });
            // Before the initial configure, the state just goes along with it.
            if changed && toplevel.is_initial_configure_sent() {
                let _ = toplevel.send_pending_configure();
            }
        }
    }

    /// Forget a menu grab that ended by itself (the menu was dismissed, a click outside it).
    fn refresh_popup_grab(&mut self) {
        if self
            .popup_grab
            .as_ref()
            .is_some_and(|grab| grab.grab.has_ended())
        {
            self.popup_grab = None;
        }
    }

    /// Close the menu that has a grab and give keyboard and pointer back. Only grabs that are
    /// still the menu's own are touched, a move or resize in progress is left alone.
    fn end_popup_grab(&mut self) {
        let Some(mut state) = self.popup_grab.take() else {
            return;
        };
        let keyboard = self.seat.get_keyboard();
        let pointer = self.seat.get_pointer();
        let keyboard_held = keyboard.as_ref().is_some_and(|k| k.has_grab(state.serial));
        let pointer_held = pointer.as_ref().is_some_and(|p| p.has_grab(state.serial));
        tracing::debug!(
            "end popup grab: root={:?} window_root={} keyboard_held={keyboard_held} pointer_held={pointer_held}",
            state.root.id(),
            state.window_root
        );

        state.grab.ungrab(PopupUngrabStrategy::All);
        if keyboard_held {
            if let Some(keyboard) = keyboard {
                keyboard.unset_grab(self);
            }
        }
        if pointer_held {
            if let Some(pointer) = pointer {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }
    }

    /// Close every menu and popover, whatever kind it is, and give keyboard and pointer back.
    ///
    /// Two things can be open, and a menu can be either or both:
    ///
    /// A menu with an `xdg_popup.grab`: `popup_grab` knows it and the seat grabs belong to it.
    /// Ended here whether its root is a window or a panel.
    ///
    /// A menu without a grab (some toolkits never ask for one): nothing on our side knows it's
    /// open, and the client only closes it on a click or a pointer leave. So every popup of every
    /// layer surface and window is told to go away with `popup_done`.
    ///
    /// Called when something takes over the screen (the overview opens or closes, the workspace
    /// changes). A client that wants its menu back just opens a new one.
    pub(crate) fn dismiss_popups(&mut self) {
        let had_grab = self.popup_grab.is_some();
        self.end_popup_grab();

        // Collect the roots first: no layer map may stay locked while clients are written to.
        let mut roots: Vec<WlSurface> = Vec::new();
        for output in &self.outputs {
            let map = layer_map_for_output(output);
            roots.extend(map.layers().map(|layer| layer.wl_surface().clone()));
        }
        roots.extend(self.desk.all_top_down().filter_map(|entry| {
            entry
                .window
                .toplevel()
                .map(|toplevel| toplevel.wl_surface().clone())
        }));

        let mut found = 0usize;
        for root in &roots {
            let popups: Vec<PopupKind> = PopupManager::popups_for_surface(root)
                .map(|(popup, _)| popup)
                .collect();
            found += popups.len();
            // Children go first: a popup is only ever dismissed after the ones on top of it.
            for popup in popups.iter().rev() {
                let _ = PopupManager::dismiss_popup(root, popup);
            }
        }
        if found > 0 || had_grab {
            tracing::debug!("dismiss popups: had_grab={had_grab} popups_found={found}");
        }
    }

    /// Whether `surface` is the root of a window (as opposed to a panel).
    pub fn is_window_root(&self, surface: &WlSurface) -> bool {
        self.desk.all_top_down().any(|entry| {
            entry
                .window
                .toplevel()
                .is_some_and(|t| t.wl_surface() == surface)
        })
    }
}
