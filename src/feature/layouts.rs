//! A place for every window with a tag of the user's.
//!
//! A window that has a tag (a `tag "…"` line of a `window-rule`, or the `tag` IPC command, see
//! `protocol::toplevel_tag`) is remembered by its app id and that tag, not by its title, which
//! changes: where it is, how big, its mode, workspace and monitor. The next window with the same
//! app id and tag opens there. Windows without a tag of the user's are not remembered, so
//! nothing changes for them, and an app that puts its windows back itself (xdg-session-management)
//! keeps doing that.
//!
//! It is saved while the window is open (like the sessions, not mid-drag) and when it closes, in
//! `$XDG_STATE_HOME/jff-wm/layouts`, the format of `desk::session_store` with the app id where
//! that has the session id and the tag where it has the window's name. One place is kept for
//! every app id and tag: windows that share both open in the same place, and of those the
//! topmost one is what gets saved.
//!
//! A window that opens in its remembered place does not join the container that has the keyboard,
//! and dialogs are never remembered, they are centred over their parent.

use std::collections::HashSet;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::desk::session_store::{self, SavedWindow, Store};
use crate::feature::osd::toplevel_names;
use crate::protocol::toplevel_tag;
use crate::state::JffState;

/// Every remembered place, by app id and then by tag.
pub struct Layouts {
    store: Store,
    /// The store has changes not yet written to the file.
    dirty: bool,
    /// Windows that were put in their remembered place and have not shown a buffer yet.
    placed: HashSet<WlSurface>,
}

impl Layouts {
    pub fn load() -> Self {
        Self {
            store: session_store::load_layouts(),
            dirty: false,
            placed: HashSet::new(),
        }
    }

    /// Whether `surface` was put in its remembered place. Asks once: the answer is taken.
    pub fn take_placed(&mut self, surface: &WlSurface) -> bool {
        self.placed.remove(surface)
    }

    fn get(&self, key: &(String, String)) -> Option<SavedWindow> {
        self.store
            .get(&key.0)
            .and_then(|tags| tags.get(&key.1))
            .cloned()
    }

    fn remember(&mut self, key: (String, String), saved: SavedWindow) {
        let tags = self.store.entry(key.0).or_default();
        if tags.get(&key.1) != Some(&saved) {
            tags.insert(key.1, saved);
            self.dirty = true;
        }
    }

    fn save_if_dirty(&mut self) {
        if self.dirty {
            session_store::save_layouts(&self.store);
            self.dirty = false;
        }
    }
}

/// What a window is remembered by: its app id and the tag the user gave it. `None` for a window
/// with no tag of the user's, or no app id.
fn key_of(surface: &WlSurface) -> Option<(String, String)> {
    let tag = toplevel_tag::user_tag_of(surface)?;
    let (app_id, _) = toplevel_names(surface);
    (!app_id.is_empty()).then_some((app_id, tag))
}

impl JffState {
    /// First configure of `toplevel`: when it has a tag of the user's and a place is remembered
    /// for its app id and that tag, put it there (the same as an app's session does). Returns
    /// whether it did.
    pub(crate) fn layout_restore(&mut self, toplevel: &ToplevelSurface) -> bool {
        let surface = toplevel.wl_surface().clone();
        let dialog = toplevel.parent().is_some() || self.dialog_toplevels.contains(&surface);
        if dialog {
            return false;
        }
        // The rules name the window now, it has not been through them yet.
        self.assign_user_tag(&surface, dialog, false);
        let Some(key) = key_of(&surface) else {
            return false;
        };
        let Some(saved) = self.layouts.get(&key) else {
            return false;
        };
        tracing::info!(
            "layout: {:?} tag {:?} opens at {:?}",
            key.0,
            key.1,
            saved.pos
        );
        self.restore_saved(toplevel, &surface, saved);
        self.layouts.placed.insert(surface);
        true
    }

    /// Remember where every window with a tag of the user's is, when that changed. Runs before
    /// every frame and is cheap when nothing did. Nothing is written mid-drag or mid-resize,
    /// it is the end of that which is worth saving.
    pub(crate) fn layouts_flush(&mut self) {
        let surfaces: Vec<(WlSurface, bool)> = self
            .desk
            .all_top_down()
            .filter_map(|entry| {
                let toplevel = entry.window.toplevel()?;
                Some((toplevel.wl_surface().clone(), self.is_dialog(&entry.window)))
            })
            .collect();
        let mut seen = HashSet::new();
        for (surface, dialog) in surfaces {
            if dialog {
                continue;
            }
            let Some(key) = key_of(&surface) else {
                continue;
            };
            // Windows that share app id and tag share a place: only the topmost is saved, or
            // they would overwrite each other every frame.
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(snapshot) = self.session_snapshot(&surface) {
                self.layouts.remember(key, snapshot);
            }
        }
        if self.dragging_view.is_none() && self.resize.is_none() {
            self.layouts.save_if_dirty();
        }
    }

    /// A window is closing: save its place as it was, while the desk still has it.
    pub(crate) fn layout_toplevel_gone(&mut self, surface: &WlSurface) {
        self.layouts.placed.remove(surface);
        let Some(key) = key_of(surface) else {
            return;
        };
        let is_dialog = self
            .desk
            .all_top_down()
            .find(|entry| {
                entry
                    .window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .is_some_and(|entry| self.is_dialog(&entry.window));
        if is_dialog {
            return;
        }
        if let Some(snapshot) = self.session_snapshot(surface) {
            self.layouts.remember(key, snapshot);
        }
        self.layouts.save_if_dirty();
    }
}
