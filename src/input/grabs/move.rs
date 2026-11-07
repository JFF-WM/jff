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
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point};

use crate::desk::Pos;
use crate::state::JffState;

pub struct MoveGrab {
    start_data: GrabStartData<JffState>,
    window: Window,
    /// Where the window was when the drag began.
    initial_pos: Pos,
    /// Set when the window was maximized as the drag began and was shrunk for it: the geometry
    /// it goes back to when un-maximized. The window is maximized again when the drag ends,
    /// but only if it's still a plain floating window by then.
    saved_restore: Option<(Pos, (i32, i32))>,
}

impl MoveGrab {
    pub fn new(start_data: GrabStartData<JffState>, window: Window, initial_pos: Pos) -> Self {
        Self {
            start_data,
            window,
            initial_pos,
            saved_restore: None,
        }
    }

    /// Drag a window that was maximized (and has been shrunk to a half-output size).
    /// `restore` is what `Desk::take_restore_where` returned for it.
    pub fn new_shrunk(
        start_data: GrabStartData<JffState>,
        window: Window,
        initial_pos: Pos,
        restore: (Pos, (i32, i32)),
    ) -> Self {
        Self {
            saved_restore: Some(restore),
            ..Self::new(start_data, window, initial_pos)
        }
    }
}

/// Size and position of a maximized window while it is dragged: half the output's width and
/// height, placed so the pointer keeps the same relative spot on the window as it had on the
/// output (grab the right edge of the screen, hold the right edge of the smaller window).
pub fn shrunk_geometry(
    output: (i32, i32, i32, i32),
    pointer: Point<f64, Logical>,
) -> (Pos, (i32, i32)) {
    let (ox, oy, ow, oh) = output;
    let size = ((ow / 2).max(1), (oh / 2).max(1));
    let rx = if ow > 0 {
        ((pointer.x - ox as f64) / ow as f64).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let ry = if oh > 0 {
        ((pointer.y - oy as f64) / oh as f64).clamp(0.0, 1.0)
    } else {
        0.5
    };
    (
        (
            (pointer.x - rx * size.0 as f64).round() as i32,
            (pointer.y - ry * size.1 as f64).round() as i32,
        ),
        size,
    )
}

/// Window position after the pointer travels from `start` to `now`.
pub fn dragged_pos(initial: Pos, start: Point<f64, Logical>, now: Point<f64, Logical>) -> Pos {
    (
        initial.0 + (now.x - start.x).round() as i32,
        initial.1 + (now.y - start.y).round() as i32,
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

        let pos = dragged_pos(self.initial_pos, self.start_data.location, event.location);
        let window = self.window.clone();
        data.desk.set_pos_where(|w| *w == window, pos);
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
        let Some(restore) = self.saved_restore.take() else {
            return; // a floating window: nothing to put back, it keeps its size
        };
        let window = self.window.clone();
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        // Something else changed the window's mode during the drag (a maximize or fullscreen
        // request, say): that's the newer decision, leave it alone.
        let changed = data.desk.is_maximized(|w| *w == window)
            || toplevel.with_pending_state(|s| {
                s.states.contains(xdg_toplevel::State::Fullscreen)
                    || s.states.contains(xdg_toplevel::State::Maximized)
            });
        let Some((pos, size)) = data
            .desk
            .find_by(|w| *w == window)
            .map(|e| (e.pos, e.window.geometry().size))
        else {
            return; // closed during the drag
        };
        if changed {
            return;
        }
        // Maximize on the monitor it was dropped on, keeping the geometry from before the
        // first maximize as the one to go back to.
        let monitor = data.rect_for_window(pos, (size.w, size.h));
        let (x, y, w, h) = data.usable_rect(monitor);
        if data
            .desk
            .remaximize_where(|e| *e == window, (x, y), restore)
        {
            toplevel.with_pending_state(|s| {
                s.states.set(xdg_toplevel::State::Maximized);
                s.size = Some((w, h).into());
            });
            toplevel.send_pending_configure();
        }
    }
}
