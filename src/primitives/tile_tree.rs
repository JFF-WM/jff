//! The layout tree of a tiling container, no Smithay/Wayland types, so it can be unit-tested on
//! its own.
//!
//! A container tiles its windows with a binary tree. A leaf is one window: `W` is whatever
//! handle the caller uses for it, the tree never looks inside. A split cuts its rectangle in
//! two along an axis, `ratio` of the room going to the first child, with `gap` px between the
//! halves. The first child is the left or the top half.
//!
//! The tree knows nothing about monitors, workspaces or focus. The caller passes the rectangle
//! in and gets rectangles out. All sizes are whole pixels, and nothing here panics on odd
//! input: a rectangle with no room gives leaves with no size, never a negative one.

/// `(x, y, width, height)`, the same shape as `layout::Rect`. Declared again here so that this
/// module depends on nothing.
pub type Rect = (i32, i32, i32, i32);

/// The ratio a new split starts with.
pub const DEFAULT_RATIO: f64 = 0.5;

/// The direction a split lays its two children out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side: the first child is on the left, the divider is a vertical line.
    Horizontal,
    /// One above the other: the first child is on top, the divider is a horizontal line.
    Vertical,
}

/// The edge of a leaf the user is dragging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The axis of the split this edge can move.
    fn axis(self) -> Axis {
        match self {
            Edge::Left | Edge::Right => Axis::Horizontal,
            Edge::Top | Edge::Bottom => Axis::Vertical,
        }
    }

    /// Whether this is the right or bottom edge, the one next to the second half.
    fn is_far(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }
}

/// One node of the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Node<W> {
    Leaf(W),
    Split {
        axis: Axis,
        /// Share of the room (the gap not counted) the first child gets, in `0.0..=1.0`.
        ratio: f64,
        first: Box<Node<W>>,
        second: Box<Node<W>>,
    },
}

/// Why [`TileTree::split`] did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// The leaf to split is not in the tree.
    NoSuchLeaf,
    /// The new window is already a leaf of the tree.
    Duplicate,
}

/// What [`TileTree::remove`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal<W> {
    /// The window is not in the tree, which is unchanged.
    NotFound,
    /// The window was the last one, the tree is empty now.
    Emptied,
    /// The window is gone and its parent split collapsed. `focus` is the leaf nearest to where
    /// it was in tree order: the first leaf of the sibling if the window was the first child,
    /// the last leaf of the sibling if it was the second.
    Removed { focus: W },
}

/// A tree of splits, empty only after its last leaf was removed.
#[derive(Clone, Debug, PartialEq)]
pub struct TileTree<W> {
    root: Option<Node<W>>,
}

impl<W: Copy + Eq> TileTree<W> {
    /// A tree with `window` as its only leaf.
    pub fn new(window: W) -> Self {
        Self {
            root: Some(Node::Leaf(window)),
        }
    }

    /// A tree that is `root`. The caller makes sure no window is a leaf twice.
    pub fn from_root(root: Node<W>) -> Self {
        Self { root: Some(root) }
    }

    /// The root of the tree, `None` when it is empty. For callers that need its shape.
    pub fn root(&self) -> Option<&Node<W>> {
        self.root.as_ref()
    }

