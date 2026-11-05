//! keyboard-shortcuts-inhibit: a client (a game, remote desktop) asks jff to keep its hands off
//! its keys. jff grants every request. Only matters while that surface has the keyboard: then
//! the config bindings are skipped in `input`, for keys and for the Alt+drag move and resize.
//! Two things stay: the VT switch keys and `toggle-shortcuts-inhibit` (`Super+Escape` by
//! default), the way out, so a window that grabbed the keys can never trap you.

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
    KeyboardShortcutsInhibitorSeat,
};
use smithay::wayland::seat::WaylandFocus;

use crate::state::JffState;

impl KeyboardShortcutsInhibitHandler for JffState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Every request is granted. It only acts while the surface has the keyboard.
        inhibitor.activate();
    }
}

impl JffState {
    /// `surface` asked for the shortcuts and its inhibitor is on.
    pub fn surface_inhibits_shortcuts(&self, surface: &WlSurface) -> bool {
        self.seat
            .keyboard_shortcuts_inhibitor_for_surface(surface)
            .is_some_and(|inhibitor| inhibitor.is_active())
    }

    /// `window` asked for the shortcuts and its inhibitor is on.
    pub fn window_inhibits_shortcuts(&self, window: &Window) -> bool {
        window
            .wl_surface()
            .is_some_and(|surface| self.surface_inhibits_shortcuts(&surface))
    }

    /// The surface that has the keyboard asked for the shortcuts and its inhibitor is on: jff's
    /// bindings don't act now.
    pub fn shortcuts_inhibited(&self) -> bool {
        self.seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .is_some_and(|focus| self.surface_inhibits_shortcuts(&focus))
    }

    /// The `toggle-shortcuts-inhibit` action: switch the inhibitor of the surface that has the
    /// keyboard off, or on again. Does nothing when it has none.
    pub fn toggle_shortcuts_inhibit(&mut self) {
        let Some(focus) = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
        else {
            return;
        };
        let Some(inhibitor) = self.seat.keyboard_shortcuts_inhibitor_for_surface(&focus) else {
            return;
        };
        if inhibitor.is_active() {
            inhibitor.inactivate();
            tracing::info!("keyboard shortcuts: jff's bindings work again in the focused window");
        } else {
            inhibitor.activate();
            tracing::info!("keyboard shortcuts: the focused window gets the keys again");
        }
    }
}
