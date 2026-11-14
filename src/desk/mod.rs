//! The desk: an ordered stack of windows with positions.
//!
//! No Smithay/Wayland types in here, on purpose, the stacking policy can be tested
//! without a compositor. `W` is whatever handle the caller uses for a window.
//!
//! The desk also keeps the tiling containers (see [`container`]). A window is in at most one.
//! The members of a container are next to each other in the stack, in tree order from the
//! bottom up, so [`Desk::top_down`], [`Desk::all_top_down`] and [`Desk::iter_mut`] walk them
//! like any other window, the first leaf first when going bottom up and last when going top
//! down. A member does not own its position: [`Desk::tile_container`] derives it from the tree.

pub mod container;
pub mod layout_store;
pub mod session_store;

use std::collections::HashMap;

pub use container::{Container, ContainerError, ContainerId, LayoutApplied, WindowId};

use crate::primitives::container_drop::{
    axis_for, in_rect, snapped_is_first, split_axis, LeafDrop,
};
use crate::primitives::layout_template::Template;
use crate::primitives::tile_tree::{Axis, Edge, Rect, Removal, SplitError, TileTree};

pub type Pos = (i32, i32);

/// Where a workspace request leads: a specific workspace, or the neighbour of the current one
/// (wraps around).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goto {
    /// A workspace by index, 0-based. Too big a number means the last workspace.
    To(usize),
    Next,
    Prev,
}

/// The workspace `goto` leads to, out of `count` workspaces, when `current` is the one shown.
pub fn resolve(goto: Goto, current: usize, count: usize) -> usize {
    let count = count.max(1);
    match goto {
        Goto::To(n) => n.min(count - 1),
        Goto::Next => (current % count + 1) % count,
        Goto::Prev => (current % count + count - 1) % count,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Floating,
    Maximized,
    Fullscreen,
    Snapped(Side),
}

pub struct Entry<W> {
    pub window: W,
    pub pos: Pos,
    /// The workspace the window lives on (0-based). Windows on other workspaces aren't shown,
    /// drawn, or reachable with the pointer.
    pub workspace: usize,
    pub mode: WindowMode,
    pub before_fullscreen: WindowMode,
    /// Where the window floats back to. `Some` unless it's floating.
    pub restore: Option<(Pos, (i32, i32))>,
    /// Whether the window's first real size has already been checked against the output. Set
    /// once, see `mark_fitted_where`.
    pub fitted: bool,
    /// When the window first had something to show (the same moment `fitted` is set). The open
    /// animation runs from here, not from `add`: a client can take 100 ms to send its first
    /// frame, and the fade would be over by then.
    pub shown: Option<std::time::Instant>,
    /// The monitor the window lives on, by name (empty: none yet). Only matters with
    /// per-output workspaces, where a window is shown when this monitor shows its workspace.
    /// The caller keeps it up to date, see [`Desk::assign_outputs`].
    pub output: String,
    /// Names this window in a container's layout tree. Given by the desk when the window is
    /// added, never changes.
    pub id: WindowId,
    /// The container the window is a member of, if any. A member's `workspace` and `output` are
    /// those of its container, and its `pos` is derived from the container's tree
    /// ([`Desk::tile_container`]).
    pub container: Option<ContainerId>,
}

pub struct Desk<W> {
    /// Bottom of the stack first, topmost window last. One stack for every workspace: the
    /// order of the windows of a workspace is the order they have here.
    entries: Vec<Entry<W>>,
    /// The workspace that's shown. With per-output workspaces: the one a monitor shows until it
    /// gets its own.
    current: usize,
    /// Every monitor has its own shown workspace (`shown`) instead of all of them showing
    /// `current`.
    per_output: bool,
    /// The workspace each monitor shows, by name, with per-output workspaces. A monitor that's
    /// not in here shows `current`. Kept for monitors that are unplugged, they pick up where
    /// they were when plugged back in.
    shown: HashMap<String, usize>,
    /// The tiling containers, oldest first. Every member of one is an entry of `entries`, and
    /// every entry with a `container` is a member of one of these.
    containers: Vec<Container>,
    /// The number the next window and the next container get.
    next_window: u64,
    next_container: u64,
}

impl<W> Default for Desk<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            current: 0,
            per_output: false,
            shown: HashMap::new(),
            containers: Vec::new(),
            next_window: 0,
            next_container: 0,
        }
    }
}

impl<W> Desk<W> {
    /// Add a window on top of the current workspace, cascading so new windows don't hide each
    /// other. Returns the position it was given.
    pub fn add(&mut self, window: W) -> Pos {
        self.add_on(window, "")
    }

    /// [`Desk::add`] on the monitor `output`: with per-output workspaces the window goes on the
    /// workspace that monitor shows, and cascades among the windows there.
    pub fn add_on(&mut self, window: W, output: &str) -> Pos {
        let workspace = self.current_on(output);
        let per_output = self.per_output;
        let on_workspace = self
            .entries
            .iter()
            .filter(|e| e.workspace == workspace && (!per_output || e.output == output))
            .count();
        let step = (on_workspace % 10) as i32;
        let pos = (40 + step * 32, 40 + step * 32);
        let id = WindowId(self.next_window);
        self.next_window += 1;
        self.entries.push(Entry {
            window,
            pos,
            workspace,
            mode: WindowMode::Floating,
            before_fullscreen: WindowMode::Floating,
            restore: None,
            fitted: false,
            shown: None,
            output: output.to_owned(),
            id,
            container: None,
        });
        pos
    }

