//! Interactive window resize: the pointer grab that runs while a window edge is dragged.
//!
//! The client asks for it with `xdg_toplevel.resize` (usually when pressing on its border or a
//! corner of its own decorations). We tell the client its new size through configure events, it
//! redraws at that size and commits.
//!
//! Pointer events (hundreds per second) only record the size the user wants. Once per event-loop
//! pass, `JffState::update_resize` sends it, and only when the client has committed the previous
//! size we sent. Clients like terminals resize a whole grid (and signal a shell that redraws its
//! prompt) on every configure, so flooding them leaves garbled output behind.

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{IsAlive, Logical, Point};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelCachedState, ToplevelSurface, XdgToplevelSurfaceData,
};

use crate::desk::Pos;
use crate::primitives::geom::{self, Edges, Size};
use crate::state::JffState;

/// A resize under way, kept in the compositor state so it can be driven from the event loop, not
/// just pointer events.
pub struct ActiveResize {
    toplevel: ToplevelSurface,
    edges: Edges,
    initial_pos: Pos,
    initial_size: Size,
    /// The button was released: stay around until the client has committed the final size, so
    /// the window can be put exactly in place.
    released: bool,
}

pub struct ResizeGrab {
    start_data: GrabStartData<JffState>,
    toplevel: ToplevelSurface,
    edges: Edges,
    initial_pos: Pos,
    initial_size: Size,
    /// Whether this resize has been handed to the compositor state yet (on the first motion).
    registered: bool,
}

impl ResizeGrab {
    pub fn new(
        start_data: GrabStartData<JffState>,
        toplevel: ToplevelSurface,
        edges: Edges,
        initial_pos: Pos,
        initial_size: Size,
    ) -> Self {
        // Tell the client a resize is in progress, toolkits use it to skip animations, etc.
        toplevel.with_pending_state(|s| s.states.set(xdg_toplevel::State::Resizing));
        toplevel.send_pending_configure();

        Self {
            start_data,
            toplevel,
            edges,
            initial_pos,
            initial_size,
            registered: false,
        }
    }
}

pub fn edges_from(edges: xdg_toplevel::ResizeEdge) -> Edges {
    use xdg_toplevel::ResizeEdge as E;
    Edges {
        left: matches!(edges, E::Left | E::TopLeft | E::BottomLeft),
        right: matches!(edges, E::Right | E::TopRight | E::BottomRight),
        top: matches!(edges, E::Top | E::TopLeft | E::TopRight),
        bottom: matches!(edges, E::Bottom | E::BottomLeft | E::BottomRight),
    }
}

impl PointerGrab<JffState> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The resized client must not see the pointer move while we own it.
        handle.motion(data, None, event);

        if !self.registered {
            self.registered = true;
            data.resize = Some(ActiveResize {
                toplevel: self.toplevel.clone(),
                edges: self.edges,
                initial_pos: self.initial_pos,
                initial_size: self.initial_size,
                released: false,
            });
        }

        let delta = (
            (event.location.x - self.start_data.location.x).round() as i32,
            (event.location.y - self.start_data.location.y).round() as i32,
        );
        let (min, max) = with_states(self.toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (
                (current.min_size.w, current.min_size.h),
                (current.max_size.w, current.max_size.h),
            )
        });

        let (_, size) = geom::resize(
            self.initial_pos,
            self.initial_size,
            delta,
            self.edges,
            min,
            max,
        );

        // Only remember what's wanted. `update_resize` sends it when the client is ready, and
        // keeps the opposite edge still when the left/top edge is dragged.
        self.toplevel
            .with_pending_state(|s| s.size = Some((size.0, size.1).into()));
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
        // Resize ends when the button that started it is let go.
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
        if self.registered {
            if let Some(resize) = data.resize.as_mut() {
                resize.released = true;
            }
        }
        // Called however the grab ends: let the client know the resize is over. This configure
        // also carries the newest wanted size, nothing is held back at the end.
        self.toplevel
            .with_pending_state(|s| s.states.unset(xdg_toplevel::State::Resizing));
        self.toplevel.send_pending_configure();
    }
}

/// Where the client is with the size we last asked of it.
struct Progress {
    /// A configure is waiting to be sent and the client is ready for it.
    can_send: bool,
    /// Nothing is waiting, and the client has committed everything we sent.
    settled: bool,
}

fn progress(toplevel: &ToplevelSurface) -> Progress {
    with_states(toplevel.wl_surface(), |states| {
        let role = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap();
        let mut cached = states.cached_state.get::<ToplevelCachedState>();
        // Size of the newest configure the client acknowledged and committed.
        let committed = cached.current().last_acked.as_ref().map(|c| c.state.size);

        let sent = role.current_server_state();
        let caught_up = committed.map_or(true, |size| size == sent.size);

        let Some(pending) = role.server_pending.as_ref() else {
            return Progress {
                can_send: false,
                settled: caught_up,
            };
        };
        if *pending == sent {
            return Progress {
                can_send: false,
                settled: caught_up,
            };
        }
        // When only the size changed, wait until the client caught up with the last one.
        // Anything else (a state flag, bounds, ...) goes out right away.
        let mut same_size = sent.clone();
        same_size.size = pending.size;
        let only_size = same_size == *pending;
        Progress {
            can_send: !only_size || caught_up,
            settled: false,
        }
    })
}

impl JffState {
    /// Drive the resize in progress. Runs once per event-loop pass, after client requests are
    /// handled: send the wanted size if the client is caught up, and keep the window's fixed
    /// corner in place using the size the client really has.
    pub fn update_resize(&mut self) {
        let Some(resize) = self.resize.as_ref() else {
            return;
        };
        let toplevel = resize.toplevel.clone();
        let (edges, initial_pos, initial_size, released) = (
            resize.edges,
            resize.initial_pos,
            resize.initial_size,
            resize.released,
        );

        if !toplevel.wl_surface().alive() {
            self.resize = None;
            return;
        }

        let state = progress(&toplevel);
        if state.can_send {
            toplevel.send_pending_configure();
        }

        let is_it =
            |w: &smithay::desktop::Window| w.toplevel().map(|t| *t == toplevel).unwrap_or(false);
        if let Some(size) = self.desk.find_by(is_it).map(|e| e.window.geometry().size) {
            let pos = geom::anchored_pos(initial_pos, initial_size, (size.w, size.h), edges);
            self.desk.set_pos_where(is_it, pos);
        }

        if released && state.settled {
            self.resize = None;
        }
    }
}
