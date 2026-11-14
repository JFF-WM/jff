//! Starting a tiling container by dropping a window beside a snapped one, and putting windows
//! into a container that exists.
//!
//! This is the Smithay side. The geometry (which half is empty, which axis the split takes) is
//! in `primitives::container_drop`, and the bookkeeping (the tree, the members, where the
//! container sits) in `desk::container`, both pure and unit-tested.
//!
//! With `tiling-containers` on, a window let go inside the empty half of a monitor that has a
//! window snapped to one side starts a container with the two of them. A window let go over a
//! container joins it, and so does a new window that opens while a container has the keyboard.
//! Everything else about a drop stays as it is, and with the setting off nothing in here runs.

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::Window;
use smithay::input::pointer::{Focus, GrabStartData};
use smithay::utils::{Logical, Point, Rectangle, Serial, SERIAL_COUNTER};

use crate::config::{Border, Config, WindowFacts};
use crate::desk::{ContainerId, Desk, Entry, Side, WindowMode};
use crate::feature::osd::toplevel_names;
use crate::feature::outputs::output_rect;
use crate::input::grabs::r#move::shrunk_geometry;
use crate::input::grabs::tile_move::{TileMoveEnd, TileMoveGrab};
use crate::input::grabs::tile_resize::TileResizeGrab;
use crate::input::BTN_LEFT;
use crate::primitives::container_drop::{in_empty_half, leaf_drop, opposite};
use crate::primitives::container_view;
use crate::primitives::geom;
use crate::primitives::layout::{self, Rect};
use crate::primitives::tile_tree::Edge;
use crate::protocol::layer_shell::{ABOVE_WINDOWS, BELOW_WINDOWS};
use crate::protocol::toplevel_tag;
use crate::render::effects::border::{container_border_element, BorderPaint};
use crate::render::elements::JffElement;
use crate::state::{configure_member, configure_mode, JffState};

/// Width of a container's border when the config has no `border` block of its own to take it
/// from: a container is always drawn with one.
const DEFAULT_BORDER_WIDTH: i32 = 2;

/// Least room (px) either side of a divider keeps when it is dragged.
const MIN_TILE: i32 = 80;

impl JffState {
    /// A drag of `window` ended with the pointer at `at`. When that starts a container, starts
    /// it and returns true: the window and the snapped one are laid out by its tree and
    /// configured, and the drop is done. Returns false, having changed nothing, when it does
    /// not, and the caller finishes the drop as usual.
    ///
    /// It starts one when `tiling-containers` is on, the window is floating or maximized and in
    /// no container, `at` is in the empty half of the monitor under it (see
    /// [`in_empty_half`]), a window is snapped to the other half, and no `window-rule` with
    /// `floating` applies to either window. The container is named by the `container-rule` that
    /// fits the snapped window, if there is one.
    ///
    /// Before that, a window let go over a container that is shown joins it instead, see
    /// [`JffState::drop_into_existing_container`].
    pub fn drop_into_container(&mut self, window: &Window, at: Point<f64, Logical>) -> bool {
        if !self.config.tiling_containers() {
            return false;
        }
        let Some(dragged) = self.desk.find_by(|w| w == window) else {
            return false;
        };
        let joins = dragged.container.is_none()
            && matches!(dragged.mode, WindowMode::Floating | WindowMode::Maximized);
        if !joins {
            return false;
        }
        // A container under the pointer takes the window in, before a new one is started beside
        // a snapped window.
        if self.drop_into_existing_container(window, at) {
            return true;
        }
        let rects = self.output_rects();
        let Some(monitor) = layout::nearest((at.x, at.y), &rects) else {
            return false;
        };
        let free = self.panel_free_rect(rects[monitor]);
        let Some((snapped, side)) = self.snapped_beside(window, &rects, monitor, free, at) else {
            return false;
        };
        // A window the user keeps floating by rule is never tiled, and does not tile another.
        if self.floats_by_rule(window) || self.floats_by_rule(&snapped) {
            return false;
        }
        let name = self
            .with_facts(&snapped, |facts| {
                self.config.container_name_for(facts).map(str::to_owned)
            })
            .flatten();

        let id =
            match self
                .desk
                .create_container_beside(|w| *w == snapped, |w| w == window, side, name)
            {
                Ok(id) => id,
                Err(err) => {
                    tracing::warn!("could not start a container: {err:?}");
                    return false;
                }
            };
        // A member does not float, snap or maximize: its place comes from the tree.
        self.desk.float_in_place_where(|w| *w == snapped);
        self.desk.float_in_place_where(|w| w == window);

        let area = geom::inset_rect(free, self.config.maximize_inset());
        self.tile_and_configure(id, area);
        tracing::info!(
            "container {} started beside a window snapped {side:?}",
            id.get()
        );
        true
    }