    /// The workspace that's shown (0-based). With per-output workspaces this is only what a
    /// monitor without its own shows, ask [`Desk::current_on`] instead.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Show another workspace on every monitor (or, with per-output workspaces, on those that
    /// don't have their own). Doesn't touch the windows.
    pub fn set_current(&mut self, workspace: usize) {
        self.current = workspace;
    }

    /// Whether every monitor has its own workspace.
    pub fn per_output(&self) -> bool {
        self.per_output
    }

    /// Switch between one workspace for all monitors and one per monitor. Turning it on, every
    /// monitor keeps showing what is shown now. Turning it off, every monitor shows what
    /// `active` (the monitor that's in use) showed.
    pub fn set_per_output(&mut self, on: bool, active: &str) {
        if on == self.per_output {
            return;
        }
        if !on {
            self.current = self.current_on(active);
        }
        self.per_output = on;
        self.shown.clear();
    }

    /// The workspace shown on the monitor `output` (0-based).
    pub fn current_on(&self, output: &str) -> usize {
        if self.per_output {
            self.shown.get(output).copied().unwrap_or(self.current)
        } else {
            self.current
        }
    }

    /// Show a workspace on the monitor `output`. Everywhere without per-output workspaces.
    /// Doesn't touch the windows.
    pub fn set_current_on(&mut self, output: &str, workspace: usize) {
        if self.per_output {
            self.shown.insert(output.to_owned(), workspace);
        } else {
            self.current = workspace;
        }
    }

    /// Whether `entry` is on the workspace that's shown where it is.
    pub fn is_visible(&self, entry: &Entry<W>) -> bool {
        entry.workspace == self.current_on(&entry.output)
    }

    /// With per-output workspaces: put every window on the monitor `home` names for it (`None`:
    /// leave it where it is). A window that was shown and moved to another monitor goes on the
    /// workspace that monitor shows, so it doesn't disappear on the way. A hidden one keeps its
    /// workspace. Does nothing without per-output workspaces.
    pub fn assign_outputs(&mut self, mut home: impl FnMut(&Entry<W>) -> Option<String>) {
        if !self.per_output {
            return;
        }
        for i in 0..self.entries.len() {
            // A member goes where its container goes, see below.
            if self.entries[i].container.is_some() {
                continue;
            }
            let Some(name) = home(&self.entries[i]) else {
                continue;
            };
            if self.entries[i].output == name {
                continue;
            }
            let was_shown = self.is_visible(&self.entries[i]);
            let target = self.current_on(&name);
            let entry = &mut self.entries[i];
            if was_shown {
                entry.workspace = target;
            }
            entry.output = name;
        }
        // A container moves as one, `home` is asked about its first window.
        for ci in 0..self.containers.len() {
            let Some(first) = self
                .containers
                .get(ci)
                .and_then(|c| c.tree.leaves().first().copied())
            else {
                continue;
            };
            let Some(name) = self
                .entries
                .iter()
                .find(|e| e.id == first)
                .and_then(|e| home(e))
            else {
                continue;
            };
            let Some(c) = self.containers.get(ci) else {
                continue;
            };
            if c.output == name {
                continue;
            }
            let was_shown = c.workspace == self.current_on(&c.output);
            let target = self.current_on(&name);
            if let Some(c) = self.containers.get_mut(ci) {
                if was_shown {
                    c.workspace = target;
                }
                c.output = name;
            }
            self.sync_members(ci);
        }
    }

    /// How many windows live on `workspace`.
    pub fn count_on(&self, workspace: usize) -> usize {
        self.entries
            .iter()
            .filter(|e| e.workspace == workspace)
            .count()
    }

    /// Where `steps` workspaces from `from` lands on monitor `output` when workspaces without
    /// windows don't count: negative goes down, positive up. Walks past empty workspaces, and
    /// stops at the last one with windows when there are fewer than asked for. `from` itself may
    /// be empty. Stays at `from` when nothing in that direction has a window. Without
    /// per-output workspaces the monitor makes no difference.
    pub fn step_occupied(&self, output: &str, from: usize, steps: i32, count: usize) -> usize {
        let last = count.max(1) - 1;
        let occupied = |workspace: usize| {
            self.entries
                .iter()
                .any(|e| e.workspace == workspace && (!self.per_output || e.output == output))
        };
        let mut at = from.min(last);
        let mut left = steps.unsigned_abs();
        while left > 0 {
            let next = if steps < 0 {
                match (0..at).rev().find(|&w| occupied(w)) {
                    Some(w) => w,
                    None => break,
                }
            } else {
                match (at + 1..=last).find(|&w| occupied(w)) {
                    Some(w) => w,
                    None => break,
                }
            };
            at = next;
            left -= 1;
        }
        at
    }

    /// The monitor and the workspace of the first window matching `pred`.
    pub fn home_of(&self, pred: impl Fn(&W) -> bool) -> Option<(String, usize)> {
        self.entries
            .iter()
            .find(|e| pred(&e.window))
            .map(|e| (e.output.clone(), e.workspace))
    }

    /// The workspace of the first window matching `pred`.
    pub fn workspace_of(&self, pred: impl Fn(&W) -> bool) -> Option<usize> {
        self.entries
            .iter()
            .find(|e| pred(&e.window))
            .map(|e| e.workspace)
    }

    /// Send the first window matching `pred` to `workspace`, on top of it. Returns false when
    /// there's no such window. A member of a container takes its whole container along, see
    /// [`Desk::move_container_to_workspace`], to send it alone, take it out first
    /// ([`Desk::leave_container`]).
    pub fn move_to_workspace(&mut self, pred: impl Fn(&W) -> bool, workspace: usize) -> bool {
        let Some(i) = self.entries.iter().position(|e| pred(&e.window)) else {
            return false;
        };
        if let Some(container) = self.entries.get(i).and_then(|e| e.container) {
            return self.move_container_to_workspace(container, workspace);
        }
        let mut entry = self.entries.remove(i);
        entry.workspace = workspace;
        self.entries.push(entry);
        true
    }

