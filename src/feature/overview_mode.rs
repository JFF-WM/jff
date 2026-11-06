//! The window overview: every window of one monitor shown as a thumbnail, to pick one from.
//!
//! Geometry lives in `overview_grid.rs` (pure, unit-tested). This file is the Smithay side:
//!
//! - [`Overview`] - state in [`JffState::overview`]: the monitor shown, a stable window order,
//!   the selection, and the focus to restore on cancel.
//! - [`OverviewGrab`] - owns the pointer while open. Clients see no pointer. Motion moves the
//!   selection, clicks are handled in `input.rs` ([`JffState::overview_button`]) because acting
//!   on them ends the grab.
//! - Keys come through the `input_intercept` closure in `input.rs` ([`overview_key`]).
//! - [`overview_elements`] - turns an [`OverviewPlan`] into render elements for both backends.
//!
//! Closing runs the opening animation backwards. Input and focus release at once, only the
//! picture lingers (`closing` is set), and the state is dropped when the animation is over.

use std::time::{Duration, Instant};

use smithay::backend::input::{ButtonState, InputTime};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{AsRenderElements, Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::Window;
use smithay::input::keyboard::Keysym;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorImageStatus, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Serial, SERIAL_COUNTER};

use crate::config;
use crate::feature::outputs::output_rect;
use crate::input::{BTN_LEFT, BTN_MIDDLE};
use crate::primitives::geom::Size;
use crate::primitives::layout::{self, Rect};
use crate::primitives::overview_grid::{self as overview, Dir, Thumb};
use crate::protocol::layer_shell::{ABOVE_WINDOWS, BELOW_WINDOWS};
use crate::render::effects::background_effect::render_for_surface_zoomed;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::effects::rounded::{is_fullscreen, ring_element, Rounding};
use crate::render::elements::JffElement;
use crate::state::JffState;

/// Free space around the thumbnails so docks and panels stay visible.
const MARGIN: i32 = 120;
/// Space between thumbnails.
const GAP: i32 = 24;
/// Open and close animation length.
const ANIMATION: Duration = Duration::from_millis(180);
/// Border width around the selected thumbnail.
const HIGHLIGHT_BORDER: i32 = 4;
/// Colour of the outline, straight (not premultiplied) alpha: it is translucent.
const HIGHLIGHT_COLOR: [f32; 4] = [0.35, 0.55, 0.95, 0.6];
/// How far the pointer moves with the left button held before it's a drag, not a click.
const DRAG_THRESHOLD: f64 = 8.0;

/// What a key does while the overview is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewKey {
    /// Leave and restore the previous focus.
    Cancel,
    /// Leave and focus the selected window.
    Select,
    Move(Dir),
    Cycle {
        backwards: bool,
    },
}

/// The overview key for `sym`, if it's one. Modifiers are ignored - Shift+Tab arrives as
/// `ISO_Left_Tab`.
pub fn overview_key(sym: Keysym) -> Option<OverviewKey> {
    if sym == Keysym::Escape {
        Some(OverviewKey::Cancel)
    } else if sym == Keysym::Return || sym == Keysym::KP_Enter {
        Some(OverviewKey::Select)
    } else if sym == Keysym::Tab {
        Some(OverviewKey::Cycle { backwards: false })
    } else if sym == Keysym::ISO_Left_Tab {
        Some(OverviewKey::Cycle { backwards: true })
    } else if sym == Keysym::Left {
        Some(OverviewKey::Move(Dir::Left))
    } else if sym == Keysym::Right {
        Some(OverviewKey::Move(Dir::Right))
    } else if sym == Keysym::Up {
        Some(OverviewKey::Move(Dir::Up))
    } else if sym == Keysym::Down {
        Some(OverviewKey::Move(Dir::Down))
    } else {
        None
    }
}

/// A thumbnail being carried by the pointer.
struct Drag {
    window: Window,
    /// Where the pointer grabbed it, as fractions of width and height.
    anchor: (f64, f64),
}