    /// A drag of `window` ended with the pointer at `at`: when that is over a container that is
    /// shown, the window joins it, split off the member under the pointer (see
    /// [`Desk::join_container`]), and the container is laid out again. Returns true then, and
    /// false, having changed nothing, when it does not: no container there. Any window joins,
    /// a dialog and one a `window-rule` keeps `floating` too. The caller has checked that the
    /// window is floating or maximized and in no container.
    fn drop_into_existing_container(&mut self, window: &Window, at: Point<f64, Logical>) -> bool {
        let point = (at.x, at.y);
        let gap = self.config.snap_gap();
        let Some(id) = self.desk.container_at(point) else {
            return false;
        };
        let leaf = self.desk.leaf_at(id, point, gap);
        if let Err(err) = self.desk.join_container(id, leaf, |w| w == window, gap) {
            tracing::warn!("could not join the container: {err:?}");
            return false;
        }
        self.finish_join(id, window);
        tracing::info!("window dropped into container {}", id.get());
        true
    }

    /// A window that first shows (`fit_to_output_once`) joins the container that has the
    /// keyboard, when there is one. Returns true when it did: the window is in the container's
    /// layout, every member is configured, and the window has the keyboard. Returns false,
    /// having changed nothing, when it did not, and the caller places the window as usual.
    ///
    /// The container is the one of the window the keyboard was on when the toplevel was created
    /// (see `focus_before_open`), because the new window takes the keyboard itself at once: that
    /// is what it was opened from. When the user moved the keyboard to another window since, that
    /// one decides. Needs `tiling-containers`, and not the overview or a locked session. Any
    /// kind of window joins: a dialog, a restored one, one a rule keeps floating. The container
    /// has to be on the window's workspace and monitor, shown, and without a fullscreen focused
    /// member (see [`Desk::container_to_join`]).
    pub(crate) fn join_container_on_map(&mut self, window: &Window) -> bool {
        if !self.config.tiling_containers() || self.overview_active() || self.session_locked() {
            return false;
        }
        let Some(root) = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface().clone())
        else {
            return false;
        };
        let opened_from = self.focus_before_open.remove(&root);
        let current = self.focused_window();
        let focus = if current.as_ref() == Some(window) {
            opened_from
        } else {
            current
        };
        let Some(focus) = focus else {
            return false;
        };
        let Some(id) = self
            .desk
            .container_to_join(|w| *w == focus, |w| w == window)
        else {
            return false;
        };
        let gap = self.config.snap_gap();
        if let Err(err) = self.desk.join_container(id, None, |w| w == window, gap) {
            tracing::warn!("could not join the container: {err:?}");
            return false;
        }
        self.finish_join(id, window);
        tracing::info!("new window joined container {}", id.get());
        true
    }

    /// `window` has just joined container `id`: lay the container out again, configuring every
    /// member, and give the keyboard to the window.
    fn finish_join(&mut self, id: ContainerId, window: &Window) {
        self.retile_container(id);
        self.focus_window(window);
    }

    /// The window snapped to one side of monitor `monitor` that leaves the half `at` is in
    /// empty, and the side. Of several, the one on top. Not `dragged`, not a member of a
    /// container, and not one that has a window snapped to the opposite side: then the half is
    /// not empty.
    fn snapped_beside(
        &self,
        dragged: &Window,
        rects: &[Rect],
        monitor: usize,
        free: Rect,
        at: Point<f64, Logical>,
    ) -> Option<(Window, Side)> {
        let mut snapped = Vec::new();
        // The current workspace only, topmost first.
        for entry in self.desk.top_down() {
            let WindowMode::Snapped(side) = entry.mode else {
                continue;
            };
            if entry.window == *dragged || entry.container.is_some() {
                continue;
            }
            let size = entry.window.geometry().size;
            let center = layout::center(entry.pos, (size.w, size.h));
            if layout::nearest(center, rects) == Some(monitor) {
                snapped.push((entry.window.clone(), side));
            }
        }
        snapped
            .iter()
            .find(|(_, side)| {
                in_empty_half(free, *side, (at.x, at.y))
                    && !snapped.iter().any(|(_, other)| *other == opposite(*side))
            })
            .cloned()
    }

    /// Whether the first `window-rule` that fits `window` says `floating`.
    fn floats_by_rule(&self, window: &Window) -> bool {
        self.with_facts(window, |facts| {
            self.config
                .window_rule_for(facts)
                .is_some_and(|rule| rule.floating)
        })
        .unwrap_or(false)
    }

    /// Ask `f` about what a rule can see of `window` now. `restored` is not known after the
    /// first commit and counts as false, the same as for a window that never was.
    fn with_facts<R>(&self, window: &Window, f: impl FnOnce(&WindowFacts<'_>) -> R) -> Option<R> {
        let toplevel = window.toplevel()?;
        let root = toplevel.wl_surface();
        let (app_id, title) = toplevel_names(root);
        let tag = toplevel_tag::tag_of(root);
        let dialog = toplevel.parent().is_some() || self.dialog_toplevels.contains(root);
        Some(f(&WindowFacts {
            app_id: &app_id,
            title: &title,
            tag: tag.as_deref(),
            dialog,
            restored: false,
        }))
    }

    /// The look of a container's border, see [`container_border_style`].
    pub fn container_border_style(&self) -> Border {
        container_border_style(&self.config)
    }

    /// The container whose border is under `at`, if any. Only containers that are shown count.
    pub fn container_border_at(&self, at: Point<f64, Logical>) -> Option<ContainerId> {
        let reach = self.container_border_style().width;
        let mut shown: Vec<ContainerId> = Vec::new();
        for entry in self.desk.top_down() {
            if let Some(id) = entry.container {
                if !shown.contains(&id) {
                    shown.push(id);
                }
            }
        }
        shown.into_iter().find(|id| {
            self.desk
                .container(*id)
                .and_then(|container| container.area())
                .is_some_and(|area| container_view::on_border(area, reach, (at.x, at.y)))
        })
    }

    /// Select a container: raise it, with all its windows, and give the keyboard to the window
    /// it had focused. Returns false when there is no such container.
    pub fn select_container(&mut self, id: ContainerId) -> bool {
        let Some(focused) = self.desk.container(id).map(|container| container.focused()) else {
            return false;
        };
        let Some(window) = self
            .desk
            .find_by_id(focused)
            .map(|entry| entry.window.clone())
        else {
            return false;
        };
        self.focus_window(&window);
        true
    }

    /// A button went down at `at` with no window under it. When that is the border of a
    /// container, selects the container and returns true. Not under a panel, which gets the
    /// click itself, and not with `tiling-containers` off.
    pub fn border_click(&mut self, at: Point<f64, Logical>) -> bool {
        if !self.config.tiling_containers() {
            return false;
        }
        if self.layer_under(at, &ABOVE_WINDOWS).is_some()
            || self.layer_popup_under(at, &BELOW_WINDOWS).is_some()
        {
            return false;
        }
        match self.container_border_at(at) {
            Some(id) => self.select_container(id),
            None => false,
        }
    }

    /// A container's cell was dropped on `target`, a monitor, from the overview. The container
    /// goes there whole, the same tree, on that monitor's shown workspace, on top, and fills
    /// the monitor's usable area whatever size the monitors are. Returns false when there is
    /// no such container.
    pub fn drop_container(&mut self, id: ContainerId, target: Rect) -> bool {
        let Some(first) = self
            .desk
            .members(id)
            .first()
            .map(|entry| entry.window.clone())
        else {
            return false;
        };
        if let Some(output) = self.output_for_rect(target) {
            self.desk.set_output_where(|w| *w == first, &output.name());
        }
        self.desk.raise_where(|w| *w == first);
        let area = self.usable_rect(target);
        self.tile_and_configure(id, area);
        true
    }

    /// Make every container fill the usable area of its monitor again, after the monitors
    /// changed size, moved, or one went away (its container then takes the closest one). A
    /// container that already fills it is left alone.
    pub fn refit_containers(&mut self) {
        let rects = self.output_rects();
        if rects.is_empty() {
            return;
        }
        let containers: Vec<(ContainerId, String, Option<Rect>)> = self
            .desk
            .containers()
            .map(|c| (c.id(), c.output().to_owned(), c.area()))
            .collect();
        for (id, output, old) in containers {
            let Some(old) = old else {
                continue;
            };
            let named = self
                .outputs
                .iter()
                .find(|o| o.name() == output)
                .map(output_rect);
            let Some(rect) = named.or_else(|| {
                layout::nearest(layout::center((old.0, old.1), (old.2, old.3)), &rects)
                    .map(|i| rects[i])
            }) else {
                continue;
            };
            let area = self.usable_rect(rect);
            if area != old {
                self.tile_and_configure(id, area);
            }
        }
    }

    /// Start a drag of the shared edges `edges` of `window` when it is a member of a container,
    /// and return true: the divider of the container's tree moves, the windows on both sides of
    /// it follow, and the window is not sized by itself. Returns false for a window in no
    /// container, which resizes as always. A maximized or fullscreen member has no divider to
    /// drag, it is answered with true and nothing happens.
    pub fn start_member_resize(
        &mut self,
        window: &Window,
        start_data: GrabStartData<JffState>,
        edges: geom::Edges,
        serial: Serial,
    ) -> bool {
        let Some(id) = self.desk.container_of(|w| w == window) else {
            return false;
        };
        let filled = self.desk.is_maximized(|w| w == window)
            || self.desk.mode_of(|w| w == window) == WindowMode::Fullscreen;
        let (Some(pointer), Some(pos), false) = (
            self.seat.get_pointer(),
            self.desk.pos_of(|w| w == window),
            filled,
        ) else {
            return true;
        };
        let size = window.geometry().size;
        let grab = TileResizeGrab::new(
            start_data,
            window.clone(),
            id,
            edges,
            (pos.0, pos.1, size.w, size.h),
        );
        pointer.set_grab(self, grab, serial, Focus::Clear);
        true
    }

    /// The pointer of a drag that began with `window` at `rect` has moved by `delta`: move the
    /// edges being dragged there, in the container's tree. Returns whether the tree changed.
    /// The windows are not laid out, the caller does (see [`JffState::retile_container`]).
    pub fn resize_member(
        &mut self,
        window: &Window,
        edges: geom::Edges,
        rect: Rect,
        delta: (i32, i32),
    ) -> bool {
        let gap = self.config.snap_gap();
        let dragged = [
            (edges.left, Edge::Left, rect.0 + delta.0),
            (edges.right, Edge::Right, rect.0 + rect.2 + delta.0),
            (edges.top, Edge::Top, rect.1 + delta.1),
            (edges.bottom, Edge::Bottom, rect.1 + rect.3 + delta.1),
        ];
        let mut moved = false;
        for (on, edge, to) in dragged {
            if !on {
                continue;
            }
            let ratio = self
                .desk
                .resize_container(|w| w == window, edge, to, gap, MIN_TILE);
            moved |= matches!(ratio, Ok(Some(_)));
        }
        moved
    }

    /// Pick up the window of container `window` with the pointer and carry it until `end`
    /// (see [`TileMoveGrab`]). Returns true when the grab started. Returns false for a window
    /// in no container, one that is fullscreen, and with `tiling-containers` off: the caller
    /// moves it as any window.
    pub fn start_tile_move(
        &mut self,
        window: &Window,
        start_data: GrabStartData<JffState>,
        serial: Serial,
        end: TileMoveEnd,
    ) -> bool {
        if !self.config.tiling_containers() || self.desk.container_of(|w| w == window).is_none() {
            return false;
        }
        if self.desk.mode_of(|w| w == window) == WindowMode::Fullscreen {
            return false;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let grab = TileMoveGrab::new(start_data, window.clone(), end);
        pointer.set_grab(self, grab, serial, Focus::Clear);
        true
    }

    /// `container-drag`: pick up the focused window if it is in a container, to be put down by
    /// the next click.
    pub fn start_focused_tile_move(&mut self) {
        let Some(window) = self.focused_window() else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let start_data = GrabStartData {
            focus: None,
            button: BTN_LEFT,
            location: pointer.current_location(),
        };
        self.start_tile_move(
            &window,
            start_data,
            SERIAL_COUNTER.next_serial(),
            TileMoveEnd::Click,
        );
    }

    /// A window of a container was put down with the pointer at `at` (see [`TileMoveGrab`]).
    ///
    /// At the edge of a monitor it leaves its container and snaps or maximizes, the same as a
    /// floating window let go there. Over a window of a container, its own or another, it goes
    /// there: the middle of that window swaps the two, an edge puts it beside on that side (see
    /// [`leaf_drop`]), and both containers are laid out again. Anywhere else (off every
    /// container, on another workspace's) it leaves its container and is maximized. Over the gap
    /// between two windows nothing happens.
    pub fn finish_tile_move(&mut self, window: &Window, at: Point<f64, Logical>) {
        let Some(from) = self.desk.container_of(|w| w == window) else {
            return;
        };
        if let Some(mode) = self.edge_drop_mode(at) {
            // Dragged to the edge that snaps: the container is over, every window of it goes
            // back to the stack, maximized.
            if matches!(mode, WindowMode::Snapped(_)) {
                self.dissolve_container(from, window);
                return;
            }
            if let Some(floating) = self.eject_from_container(window, at) {
                if let Some(toplevel) = window.toplevel() {
                    self.set_mode(toplevel, mode);
                }
                // It goes back to the size it was given on leaving, not the size of its tile.
                self.desk.set_restore_where(|w| w == window, floating);
            }
            return;
        }
        let point = (at.x, at.y);
        let gap = self.config.snap_gap();
        let Some(id) = self.desk.container_at(point) else {
            // Carried out of every container: it leaves its own and is maximized, on the
            // monitor it was put down on.
            if let Some(floating) = self.eject_from_container(window, at) {
                if let Some(toplevel) = window.toplevel() {
                    self.set_mode(toplevel, WindowMode::Maximized);
                }
                // It goes back to the size it was given on leaving, not the size of its tile.
                self.desk.set_restore_where(|w| w == window, floating);
            }
            return;
        };
        let Some((target, rect)) = self.desk.drop_target(id, point, gap) else {
            return;
        };
        let drop = leaf_drop(rect, point);
        if let Err(err) = self
            .desk
            .move_member(|w| w == window, id, target, drop, gap)
        {
            tracing::warn!("could not move the window in its container: {err:?}");
            return;
        }
        self.retile_container(id);
        if from != id {
            self.retile_container(from);
        }
        self.focus_window(window);
    }

    /// Take container `id` apart: every member leaves it and is maximized, stacked on top of
    /// the others, and the container is gone. `keep` (the window the user carried) goes on top
    /// and has the keyboard. A fullscreen member stays fullscreen.
    fn dissolve_container(&mut self, id: ContainerId, keep: &Window) {
        let members: Vec<Window> = self
            .desk
            .members(id)
            .iter()
            .map(|entry| entry.window.clone())
            .collect();
        let others = members.iter().filter(|w| *w != keep);
        let carried = members.iter().filter(|w| *w == keep);
        for window in others.chain(carried) {
            if !self.desk.leave_container(|w| w == window) {
                continue;
            }
            if self.desk.mode_of(|w| w == window) == WindowMode::Fullscreen {
                continue;
            }
            if let Some(toplevel) = window.toplevel() {
                self.set_mode(toplevel, WindowMode::Maximized);
            }
        }
        self.focus_window(keep);
        tracing::info!("container {} dissolved", id.get());
    }

    /// `container-eject`: take the focused window out of its container.
    pub fn eject_focused(&mut self) {
        let Some(window) = self.focused_window() else {
            return;
        };
        let Some(pos) = self.desk.pos_of(|w| *w == window) else {
            return;
        };
        if self.desk.mode_of(|w| *w == window) == WindowMode::Fullscreen {
            return;
        }
        let size = window.geometry().size;
        let center = (
            f64::from(pos.0) + f64::from(size.w) / 2.0,
            f64::from(pos.1) + f64::from(size.h) / 2.0,
        );
        self.eject_from_container(&window, center.into());
    }

    /// Take `window` out of its container: the tree closes up around it and the others take its
    /// room, and it floats at half the size of its monitor, put so that `at` is at the same
    /// relative spot in it as in the monitor. It keeps the keyboard. Returns where it was put
    /// and its size, or `None`, having changed nothing, for a window that is in no container.
    fn eject_from_container(
        &mut self,
        window: &Window,
        at: Point<f64, Logical>,
    ) -> Option<((i32, i32), (i32, i32))> {
        let from = self.desk.container_of(|w| w == window)?;
        let pos = self.desk.pos_of(|w| w == window)?;
        let size = window.geometry().size;
        let monitor = self.rect_for_window(pos, (size.w, size.h));
        let (new_pos, new_size) = shrunk_geometry(monitor, at);
        if !self.desk.leave_container(|w| w == window) {
            return None;
        }
        self.desk.set_pos_where(|w| w == window, new_pos);
        if let Some(toplevel) = window.toplevel() {
            configure_mode(toplevel, WindowMode::Floating, false, Some(new_size));
        }
        self.retile_container(from);
        self.focus_window(window);
        tracing::info!("window left container {}", from.get());
        Some((new_pos, new_size))
    }

    /// The rectangle a container started by a snap fills: the usable area of `monitor`, the same
    /// as any other container.
    fn snapped_area(&self, monitor: Rect) -> Rect {
        geom::inset_rect(self.panel_free_rect(monitor), self.config.maximize_inset())
    }

    /// `window` is snapped to `side` (a drag to the edge of the monitor, `snap-*` binds): with
    /// `tiling-containers` on it starts a container of its own that fills the monitor's usable
    /// area, tiled together with every other window of the workspace on that monitor, and
    /// windows dropped on it or opened from it join it from then on. Returns
    /// true when it did. Returns false, having changed nothing, when it does not, and the
    /// caller snaps the window as usual: `tiling-containers` is off, the window is in a
    /// container already or fullscreen, or a `window-rule` keeps it `floating`.
    pub(crate) fn snap_as_container(&mut self, window: &Window, side: Side) -> bool {
        if !self.config.tiling_containers() || self.floats_by_rule(window) {
            return false;
        }
        let Some((pos, size)) = self
            .desk
            .find_by(|w| w == window)
            .filter(|e| e.container.is_none() && e.mode != WindowMode::Fullscreen)
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return false;
        };
        let monitor = self.rect_for_window(pos, (size.w, size.h));
        let area = self.snapped_area(monitor);
        let name = self
            .with_facts(window, |facts| {
                self.config.container_name_for(facts).map(str::to_owned)
            })
            .flatten();
        // Every other window of the workspace on the same monitor is tiled with it, topmost
        // first: the ones that are in no container, not fullscreen, and that no `window-rule`
        // keeps floating.
        let rects = self.output_rects();
        let monitor_index = layout::nearest(layout::center(pos, (size.w, size.h)), &rects);
        let others: Vec<Window> = self
            .desk
            .top_down()
            .filter(|e| {
                e.window != *window && e.container.is_none() && e.mode != WindowMode::Fullscreen
            })
            .filter(|e| {
                let size = e.window.geometry().size;
                layout::nearest(layout::center(e.pos, (size.w, size.h)), &rects) == monitor_index
            })
            .filter(|e| !self.floats_by_rule(&e.window))
            .map(|e| e.window.clone())
            .collect();
        // The first one goes on the side opposite to the snap, the rest split the window that
        // joined last, along its longer side.
        let created = match others.first() {
            Some(first) => {
                self.desk
                    .create_container_beside(|w| w == window, |w| w == first, side, name)
            }
            None => self.desk.create_container(|w| w == window, name),
        };
        let id = match created {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!("could not start a container: {err:?}");
                return false;
            }
        };
        let gap = self.config.snap_gap();
        for other in others.iter().skip(1) {
            if let Err(err) = self.desk.join_container(id, None, |w| w == other, gap) {
                tracing::warn!("could not add a window to the container: {err:?}");
            }
        }
        // A member does not float, snap or maximize: its place comes from the tree.
        self.desk.float_in_place_where(|w| w == window);
        for other in &others {
            self.desk.float_in_place_where(|w| w == other);
        }
        // The snapped window keeps the focus inside the container, as it has the keyboard.
        self.desk.set_container_focus(|w| w == window);
        self.tile_and_configure(id, area);
        tracing::info!(
            "container {} started by a window snapped {side:?}",
            id.get()
        );
        true
    }

    /// Lay container `id` out in `area` and tell every member its rectangle.
    fn tile_and_configure(&mut self, id: ContainerId, area: Rect) {
        let zoomed = self
            .desk
            .container(id)
            .and_then(|container| container.zoomed());
        let tiles = self.desk.tile_container(id, area, self.config.snap_gap());
        for (member, rect) in tiles {
            // A fullscreen member stays as it is until it leaves fullscreen.
            if self
                .desk
                .find_by_id(member)
                .is_some_and(|entry| entry.mode == WindowMode::Fullscreen)
            {
                continue;
            }
            let toplevel = self
                .desk
                .find_by_id(member)
                .and_then(|entry| entry.window.toplevel().cloned());
            if let Some(toplevel) = toplevel {
                configure_member(&toplevel, (rect.2, rect.3), zoomed == Some(member));
            }
        }
    }

    /// Lay container `id` out again where it is, after its tree or its maximized member
    /// changed, and configure every member. Does nothing for a container that was never laid
    /// out or is gone.
    pub fn retile_container(&mut self, id: ContainerId) {
        let Some(area) = self
            .desk
            .container(id)
            .and_then(|container| container.area())
        else {
            return;
        };
        self.tile_and_configure(id, area);
    }

    /// A member of container `id` was closed and has left the desk: the others take its room,
    /// and when it had the keyboard, the container's focused window gets it. The container is
    /// gone with its last window, and then there is nothing to do.
    pub fn container_after_close(&mut self, id: ContainerId, closed: &Window) {
        let Some(focused) = self.desk.container(id).map(|container| container.focused()) else {
            return;
        };
        self.retile_container(id);
        if self.focused_window.as_ref() != Some(closed) {
            return;
        }
        let next = self
            .desk
            .find_by_id(focused)
            .map(|entry| entry.window.clone());
        if let Some(window) = next {
            self.focus_window(&window);
        }
    }

    /// Carry out a maximize or a put back (`target`) for `window` when it is a member of a
    /// container, and return true: the member fills the container's area or goes back to its
    /// place in the tree, and everyone is configured. A member never floats or snaps, so those
    /// answer with the tree's rectangle. Returns false for a window in no container and for
    /// fullscreen, which the caller does as for any window.
    pub fn set_member_mode(&mut self, window: &Window, target: WindowMode) -> bool {
        let Some(id) = self.desk.container_of(|w| w == window) else {
            return false;
        };
        let zoom = match target {
            WindowMode::Fullscreen => return false,
            // A member is never maximized, whoever asks (`maximize-new-windows`, the client, the
            // keybind): it keeps its tile. Still answered with a configure, as the protocol
            // requires.
            WindowMode::Maximized => {
                self.retile_container(id);
                return true;
            }
            WindowMode::Floating => {
                // Also what leaving fullscreen comes to.
                self.desk.float_in_place_where(|w| w == window);
                false
            }
            WindowMode::Snapped(_) => {
                self.retile_container(id);
                return true;
            }
        };
        if self.desk.zoom_in_container(|w| w == window, zoom).is_err() {
            return false;
        }
        self.retile_container(id);
        true
    }

    /// Give the keyboard to the window after (`backwards` false) or before the focused one in
    /// its container, in tree order, wrapping. When the focused one is maximized, the next one
    /// is maximized instead, so what is shown is what has the keyboard. Does nothing when the
    /// focused window is in no container.
    pub fn focus_container_step(&mut self, backwards: bool) {
        let Some(window) = self.focused_window() else {
            return;
        };
        let Some(id) = self.desk.container_of(|w| *w == window) else {
            return;
        };
        let Some(next) = self
            .desk
            .step_in_container(|w| *w == window, !backwards)
            .map(|entry| entry.window.clone())
        else {
            return;
        };
        if next == window {
            return;
        }
        if self.desk.is_maximized(|w| *w == window)
            && self.desk.zoom_in_container(|w| *w == next, true).is_ok()
        {
            self.retile_container(id);
        }
        self.focus_window(&next);
    }

    /// Focus the first container named `name` on the current workspace (see
    /// [`JffState::select_container`]). When there is none, shows an error toast and returns
    /// false.
    pub fn focus_container_named(&mut self, name: &str) -> bool {
        let workspace = self.current_workspace();
        let found = self
            .desk
            .container_named(workspace, name)
            .is_some_and(|id| self.select_container(id));
        if !found {
            self.show_error(
                "No such container",
                &format!("There is no container named \"{name}\" on this workspace."),
            );
        }
        found
    }
}

