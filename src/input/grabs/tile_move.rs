//! Interactive move of a window of a tiling container: the pointer grab that runs while one is
//! carried to another place.
//!
//! Nothing is drawn while it goes, the window keeps its place in the tree until it is put down.
//! Where the pointer is then decides (see `JffState::finish_tile_move`): over another window of
//! a container it goes there, at the edge of the monitor or off every container it leaves its
//! container. Two ways to end it: letting go of the button that started the drag (a titlebar or
//! `drag-modifier` drag), or, when a key binding picked the window up (`container-drag`), the
//! next click: left puts it down, right cancels.

use smithay::backend::input::ButtonState;
use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point};

use crate::input::BTN_LEFT;
use crate::state::JffState;

/// How a tile move ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileMoveEnd {
    /// When the button that began the drag is let go. A drag that never moved puts nothing
    /// down.
    Release,
    /// On the next click: left puts the window down, any other button cancels.
    Click,
}

pub struct TileMoveGrab {
    start_data: GrabStartData<JffState>,
    window: Window,
    end: TileMoveEnd,
    last: Point<f64, Logical>,
    moved: bool,
    cancelled: bool,
}

impl TileMoveGrab {
    pub fn new(start_data: GrabStartData<JffState>, window: Window, end: TileMoveEnd) -> Self {
        Self {
            last: start_data.location,
            start_data,
            window,
            end,
            moved: false,
            cancelled: false,
        }
    }
}

impl PointerGrab<JffState> for TileMoveGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The carried client must not see the pointer move while we own it.
        handle.motion(data, None, event);
        self.last = event.location;
        self.moved |= event.location != self.start_data.location;
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
        let done = match self.end {
            TileMoveEnd::Release => !handle.current_pressed().contains(&self.start_data.button),
            TileMoveEnd::Click => {
                let pressed = event.state == ButtonState::Pressed;
                self.cancelled = pressed && event.button != BTN_LEFT;
                pressed
            }
        };
        if done {
            // The click that puts the window down has no release for the client to see.
            if self.end == TileMoveEnd::Click {
                data.swallowed_buttons.insert(event.button);
            }
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
        let carried = self.moved || self.end == TileMoveEnd::Click;
        if carried && !self.cancelled {
            data.finish_tile_move(&self.window, self.last);
        }
    }
}
