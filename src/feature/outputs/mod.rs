//! Monitors in the shared desktop space: where each one sits, and what that means for windows
//! and the pointer. Backend-agnostic, the TTY backend calls in when monitors come and go.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use smithay::backend::input::{AbsolutePositionEvent, InputBackend, InputTime};
use smithay::desktop::layer_map_for_output;
use smithay::desktop::space::SpaceElement;
use smithay::input::pointer::MotionEvent;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform, SERIAL_COUNTER};
use smithay::wayland::fractional_scale::with_fractional_scale;

use crate::config::{Config, ModeRequest, OutputConfig, RuleOutput, WindowRule};
use crate::desk::{Side, WindowId, WindowMode};
use crate::primitives::edid::Identity;
use crate::primitives::geom;
use crate::primitives::layout::{self, Rect};
use crate::primitives::suspend::{behind_cover, suspended};
use crate::state::JffState;

/// Area used when there is no monitor at all.
const NO_OUTPUT: Rect = (0, 0, 800, 600);

const EDGE_MARGIN: f64 = 2.0;

/// The longest an unplugged monitor's global stays around for clients to react first. Clients
/// that acknowledge its removal (wl_fixes v2) can end the wait sooner, see [`Retirement`].
pub const GLOBAL_GRACE: Duration = Duration::from_secs(10);

/// What a removed global waits for before it is destroyed. A client that bound a global at the
/// moment it was removed gets an error, so it is kept until nobody can try that any more.
///
/// Clients that bound wl_fixes v2 say so with `ack_global_remove`, once all of them have, and
/// no client is left that can't (an older libwayland, or one that never bound wl_fixes), the
/// global is destroyed. Otherwise the wait runs [`GLOBAL_GRACE`]. Clients that connect after the
/// removal never saw the global, they are not in either set. `C` is a client's id.
#[derive(Clone, Debug)]
pub struct Retirement<C> {
    since: Instant,
    /// Clients that will acknowledge, and have not yet.
    waiting: HashSet<C>,
    /// Clients that will not acknowledge: only time helps.
    unaware: HashSet<C>,
}

impl<C: Eq + Hash> Retirement<C> {
    pub fn new(since: Instant, waiting: HashSet<C>, unaware: HashSet<C>) -> Self {
        Self {
            since,
            waiting,
            unaware,
        }
    }

    /// `client` acknowledged the removal.
    pub fn ack(&mut self, client: &C) {
        self.waiting.remove(client);
    }

    /// Whether the global can be destroyed now, given the clients still `connected`. A client
    /// that left needs nothing more.
    pub fn ready(&mut self, now: Instant, connected: &HashSet<C>) -> bool {
        if now.duration_since(self.since) >= GLOBAL_GRACE {
            return true;
        }
        self.waiting.retain(|client| connected.contains(client));
        self.unaware.retain(|client| connected.contains(client));
        self.waiting.is_empty() && self.unaware.is_empty()
    }
}

/// One video mode a connected monitor offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorMode {
    pub width: i32,
    pub height: i32,
    /// Refresh in mHz, as `wl_output` reports it.
    pub refresh: i32,
    pub preferred: bool,
}

/// A connected monitor, lit or not, and its modes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Monitor {
    /// Connector name, like `DP-1`.
    pub name: String,
    /// Physical size in mm, `(0, 0)` when unknown.
    pub size_mm: (i32, i32),
    pub modes: Vec<MonitorMode>,
    /// Who made it and which one it is, from its EDID.
    pub identity: Identity,
}

impl Monitor {
    /// What a person would call the monitor: `make model (name)`, or just the connector name
    /// when the EDID says neither make nor model.
    pub fn description(&self) -> String {
        if self.identity.make.is_none() && self.identity.model.is_none() {
            return self.name.clone();
        }
        format!(
            "{} {} ({})",
            self.identity.make(),
            self.identity.model(),
            self.name
        )
    }