/// The look of a container's border: the `border` block of the config when it has one, else a
/// thin one in the default colours. A container is a visible object, so it always has one.
pub fn container_border_style(config: &Config) -> Border {
    let border = config.border();
    if border.on() {
        border
    } else {
        Border {
            width: DEFAULT_BORDER_WIDTH,
            ..Border::default()
        }
    }
}

/// The border of the container `entry` is a member of, to push right after the elements of
/// `entry` while drawing the windows top down, or `None`. Only the lowest member of the
/// container returns it, so the ring is behind every member and drawn once, and only a container
/// that has been laid out has one. `origin` is the top-left of the monitor being drawn, in the
/// shared space.
///
/// Not a method: the renderer is borrowed out of the state where this is called, so it takes the
/// parts it reads.
pub fn container_border_after(
    desk: &Desk<Window>,
    config: &Config,
    focused: Option<&Window>,
    renderer: &mut GlesRenderer,
    entry: &Entry<Window>,
    origin: Point<i32, Logical>,
) -> Option<JffElement> {
    let id = entry.container?;
    let area = desk.container(id)?.area()?;
    let lowest = desk
        .top_down()
        .filter(|member| member.container == Some(id))
        .last()?;
    if lowest.window != entry.window {
        return None;
    }
    let focused = focused.is_some_and(|window| desk.container_of(|w| w == window) == Some(id));
    let rect = Rectangle::<i32, Logical>::new(
        (area.0 - origin.x, area.1 - origin.y).into(),
        (area.2, area.3).into(),
    );
    let paint = BorderPaint {
        style: container_border_style(config),
        focused,
    };
    container_border_element(renderer, id.get(), rect, config.radius(), paint)
        .map(JffElement::Border)
}
