//! The desk: an ordered stack of windows with positions.
//!
//! No Smithay/Wayland types in here, on purpose, the stacking policy can be tested
//! without a compositor. `W` is whatever handle the caller uses for a window.

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

pub struct Entry<W> {
    pub window: W,
    pub pos: Pos,
    /// The workspace the window lives on (0-based). Windows on other workspaces aren't shown,
    /// drawn, or reachable with the pointer.
    pub workspace: usize,
    /// Where to put the window back when it's un-maximized, and the size it had. `Some` means
    /// it's maximized right now.
    pub restore: Option<(Pos, (i32, i32))>,
    /// Whether the window's first real size has already been checked against the output. Set
    /// once, see `mark_fitted_where`.
    pub fitted: bool,
    /// When the window first had something to show (the same moment `fitted` is set). The open
    /// animation runs from here, not from `add`: a client can take 100 ms to send its first
    /// frame, and the fade would be over by then.
    pub shown: Option<std::time::Instant>,
}

pub struct Desk<W> {
    /// Bottom of the stack first, topmost window last. One stack for every workspace: the
    /// order of the windows of a workspace is the order they have here.
    entries: Vec<Entry<W>>,
    /// The workspace that's shown.
    current: usize,
}

impl<W> Default for Desk<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            current: 0,
        }
    }
}

impl<W> Desk<W> {
    /// Add a window on top of the current workspace, cascading so new windows don't hide each
    /// other. Returns the position it was given.
    pub fn add(&mut self, window: W) -> Pos {
        let step = (self.count_on(self.current) % 10) as i32;
        let pos = (40 + step * 32, 40 + step * 32);
        self.entries.push(Entry {
            window,
            pos,
            workspace: self.current,
            restore: None,
            fitted: false,
            shown: None,
        });
        pos
    }

    /// The workspace that's shown (0-based).
    pub fn current(&self) -> usize {
        self.current
    }

    /// Show another workspace. Doesn't touch the windows.
    pub fn set_current(&mut self, workspace: usize) {
        self.current = workspace;
    }

    /// Whether `entry` is on the workspace that's shown.
    pub fn is_visible(&self, entry: &Entry<W>) -> bool {
        entry.workspace == self.current
    }

    /// How many windows live on `workspace`.
    pub fn count_on(&self, workspace: usize) -> usize {
        self.entries
            .iter()
            .filter(|e| e.workspace == workspace)
            .count()
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
    }

    /// Windows of the current workspace, topmost first - the order renderers want. Windows on
    /// other workspaces are skipped, use [`Desk::all_top_down`] for those.
    pub fn top_down(&self) -> impl Iterator<Item = &Entry<W>> {
        let current = self.current;
        self.entries
            .iter()
            .rev()
            .filter(move |e| e.workspace == current)
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

    pub fn is_maximized(&self, pred: impl Fn(&W) -> bool) -> bool {
        self.entries
            .iter()
            .find(|e| pred(&e.window))
            .map(|e| e.restore.is_some())
            .unwrap_or(false)
    }

    /// Mark the window maximized, remembering where it was. Returns false if it's already
    /// maximized or not found - the remembered geometry is never overwritten.
    pub fn maximize_where(
        &mut self,
        pred: impl Fn(&W) -> bool,
        area_pos: Pos,
        size: (i32, i32),
    ) -> bool {
        match self.entries.iter_mut().find(|e| pred(&e.window)) {
            Some(e) if e.restore.is_none() => {
                e.restore = Some((e.pos, size));
                e.pos = area_pos;
                true
            }
            _ => false,
        }
    }

    /// Undo `maximize_where`: put the window back and return the size it should have.
    pub fn unmaximize_where(&mut self, pred: impl Fn(&W) -> bool) -> Option<(i32, i32)> {
        let e = self.entries.iter_mut().find(|e| pred(&e.window))?;
        let (pos, size) = e.restore.take()?;
        e.pos = pos;
        Some(size)
    }

    /// Take the remembered geometry out of a maximized window, leaving it where it is and no
    /// longer maximized as far as the desk is concerned. Pair with `remaximize_where`.
    pub fn take_restore_where(&mut self, pred: impl Fn(&W) -> bool) -> Option<(Pos, (i32, i32))> {
        self.entries
            .iter_mut()
            .find(|e| pred(&e.window))?
            .restore
            .take()
    }

    /// Maximize a window that isn't maximized, going back to the geometry `restore` that
    /// `take_restore_where` returned instead of the current one. Returns false (and changes
    /// nothing) if the window is gone or became maximized meanwhile.
    pub fn remaximize_where(
        &mut self,
        pred: impl Fn(&W) -> bool,
        area_pos: Pos,
        restore: (Pos, (i32, i32)),
    ) -> bool {
        match self.entries.iter_mut().find(|e| pred(&e.window)) {
            Some(e) if e.restore.is_none() => {
                e.restore = Some(restore);
                e.pos = area_pos;
                true
            }
            _ => false,
        }
    }

    /// Place the first window matching `pred` at `pos`.
    pub fn set_pos_where(&mut self, pred: impl Fn(&W) -> bool, pos: Pos) {
        if let Some(e) = self.entries.iter_mut().find(|e| pred(&e.window)) {
            e.pos = pos;
        }
    }

    /// Change where the first maximized window matching `pred` goes back to. No-op if the
    /// window isn't maximized.
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
