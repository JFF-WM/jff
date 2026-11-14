//! A tiling container: a group of windows that tile among themselves on one workspace of one
//! monitor.
//!
//! No Smithay/Wayland types in here, like the rest of the desk. A container is not a workspace:
//! it is one object that sits on a workspace, next to the floating windows and the other
//! containers there.
//!
//! A [`Container`] cannot be built or changed from outside the desk: [`Desk`](super::Desk)
//! keeps it in step with its windows (the workspace and the monitor of every member, the order
//! of the members in the stack), and what is written here is read through the getters.

use crate::primitives::tile_tree::{Rect, TileTree};

/// Names a window of a [`Desk`](super::Desk) for as long as the desk has it. The layout tree
/// holds these, because it needs a handle that is `Copy` and the desk's own `W` need not be.
/// The desk hands one out for every window it adds and never uses a number twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub(super) u64);

impl WindowId {
    /// The number behind the id, for output (IPC) and logs.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Names a [`Container`] of a [`Desk`](super::Desk). Unique for as long as the desk lives, a
/// number is not reused after its container is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContainerId(pub(super) u64);

impl ContainerId {
    /// The number behind the id, for output (IPC) and logs.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Why a container operation of the desk did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerError {
    /// There is no container with that id (it may be gone: the last window leaves, it goes).
    NoSuchContainer,
    /// No window of the desk matches.
    NoSuchWindow,
    /// Both windows asked about are the same one.
    SameWindow,
    /// The window is not a member of the container it was asked about.
    NotInContainer,
    /// The window is a member of a container already (this one or another), a window is in at
    /// most one.
    AlreadyInContainer,
}

/// A group of windows tiled by a tree, on one workspace of one monitor.
///
/// It always has at least one window: the desk removes it with its last one. `focused` is
/// always one of them.
#[derive(Clone, Debug)]
pub struct Container {
    pub(super) id: ContainerId,
    pub(super) name: Option<String>,
    pub(super) tree: TileTree<WindowId>,
    pub(super) focused: WindowId,
    pub(super) workspace: usize,
    pub(super) output: String,
    pub(super) background: Option<String>,
    pub(super) area: Option<Rect>,
    pub(super) zoomed: Option<WindowId>,
}

impl Container {
    pub fn id(&self) -> ContainerId {
        self.id
    }

    /// The name the user gave it, if any.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The layout of its windows: the shape, the ratios, the order of the leaves.
    pub fn tree(&self) -> &TileTree<WindowId> {
        &self.tree
    }

    /// The window that has the focus inside the container, the one a new window is split off.
    pub fn focused(&self) -> WindowId {
        self.focused
    }

    /// The workspace it sits on (0-based), the same as that of every window in it.
    pub fn workspace(&self) -> usize {
        self.workspace
    }

    /// The monitor it sits on, by name (empty: none yet), the same as that of every window in
    /// it.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// A wallpaper for the container, as a path. Always `None` for now: the field is here so
    /// that giving containers a background later is a fill-in and not a refactor.
    pub fn background(&self) -> Option<&str> {
        self.background.as_deref()
    }

    /// The rectangle the container was last laid out in, border not included. `None` until it
    /// is laid out ([`Desk::tile_container`](super::Desk::tile_container)).
    pub fn area(&self) -> Option<Rect> {
        self.area
    }

    /// The member that is maximized: it fills [`Container::area`] and covers the others, which
    /// keep their places in the tree. `None` when the container is laid out by its tree alone.
    pub fn zoomed(&self) -> Option<WindowId> {
        self.zoomed
    }
}