    /// Whether the last leaf is gone.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// How many leaves the tree has.
    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    pub fn contains(&self, window: W) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| node_contains(root, window))
    }

    /// Every leaf in tree order: the first child before the second, depth first.
    pub fn leaves(&self) -> Vec<W> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_leaves(root, &mut out);
        }
        out
    }

    /// The rectangle of every leaf, in tree order, when the tree fills `area`.
    ///
    /// A split of length `len` along its axis keeps `gap` px (at most `len`, never negative)
    /// between its children and shares the rest: the first child gets `ratio` of it, rounded
    /// to a whole pixel, the second child the remainder. So the leaves and the gaps add up to
    /// `area` exactly, with nothing lost and nothing counted twice. There is no gap along the
    /// outer edge of `area`.
    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(W, Rect)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_rects(root, area, gap, &mut out);
        }
        out
    }

    /// Split the leaf `focused` along `axis`. It stays the first half, `new` is the second,
    /// and the two share the room equally.
    pub fn split(&mut self, focused: W, axis: Axis, new: W) -> Result<(), SplitError> {
        if !self.contains(focused) {
            return Err(SplitError::NoSuchLeaf);
        }
        if self.contains(new) {
            return Err(SplitError::Duplicate);
        }
        let root = self.root.as_mut().ok_or(SplitError::NoSuchLeaf)?;
        if split_leaf(root, focused, axis, new) {
            Ok(())
        } else {
            Err(SplitError::NoSuchLeaf)
        }
    }

    /// Take `window` out. Its parent split goes away too, the sibling takes the parent's place
    /// with its own splits and ratios untouched.
    pub fn remove(&mut self, window: W) -> Removal<W> {
        let Some(root) = self.root.take() else {
            return Removal::NotFound;
        };
        let (rest, near) = remove_node(root, window);
        self.root = rest;
        match (&self.root, near) {
            (None, _) => Removal::Emptied,
            (Some(_), Some(focus)) => Removal::Removed { focus },
            (Some(_), None) => Removal::NotFound,
        }
    }

    /// Drag the `edge` of `leaf` to the coordinate `to` (x for the left and right edges, y for
    /// the top and bottom ones), in the same space as `area`. `area` and `gap` are what
    /// [`layout`](Self::layout) was given, they say how long the split is.
    ///
    /// The edge moves the divider of the nearest split around `leaf` that has `leaf` on that
    /// side of it: the right edge of a leaf in the first half, the left edge of a leaf in the
    /// second half, and the same for top and bottom. Neither half ends up smaller than `min`
    /// px, so a drag far past the other side stops there. Where the split is too short for two
    /// halves of `min` px, the ratio goes back to [`DEFAULT_RATIO`].
    ///
    /// The ratio is worked out from `to` alone, not from the old ratio, so a drag that sends
    /// the same position twice gives the same result, and rounding cannot build up.
    ///
    /// Returns the new ratio, or `None` when nothing moved: `leaf` is not in the tree, or the
    /// edge is on the outer border of the container.
    pub fn resize(
        &mut self,
        leaf: W,
        edge: Edge,
        to: i32,
        area: Rect,
        gap: i32,
        min: i32,
    ) -> Option<f64> {
        resize_node(self.root.as_mut()?, leaf, edge, to, area, gap, min)
    }

    /// Exchange the places of the leaves `a` and `b`: each takes the rectangle the other had.
    /// The shape and the ratios stay. Returns false, and changes nothing, when either is not in
    /// the tree or they are the same one.
    pub fn swap_leaves(&mut self, a: W, b: W) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        if let Some(root) = self.root.as_mut() {
            swap_in(root, a, b);
        }
        true
    }

    /// The leaf after `from` in tree order, the first one after the last. `None` if `from` is
    /// not in the tree.
    pub fn next_leaf(&self, from: W) -> Option<W> {
        self.step(from, true)
    }

    /// The leaf before `from` in tree order, the last one before the first. `None` if `from`
    /// is not in the tree.
    pub fn prev_leaf(&self, from: W) -> Option<W> {
        self.step(from, false)
    }

    fn step(&self, from: W, forward: bool) -> Option<W> {
        let leaves = self.leaves();
        let at = leaves.iter().position(|w| *w == from)?;
        let n = leaves.len();
        let to = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        leaves.get(to).copied()
    }
}

/// The share of `avail` px the first child gets. `ratio` is trusted only so far as to stay in
/// `0.0..=1.0`.
fn first_len(ratio: f64, avail: i32) -> i32 {
    let ratio = if ratio.is_nan() {
        DEFAULT_RATIO
    } else {
        ratio.clamp(0.0, 1.0)
    };
    ((ratio * f64::from(avail)).round() as i32).clamp(0, avail)
}

/// The rectangles of the two children of a split that fills `rect`.
fn split_rect(rect: Rect, axis: Axis, ratio: f64, gap: i32) -> (Rect, Rect) {
    let (x, y) = (rect.0, rect.1);
    let (w, h) = (rect.2.max(0), rect.3.max(0));
    let len = match axis {
        Axis::Horizontal => w,
        Axis::Vertical => h,
    };
    let gap = gap.clamp(0, len);
    let first = first_len(ratio, len - gap);
    let second = len - gap - first;
    match axis {
        Axis::Horizontal => ((x, y, first, h), (x + first + gap, y, second, h)),
        Axis::Vertical => ((x, y, w, first), (x, y + first + gap, w, second)),
    }
}