/// The overview, while open or animating closed.
pub struct Overview {
    output: Output,
    /// Windows in a stable order (most recently used first at open time). Windows that come and
    /// go are appended or dropped, the rest never reshuffles, so raising a window on exit
    /// doesn't move thumbnails around during the closing animation.
    windows: Vec<Window>,
    selected: Option<Window>,
    /// Focus before opening, restored on cancel.
    previous_focus: Option<Window>,
    /// Button pressed in the overview, not released yet.
    pressed: Option<u32>,
    /// (window, pointer pos) captured when the left button went down on a thumbnail. Moving far
    /// enough starts a drag.
    press_origin: Option<(Window, Point<f64, Logical>)>,
    /// Thumbnail being dragged. Dropped on another monitor, it moves its window there.
    drag: Option<Drag>,
    /// Buttons pressed on a layer surface and not released yet: the client still gets the
    /// release, wherever the pointer ends up.
    layer_buttons: Vec<u32>,
    /// Our pointer grab's serial, so we can recognise it when ending.
    grab_serial: Serial,
    opened: Instant,
    /// Set once we leave: input released, animation runs backwards.
    closing: Option<Instant>,
    /// Stable id of the highlight, so damage tracking sees it move rather than reappear.
    highlight: Id,
}

impl Overview {
    /// Whether this overview is on `output`.
    pub fn shows(&self, output: &Output) -> bool {
        self.output == *output
    }

    /// 0.0 = windows at their real spot, 1.0 = windows in the grid.
    fn progress(&self) -> f64 {
        match self.closing {
            Some(since) => {
                1.0 - overview::ease_out_cubic(overview::progress(since.elapsed(), ANIMATION))
            }
            None => overview::ease_out_cubic(overview::progress(self.opened.elapsed(), ANIMATION)),
        }
    }
}

/// What a renderer needs for one frame of the overview.
pub struct OverviewPlan {
    /// The monitor showing the overview. Others draw as usual.
    pub output: Output,
    /// (window, where to draw it now) - animation included, in the shared space.
    pub thumbs: Vec<(Window, Thumb)>,
    /// Index into `thumbs` of the selected window.
    pub selected: Option<usize>,
    /// Index into `thumbs` of the dragged thumbnail (drawn on top).
    pub dragged: Option<usize>,
    /// Corner radius of windows (the `radius` setting), thumbnails get it shrunk with them.
    pub radius: i32,
    highlight: Id,
}

