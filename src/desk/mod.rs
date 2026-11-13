//! The desk: an ordered stack of windows with positions.
//!
//! No Smithay/Wayland types in here, on purpose, the stacking policy can be tested
//! without a compositor. `W` is whatever handle the caller uses for a window.

pub mod session_store;

use std::collections::HashMap;

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
}

impl<W> Default for Desk<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            current: 0,
            per_output: false,
            shown: HashMap::new(),
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
    /// there's no such window.
    pub fn move_to_workspace(&mut self, pred: impl Fn(&W) -> bool, workspace: usize) -> bool {
        let Some(i) = self.entries.iter().position(|e| pred(&e.window)) else {
            return false;
        };
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

    pub fn is_maximized(&self, pred: impl Fn(&W) -> bool) -> bool {
        self.mode_of(pred) == WindowMode::Maximized
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
    /// false when there's no such window.
    pub fn set_output_where(&mut self, pred: impl Fn(&W) -> bool, output: &str) -> bool {
        let shown = self.current_on(output);
        let per_output = self.per_output;
        let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) else {
            return false;
        };
        e.output = output.to_owned();
        if per_output {
            e.workspace = shown;
        }
        true
    }

    /// Place the first window matching `pred` at `pos`.
    pub fn set_pos_where(&mut self, pred: impl Fn(&W) -> bool, pos: Pos) {
        if let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) {
            e.pos = pos;
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

    pub fn remove_where(&mut self, pred: impl Fn(&W) -> bool) {
        self.entries.retain(|e| !pred(&e.window));
    }

    /// Move the first window matching `pred` to the top of the stack.
    pub fn raise_where(&mut self, pred: impl Fn(&W) -> bool) {
        if let Some(i) = self.entries.iter().position(|e| pred(&e.window)) {
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