fn collect_leaves<W: Copy>(node: &Node<W>, out: &mut Vec<W>) {
    match node {
        Node::Leaf(w) => out.push(*w),
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

fn collect_rects<W: Copy>(node: &Node<W>, rect: Rect, gap: i32, out: &mut Vec<(W, Rect)>) {
    match node {
        Node::Leaf(w) => out.push((*w, (rect.0, rect.1, rect.2.max(0), rect.3.max(0)))),
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (a, b) = split_rect(rect, *axis, *ratio, gap);
            collect_rects(first, a, gap, out);
            collect_rects(second, b, gap, out);
        }
    }
}

fn node_contains<W: Copy + Eq>(node: &Node<W>, target: W) -> bool {
    match node {
        Node::Leaf(w) => *w == target,
        Node::Split { first, second, .. } => {
            node_contains(first, target) || node_contains(second, target)
        }
    }
}

/// Rename the leaf `a` to `b` and the leaf `b` to `a` under `node`.
fn swap_in<W: Copy + Eq>(node: &mut Node<W>, a: W, b: W) {
    match node {
        Node::Leaf(w) => {
            if *w == a {
                *w = b;
            } else if *w == b {
                *w = a;
            }
        }
        Node::Split { first, second, .. } => {
            swap_in(first, a, b);
            swap_in(second, a, b);
        }
    }
}

/// The first leaf of `node` in tree order, or the last one with `last`.
fn edge_leaf<W: Copy>(node: &Node<W>, last: bool) -> W {
    let mut node = node;
    loop {
        match node {
            Node::Leaf(w) => return *w,
            Node::Split { first, second, .. } => {
                node = if last { &**second } else { &**first };
            }
        }
    }
}

/// Replace the leaf `target` under `node` with a split holding it and `new`. Whether it was
/// found.
fn split_leaf<W: Copy + Eq>(node: &mut Node<W>, target: W, axis: Axis, new: W) -> bool {
    if matches!(*node, Node::Leaf(w) if w == target) {
        *node = Node::Split {
            axis,
            ratio: DEFAULT_RATIO,
            first: Box::new(Node::Leaf(target)),
            second: Box::new(Node::Leaf(new)),
        };
        return true;
    }
    match node {
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            split_leaf(first, target, axis, new) || split_leaf(second, target, axis, new)
        }
    }
}

/// `node` without the leaf `target`, and the leaf nearest to where it was. Both are `None` when
/// `node` was that leaf itself. The leaf is `None` and the node comes back unchanged when
/// `target` was not under it.
fn remove_node<W: Copy + Eq>(node: Node<W>, target: W) -> (Option<Node<W>>, Option<W>) {
    match node {
        Node::Leaf(w) if w == target => (None, None),
        Node::Leaf(w) => (Some(Node::Leaf(w)), None),
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (rest, near) = remove_node(*first, target);
            let Some(first) = rest else {
                // The first child was the leaf: the second takes the split's place.
                let focus = edge_leaf(&*second, false);
                return (Some(*second), Some(focus));
            };
            if near.is_some() {
                let split = Node::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second,
                };
                return (Some(split), near);
            }
            let (rest, near) = remove_node(*second, target);
            let Some(second) = rest else {
                let focus = edge_leaf(&first, true);
                return (Some(first), Some(focus));
            };
            let split = Node::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            };
            (Some(split), near)
        }
    }
}

/// The work of [`TileTree::resize`] under `node`, which fills `rect`.
fn resize_node<W: Copy + Eq>(
    node: &mut Node<W>,
    leaf: W,
    edge: Edge,
    to: i32,
    rect: Rect,
    gap: i32,
    min: i32,
) -> Option<f64> {
    let Node::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let axis = *axis;
    let (a, b) = split_rect(rect, axis, *ratio, gap);
    let in_first = node_contains(first, leaf);
    if !in_first && !node_contains(second, leaf) {
        return None;
    }

    // The nearest split that fits wins, so look further down first.
    let deeper = if in_first {
        resize_node(first, leaf, edge, to, a, gap, min)
    } else {
        resize_node(second, leaf, edge, to, b, gap, min)
    };
    if deeper.is_some() {
        return deeper;
    }
    // The divider is the right/bottom edge of the first half and the left/top edge of the
    // second one.
    if edge.axis() != axis || edge.is_far() != in_first {
        return None;
    }

    let (start, len) = match axis {
        Axis::Horizontal => (rect.0, rect.2.max(0)),
        Axis::Vertical => (rect.1, rect.3.max(0)),
    };
    let gap = gap.clamp(0, len);
    let avail = len - gap;
    let min = min.max(0);
    let new = if avail == 0 || avail < min.saturating_mul(2) {
        DEFAULT_RATIO
    } else {
        // How long the first half must be for the dragged edge to land on `to`: the far edge of
        // the first half is its end, the near edge of the second half is one gap after it.
        let from_start = to.saturating_sub(start);
        let wanted = if edge.is_far() {
            from_start
        } else {
            from_start.saturating_sub(gap)
        };
        f64::from(wanted.clamp(min, avail - min)) / f64::from(avail)
    };
    *ratio = new;
    Some(new)
}
