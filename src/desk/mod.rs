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

#[cfg(test)]
mod tests {
    use super::*;

    fn order(d: &Desk<&'static str>) -> Vec<&'static str> {
        d.top_down().map(|e| e.window).collect()
    }

    #[test]
    fn new_windows_go_on_top_and_cascade() {
        let mut d = Desk::default();
        let a = d.add("a");
        let b = d.add("b");
        assert_eq!(order(&d), ["b", "a"]);
        assert_ne!(a, b);
    }

    #[test]
    fn raise_reorders() {
        let mut d = Desk::default();
        d.add("a");
        d.add("b");
        d.add("c");
        d.raise_where(|w| *w == "a");
        assert_eq!(order(&d), ["a", "c", "b"]);
    }

    #[test]
    fn set_pos_moves_only_the_match() {
        let mut d = Desk::default();
        let a = d.add("a");
        d.add("b");
        d.set_pos_where(|w| *w == "b", (500, 300));
        assert_eq!(d.pos_of(|w| *w == "b"), Some((500, 300)));
        assert_eq!(d.pos_of(|w| *w == "a"), Some(a));
        assert_eq!(d.pos_of(|w| *w == "zzz"), None);
    }

    #[test]
    fn maximize_then_restore_round_trips() {
        let mut d = Desk::default();
        let a = d.add("a");
        assert!(d.maximize_where(|w| *w == "a", (0, 0), (300, 200)));
        assert!(d.is_maximized(|w| *w == "a"));
        assert_eq!(d.pos_of(|w| *w == "a"), Some((0, 0)));
        // A second maximize must not clobber the remembered geometry.
        assert!(!d.maximize_where(|w| *w == "a", (0, 0), (999, 999)));
        assert_eq!(d.unmaximize_where(|w| *w == "a"), Some((300, 200)));
        assert_eq!(d.pos_of(|w| *w == "a"), Some(a));
        assert!(!d.is_maximized(|w| *w == "a"));
        assert_eq!(d.unmaximize_where(|w| *w == "a"), None);
    }

    #[test]
    fn restore_pos_changes_only_while_maximized() {
        let mut d = Desk::default();
        d.add("a");
        d.set_restore_pos_where(|w| *w == "a", (5, 5));
        assert!(d.maximize_where(|w| *w == "a", (0, 0), (300, 200)));
        d.set_restore_pos_where(|w| *w == "a", (2000, 70));
        assert_eq!(d.unmaximize_where(|w| *w == "a"), Some((300, 200)));
        assert_eq!(d.pos_of(|w| *w == "a"), Some((2000, 70)));
    }

    #[test]
    fn fitted_fires_once() {
        let mut d = Desk::default();
        d.add("a");
        assert!(d.mark_fitted_where(|w| *w == "a"));
        assert!(!d.mark_fitted_where(|w| *w == "a"));
        assert!(!d.mark_fitted_where(|w| *w == "nope"));
    }

    #[test]
    fn only_the_current_workspace_is_visible() {
        let mut d = Desk::default();
        d.add("a");
        d.add("b");
        d.set_current(1);
        // New windows land on the workspace being shown, and cascade from its start.
        let c = d.add("c");
        assert_eq!(c, (40, 40));
        assert_eq!(order(&d), ["c"]);
        assert_eq!(d.all_top_down().count(), 3);
        d.set_current(0);
        assert_eq!(order(&d), ["b", "a"]);
        assert_eq!(d.workspace_of(|w| *w == "c"), Some(1));
        assert_eq!(d.workspace_of(|w| *w == "zzz"), None);
        assert_eq!(d.count_on(0), 2);
    }

    #[test]
    fn moving_a_window_changes_what_is_visible() {
        let mut d = Desk::default();
        d.add("a");
        d.add("b");
        assert!(d.move_to_workspace(|w| *w == "b", 2));
        assert_eq!(order(&d), ["a"]);
        d.set_current(2);
        assert_eq!(order(&d), ["b"]);
        assert!(!d.move_to_workspace(|w| *w == "zzz", 0));
        // Raising keeps workspaces apart.
        d.raise_where(|w| *w == "a");
        assert_eq!(order(&d), ["b"]);
    }

    #[test]
    fn fewer_workspaces_fold_into_the_last() {
        let mut d = Desk::default();
        d.add("a");
        d.set_current(5);
        d.add("b");
        d.clamp_workspaces(3);
        assert_eq!(d.current(), 2);
        assert_eq!(order(&d), ["b"]);
        assert_eq!(d.workspace_of(|w| *w == "a"), Some(0));
        d.clamp_workspaces(0);
        assert_eq!(d.current(), 0);
    }

    #[test]
    fn goto_wraps_and_clamps() {
        assert_eq!(resolve(Goto::To(2), 0, 9), 2);
        assert_eq!(resolve(Goto::To(20), 0, 9), 8);
        assert_eq!(resolve(Goto::Next, 8, 9), 0);
        assert_eq!(resolve(Goto::Prev, 0, 9), 8);
        assert_eq!(resolve(Goto::Next, 0, 1), 0);
        assert_eq!(resolve(Goto::Prev, 3, 0), 0);
    }

    #[test]
    fn remove_drops_window() {
        let mut d = Desk::default();
        d.add("a");
        d.add("b");
        d.remove_where(|w| *w == "a");
        assert_eq!(order(&d), ["b"]);
        assert_eq!(d.len(), 1);
    }
}