    /// Whether the monitor offers the requested size (and refresh, when one is given).
    pub fn has_mode(&self, request: &ModeRequest) -> bool {
        self.modes.iter().any(|m| {
            (m.width, m.height) == (request.width, request.height)
                && request
                    .refresh
                    .map_or(true, |hz| mhz_to_hz(m.refresh) == hz)
        })
    }
}

/// `wl_output` refresh (mHz) to whole Hz, as the config uses.
pub fn mhz_to_hz(mhz: i32) -> i32 {
    (mhz + 500) / 1000
}

/// The size of the picture drawn for `output`, in pixels: its mode in the orientation the
/// monitor is used in, so width and height are swapped for a quarter turn. The output's
/// compositor turns the frame onto the panel, so everything is drawn upright at this size.
/// `None` when the monitor has no mode (is off).
pub fn frame_size(output: &Output) -> Option<(i32, i32)> {
    let mode = output.current_mode()?;
    let size = output.current_transform().transform_size(mode.size);
    Some((size.w, size.h))
}

/// The scale `name` is asked to be drawn at, 1 when the config says nothing.
pub fn wanted_scale(config: &Config, name: &str) -> f64 {
    config.output(name).map_or(1.0, OutputConfig::scale)
}

/// The area `output` covers in the shared space, in logical pixels: the size of its picture
/// divided by its scale.
pub fn output_rect(output: &Output) -> Rect {
    let loc = output.current_location();
    let size = frame_size(output).unwrap_or((0, 0));
    let scale = output.current_scale().fractional_scale();
    let logical = |pixels: i32| (f64::from(pixels) / scale).round() as i32;
    (loc.x, loc.y, logical(size.0), logical(size.1))
}

/// The transform that undoes `transform`. Smithay's `Transform::invert` swaps 90 and 270, which is
/// right for a turn, but it also swaps `Flipped90` and `Flipped270`. Those two are reflections
/// across a diagonal, and a reflection is its own inverse, so they stay as they are.
fn undo(transform: Transform) -> Transform {
    match transform {
        Transform::Flipped90 | Transform::Flipped270 => transform,
        other => other.invert(),
    }
}

/// Where a point of a panel's own picture (`native` pixels, `point` in them) is in the upright
/// frame drawn for it: what `transform` did to the frame, undone.
pub fn upright_point(transform: Transform, native: (i32, i32), point: (f64, f64)) -> (f64, f64) {
    let area: Size<f64, Logical> = (f64::from(native.0), f64::from(native.1)).into();
    let upright = undo(transform).transform_point_in(Point::from(point), &area);
    (upright.x, upright.y)
}

