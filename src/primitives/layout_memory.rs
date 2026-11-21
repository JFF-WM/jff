//! How a container was arranged, kept so the same windows tiled again get the same shape.
//!
//! A container is remembered by the app ids of its windows (the sorted list, `kitty, kitty,
//! kitty`), not by the container: the container is gone when its windows are taken apart, and
//! what comes back later is a new one. An arrangement is the splits and their ratios (a
//! [`Template`] with only free slots) and which window stood in which slot. When windows with
//! the same app ids are tiled together again, [`LayoutMemory::recall`] says which template to
//! use and in which order to offer the windows to its slots: a window that was in the container
//! goes back to its own slot, the others take the slot of a window with the same app id.
//!
//! Pure: the window is a type `W` the caller picks, there are no Smithay types in here.

use crate::primitives::layout_template::Template;

/// Most arrangements kept. A new one past that pushes out the one remembered longest ago.
pub const MAX_REMEMBERED: usize = 32;

#[derive(Clone, Debug)]
struct Remembered<W> {
    /// The app ids of the windows, sorted.
    key: Vec<String>,
    template: Template,
    /// The windows in the order of the template's slots, with their app ids.
    leaves: Vec<(W, String)>,
}

/// The arrangements remembered so far, the newest last.
#[derive(Clone, Debug)]
pub struct LayoutMemory<W> {
    entries: Vec<Remembered<W>>,
}

impl<W> Default for LayoutMemory<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

/// What [`LayoutMemory::recall`] found.
#[derive(Clone, Debug, PartialEq)]
pub struct Recalled<'a, W> {
    pub template: &'a Template,
    /// Every window asked about, in the order to offer them to the template's slots.
    pub order: Vec<W>,
}

/// The sorted app ids of `windows`. `None` for fewer than two windows, nothing is arranged in
/// one, and for a window with no app id, which says nothing about what it is.
fn key_of<W>(windows: &[(W, String)]) -> Option<Vec<String>> {
    if windows.len() < 2 || windows.iter().any(|(_, app)| app.is_empty()) {
        return None;
    }
    let mut key: Vec<String> = windows.iter().map(|(_, app)| app.clone()).collect();
    key.sort();
    Some(key)
}

impl<W: Copy + Eq> LayoutMemory<W> {
    /// Remember `template` for these windows, in the order of its slots, replacing what was
    /// remembered for the same app ids. Returns false, remembering nothing, for fewer than two
    /// windows, a window with no app id, and a template whose slots are not one per window.
    pub fn remember(&mut self, template: Template, leaves: Vec<(W, String)>) -> bool {
        let Some(key) = key_of(&leaves) else {
            return false;
        };
        if template.slots().len() != leaves.len() {
            return false;
        }
        self.entries.retain(|entry| entry.key != key);
        self.entries.push(Remembered {
            key,
            template,
            leaves,
        });
        if self.entries.len() > MAX_REMEMBERED {
            self.entries.remove(0);
        }
        true
    }

    /// The arrangement remembered for windows with these app ids, `None` when there is none.
    ///
    /// `order` holds every one of `windows`. Slot by slot, a window that was remembered in that
    /// slot is put there first, a slot nobody of the remembered windows is left for is given to
    /// the first window that has the app id the slot had.
    pub fn recall(&self, windows: &[(W, String)]) -> Option<Recalled<'_, W>> {
        let key = key_of(windows)?;
        let entry = self.entries.iter().find(|entry| entry.key == key)?;
        let mut slots: Vec<Option<W>> = vec![None; entry.leaves.len()];
        let mut left: Vec<(W, &str)> = windows
            .iter()
            .map(|(window, app)| (*window, app.as_str()))
            .collect();
        for (slot, (wanted, _)) in slots.iter_mut().zip(&entry.leaves) {
            if let Some(at) = left.iter().position(|(window, _)| window == wanted) {
                *slot = Some(left.remove(at).0);
            }
        }
        for (slot, (_, app)) in slots.iter_mut().zip(&entry.leaves) {
            if slot.is_some() {
                continue;
            }
            if let Some(at) = left.iter().position(|(_, other)| *other == app.as_str()) {
                *slot = Some(left.remove(at).0);
            }
        }
        let order = slots
            .into_iter()
            .flatten()
            .chain(left.into_iter().map(|(window, _)| window))
            .collect();
        Some(Recalled {
            template: &entry.template,
            order,
        })
    }

    /// How many arrangements are remembered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