/// Push the render elements for one thumbnail, placed relative to `origin`.
fn thumb_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    thumb: &Thumb,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    radius: i32,
    out: &mut Vec<JffElement>,
    snapshot: Option<&WallpaperSnapshot>,
) {
    let scale = Scale::from(1.0f64);
    // Draw the window at (0, 0) with its geometry corner, shrink it around that corner, then
    // move the result to its slot.
    let geo = window.geometry();
    let loc: Point<i32, Physical> =
        Point::<i32, Logical>::from((-geo.loc.x, -geo.loc.y)).to_physical_precise_round(scale);
    let slot: Point<i32, Physical> =
        Point::<i32, Logical>::from((thumb.pos.0 - origin.x, thumb.pos.1 - origin.y))
            .to_physical_precise_round(scale);
    // The thumbnail's corners shrink with it: its rectangle on screen and the radius times the
    // scale. Fullscreen windows are square, as they are live.
    let rounding = if is_fullscreen(window) {
        None
    } else {
        Rounding::new(
            renderer,
            Rectangle::new(
                (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
                (thumb.size.0, thumb.size.1).into(),
            ),
            (f64::from(radius) * thumb.scale).round() as i32,
        )
    };
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        window.render_elements(&mut *renderer, loc, scale, 1.0);
    for surface in surfaces {
        let shrunk = RescaleRenderElement::from_element(
            surface,
            Point::<i32, Physical>::from((0, 0)),
            thumb.scale,
        );
        let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
        out.push(match &rounding {
            Some(rounding) => rounding.wrap_thumb(placed),
            None => JffElement::Thumb(placed),
        });
    }
    // The blur goes right behind the thumbnail: after its elements in the list. The window's
    // origin sits at the slot minus its geometry offset, both shrunk with the thumbnail.
    if let (Some(blur), Some(toplevel)) = (blur, window.toplevel()) {
        let surface_loc = Point::<f64, Logical>::from((
            (thumb.pos.0 - origin.x) as f64 - geo.loc.x as f64 * thumb.scale,
            (thumb.pos.1 - origin.y) as f64 - geo.loc.y as f64 * thumb.scale,
        ));
        render_for_surface_zoomed(
            toplevel.wl_surface(),
            surface_loc,
            thumb.scale,
            blur.all_windows.then_some(geo),
            1.0,
            blur,
            rounding.as_ref(),
            &mut |element| {
                // With a snapshot the blur is of the wallpaper alone, whatever is behind.
                out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
            },
        );
    }
}

/// Thumbnails and highlight, front to back. `origin` is the monitor's top-left in the shared
/// space, elements are placed relative to it.
///
/// `snapshot`: blur the wallpaper copy in it rather than the live frame (`None`: the live
/// frame, so windows behind a thumbnail are blurred too).
pub fn overview_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();

    // The dragged thumbnail sits in front of the rest.
    let order = plan
        .dragged
        .into_iter()
        .chain((0..plan.thumbs.len()).filter(|i| Some(*i) != plan.dragged));
    for i in order {
        let (window, thumb) = &plan.thumbs[i];
        thumb_elements(
            renderer, window, thumb, origin, blur, plan.radius, &mut out, snapshot,
        );
    }

    // The highlight is pushed last so it's drawn behind: only its rim shows. It is a rounded
    // outline following the thumbnail's corners (`radius` shrunk with the thumbnail, fullscreen
    // windows are square), hollow so a translucent window shows no tint behind it.
    if let Some((window, thumb)) = plan.selected.and_then(|i| plan.thumbs.get(i)) {
        let radius = if is_fullscreen(window) {
            0
        } else {
            (f64::from(plan.radius) * thumb.scale).round() as i32
        };
        let thumb_rect = Rectangle::<i32, Logical>::new(
            (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
            (thumb.size.0, thumb.size.1).into(),
        );
        if let Some(ring) = ring_element(
            renderer,
            thumb_rect,
            HIGHLIGHT_BORDER,
            radius,
            HIGHLIGHT_COLOR,
        ) {
            out.push(JffElement::Ring(ring));
        } else {
            // The outline shader is unavailable: a plain rectangle behind the thumbnail.
            let geometry = Rectangle::<i32, Physical>::new(
                (
                    thumb.pos.0 - origin.x - HIGHLIGHT_BORDER,
                    thumb.pos.1 - origin.y - HIGHLIGHT_BORDER,
                )
                    .into(),
                (
                    thumb.size.0 + 2 * HIGHLIGHT_BORDER,
                    thumb.size.1 + 2 * HIGHLIGHT_BORDER,
                )
                    .into(),
            );
            let [r, g, b, a] = HIGHLIGHT_COLOR;
            out.push(JffElement::Solid(SolidColorRenderElement::new(
                plan.highlight.clone(),
                geometry,
                CommitCounter::default(),
                // Solid colours are premultiplied.
                [r * a, g * a, b * a, a],
                Kind::Unspecified,
            )));
        }
    }
    out
}

/// Elements for the dragged thumbnail on a monitor that isn't showing the overview, so you can
/// see the window travelling there. `origin` is the monitor's top-left at `rect`.
pub fn overview_drag_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    rect: Rect,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();
    if let Some((window, thumb)) = plan.dragged.and_then(|i| plan.thumbs.get(i)) {
        if layout::intersects(thumb.rect(), rect) {
            thumb_elements(
                renderer, window, thumb, origin, blur, plan.radius, &mut out, snapshot,
            );
        }
    }
    out
}