impl JffState {
    /// Where an absolute device (touchscreen, pen) event is on `output`, from the monitor's own
    /// top left, in logical pixels. The device reports where it was touched on the panel as
    /// built, so on a turned monitor that is turned back to the upright frame the windows are in,
    /// and a scaled one is divided by its scale. (Winit has no turned panel: its output only
    /// mirrors the picture on its way into the host's window.)
    pub fn absolute_on_output<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        output: &Output,
        event: &E,
    ) -> Point<f64, Logical> {
        let (native, transform) = match output.current_mode() {
            Some(mode) if self.backend.is_tty() => {
                ((mode.size.w, mode.size.h), output.current_transform())
            }
            _ => {
                let rect = output_rect(output);
                ((rect.2, rect.3), Transform::Normal)
            }
        };
        let at = event.position_transformed(Size::<i32, Logical>::from(native));
        let scale = output.current_scale().fractional_scale();
        let (x, y) = upright_point(transform, native, (at.x, at.y));
        (x / scale, y / scale).into()
    }

    /// Area of every monitor, in the order they were added.
    pub fn output_rects(&self) -> Vec<Rect> {
        self.outputs.iter().map(output_rect).collect()
    }

    /// Monitor the pointer is on, or the closest one.
    pub fn rect_at_pointer(&self) -> Rect {
        let rects = self.output_rects();
        let p = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        layout::nearest((p.x, p.y), &rects)
            .map(|i| rects[i])
            .unwrap_or(NO_OUTPUT)
    }

    /// The monitor a new window should open on, as an index into `outputs`.
    ///
    /// The monitor the pointer is on, unless a window already fills it (there is no free room
    /// there). Then the closest monitor with room is used, so a second window doesn't open on
    /// top of a maximized first one. A window that isn't shown yet counts
    /// as filling its monitor when new windows are maximized, since that is what it will do.
    /// A monitor with a tiling container on it is filled too.
    pub fn placement_output_index(&self) -> Option<usize> {
        let rects = self.output_rects();
        if rects.is_empty() {
            return None;
        }
        let pointer = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        let pointer = (pointer.x, pointer.y);
        let home = layout::nearest(pointer, &rects)?;

        let auto_maximize = self.config.maximize_new_windows();
        let mut filled = vec![false; rects.len()];
        for entry in self.desk.all_top_down() {
            if !self.desk.is_visible(entry) {
                continue;
            }
            // A container fills its monitor, whatever the modes of its members say: they are
            // placed by the tree, and float as far as the desk is concerned.
            if let Some(area) = entry
                .container
                .and_then(|id| self.desk.container(id))
                .and_then(|container| container.area())
            {
                let at = layout::center((area.0, area.1), (area.2, area.3));
                if let Some(i) = layout::nearest(at, &rects) {
                    filled[i] = true;
                }
                continue;
            }
            let fills = match entry.mode {
                WindowMode::Maximized | WindowMode::Fullscreen => true,
                WindowMode::Floating => auto_maximize && !entry.fitted,
                WindowMode::Snapped(_) => false,
            };
            if !fills {
                continue;
            }
            let size = entry.window.geometry().size;
            let at = layout::center(entry.pos, (size.w, size.h));
            if let Some(i) = layout::nearest(at, &rects) {
                filled[i] = true;
            }
        }
        if !filled[home] {
            return Some(home);
        }
        let distance = |i: usize| {
            let r = rects[i];
            let c = layout::center((r.0, r.1), (r.2, r.3));
            (c.0 - pointer.0).powi(2) + (c.1 - pointer.1).powi(2)
        };
        (0..rects.len())
            .filter(|&i| !filled[i])
            .min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
            .or(Some(home))
    }

    /// [`JffState::placement_output_index`], except that the monitor a window rule names comes
    /// first: `open-on-output "DP-1"` is the monitor with that name, `"pointer"` the one the
    /// pointer is on. A monitor that isn't connected (the rule is for a setup that isn't
    /// plugged in) is skipped with a warning, and the usual choice stands.
    pub fn placement_output_index_for(&self, rule: Option<&WindowRule>) -> Option<usize> {
        match rule.and_then(|rule| rule.output.as_ref()) {
            Some(RuleOutput::Pointer) => {
                let rects = self.output_rects();
                let pointer = self
                    .seat
                    .get_pointer()
                    .map(|p| p.current_location())
                    .unwrap_or_default();
                if let Some(i) = layout::nearest((pointer.x, pointer.y), &rects) {
                    return Some(i);
                }
            }
            Some(RuleOutput::Named(name)) => {
                if let Some(i) = self.outputs.iter().position(|o| o.name() == *name) {
                    return Some(i);
                }
                tracing::warn!("window rule: output {name:?} is not connected");
            }
            None => {}
        }
        self.placement_output_index()
    }

    pub fn edge_drop_mode(&self, at: Point<f64, Logical>) -> Option<WindowMode> {
        let rects = self.output_rects();
        let i = layout::nearest((at.x, at.y), &rects)?;
        match geom::edge_at(rects[i], (at.x, at.y), EDGE_MARGIN)? {
            side @ (Side::Left | Side::Right) => Some(WindowMode::Snapped(side)),
            _ => Some(WindowMode::Maximized),
        }
    }

    /// Monitor a window of `size` at `pos` belongs to: the one under its center.
    pub fn rect_for_window(&self, pos: (i32, i32), size: (i32, i32)) -> Rect {
        let rects = self.output_rects();
        layout::nearest(layout::center(pos, size), &rects)
            .map(|i| rects[i])
            .unwrap_or(NO_OUTPUT)
    }

    /// Start using a monitor: advertise it, lay it out with the others.
    pub fn add_output(&mut self, output: Output) {
        let global = output.create_global::<Self>(&self.display_handle);
        self.output_globals.insert(output.name(), global);
        self.outputs.push(output);
        self.relayout_outputs();
    }

    /// Stop using a monitor: windows move to the remaining ones, clients are told it's gone.
    pub fn remove_output(&mut self, output: &Output) {
        self.outputs.retain(|o| o != output);
        // Panels on a monitor that's gone have nowhere to be: tell them to quit (or move).
        for layer in smithay::desktop::layer_map_for_output(output).layers() {
            layer.layer_surface().send_close();
        }
        if self.overview.as_ref().is_some_and(|ov| ov.shows(output)) {
            self.abort_overview();
        }
        if self.screenshot.as_ref().is_some_and(|ui| ui.shows(output)) {
            self.cancel_screenshot();
        }
        if let Some(global) = self.output_globals.remove(&output.name()) {
            self.display_handle.disable_global::<Self>(global.clone());
            let retirement = self.retirement();
            self.retiring_globals.push((global, retirement));
        }
        // Every window, including ones on workspaces that aren't shown.
        for entry in self.desk.all_top_down() {
            entry.window.output_leave(output);
        }
        self.relayout_outputs();
    }

    /// Who a global removed now has to wait for: clients that bound wl_fixes v2 acknowledge it,
    /// all the others can only be waited out.
    fn retirement(&mut self) -> Retirement<ClientId> {
        let connected = crate::state::connected_clients();
        // Clients that left are forgotten here, the set would only grow otherwise.
        self.fixes.acking.retain(|id| connected.contains(id));
        let (waiting, unaware): (HashSet<_>, HashSet<_>) = connected
            .into_iter()
            .partition(|id| self.fixes.acking.contains(id));
        Retirement::new(Instant::now(), waiting, unaware)
    }

    /// `client` sent `wl_fixes.ack_global_remove` for the global it knows as `name`. A name
    /// matching no global that is going away (it was destroyed already) is ignored.
    pub fn ack_global_remove(&mut self, client: &Client, name: u32) {
        let handle = self.display_handle.backend_handle();
        let id = client.id();
        for (global, retirement) in &mut self.retiring_globals {
            // A global's name is the client's own: registries number them per connection.
            if handle.global_name(global.clone(), id.clone()) == Some(name) {
                retirement.ack(&id);
            }
        }
    }

    /// Destroy globals of unplugged monitors that every client has acknowledged, or that were
    /// disabled long enough ago.
    pub fn reap_globals(&mut self) {
        if self.retiring_globals.is_empty() {
            return;
        }
        let now = Instant::now();
        let connected = crate::state::connected_clients();
        for (global, mut retirement) in std::mem::take(&mut self.retiring_globals) {
            if retirement.ready(now, &connected) {
                self.display_handle.remove_global::<Self>(global);
            } else {
                self.retiring_globals.push((global, retirement));
            }
        }
    }

    /// Apply a changed config to the monitors that are already connected.
    pub fn reconcile_outputs(&mut self) {
        if self.backend.is_tty() {
            self.scan_tty_outputs();
        }
        self.relayout_outputs();
    }

    /// Place every monitor in the shared space, then bring windows and the pointer along:
    /// maximized windows follow their monitor, anything left out of reach is moved back.
    pub fn relayout_outputs(&mut self) {
        let sizes: Vec<(i32, i32)> = self.output_rects().iter().map(|r| (r.2, r.3)).collect();
        let fixed: Vec<Option<(i32, i32)>> = self
            .outputs
            .iter()
            .map(|o| self.config.output(&o.name()).and_then(|c| c.position))
            .collect();
        for (output, pos) in self.outputs.iter().zip(layout::arrange(&sizes, &fixed)) {
            let loc = output.current_location();
            if (loc.x, loc.y) != pos {
                output.change_current_state(None, None, None, Some(pos.into()));
            }
        }

        // Monitors may have changed size: lay panels out again before windows look at their room.
        self.arrange_layers();

        let rects = self.output_rects();
        let usable: Vec<Rect> = rects.iter().map(|r| self.usable_rect(*r)).collect();
        let free: Vec<Rect> = rects.iter().map(|r| self.panel_free_rect(*r)).collect();
        let (inset, snap_gap) = (self.config.maximize_inset(), self.config.snap_gap());
        // Zones keep their place on their monitor, the windows below then only see what is left.
        self.zones_follow_outputs();
        if !rects.is_empty() {
            for entry in self.desk.iter_mut() {
                let size = entry.window.geometry().size;
                let size = (size.w, size.h);
                if entry.mode != WindowMode::Floating {
                    let Some(i) = layout::nearest(layout::center(entry.pos, size), &rects) else {
                        continue;
                    };
                    let rect = match entry.mode {
                        WindowMode::Fullscreen => rects[i],
                        WindowMode::Snapped(side) => {
                            geom::snapped_rect(free[i], inset, snap_gap, side)
                        }
                        _ => usable[i],
                    };
                    entry.pos = (rect.0, rect.1);
                    if let Some(toplevel) = entry.window.toplevel() {
                        toplevel.with_pending_state(|s| s.size = Some((rect.2, rect.3).into()));
                        toplevel.send_pending_configure();
                    }
                } else if !layout::reachable(entry.pos, size, &rects) {
                    let Some(i) = layout::nearest(layout::grab_point(entry.pos, size), &rects)
                    else {
                        continue;
                    };
                    entry.pos = layout::fit_into(entry.pos, size, rects[i]);
                }
            }
        }

        // A container fills its monitor, so it follows the monitor's new size.
        self.refit_containers();
        self.relayout_lock_surfaces();
        self.clamp_pointer();
        self.reconcile_window_workspaces();
        self.refresh_window_outputs();
        // wlr-output-management clients (wlr-randr, kanshi) want to know about any change.
        self.output_management_changed();
    }

    /// Pull the pointer back onto a monitor if its previous one is gone or moved.
    fn clamp_pointer(&mut self) {
        let rects = self.output_rects();
        if rects.is_empty() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let loc = pointer.current_location();
        let (x, y) = layout::clamp_to_rects((loc.x, loc.y), &rects);
        if (x, y) == (loc.x, loc.y) {
            return;
        }
        let pos: Point<f64, Logical> = (x, y).into();
        let under = self.pointer_focus_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    /// The window that covers the monitor at `rect` with a fullscreen window the windows behind
    /// it can't be seen through (see `fullscreen_covers`): the topmost one there. `None` when
    /// nothing covers the monitor.
    fn fullscreen_cover_id(&self, rect: Rect) -> Option<WindowId> {
        if !crate::protocol::layer_shell::fullscreen_covers(&self.desk, rect) {
            return None;
        }
        self.desk
            .top_down()
            .find(|entry| {
                let size = entry.window.geometry().size;
                layout::intersects((entry.pos.0, entry.pos.1, size.w, size.h), rect)
            })
            .map(|entry| entry.id)
    }

    /// Whether `entry` is hidden by a fullscreen window on every monitor it is on, `covers`
    /// being what [`Self::fullscreen_cover_id`] says for each of `outputs`.
    fn behind_a_cover(
        &self,
        entry: &crate::desk::Entry<smithay::desktop::Window>,
        outputs: &[(&Output, Rect)],
        covers: &[Option<WindowId>],
    ) -> bool {
        let size = entry.window.geometry().size;
        let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
        behind_cover(
            entry.id,
            outputs
                .iter()
                .zip(covers)
                .map(|((_, rect), cover)| (layout::intersects(area, *rect), *cover)),
        )
    }

    /// A hash of what [`Self::refresh_window_outputs`] decides from: every monitor's name, place
    /// and scale, and every window's surface, position, size and whether it is visible. Costs one
    /// pass over the windows and nothing is sent or locked, unlike the pass it guards.
    fn window_outputs_signature(
        &self,
        outputs: &[(&Output, Rect)],
        covers: &[Option<WindowId>],
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        outputs.len().hash(&mut hasher);
        for (output, rect) in outputs {
            output.name().hash(&mut hasher);
            rect.hash(&mut hasher);
            output
                .current_scale()
                .fractional_scale()
                .to_bits()
                .hash(&mut hasher);
        }
        for entry in self.desk.all_top_down() {
            // A window with no toplevel (X11) is told apart by its place alone.
            entry
                .window
                .toplevel()
                .map(|toplevel| toplevel.wl_surface().id())
                .hash(&mut hasher);
            let size = entry.window.geometry().size;
            entry.pos.hash(&mut hasher);
            (size.w, size.h).hash(&mut hasher);
            self.desk.is_visible(entry).hash(&mut hasher);
            // Hidden or covered, and not shown yet, decide `suspended`.
            entry.fitted.hash(&mut hasher);
            self.behind_a_cover(entry, outputs, covers)
                .hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Tell every window which monitors it's on (clients use it for scale, and to pause
    /// animations on windows nobody can see). A window on a hidden workspace is on no monitor.
    /// A window nobody can see (on a workspace that is not shown, put away or minimized, or
    /// behind a fullscreen window that covers its monitor) is also told it is suspended
    /// (`xdg_toplevel` state `suspended`), and is told it is not any more when it shows again.
    ///
    /// Runs every frame of every monitor, so it does nothing unless something it reads has
    /// changed: a surface committed (a new subsurface, popup or panel), or the signature of the
    /// monitors and the windows' places differs from the last pass.
    pub fn refresh_window_outputs(&self) {
        let outputs: Vec<(&Output, Rect)> =
            self.outputs.iter().map(|o| (o, output_rect(o))).collect();
        let dirty = self.window_outputs_dirty.swap(false, Ordering::Relaxed);
        let covers: Vec<Option<WindowId>> = outputs
            .iter()
            .map(|(_, rect)| self.fullscreen_cover_id(*rect))
            .collect();
        let signature = self.window_outputs_signature(&outputs, &covers);
        if !dirty && self.window_outputs_sig.load(Ordering::Relaxed) == signature {
            return;
        }
        self.window_outputs_sig.store(signature, Ordering::Relaxed);
        let overlap = Rectangle::<i32, Logical>::from_size(Size::from((i32::MAX, i32::MAX)));
        for entry in self.desk.all_top_down() {
            let size = entry.window.geometry().size;
            let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
            let visible = self.desk.is_visible(entry);
            if let Some(toplevel) = entry.window.toplevel() {
                let behind = self.behind_a_cover(entry, &outputs, &covers);
                set_suspended(toplevel, suspended(visible, entry.fitted, behind));
            }
            for (output, rect) in &outputs {
                if visible && layout::intersects(area, *rect) {
                    entry.window.output_enter(output, overlap);
                    // wp_fractional_scale_v1: the window learns this monitor's scale.
                    let scale = output.current_scale().fractional_scale();
                    entry.window.with_surfaces(|_, states| {
                        with_fractional_scale(states, |fractional| {
                            fractional.set_preferred_scale(scale)
                        });
                    });
                } else {
                    entry.window.output_leave(output);
                }
            }
        }
        // Panels and wallpapers learn the scale of their monitor as well.
        for output in &self.outputs {
            let scale = output.current_scale().fractional_scale();
            for layer in layer_map_for_output(output).layers() {
                layer.with_surfaces(|_, states| {
                    with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale)
                    });
                });
            }
        }
    }
}

/// Set or clear the `suspended` state of a toplevel, and send the change at once when the
/// initial configure has gone out (before it, the state goes along with it).
fn set_suspended(toplevel: &smithay::wayland::shell::xdg::ToplevelSurface, suspended: bool) {
    let changed = toplevel.with_pending_state(|state| {
        let was = state.states.contains(xdg_toplevel::State::Suspended);
        if suspended {
            state.states.set(xdg_toplevel::State::Suspended);
        } else {
            state.states.unset(xdg_toplevel::State::Suspended);
        }
        was != suspended
    });
    if changed && toplevel.is_initial_configure_sent() {
        let _ = toplevel.send_pending_configure();
    }
}
