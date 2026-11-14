//! Interactive resize of a window in a tiling container: the pointer grab that runs while a
//! shared edge is dragged.
//!
//! Nothing here sizes the window itself. The drag moves the divider of the container's layout
//! tree (`TileTree::resize`), the other window on that divider follows, and the whole container is
//! laid out and configured again. Pointer events come hundreds of times a second, the tree is
//! changed on every one but the windows are only configured about 60 times a second and once more
//! when the button is let go, so a terminal is not flooded with sizes.

use std::time::{Duration, Instant};

use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point};

use crate::desk::ContainerId;
use crate::primitives::geom::Edges;
use crate::primitives::layout::Rect;
use crate::state::JffState;

/// Shortest time between two layouts of the container while the drag goes on.
const RELAYOUT_EVERY: Duration = Duration::from_millis(16);

pub struct TileResizeGrab {
    start_data: GrabStartData<JffState>,
    window: Window,
    container: ContainerId,
    edges: Edges,
    /// Where the window was laid out when the drag began.
    rect: Rect,
    last_layout: Instant,
}

impl TileResizeGrab {
    pub fn new(
        start_data: GrabStartData<JffState>,
        window: Window,
        container: ContainerId,
        edges: Edges,
        rect: Rect,
    ) -> Self {
        Self {
            start_data,
            window,
            container,
            edges,
            rect,
            last_layout: Instant::now(),
        }
    }
}

impl PointerGrab<JffState> for TileResizeGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The client must not see the pointer move while we own it.
        handle.motion(data, None, event);
        let delta = (
            (event.location.x - self.start_data.location.x).round() as i32,
            (event.location.y - self.start_data.location.y).round() as i32,
        );
        let moved = data.resize_member(&self.window, self.edges, self.rect, delta);
        if moved && self.last_layout.elapsed() >= RELAYOUT_EVERY {
            self.last_layout = Instant::now();
            data.retile_container(self.container);
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
        // The drag ends when the button that started it is let go.
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
        // However the drag ends, the windows get the size the tree gave them.
        data.retile_container(self.container);
        data.autosave_container_layout(self.container);
    }
}