impl JffState {
    /// Whether the overview takes input right now (not while it's only animating closed).
    pub fn overview_active(&self) -> bool {
        self.overview
            .as_ref()
            .is_some_and(|ov| ov.closing.is_none())
    }

    /// Where the pointer may go: every monitor, also while the overview is open. The overview
    /// stays on its own monitor, other monitors keep their panels and draw as usual.
    pub fn pointer_bounds(&self) -> Vec<Rect> {
        self.output_rects()
    }

    /// Whether a thumbnail is being dragged.
    fn overview_dragging(&self) -> bool {
        self.overview.as_ref().is_some_and(|ov| ov.drag.is_some())
    }

    pub fn toggle_overview(&mut self) {
        if self.overview_active() {
            self.end_overview(None);
        } else {
            self.open_overview();
        }
    }

    /// Pick the monitor: the one under the pointer, so no click is needed to choose it. If it
    /// has no windows, fall back to the monitor with the focused window, so the key still does
    /// something.
    fn overview_output(&self) -> Option<Output> {
        let under_pointer = self.output_at_pointer();
        if let Some(output) = &under_pointer {
            if !self.windows_on(output_rect(output)).is_empty() {
                return under_pointer;
            }
        }
        let focused_rect = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus())
            .and_then(|focus| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
            })
            .map(|entry| {
                let size = entry.window.geometry().size;
                self.rect_for_window(entry.pos, (size.w, size.h))
            });
        focused_rect
            .and_then(|rect| {
                self.outputs
                    .iter()
                    .find(|o| output_rect(o) == rect)
                    .cloned()
            })
            .or(under_pointer)
    }

    /// Windows whose center is on `rect`, topmost first. Windows that haven't committed a buffer
    /// yet have no size and are skipped.
    fn windows_on(&self, rect: Rect) -> Vec<Window> {
        self.desk
            .top_down()
            .filter(|entry| {
                let size = entry.window.geometry().size;
                size.w > 0
                    && size.h > 0
                    && self.rect_for_window(entry.pos, (size.w, size.h)) == rect
            })
            .map(|entry| entry.window.clone())
            .collect()
    }

    /// Open the overview. No-op while a move or resize is going on, or when there's no window to
    /// show.
    pub fn open_overview(&mut self) {
        if self.overview_active() {
            return;
        }
        // Still animating closed: finish that first.
        self.overview = None;

        // Overview and screenshot selection never open together - the overview wins.
        self.cancel_screenshot();

        // An open menu or popover has to go first: with a grab it holds the pointer (which
        // would block the overview opening) and without one only the client knows it's open.
        // Only the menu's own grabs are released, a move or resize keeps its pointer and still
        // stops the overview.
        self.dismiss_popups();

        if self.resize.is_some() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return; // a move or a resize has the pointer
        }
        let Some(output) = self.overview_output() else {
            return;
        };
        let rect = output_rect(&output);
        let windows = self.windows_on(rect);
        if windows.is_empty() {
            return;
        }

        let keyboard = self.seat.get_keyboard();
        let focused = keyboard
            .as_ref()
            .and_then(|k| k.current_focus())
            .and_then(|focus| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
                    .map(|entry| entry.window.clone())
            });
        let selected = focused
            .clone()
            .filter(|w| windows.contains(w))
            .or_else(|| windows.first().cloned());

        // Bring the pointer onto the monitor first. This also tells the client under it that
        // the pointer left.
        let at = pointer.current_location();
        let (x, y) = layout::clamp_to_rects((at.x, at.y), &[rect]);
        let at: Point<f64, Logical> = (x, y).into();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);

        let grab_serial = SERIAL_COUNTER.next_serial();
        self.overview = Some(Overview {
            output,
            windows,
            selected,
            previous_focus: focused,
            pressed: None,
            press_origin: None,
            drag: None,
            layer_buttons: Vec::new(),
            grab_serial,
            opened: Instant::now(),
            closing: None,
            highlight: Id::new(),
        });

        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: at,
        };
        pointer.set_grab(
            self,
            OverviewGrab::new(start_data),
            grab_serial,
            Focus::Clear,
        );

        // Nobody has the keyboard while the overview is open: the old focus gets a proper
        // leave, so keys held down at that moment end cleanly.
        self.update_keyboard_focus();
        // A client may have left its own cursor image behind.
        self.cursor.set_status(CursorImageStatus::default_named());
    }

    /// Leave the overview and start the closing animation. `select` is the window to focus and
    /// raise, `None` restores the focus from before opening.
    ///
    /// Must not be called from inside a method of [`OverviewGrab`] - the pointer is locked there.
    pub fn end_overview(&mut self, select: Option<Window>) {
        // Menus raised from a panel while the overview was open go with it: a click on empty
        // space or a key ends the overview without the menu ever seeing it. Do this before
        // letting go of the pointer, so the menu's own grab isn't left behind.
        if self.overview_active() {
            self.dismiss_popups();
        }

        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if ov.closing.is_some() {
            return;
        }

        let target = select.or_else(|| ov.previous_focus.clone());
        ov.selected = target.clone().filter(|w| ov.windows.contains(w));

        ov.closing = Some(Instant::now());
        ov.drag = None;
        ov.press_origin = None;
        let grab_serial = ov.grab_serial;

        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.has_grab(grab_serial) {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }
        // The window may be gone by now (cancel after it closed).
        let target = target.filter(|w| self.desk.find_by(|x| x == w).is_some());
        match target {
            Some(window) => self.focus_window(&window),
            // Nothing to go back to: keyboard goes to whatever is on top.
            None => self.refocus_topmost(),
        }
        self.refresh_pointer_focus();
    }

    /// Drop the overview at once, no animation (monitor unplugged, VT switch).
    pub fn abort_overview(&mut self) {
        if self.overview.is_none() {
            return;
        }
        self.end_overview(None);
        self.overview = None;
    }

    /// Tell the client under the pointer that the pointer is there. The grab ignored what was
    /// under it, and windows may have moved since.
    pub fn refresh_pointer_focus(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let at = pointer.current_location();
        let under = self.pointer_focus_under(at);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    /// Final (not animated) place of every window in the overview.
    fn overview_slots(&self) -> Vec<(Window, Thumb)> {
        let Some(ov) = self.overview.as_ref() else {
            return Vec::new();
        };
        let area = layout::grow(output_rect(&ov.output), -MARGIN);
        let sizes: Vec<Size> = ov
            .windows
            .iter()
            .map(|w| {
                let size = w.geometry().size;
                (size.w, size.h)
            })
            .collect();
        ov.windows
            .iter()
            .cloned()
            .zip(overview::arrange(&sizes, area, GAP))
            .collect()
    }

    /// Layer surface the pointer can reach at `p` while the overview is open: overlay and top
    /// layers are in front of the thumbnails, bottom and background are behind.
    pub fn overview_layer_under(
        &self,
        p: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.overview_dragging() {
            return None;
        }
        if let Some((_, surface, loc)) = self.layer_under(p, &ABOVE_WINDOWS) {
            return Some((surface, loc));
        }
        // A menu of a panel behind the thumbnails is in front of them.
        if let Some((_, surface, loc)) = self.layer_popup_under(p, &BELOW_WINDOWS) {
            return Some((surface, loc));
        }
        if self.overview_hit(p).is_some() {
            return None;
        }
        self.layer_under(p, &BELOW_WINDOWS)
            .map(|(_, surface, loc)| (surface, loc))
    }

    /// The window whose thumbnail is under `p`.
    fn overview_hit(&self, p: Point<f64, Logical>) -> Option<Window> {
        let slots = self.overview_slots();
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        overview::hit_test(&thumbs, (p.x, p.y)).map(|i| slots[i].0.clone())
    }

    /// Pointer moved: select what it's over. Over empty space the selection stays.
    pub fn overview_hover(&mut self, p: Point<f64, Logical>) {
        if self.overview_dragging() || self.overview_start_drag(p) {
            return;
        }
        // A panel in front of the thumbnails isn't part of the overview.
        if self.layer_under(p, &ABOVE_WINDOWS).is_some()
            || self.layer_popup_under(p, &BELOW_WINDOWS).is_some()
        {
            return;
        }
        if let Some(window) = self.overview_hit(p) {
            if let Some(ov) = self.overview.as_mut() {
                ov.selected = Some(window);
            }
        }
    }

    /// Start dragging the thumbnail the left button went down on, once the pointer at `p` has
    /// moved past `DRAG_THRESHOLD`. Returns whether a drag is going on now.
    fn overview_start_drag(&mut self, p: Point<f64, Logical>) -> bool {
        let Some((window, from)) = self
            .overview
            .as_ref()
            .and_then(|ov| ov.press_origin.clone())
        else {
            return false;
        };
        if !overview::moved_past((from.x, from.y), (p.x, p.y), DRAG_THRESHOLD) {
            return false;
        }
        let slots = self.overview_slots();
        let Some((_, thumb)) = slots.iter().find(|(w, _)| *w == window) else {
            return false;
        };
        let anchor = overview::anchor_in(thumb, (from.x, from.y));
        let Some(ov) = self.overview.as_mut() else {
            return false;
        };
        ov.press_origin = None;
        ov.selected = Some(window.clone());
        ov.drag = Some(Drag { window, anchor });
        true
    }

    /// The dragged thumbnail was let go. On another monitor the window moves there, keeping its
    /// real size and the point it was held by under the pointer. On the same monitor nothing
    /// changes, the thumbnail slides back to its slot.
    fn overview_drop(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some((drag, source)) = self.overview.as_mut().and_then(|ov| {
            let drag = ov.drag.take()?;
            Some((drag, output_rect(&ov.output)))
        }) else {
            return;
        };
        let at = pointer.current_location();
        let rects = self.output_rects();
        let target = layout::nearest((at.x, at.y), &rects)
            .map(|i| rects[i])
            .filter(|target| *target != source);
        let moving = self
            .desk
            .find_by(|w| *w == drag.window)
            .map(|entry| (entry.window.geometry().size, entry.restore));

        if let (Some(target), Some((size, restore))) = (target, moving) {
            let window = drag.window;
            let pos = if restore.is_some() {
                // Maximized / fullscreen windows get fitted to the monitor right after, they
                // only need to end up on the right one.
                (
                    target.0 + (target.2 - size.w) / 2,
                    target.1 + (target.3 - size.h) / 2,
                )
            } else {
                overview::drop_pos(drag.anchor, (at.x, at.y), (size.w, size.h), target)
            };
            self.desk.set_pos_where(|w| *w == window, pos);
            self.desk.raise_where(|w| *w == window);
            if let Some((restore_pos, restore_size)) = restore {
                let carried = (
                    restore_pos.0 + target.0 - source.0,
                    restore_pos.1 + target.1 - source.1,
                );
                self.desk.set_restore_pos_where(
                    |w| *w == window,
                    layout::fit_into(carried, restore_size, target),
                );
                self.relayout_outputs();
            }
        }
    }

    /// Button event while the overview is open. A click counts on release, so nothing of it is
    /// left over for the window that gets focused: left selects, middle closes, any button on
    /// empty space (or on another monitor) cancels. Left-drag on a thumbnail drags it, dropped
    /// on another monitor, the window moves there.
    pub fn overview_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        // Clicks on panels and layer surfaces go to their client, the overview keeps its
        // clicks for thumbnails and empty space.
        let to_layer = match state {
            ButtonState::Pressed => self
                .overview_layer_under(pointer.current_location())
                .is_some(),
            ButtonState::Released => self.overview.as_mut().is_some_and(|ov| {
                let before = ov.layer_buttons.len();
                ov.layer_buttons.retain(|b| *b != button);
                ov.layer_buttons.len() != before
            }),
        };
        if to_layer {
            if state == ButtonState::Pressed {
                if let Some(ov) = self.overview.as_mut() {
                    ov.layer_buttons.push(button);
                }
                // The pointer may not have moved since opening: make sure the surface under it
                // has the pointer before the press.
                let at = pointer.current_location();
                pointer.motion(
                    self,
                    None,
                    &MotionEvent {
                        location: at,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.frame(self);
            return;
        }
        match state {
            ButtonState::Pressed => {
                if self.overview_dragging() {
                    return;
                }
                let at = pointer.current_location();
                let hit = if button == BTN_LEFT {
                    self.overview_hit(at)
                } else {
                    None
                };
                if let Some(ov) = self.overview.as_mut() {
                    ov.pressed = Some(button);
                    // A press is a selection: motion may not have run since the pointer reached
                    // this thumbnail.
                    if let Some(window) = &hit {
                        ov.selected = Some(window.clone());
                    }
                    ov.press_origin = hit.map(|window| (window, at));
                }
            }
            ButtonState::Released => {
                if self.overview_dragging() && button != BTN_LEFT {
                    return;
                }
                let armed = self.overview.as_mut().and_then(|ov| {
                    ov.press_origin = None;
                    ov.pressed.take()
                });
                if armed != Some(button) {
                    return;
                }
                if button == BTN_LEFT && self.overview_dragging() {
                    self.overview_drop();
                    return;
                }
                match self.overview_hit(pointer.current_location()) {
                    Some(window) if button == BTN_LEFT => self.end_overview(Some(window)),
                    Some(window) if button == BTN_MIDDLE => {
                        if let Some(toplevel) = window.toplevel() {
                            toplevel.send_close();
                        }
                    }
                    Some(_) => {}
                    None => self.end_overview(None),
                }
            }
        }
    }

    /// Carry out a key pressed while the overview is open.
    pub fn handle_overview_key(&mut self, key: OverviewKey) {
        match key {
            OverviewKey::Cancel => self.end_overview(None),
            OverviewKey::Select => {
                let selected = self.overview.as_ref().and_then(|ov| ov.selected.clone());
                self.end_overview(selected);
            }
            OverviewKey::Move(dir) => {
                self.select_with(|thumbs, i| overview::neighbor(thumbs, i, dir))
            }
            OverviewKey::Cycle { backwards } => {
                self.select_with(|thumbs, i| Some(overview::cycle(thumbs.len(), i, backwards)));
            }
        }
    }

    /// Move the selection to whatever `pick` returns, given the thumbnails and the current
    /// index.
    fn select_with(&mut self, pick: impl FnOnce(&[Thumb], usize) -> Option<usize>) {
        let slots = self.overview_slots();
        if slots.is_empty() {
            return;
        }
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        let current = self
            .overview
            .as_ref()
            .and_then(|ov| ov.selected.as_ref())
            .and_then(|s| slots.iter().position(|(w, _)| w == s));
        let next = match current {
            Some(i) => pick(&thumbs, i),
            None => Some(0),
        };
        if let (Some(next), Some(ov)) = (next, self.overview.as_mut()) {
            ov.selected = Some(slots[next].0.clone());
        }
    }

    /// Once per frame, before drawing: finish a closed overview and follow windows that were
    /// created or destroyed while it's open.
    fn overview_tick(&mut self) {
        let Some(ov) = self.overview.as_ref() else {
            return;
        };
        if ov.closing.is_some_and(|since| since.elapsed() >= ANIMATION) {
            self.overview = None;
            return;
        }
        // (A window created meanwhile can't take the keyboard: while the overview is open the
        // keyboard focus is the overview's - see `focus.rs`.)
        let Some(ov) = self.overview.as_ref() else {
            return;
        };
        let present = self.windows_on(output_rect(&ov.output));

        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        let old_index = ov
            .selected
            .as_ref()
            .and_then(|s| ov.windows.iter().position(|w| w == s));
        ov.windows.retain(|w| present.contains(w));
        for window in present {
            if !ov.windows.contains(&window) {
                ov.windows.push(window);
            }
        }
        // The selected window is gone: take the one that moved into its place.
        let selected_alive = ov.selected.as_ref().is_some_and(|s| ov.windows.contains(s));
        if !selected_alive {
            let last = ov.windows.len().saturating_sub(1);
            ov.selected = ov.windows.get(old_index.unwrap_or(0).min(last)).cloned();
        }
        let nothing_left = ov.windows.is_empty() && ov.closing.is_none();
        if nothing_left {
            self.abort_overview();
        }
    }

    /// What to draw for the overview this frame, if there is one. Call before borrowing the
    /// backend - the layout is recomputed every frame, there are few windows.
    pub fn overview_plan(&mut self) -> Option<OverviewPlan> {
        self.overview_tick();
        let ov = self.overview.as_ref()?;
        let t = ov.progress();
        let at = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();

        let thumbs: Vec<(Window, Thumb)> = self
            .overview_slots()
            .into_iter()
            .map(|(window, grid)| {
                // The animation runs between where the window really is and its grid slot.
                let real = self
                    .desk
                    .find_by(|w| *w == window)
                    .map(|entry| {
                        let size = entry.window.geometry().size;
                        Thumb {
                            pos: entry.pos,
                            size: (size.w, size.h),
                            scale: 1.0,
                        }
                    })
                    .unwrap_or(grid);
                let thumb = overview::lerp_thumb(real, grid, t);
                let thumb = match ov.drag.as_ref().filter(|drag| drag.window == window) {
                    Some(drag) => overview::dragged_thumb(thumb, drag.anchor, (at.x, at.y)),
                    None => thumb,
                };
                (window, thumb)
            })
            .collect();
        // Thumbnails draw front to back in list order. While closing, the just-focused window
        // was raised in the desk, so follow the desk's real stacking: the window being selected
        // must travel in front, not behind the one that was active.
        let mut thumbs = thumbs;
        if ov.closing.is_some() {
            let stacking: Vec<Window> = self
                .desk
                .top_down()
                .map(|entry| entry.window.clone())
                .collect();
            thumbs.sort_by_key(|(window, _)| {
                stacking
                    .iter()
                    .position(|w| w == window)
                    .unwrap_or(usize::MAX)
            });
        }
        let dragged = ov
            .drag
            .as_ref()
            .and_then(|drag| thumbs.iter().position(|(w, _)| *w == drag.window));
        let selected = ov
            .selected
            .as_ref()
            .and_then(|s| thumbs.iter().position(|(w, _)| w == s));

        Some(OverviewPlan {
            output: ov.output.clone(),
            thumbs,
            selected,
            dragged,
            radius: self.config.radius(),
            highlight: ov.highlight.clone(),
        })
    }
}

/// Owns the pointer while the overview is open: windows see nothing, layer surfaces (panels,
/// widgets) still do, motion moves the selection.
pub struct OverviewGrab {
    start_data: GrabStartData<JffState>,
}

impl OverviewGrab {
    pub fn new(start_data: GrabStartData<JffState>) -> Self {
        Self { start_data }
    }
}

impl PointerGrab<JffState> for OverviewGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Panels and layer surfaces still get the pointer, windows don't.
        let focus = data.overview_layer_under(event.location);
        handle.motion(data, focus, event);
        data.overview_hover(event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    // Buttons are handled in `input.rs` before they reach the pointer. Anything that gets here
    // has no focus to go to.
    fn button(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
    }

    fn axis(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut JffState, handle: &mut PointerInnerHandle<'_, JffState>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }
    fn gesture_swipe_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }
    fn gesture_swipe_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }
    fn gesture_pinch_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }
    fn gesture_pinch_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }
    fn gesture_pinch_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }
    fn gesture_hold_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }
    fn gesture_hold_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<JffState> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut JffState) {}
}
