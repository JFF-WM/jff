//! idle-inhibit and ext-idle-notify: jff doesn't blank the screen itself, tools like swayidle ask
//! when the user was last active. `JffState::process_input_event` reports every input event to
//! the notifier. A client (mpv, a browser playing video, a game) uses zwp_idle_inhibit_v1 to ask
//! for the screen to stay awake while one of its surfaces is shown: while there's such a surface
//! on the shown workspace, jff tells the notifier it's inhibited.

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::get_parent;
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::seat::WaylandFocus;

use crate::state::JffState;

impl IdleNotifierHandler for JffState {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

impl IdleInhibitHandler for JffState {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.insert(surface);
        self.update_idle_inhibited();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.remove(&surface);
        self.update_idle_inhibited();
    }
}

impl JffState {
    /// Tell idle tools whether a shown surface keeps the screen awake. Runs when an inhibitor
    /// comes or goes, and once per event-loop iteration (a window may have moved to another
    /// workspace or closed). Cheap while nobody inhibits.
    pub fn update_idle_inhibited(&mut self) {
        if self.idle_inhibitors.is_empty() && !self.idle_inhibited {
            return;
        }
        self.idle_inhibitors.retain(|surface| surface.is_alive());
        let inhibited = self.idle_inhibitors.iter().any(|surface| {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let is_root = |window: &Window| window.wl_surface().is_some_and(|s| *s == root);
            // A window on another workspace isn't shown. A surface that isn't any window's (a
            // panel or a lock screen) is.
            self.desk.top_down().any(|entry| is_root(&entry.window))
                || !self.desk.all_top_down().any(|entry| is_root(&entry.window))
        });
        if inhibited != self.idle_inhibited {
            self.idle_inhibited = inhibited;
            self.idle_notifier_state.set_is_inhibited(inhibited);
            tracing::debug!("idle inhibited: {inhibited}");
        }
    }
}