    /// There are only `count` workspaces now: windows beyond them (and the shown workspace)
    /// move to the last one.
    pub fn clamp_workspaces(&mut self, count: usize) {
        let last = count.max(1) - 1;
        for entry in &mut self.entries {
            entry.workspace = entry.workspace.min(last);
        }
        // The same number as that of its members, so they stay together.
        for container in &mut self.containers {
            container.workspace = container.workspace.min(last);
        }
        self.current = self.current.min(last);
        for shown in self.shown.values_mut() {
            *shown = (*shown).min(last);
        }
    }

    /// Windows of the current workspace, topmost first - the order renderers want. Windows on
    /// other workspaces are skipped, use [`Desk::all_top_down`] for those.
    pub fn top_down(&self) -> impl Iterator<Item = &Entry<W>> {
        self.entries
            .iter()
            .rev()
            .filter(move |e| self.is_visible(e))
    }

    /// Every window of every workspace, topmost first.
    pub fn all_top_down(&self) -> impl Iterator<Item = &Entry<W>> {
        self.entries.iter().rev()
    }

    /// Every entry, bottom of the stack first, open to changes. Note the direction: this walks
    /// the storage vector as-is, which is the *opposite* of `top_down` / `all_top_down`.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry<W>> {
        self.entries.iter_mut()
    }

    pub fn find_by(&self, pred: impl Fn(&W) -> bool) -> Option<&Entry<W>> {
        self.entries.iter().find(|e| pred(&e.window))
    }

    /// Position of the first window matching `pred`.
    pub fn pos_of(&self, pred: impl Fn(&W) -> bool) -> Option<Pos> {
        self.entries.iter().find(|e| pred(&e.window)).map(|e| e.pos)
    }

    /// Returns true the first time it's called for the matching window, false afterwards.
    pub fn mark_fitted_where(&mut self, pred: impl Fn(&W) -> bool) -> bool {
        match self.entries.iter_mut().find(|e| pred(&e.window)) {
            Some(e) if !e.fitted => {
                e.fitted = true;
                e.shown = Some(std::time::Instant::now());
                true
            }
            _ => false,
        }
    }

    pub fn mode_of(&self, pred: impl Fn(&W) -> bool) -> WindowMode {
        self.find_by(pred).map(|e| e.mode).unwrap_or_default()
    }

    pub fn is_floating(&self, pred: impl Fn(&W) -> bool) -> bool {
        self.mode_of(pred) == WindowMode::Floating
    }

    /// Whether the window is maximized: in that mode, or the maximized member of a container
    /// (see [`Desk::zoom_in_container`]).
    pub fn is_maximized(&self, pred: impl Fn(&W) -> bool) -> bool {
        self.find_by(pred).is_some_and(|e| {
            e.mode == WindowMode::Maximized
                || e.container
                    .and_then(|id| self.container(id))
                    .is_some_and(|c| c.zoomed == Some(e.id))
        })
    }

    pub fn mode_after_fullscreen(&self, pred: impl Fn(&W) -> bool) -> WindowMode {
        match self.find_by(pred) {
            Some(e) if e.mode == WindowMode::Fullscreen => e.before_fullscreen,
            Some(e) => e.mode,
            None => WindowMode::Floating,
        }
    }

    /// What floats back is only taken while the window floats, so mode to mode keeps it.
    pub fn set_mode_where(
        &mut self,
        pred: impl Fn(&W) -> bool,
        mode: WindowMode,
        area_pos: Pos,
        size: (i32, i32),
    ) -> bool {
        if mode == WindowMode::Floating {
            return false;
        }
        let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) else {
            return false;
        };
        if e.mode == WindowMode::Floating {
            e.restore = Some((e.pos, size));
        }
        if mode == WindowMode::Fullscreen && e.mode != WindowMode::Fullscreen {
            e.before_fullscreen = e.mode;
        }
        e.mode = mode;
        e.pos = area_pos;
        true
    }

    pub fn float_where(&mut self, pred: impl Fn(&W) -> bool) -> Option<(i32, i32)> {
        let e = self.entries.iter_mut().find(|e| pred(&e.window))?;
        let (pos, size) = e.restore.take()?;
        e.pos = pos;
        e.mode = WindowMode::Floating;
        e.before_fullscreen = WindowMode::Floating;
        Some(size)
    }

    pub fn float_in_place_where(&mut self, pred: impl Fn(&W) -> bool) -> Option<(Pos, (i32, i32))> {
        let e = self.entries.iter_mut().find(|e| pred(&e.window))?;
        e.mode = WindowMode::Floating;
        e.before_fullscreen = WindowMode::Floating;
        e.restore.take()
    }

    pub fn set_restore_where(&mut self, pred: impl Fn(&W) -> bool, restore: (Pos, (i32, i32))) {
        if let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) {
            if e.restore.is_some() {
                e.restore = Some(restore);
            }
        }
    }

    /// Put the first window matching `pred` on the monitor `output`. With per-output
    /// workspaces it lands on the workspace that monitor shows, so it stays in view. Returns
    /// false when there's no such window. A member of a container takes its whole container
    /// along.
    pub fn set_output_where(&mut self, pred: impl Fn(&W) -> bool, output: &str) -> bool {
        let shown = self.current_on(output);
        let per_output = self.per_output;
        let Some(i) = self.entries.iter().position(|e| pred(&e.window)) else {
            return false;
        };
        if let Some(container) = self.entries.get(i).and_then(|e| e.container) {
            if let Some(ci) = self.container_index(container) {
                if let Some(c) = self.containers.get_mut(ci) {
                    c.output = output.to_owned();
                    if per_output {
                        c.workspace = shown;
                    }
                }
                self.sync_members(ci);
            }
            return true;
        }
        let Some(e) = self.entries.get_mut(i) else {
            return false;
        };
        e.output = output.to_owned();
        if per_output {
            e.workspace = shown;
        }
        true
    }

    /// Place the first window matching `pred` at `pos`. A member of a container is left where
    /// it is: its position comes from the container's tree ([`Desk::tile_container`]).
    pub fn set_pos_where(&mut self, pred: impl Fn(&W) -> bool, pos: Pos) {
        if let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) {
            if e.container.is_none() {
                e.pos = pos;
            }
        }
    }

    pub fn set_restore_pos_where(&mut self, pred: impl Fn(&W) -> bool, pos: Pos) {
        if let Some(restore) = self
            .entries
            .iter_mut()
            .find(|e| pred(&e.window))
            .and_then(|e| e.restore.as_mut())
        {
            restore.0 = pos;
        }
    }

    /// Take every window matching `pred` off the desk. A member leaves the tree of its
    /// container, which closes up around it, and goes with its last window.
    pub fn remove_where(&mut self, pred: impl Fn(&W) -> bool) {
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut gone = Vec::new();
        for entry in std::mem::take(&mut self.entries) {
            if pred(&entry.window) {
                if let Some(container) = entry.container {
                    gone.push((container, entry.id));
                }
            } else {
                kept.push(entry);
            }
        }
        self.entries = kept;
        for (container, window) in gone {
            self.drop_leaf(container, window);
        }
    }

    /// Move the first window matching `pred` to the top of the stack. A member of a container
    /// takes its whole container up with it, the members stay together in tree order.
    pub fn raise_where(&mut self, pred: impl Fn(&W) -> bool) {
        if let Some(i) = self.entries.iter().position(|e| pred(&e.window)) {
            if let Some(container) = self.entries.get(i).and_then(|e| e.container) {
                self.gather_members(container, true);
                return;
            }
            let entry = self.entries.remove(i);
            self.entries.push(entry);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Tiling containers.
impl<W> Desk<W> {
    /// Every container, oldest first.
    pub fn containers(&self) -> impl Iterator<Item = &Container> {
        self.containers.iter()
    }

    pub fn container(&self, id: ContainerId) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }

    /// The container the first window matching `pred` is a member of. `None` when there is no
    /// such window or it is in none.
    pub fn container_of(&self, pred: impl Fn(&W) -> bool) -> Option<ContainerId> {
        self.entries
            .iter()
            .find(|e| pred(&e.window))
            .and_then(|e| e.container)
    }

    /// The window `id` names.
    pub fn find_by_id(&self, id: WindowId) -> Option<&Entry<W>> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The members of a container in tree order, empty when there is no such container.
    pub fn members(&self, id: ContainerId) -> Vec<&Entry<W>> {
        let Some(container) = self.container(id) else {
            return Vec::new();
        };
        container
            .tree
            .leaves()
            .into_iter()
            .filter_map(|window| self.find_by_id(window))
            .collect()
    }

    /// Start a container with the first window matching `pred` as its only member. It sits
    /// where the window is: its workspace and its monitor. The window keeps its mode, what a
    /// member may be is for the caller to decide.
    pub fn create_container(
        &mut self,
        pred: impl Fn(&W) -> bool,
        name: Option<String>,
    ) -> Result<ContainerId, ContainerError> {
        let Some(entry) = self.entries.iter_mut().find(|e| pred(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        if entry.container.is_some() {
            return Err(ContainerError::AlreadyInContainer);
        }
        let id = ContainerId(self.next_container);
        self.next_container += 1;
        entry.container = Some(id);
        self.containers.push(Container {
            id,
            name,
            tree: TileTree::new(entry.id),
            focused: entry.id,
            workspace: entry.workspace,
            output: entry.output.clone(),
            background: None,
            area: None,
            zoomed: None,
        });
        Ok(id)
    }

    /// Start a container for a window dropped beside one that is snapped to `side`: the first
    /// window matching `snapped` and the first matching `dragged` become its two windows, split
    /// along the axis of `side` so that each stays on its own side (the snapped one is the left
    /// or top half for `Left` and `Top`, the dragged one for `Right` and `Bottom`). The
    /// container sits where the snapped window is, its workspace and its monitor, and the
    /// dragged window joins it there. The focus inside it is on the dragged window.
    ///
    /// The windows keep their mode, as in [`Desk::create_container`]. It fails, and changes
    /// nothing, when a window is missing, they are the same one, or either is a member of a
    /// container already.
    pub fn create_container_beside(
        &mut self,
        snapped: impl Fn(&W) -> bool,
        dragged: impl Fn(&W) -> bool,
        side: Side,
        name: Option<String>,
    ) -> Result<ContainerId, ContainerError> {
        let Some(snapped) = self.entries.iter().find(|e| snapped(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let Some(dragged) = self.entries.iter().find(|e| dragged(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        if snapped.id == dragged.id {
            return Err(ContainerError::SameWindow);
        }
        if snapped.container.is_some() || dragged.container.is_some() {
            return Err(ContainerError::AlreadyInContainer);
        }
        let (snapped_id, dragged_id) = (snapped.id, dragged.id);
        let (workspace, output) = (snapped.workspace, snapped.output.clone());
        let (first, second) = if snapped_is_first(side) {
            (snapped_id, dragged_id)
        } else {
            (dragged_id, snapped_id)
        };
        let mut tree = TileTree::new(first);
        tree.split(first, axis_for(side), second)
            .map_err(|_| ContainerError::AlreadyInContainer)?;

        let id = ContainerId(self.next_container);
        self.next_container += 1;
        for entry in &mut self.entries {
            if entry.id == snapped_id || entry.id == dragged_id {
                entry.container = Some(id);
            }
        }
        self.containers.push(Container {
            id,
            name,
            tree,
            focused: dragged_id,
            workspace,
            output,
            background: None,
            area: None,
            zoomed: None,
        });
        let ci = self.containers.len() - 1;
        self.sync_members(ci);
        self.gather_members(id, false);
        Ok(id)
    }

    /// Split the member of `container` matching `at` along `axis` and put the window matching
    /// `new` in the second half. `new` joins the container: it moves to its workspace and
    /// monitor, and takes its place in the stack next to the other members. The focus inside
    /// the container stays where it was.
    pub fn split_container(
        &mut self,
        container: ContainerId,
        at: impl Fn(&W) -> bool,
        axis: Axis,
        new: impl Fn(&W) -> bool,
    ) -> Result<(), ContainerError> {
        if self.container_index(container).is_none() {
            return Err(ContainerError::NoSuchContainer);
        }
        let Some(at) = self.entries.iter().find(|e| at(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        if at.container != Some(container) {
            return Err(ContainerError::NotInContainer);
        }
        let at = at.id;
        let Some(new) = self.entries.iter().find(|e| new(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let new = new.id;
        self.split_ids(container, at, axis, new)
    }

    /// [`Desk::split_container`] for windows named by id.
    fn split_ids(
        &mut self,
        container: ContainerId,
        at: WindowId,
        axis: Axis,
        new: WindowId,
    ) -> Result<(), ContainerError> {
        let Some(ci) = self.container_index(container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        let Some(at_entry) = self.find_by_id(at) else {
            return Err(ContainerError::NoSuchWindow);
        };
        if at_entry.container != Some(container) {
            return Err(ContainerError::NotInContainer);
        }
        let Some(new_entry) = self.find_by_id(new) else {
            return Err(ContainerError::NoSuchWindow);
        };
        if new_entry.container.is_some() {
            return Err(ContainerError::AlreadyInContainer);
        }
        let split = match self.containers.get_mut(ci) {
            Some(c) => c.tree.split(at, axis, new),
            None => return Err(ContainerError::NoSuchContainer),
        };
        split.map_err(|e| match e {
            SplitError::NoSuchLeaf => ContainerError::NotInContainer,
            SplitError::Duplicate => ContainerError::AlreadyInContainer,
        })?;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == new) {
            entry.container = Some(container);
        }
        self.sync_members(ci);
        self.gather_members(container, false);
        Ok(())
    }

    /// The container a window that just opened joins, if any: the one the window matching
    /// `focus` (what has the keyboard) is a member of. It has to be shown, on the same workspace
    /// and monitor as the window matching `new`, laid out already, and not have the focused
    /// member fullscreen, which keeps the monitor for itself. `new` must be in no container.
    /// Any kind of window joins, the desk does not look at what it is.
    pub fn container_to_join(
        &self,
        focus: impl Fn(&W) -> bool,
        new: impl Fn(&W) -> bool,
    ) -> Option<ContainerId> {
        let focus = self.entries.iter().find(|e| focus(&e.window))?;
        let new = self.entries.iter().find(|e| new(&e.window))?;
        let container = self.container(focus.container?)?;
        if new.container.is_some() || container.area.is_none() {
            return None;
        }
        if new.workspace != container.workspace || new.output != container.output {
            return None;
        }
        if self.current_on(&container.output) != container.workspace {
            return None;
        }
        let fullscreen = |id: WindowId| {
            self.find_by_id(id)
                .is_some_and(|e| e.mode == WindowMode::Fullscreen)
        };
        if fullscreen(focus.id) || fullscreen(container.focused) {
            return None;
        }
        Some(container.id)
    }

    /// The shown container whose area holds `p`, the topmost one when several do (containers
    /// that sit on the same monitor and workspace fill the same room). `None` for a point over
    /// no container, or over one that is not laid out or not shown.
    pub fn container_at(&self, p: (f64, f64)) -> Option<ContainerId> {
        self.top_down().find_map(|entry| {
            let container = self.container(entry.container?)?;
            in_rect(container.area?, p).then_some(container.id)
        })
    }

    /// The member of `container` a window dropped at `p` is split off: the maximized one when
    /// there is one, because it covers the others, else the one whose rectangle holds `p` when
    /// the tree is laid out in the container's area with `gap` px between the windows, else (`p`
    /// is in a gap, or the container has no area) the focused one. `None` when there is no such
    /// container.
    pub fn leaf_at(&self, container: ContainerId, p: (f64, f64), gap: i32) -> Option<WindowId> {
        let c = self.container(container)?;
        if let Some(zoomed) = c.zoomed {
            return Some(zoomed);
        }
        let under = c.area.and_then(|area| {
            c.tree
                .layout(area, gap)
                .into_iter()
                .find(|(_, rect)| in_rect(*rect, p))
                .map(|(window, _)| window)
        });
        Some(under.unwrap_or(c.focused))
    }

    /// Put the window matching `new` into `container`: it is split off the member `at` (the
    /// focused one when `None`) along the longer side of that member's rectangle (see
    /// [`split_axis`]) and goes in the second half, so what is there keeps its side. `new`
    /// moves to the workspace and monitor of the container and takes its place in the stack
    /// next to the other members. It does not float, snap or maximize any more, its place comes
    /// from the tree, and it becomes the focused member of the container, and the maximized one
    /// when a member is maximized, so it is what is shown. The caller lays the container out
    /// again ([`Desk::tile_container`]).
    ///
    /// Fails, and changes nothing, when there is no such container or window, `at` is not a
    /// member of the container, or `new` is a member of a container already.
    pub fn join_container(
        &mut self,
        container: ContainerId,
        at: Option<WindowId>,
        new: impl Fn(&W) -> bool,
        gap: i32,
    ) -> Result<(), ContainerError> {
        let Some(c) = self.container(container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        let at = at.unwrap_or(c.focused);
        let Some(new) = self.entries.iter().find(|e| new(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let new = new.id;
        self.join_ids(container, at, new, None, gap)
    }

    /// [`Desk::join_container`] for windows named by id. With `beside` the window goes on that
    /// side of `at` (left and top are the first half) and not along the longer side of it.
    fn join_ids(
        &mut self,
        container: ContainerId,
        at: WindowId,
        new: WindowId,
        beside: Option<Side>,
        gap: i32,
    ) -> Result<(), ContainerError> {
        let Some(c) = self.container(container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        // The rectangle the member has now, not the maximized one: the tree is what gets cut.
        let axis = match beside {
            Some(side) => axis_for(side),
            None => c
                .area
                .and_then(|area| {
                    c.tree
                        .layout(area, gap)
                        .into_iter()
                        .find(|(window, _)| *window == at)
                        .map(|(_, rect)| split_axis(rect))
                })
                .unwrap_or(Axis::Horizontal),
        };
        self.split_ids(container, at, axis, new)?;
        if let Some(c) = self.containers.iter_mut().find(|c| c.id == container) {
            // The new window is the second half, a drop on the left or top edge wants it first.
            if beside.is_some_and(snapped_is_first) {
                c.tree.swap_leaves(at, new);
            }
            c.focused = new;
            if c.zoomed.is_some() {
                c.zoomed = Some(new);
            }
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == new) {
            entry.mode = WindowMode::Floating;
            entry.before_fullscreen = WindowMode::Floating;
            entry.restore = None;
        }
        // The maximized member is the topmost of them, and the order in the tree is the order
        // in the stack.
        self.gather_members(container, false);
        Ok(())
    }

    /// Rebuilds the tree of `container` from `template`, windows matched to slots by `tag` (see [`Template::fill`]), extras split off the last leaf.
    pub fn apply_container_layout(
        &mut self,
        container: ContainerId,
        template: &Template,
        tag: impl Fn(&W) -> Option<String>,
        gap: i32,
    ) -> Result<LayoutApplied, ContainerError> {
        let Some(ci) = self.container_index(container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        let (order, area) = match self.containers.get(ci) {
            Some(c) => (c.tree.leaves(), c.area),
            None => return Err(ContainerError::NoSuchContainer),
        };
        let windows: Vec<(WindowId, Option<String>)> = order
            .iter()
            .filter_map(|id| self.find_by_id(*id).map(|e| (*id, tag(&e.window))))
            .collect();
        let Some(fill) = template.fill(&windows) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let mut tree = TileTree::from_root(fill.root);
        for window in &fill.extra {
            let Some(at) = tree.leaves().last().copied() else {
                break;
            };
            let axis = area
                .and_then(|area| {
                    tree.layout(area, gap)
                        .into_iter()
                        .find(|(leaf, _)| *leaf == at)
                        .map(|(_, rect)| split_axis(rect))
                })
                .unwrap_or(Axis::Horizontal);
            // A window is never a leaf twice, so this cannot fail.
            let _ = tree.split(at, axis, *window);
        }
        if let Some(c) = self.containers.get_mut(ci) {
            c.tree = tree;
        }
        self.gather_members(container, false);
        Ok(LayoutApplied {
            slots: template.slots().len(),
            empty: fill.empty,
            extra: fill.extra.len(),
        })
    }

    /// Where a member dropped at `p` on `container` lands: the leaf under `p` and its rectangle
    /// when the tree is laid out in the container's area with `gap` px between the windows.
    /// With a maximized member that is the maximized one, and the whole area, because it covers
    /// the others. `None` for a point in a gap or outside the container, and for a container
    /// that is gone or not laid out.
    pub fn drop_target(
        &self,
        container: ContainerId,
        p: (f64, f64),
        gap: i32,
    ) -> Option<(WindowId, Rect)> {
        let c = self.container(container)?;
        let area = c.area?;
        if let Some(zoomed) = c.zoomed {
            return in_rect(area, p).then_some((zoomed, area));
        }
        c.tree
            .layout(area, gap)
            .into_iter()
            .find(|(_, rect)| in_rect(*rect, p))
    }

    /// Move the member matching `window` onto the member `target` of `container`, the way `drop`
    /// says (see [`leaf_drop`](crate::primitives::container_drop::leaf_drop)). Within its own
    /// container the window takes the place of `target` or goes beside it, and the tree closes up
    /// around where it was. From another container it leaves that one (which goes with its last
    /// window) and joins this, beside `target` for an edge, and for the middle split off it along
    /// its longer side, as a window that is in none does ([`Desk::join_container`]). Dropping a
    /// window on itself changes nothing. It becomes the focused member of the container, and the
    /// maximized one if a member is. The caller lays the container out again, and the container
    /// it left too when that is another one.
    ///
    /// Fails, and changes nothing, when there is no such container or window, `window` is in no
    /// container (it joins with [`Desk::join_container`]), or `target` is not a member of
    /// `container`.
    pub fn move_member(
        &mut self,
        window: impl Fn(&W) -> bool,
        container: ContainerId,
        target: WindowId,
        drop: LeafDrop,
        gap: i32,
    ) -> Result<(), ContainerError> {
        if self.container_index(container).is_none() {
            return Err(ContainerError::NoSuchContainer);
        }
        let Some(moved) = self.entries.iter().find(|e| window(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let Some(from) = moved.container else {
            return Err(ContainerError::NotInContainer);
        };
        let moved = moved.id;
        match self.find_by_id(target) {
            None => return Err(ContainerError::NoSuchWindow),
            Some(entry) if entry.container != Some(container) => {
                return Err(ContainerError::NotInContainer)
            }
            Some(_) => {}
        }
        if moved == target {
            return Ok(());
        }
        let beside = match drop {
            LeafDrop::Beside(side) => Some(side),
            LeafDrop::Swap => None,
        };
        if from != container {
            // Out of the old container, which closes up, and into this one.
            self.leave_container(|w| window(w));
            return self.join_ids(container, target, moved, beside, gap);
        }
        let Some(c) = self.containers.iter_mut().find(|c| c.id == container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        match beside {
            None => {
                c.tree.swap_leaves(moved, target);
            }
            Some(side) => {
                let before = c.tree.clone();
                c.tree.remove(moved);
                if c.tree.split(target, axis_for(side), moved).is_err() {
                    // Not reachable, the target is in the tree. Keep the tree as it was
                    // rather than lose the window.
                    c.tree = before;
                    return Err(ContainerError::NotInContainer);
                }
                if snapped_is_first(side) {
                    c.tree.swap_leaves(target, moved);
                }
            }
        }
        c.focused = moved;
        self.gather_members(container, false);
        Ok(())
    }

    /// Take the first window matching `pred` out of its container: the tree closes up around
    /// it, and the container goes if it was the last one. The window stays on the desk where it
    /// was laid out, on top of the stack, with the mode it had: what it becomes is for the
    /// caller to decide. Returns false when there is no such window or it is in no container.
    pub fn leave_container(&mut self, pred: impl Fn(&W) -> bool) -> bool {
        let Some(i) = self.entries.iter().position(|e| pred(&e.window)) else {
            return false;
        };
        let mut entry = self.entries.remove(i);
        let Some(container) = entry.container.take() else {
            self.entries.insert(i, entry);
            return false;
        };
        let window = entry.id;
        self.entries.push(entry);
        self.drop_leaf(container, window);
        true
    }

    /// Make the first window matching `pred` the focused one of its container. Returns false
    /// when there is no such window or it is in no container.
    pub fn set_container_focus(&mut self, pred: impl Fn(&W) -> bool) -> bool {
        let Some((container, window)) = self
            .entries
            .iter()
            .find(|e| pred(&e.window))
            .and_then(|e| e.container.map(|c| (c, e.id)))
        else {
            return false;
        };
        match self.containers.iter_mut().find(|c| c.id == container) {
            Some(c) => {
                c.focused = window;
                true
            }
            None => false,
        }
    }

    /// Name a container, or take its name away. Returns false when there is no such container.
    pub fn set_container_name(&mut self, id: ContainerId, name: Option<String>) -> bool {
        match self.containers.iter_mut().find(|c| c.id == id) {
            Some(c) => {
                c.name = name;
                true
            }
            None => false,
        }
    }

    /// Maximize (`on`) or put back the member of a container matching `pred`. The maximized
    /// member fills the container's area ([`Desk::tile_container`]) and is the topmost of the
    /// members, the container and its tree stay as they are. Only one member is maximized at a
    /// time: maximizing another moves it. Putting back a member that is not maximized changes
    /// nothing. Fails, and changes nothing, when there is no such window or it is in no
    /// container.
    pub fn zoom_in_container(
        &mut self,
        pred: impl Fn(&W) -> bool,
        on: bool,
    ) -> Result<(), ContainerError> {
        let Some(entry) = self.entries.iter().find(|e| pred(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let Some(container) = entry.container else {
            return Err(ContainerError::NotInContainer);
        };
        let window = entry.id;
        let Some(c) = self.containers.iter_mut().find(|c| c.id == container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        if on {
            c.zoomed = Some(window);
        } else if c.zoomed == Some(window) {
            c.zoomed = None;
        }
        self.gather_members(container, false);
        Ok(())
    }

    /// Drag the `edge` of the member matching `pred` to `to` (a coordinate in the shared space)
    /// by moving the divider of its container's tree, see
    /// [`TileTree::resize`](crate::primitives::tile_tree::TileTree::resize): the window on the
    /// other side of the divider follows, and no half gets smaller than `min` px. Only the
    /// tree changes, [`Desk::tile_container`] lays it out. Returns the new ratio, or `None`
    /// when nothing moved: the edge is on the outer border of the container, or the container
    /// has not been laid out. Fails when there is no such window or it is in no container.
    pub fn resize_container(
        &mut self,
        pred: impl Fn(&W) -> bool,
        edge: Edge,
        to: i32,
        gap: i32,
        min: i32,
    ) -> Result<Option<f64>, ContainerError> {
        let Some(entry) = self.entries.iter().find(|e| pred(&e.window)) else {
            return Err(ContainerError::NoSuchWindow);
        };
        let Some(container) = entry.container else {
            return Err(ContainerError::NotInContainer);
        };
        let window = entry.id;
        let Some(c) = self.containers.iter_mut().find(|c| c.id == container) else {
            return Err(ContainerError::NoSuchContainer);
        };
        let Some(area) = c.area else {
            return Ok(None);
        };
        Ok(c.tree.resize(window, edge, to, area, gap, min))
    }

    /// The member of the container of the first window matching `pred` that follows it in
    /// tree order (`forward`) or precedes it, wrapping. The window itself when it is alone in
    /// its container. `None` when there is no such window or it is in no container.
    pub fn step_in_container(&self, pred: impl Fn(&W) -> bool, forward: bool) -> Option<&Entry<W>> {
        let entry = self.entries.iter().find(|e| pred(&e.window))?;
        let tree = &self.container(entry.container?)?.tree;
        let to = if forward {
            tree.next_leaf(entry.id)
        } else {
            tree.prev_leaf(entry.id)
        }?;
        self.find_by_id(to)
    }

    /// The containers on `workspace` (0-based), oldest first.
    pub fn containers_on(&self, workspace: usize) -> Vec<&Container> {
        self.containers
            .iter()
            .filter(|c| c.workspace == workspace)
            .collect()
    }

    /// The oldest container on `workspace` (0-based) with this name.
    pub fn container_named(&self, workspace: usize, name: &str) -> Option<ContainerId> {
        self.containers
            .iter()
            .find(|c| c.workspace == workspace && c.name.as_deref() == Some(name))
            .map(|c| c.id)
    }

    /// Send a container, with all its windows, to `workspace`, on top of it. Returns false when
    /// there is no such container.
    pub fn move_container_to_workspace(&mut self, id: ContainerId, workspace: usize) -> bool {
        let Some(ci) = self.container_index(id) else {
            return false;
        };
        if let Some(c) = self.containers.get_mut(ci) {
            c.workspace = workspace;
        }
        self.sync_members(ci);
        self.gather_members(id, true);
        true
    }

    /// Lay a container's tree out in `area`, `gap` px between the windows (see
    /// [`TileTree::layout`](crate::primitives::tile_tree::TileTree::layout)), and put every
    /// member at the corner of its rectangle. Returns the rectangles in tree order so the
    /// caller can configure the windows to their size, empty when there is no such container.
    /// The maximized member, if there is one, gets `area` whole.
    pub fn tile_container(
        &mut self,
        id: ContainerId,
        area: Rect,
        gap: i32,
    ) -> Vec<(WindowId, Rect)> {
        let Some(container) = self.container(id) else {
            return Vec::new();
        };
        let mut rects = container.tree.layout(area, gap);
        // The maximized member fills the area and covers the rest, which stay where the tree
        // put them.
        if let Some(zoomed) = container.zoomed {
            if let Some(tile) = rects.iter_mut().find(|(window, _)| *window == zoomed) {
                tile.1 = area;
            }
        }
        if let Some(container) = self.containers.iter_mut().find(|c| c.id == id) {
            container.area = Some(area);
        }
        for (window, rect) in &rects {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *window) {
                // A fullscreen member keeps the monitor, not its place in the tree.
                if entry.mode != WindowMode::Fullscreen {
                    entry.pos = (rect.0, rect.1);
                }
            }
        }
        rects
    }

    fn container_index(&self, id: ContainerId) -> Option<usize> {
        self.containers.iter().position(|c| c.id == id)
    }

    /// Give every member of the container at `ci` the workspace and the monitor of the
    /// container.
    fn sync_members(&mut self, ci: usize) {
        let Some(c) = self.containers.get(ci) else {
            return;
        };
        let (id, workspace, output) = (c.id, c.workspace, c.output.clone());
        for entry in &mut self.entries {
            if entry.container == Some(id) {
                entry.workspace = workspace;
                entry.output = output.clone();
            }
        }
    }

    /// Put the members of a container next to each other in the stack, in tree order from the
    /// bottom up: on top of the stack with `top`, otherwise where the topmost member was.
    fn gather_members(&mut self, id: ContainerId, top: bool) {
        let Some(ci) = self.container_index(id) else {
            return;
        };
        let order = self
            .containers
            .get(ci)
            .map(|c| c.tree.leaves())
            .unwrap_or_default();
        let mut others = Vec::with_capacity(self.entries.len());
        let mut members = Vec::new();
        // How many other windows are below the topmost member.
        let mut at = 0;
        for entry in std::mem::take(&mut self.entries) {
            if entry.container == Some(id) {
                at = others.len();
                members.push(entry);
            } else {
                others.push(entry);
            }
        }
        // A maximized or fullscreen member is the topmost of them, it covers the others.
        let zoomed = self.containers.get(ci).and_then(|c| c.zoomed);
        members.sort_by_key(|e| {
            (
                zoomed == Some(e.id) || e.mode == WindowMode::Fullscreen,
                order.iter().position(|w| *w == e.id).unwrap_or(usize::MAX),
            )
        });
        let at = if top { others.len() } else { at };
        let above = others.split_off(at);
        others.extend(members);
        others.extend(above);
        self.entries = others;
    }

    /// `window` is no longer in the tree of `container`: take it out, and drop the container
    /// when it was the last one. The focus moves to a neighbour when it was on `window`.
    fn drop_leaf(&mut self, container: ContainerId, window: WindowId) {
        let Some(ci) = self.container_index(container) else {
            return;
        };
        let Some(c) = self.containers.get_mut(ci) else {
            return;
        };
        // A maximized member that goes takes the maximize with it: the others show again.
        if c.zoomed == Some(window) {
            c.zoomed = None;
        }
        match c.tree.remove(window) {
            Removal::Emptied => {
                self.containers.remove(ci);
            }
            Removal::Removed { focus } => {
                if c.focused == window {
                    c.focused = focus;
                }
            }
            Removal::NotFound => {}
        }
    }
}
