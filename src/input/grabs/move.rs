//! Interactive window move: the pointer grab that runs while a window is dragged.
//!
//! Clients that draw their own titlebar (GTK, Qt, most toolkits) send `xdg_toplevel.move` when
//! the user presses on it. We take the pointer until the button that started the drag is
//! released, and shift the window by however far the pointer travels.

use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point};

use crate::desk::{Pos, WindowMode};
use crate::primitives::geom;
use crate::protocol::xx_zones::ZoneDrag;
use crate::state::JffState;

pub struct MoveGrab {
    start_data: GrabStartData<JffState>,
    window: Window,
    /// Where the window was when the drag began.
    initial_pos: Pos,
    /// Mode the window was taken out of for the drag. Maximized ones go back on release,
    /// snapped ones only if they weren't moved.
    back_to: Option<WindowMode>,
    saved_restore: Option<(Pos, (i32, i32))>,
    last: Point<f64, Logical>,
    moved: bool,
    /// The zone that moves along (Alt+drag on a member of a zone).
    zone: Option<ZoneDrag>,
}

impl MoveGrab {
    pub fn new(start_data: GrabStartData<JffState>, window: Window, initial_pos: Pos) -> Self {
        Self {
            last: start_data.location,
            start_data,
            window,
            initial_pos,
            back_to: None,
            saved_restore: None,
            moved: false,
            zone: None,
        }
    }

    /// Move the zone of the window along with it.
    pub fn with_zone(mut self, zone: ZoneDrag) -> Self {
        self.zone = Some(zone);
        self
    }

    pub fn new_unfilled(
        start_data: GrabStartData<JffState>,
        window: Window,
        initial_pos: Pos,
        mode: WindowMode,
        restore: Option<(Pos, (i32, i32))>,
    ) -> Self {
        Self {
            back_to: Some(mode),
            saved_restore: restore,
            ..Self::new(start_data, window, initial_pos)
        }
    }
}

/// Window position after the pointer travels from `start` to `now`.
pub fn dragged_pos(initial: Pos, start: Point<f64, Logical>, now: Point<f64, Logical>) -> Pos {
    (
        initial.0 + (now.x - start.x).round() as i32,
        initial.1 + (now.y - start.y).round() as i32,
    )
}

pub fn shrunk_geometry(
    monitor: (i32, i32, i32, i32),
    pointer: Point<f64, Logical>,
) -> (Pos, (i32, i32)) {
    let size = ((monitor.2 / 2).max(1), (monitor.3 / 2).max(1));
    (
        geom::pointer_anchored_pos(monitor, size, (pointer.x, pointer.y)),
        size,
    )
}

impl PointerGrab<JffState> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The dragged client must not see the pointer move while we own it.
        handle.motion(data, None, event);

        self.last = event.location;
        self.moved |= event.location != self.start_data.location;
        let pos = dragged_pos(self.initial_pos, self.start_data.location, event.location);
        let window = self.window.clone();
        data.desk.set_pos_where(|w| *w == window, pos);
        if let Some(zone) = &self.zone {
            data.zone_drag_to(
                zone,
                (pos.0 - self.initial_pos.0, pos.1 - self.initial_pos.1),
            );
        }
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

    fn button(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        // Drag ends when the button that started it is let go.
        if !handle.current_pressed().contains(&self.start_data.button) {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
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

    fn unset(&mut self, data: &mut JffState) {
        data.dragging_view = None;
        let with_zone = self.zone.is_some();
        if let Some(zone) = self.zone.take() {
            data.zone_drag_end(zone);
        }
        let window = self.window.clone();
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        let at_edge = if self.moved {
            data.edge_drop_mode(self.last)
        } else {
            None
        };
        // The edge of the monitor keeps meaning snap or maximize. Anywhere else, a window let go
        // in the empty half beside a snapped one starts a container (with `tiling-containers`).
        if self.moved
            && at_edge.is_none()
            && !with_zone
            && data.drop_into_container(&window, self.last)
        {
            return;
        }
        let back = match self.back_to {
            Some(WindowMode::Maximized) => Some(WindowMode::Maximized),
            Some(mode) if !self.moved => Some(mode),
            _ => None,
        };
        let Some(target) = at_edge.or(back) else {
            return;
        };
        // Gone, or somebody else changed its mode while it was dragged.
        let untouched = data.desk.find_by(|w| *w == window).is_some()
            && data.desk.is_floating(|w| *w == window);
        if !untouched {
            return;
        }
        data.set_mode(&toplevel, target);
        if let Some(restore) = self.saved_restore.take() {
            data.desk.set_restore_where(|w| *w == window, restore);
        }
    }
}
